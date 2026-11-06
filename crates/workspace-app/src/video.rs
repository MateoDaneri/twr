//! One bounded local decoder/player. IO, decode and presentation clock stay off UI.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
const FPS: usize = 12;
enum Input {
    Local { root: PathBuf, source: PathBuf },
    Attachment(String),
}

#[derive(Clone, Copy)]
pub enum Control {
    Toggle,
    Seek(i32),
    Restart,
}

pub struct Update {
    pub revision: u64,
    pub message: String,
    pub frame: Option<image::RgbaImage>, // BGRA for GPUI, at most one pending raster.
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
}
impl Default for Update {
    fn default() -> Self {
        Self {
            revision: 1,
            message: "Preparando video local… · q cancela".into(),
            frame: None,
            position: 0.,
            duration: 0.,
            paused: true,
        }
    }
}
pub struct Player {
    controls: mpsc::SyncSender<Control>,
    state: Arc<Mutex<Update>>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Player {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Player {
    pub fn start(
        helper: PathBuf,
        root: PathBuf,
        source: PathBuf,
        cache: PathBuf,
        decoder: PathBuf,
        wake: async_channel::Sender<()>,
    ) -> Result<Self, String> {
        Self::start_input(helper, Input::Local { root, source }, cache, decoder, wake)
    }
    pub fn start_attachment(
        helper: PathBuf,
        url: String,
        cache: PathBuf,
        decoder: PathBuf,
        wake: async_channel::Sender<()>,
    ) -> Result<Self, String> {
        if !crate::pr_video::eligible_url(&url) {
            return Err("Video: adjunto no permitido".into());
        }
        Self::start_input(helper, Input::Attachment(url), cache, decoder, wake)
    }
    fn start_input(
        helper: PathBuf,
        input: Input,
        cache: PathBuf,
        decoder: PathBuf,
        wake: async_channel::Sender<()>,
    ) -> Result<Self, String> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("Video anterior cerrándose; reintentá en un momento".into());
        }
        let (controls, receive) = mpsc::sync_channel(8);
        let mut initial = Update::default();
        if matches!(&input, Input::Attachment(_)) {
            initial.message = "Descargando adjunto del PR… · q cancela".into();
        }
        let state = Arc::new(Mutex::new(initial));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            struct ActiveGuard;
            impl Drop for ActiveGuard {
                fn drop(&mut self) {
                    ACTIVE.store(false, Ordering::Release);
                }
            }
            let _active = ActiveGuard;
            let result = (|| {
                let job = Job::new(&cache)?;
                let mut preparation = match input {
                    Input::Local { root, source } => {
                        Preparation::start(&helper, &root, &source, &decoder, &job.0)?
                    }
                    Input::Attachment(url) => {
                        Preparation::attachment(&helper, &url, &decoder, &job.0)?
                    }
                };
                playback(
                    &job.0,
                    &mut preparation,
                    &receive,
                    &worker_state,
                    &worker_cancel,
                    &wake,
                )
            })();
            if let Err(message) = result
                && !worker_cancel.load(Ordering::Acquire)
            {
                let mut state = worker_state.lock().unwrap();
                state.revision += 1;
                state.message = message;
                state.paused = true;
                drop(state);
                let _ = wake.try_send(());
            }
        });
        Ok(Self {
            controls,
            state,
            cancel,
        })
    }
    pub fn control(&self, control: Control) -> bool {
        self.controls.try_send(control).is_ok()
    }
    pub fn update(&self, seen: u64) -> Option<Update> {
        let mut state = self.state.lock().unwrap();
        if state.revision == seen {
            return None;
        }
        Some(Update {
            revision: state.revision,
            message: state.message.clone(),
            frame: state.frame.take(),
            position: state.position,
            duration: state.duration,
            paused: state.paused,
        })
    }
}

const OWNED: &[u8] = b"terminal-workspace-video-job-v2\n";
const CACHE_BUDGET: u64 = 512 * 1024 * 1024;
// Remote jobs can hold the download, its decoder copy and 256 MiB of frames.
const JOB_RESERVATION: u64 = 464 * 1024 * 1024;
const STALE: Duration = Duration::from_secs(24 * 60 * 60);

fn exclusive(file: &File) -> bool {
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

fn owned_marker(path: &Path) -> Option<File> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.join(".owned"))
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::getuid() }
        || meta.nlink() != 1
        || meta.permissions().mode() & 0o077 != 0
        || meta.len() != OWNED.len() as u64
    {
        return None;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    (bytes == OWNED).then_some(file)
}

fn job_size(path: &Path, depth: usize, entries: &mut usize) -> Option<u64> {
    if depth > 4 {
        return None;
    }
    let mut bytes = 0u64;
    for item in fs::read_dir(path).ok()? {
        *entries += 1;
        if *entries > 4096 {
            return None;
        }
        let path = item.ok()?.path();
        let meta = path.symlink_metadata().ok()?;
        if meta.uid() != unsafe { libc::getuid() } || meta.file_type().is_symlink() {
            return None;
        }
        bytes = bytes.checked_add(if meta.is_dir() {
            job_size(&path, depth + 1, entries)?
        } else if meta.is_file() && meta.nlink() == 1 {
            meta.len()
        } else {
            return None;
        })?;
    }
    Some(bytes)
}

fn reclaim(cache: &Path, now: SystemTime, budget: u64, reservation: u64) -> Result<(), String> {
    // Caller holds the directory flock through admission and creation. A job
    // holds its marker flock until its worker has stopped and removed its files.
    // Legacy v1 markers cannot identify a live lease, so they are never evicted.
    let mut retained = 0u64;
    let mut abandoned = Vec::new();
    for (index, item) in fs::read_dir(cache)
        .map_err(|_| "Video: no se pudo revisar cache")?
        .enumerate()
    {
        if index >= 512 {
            return Err(
                "Video: demasiadas entradas en cache; no se borró contenido desconocido".into(),
            );
        }
        let path = item.map_err(|_| "Video: cache no disponible")?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(parts) = name
            .strip_prefix("video-job-")
            .and_then(|name| name.split_once('-'))
        else {
            continue;
        };
        if parts.0.parse::<u32>().is_err() || parts.1.parse::<u128>().is_err() {
            continue;
        }
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        if !meta.is_dir()
            || meta.uid() != unsafe { libc::getuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            continue;
        }
        let Some(lease) = owned_marker(&path) else {
            continue;
        };
        if !exclusive(&lease) {
            retained = retained.saturating_add(reservation);
            continue;
        }
        let Some(bytes) = job_size(&path, 0, &mut 0) else {
            return Err(
                "Video: cache propia contiene entradas inesperadas; se conserva para revisión"
                    .into(),
            );
        };
        let modified = meta.modified().unwrap_or(now);
        retained = retained.saturating_add(bytes);
        abandoned.push((modified, bytes, path, lease));
    }
    abandoned.sort_by_key(|job| job.0);
    for (modified, bytes, path, _lease) in abandoned {
        if now.duration_since(modified).unwrap_or_default() >= STALE
            || retained.saturating_add(reservation) > budget
        {
            fs::remove_dir_all(&path)
                .map_err(|_| "Video: no se pudo retirar cache propia abandonada")?;
            retained = retained.saturating_sub(bytes);
        }
    }
    if retained.saturating_add(reservation) > budget {
        return Err("Video: otro reproductor ocupa la cache; cerralo y reintentá".into());
    }
    Ok(())
}

struct Job(PathBuf, File);
impl Job {
    fn new(cache: &Path) -> Result<Self, String> {
        let meta = cache
            .symlink_metadata()
            .map_err(|_| "Video: cache no disponible")?;
        if cache.canonicalize().ok().as_deref() != Some(cache)
            || !meta.is_dir()
            || meta.uid() != unsafe { libc::getuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err("Video: cache debe ser un directorio privado".into());
        }
        let admission = File::open(cache).map_err(|_| "Video: cache no disponible")?;
        if !exclusive(&admission) {
            return Err("Video: cache ocupada; reintentá en un momento".into());
        }
        reclaim(cache, SystemTime::now(), CACHE_BUDGET, JOB_RESERVATION)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = cache.join(format!("video-job-{}-{stamp}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "Video: no se pudo crear cache")?;
        // The marker identifies only files generated by this player, never input.
        let mut lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path.join(".owned"))
            .map_err(|_| "Video: no se pudo identificar cache")?;
        if !exclusive(&lease) {
            return Err("Video: no se pudo reservar cache propia".into());
        }
        lease
            .write_all(OWNED)
            .map_err(|_| "Video: no se pudo identificar cache")?;
        Ok(Self(path, lease))
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        if self.0.canonicalize().ok().as_ref() == Some(&self.0)
            && self.1.metadata().is_ok_and(|meta| meta.nlink() == 1)
            && fs::read(self.0.join(".owned")).is_ok_and(|b| b == OWNED)
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

struct Preparation {
    child: Child,
    started: Instant,
    complete: bool,
    timeout: Duration,
}
impl Preparation {
    fn start(
        helper: &Path,
        root: &Path,
        source: &Path,
        decoder: &Path,
        job: &Path,
    ) -> Result<Self, String> {
        let mut command = Command::new("/usr/bin/python3");
        command
            .args(["-I", "-S"])
            .arg(helper)
            .arg(source)
            .arg(root)
            .arg(job)
            .arg(decoder)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", job)
            .env("TMPDIR", job)
            .env("TW_VIDEO_PARENT_PID", std::process::id().to_string());
        Self::launch(command, job, None, Duration::from_secs(140))
    }
    fn attachment(helper: &Path, url: &str, decoder: &Path, job: &Path) -> Result<Self, String> {
        let mut command = crate::pr_video::authenticated_helper(helper);
        command
            .arg("download")
            .arg(job)
            .arg(decoder)
            .env("TMPDIR", job);
        let input = serde_json::to_vec(&serde_json::json!({"url":url}))
            .map_err(|_| "Video: adjunto inválido")?;
        Self::launch(command, job, Some(&input), Duration::from_secs(220))
    }
    fn launch(
        mut command: Command,
        job: &Path,
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let error = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(job.join("helper.log"))
            .map_err(|_| "Video: no se pudo abrir log privado")?;
        let child = command
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(error)
            .process_group(0)
            .spawn()
            .map_err(|_| "Video: helper no disponible")?;
        let mut preparation = Self {
            child,
            started: Instant::now(),
            complete: false,
            timeout,
        };
        if let Some(input) = input {
            preparation
                .child
                .stdin
                .take()
                .unwrap()
                .write_all(input)
                .map_err(|_| "Video: no se pudo iniciar descarga")?;
        }
        Ok(preparation)
    }
    fn poll(&mut self, job: &Path) -> Result<bool, String> {
        if self.complete {
            return Ok(true);
        }
        if self.started.elapsed() > self.timeout {
            return Err("Video: preparación fuera de tiempo".into());
        }
        let Some(status) = self
            .child
            .try_wait()
            .map_err(|_| "Video: no se pudo supervisar decoder")?
        else {
            return Ok(false);
        };
        if status.success() {
            self.complete = true;
            return Ok(true);
        }
        Err(preparation_error(job))
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        // Only the process group created by this worker. No process lookup.
        // A completed helper waited for its children. Do not signal its old
        // process-group ID after a long playback, when the ID could be reused.
        if !self.complete && matches!(self.child.try_wait(), Ok(None)) {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.wait();
    }
}
fn preparation_error(job: &Path) -> String {
    let mut bytes = Vec::new();
    if let Ok(file) = fs::File::open(job.join("helper.log")) {
        let _ = file.take(1001).read_to_end(&mut bytes);
    }
    let message = String::from_utf8_lossy(&bytes);
    if message.starts_with("Video:") && bytes.len() <= 1000 {
        message.trim().to_owned()
    } else {
        "Video: no se pudo preparar el archivo".into()
    }
}

#[derive(serde::Deserialize)]
struct Manifest {
    schema: u8,
    fps: usize,
    frames: usize,
    duration: f64,
    cache_bytes: u64,
}
fn manifest(job: &Path, complete: bool) -> Result<Option<Manifest>, String> {
    let path = job.join(if complete {
        "manifest.json"
    } else {
        "progress.json"
    });
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if !complete && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Video: metadata ausente".into()),
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err("Video: metadata inválida".into());
    }
    let mut data = Vec::new();
    file.take(4097)
        .read_to_end(&mut data)
        .map_err(|_| "Video: metadata no disponible")?;
    if data.len() > 4096 {
        return Err("Video: metadata fuera de límites".into());
    }
    let m: Manifest = serde_json::from_slice(&data).map_err(|_| "Video: metadata inválida")?;
    if m.schema != 1
        || m.fps != FPS
        || !(1..=2160).contains(&m.frames)
        || !m.duration.is_finite()
        || m.duration <= 0.
        || m.duration > 180.
        || m.cache_bytes > 256 * 1024 * 1024
    {
        return Err("Video: metadata fuera de límites".into());
    }
    Ok(Some(m))
}
fn frame(job: &Path, index: usize) -> Result<image::RgbaImage, String> {
    let path = job.join(format!("frames/frame-{:06}.png", index + 1));
    let meta = path
        .symlink_metadata()
        .map_err(|_| "Video: cuadro ausente")?;
    if !meta.is_file() || meta.len() > 8 * 1024 * 1024 {
        return Err("Video: cuadro fuera de límites".into());
    }
    let mut reader = image::ImageReader::open(path).map_err(|_| "Video: cuadro no disponible")?;
    reader.set_format(image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1280);
    limits.max_image_height = Some(1280);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let mut image = reader
        .decode()
        .map_err(|_| "Video: PNG inválido o fuera de límites")?
        .into_rgba8();
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(image)
}
fn seek(index: usize, seconds: i32, frames: usize) -> usize {
    (index as i64 + seconds as i64 * FPS as i64).clamp(0, frames.saturating_sub(1) as i64) as usize
}
fn playback(
    job: &Path,
    preparation: &mut Preparation,
    commands: &mpsc::Receiver<Control>,
    state: &Mutex<Update>,
    cancel: &AtomicBool,
    wake: &async_channel::Sender<()>,
) -> Result<(), String> {
    let mut index = 0;
    let mut paused = false;
    let mut dirty = true;
    let mut next = Instant::now() + Duration::from_secs_f64(1. / FPS as f64);
    let mut frames = 0;
    let mut duration = 0.;
    let mut final_manifest = false;
    let mut seek_target: Option<usize> = None;
    let mut was_buffering = false;
    while !cancel.load(Ordering::Acquire) {
        let complete = preparation.poll(job)?;
        if !final_manifest && let Some(manifest) = manifest(job, complete)? {
            if manifest.frames < frames || (duration > 0. && manifest.duration != duration) {
                return Err("Video: progreso inconsistente".into());
            }
            frames = manifest.frames;
            duration = manifest.duration;
            final_manifest = complete;
        }
        if frames == 0 {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        if let Some(target) = seek_target
            && (target < frames || complete)
        {
            index = target.min(frames - 1);
            seek_target = None;
            dirty = true;
            next = Instant::now() + Duration::from_secs_f64(1. / FPS as f64);
        }
        let buffering = seek_target.is_some() || (!paused && index + 1 >= frames && !complete);
        dirty |= buffering != was_buffering;
        was_buffering = buffering;
        if dirty {
            let pixels = frame(job, index)?;
            let mut state = state.lock().unwrap();
            state.revision += 1;
            state.frame = Some(pixels);
            state.position = index as f64 / FPS as f64;
            state.duration = duration;
            state.paused = paused;
            state.message = if buffering {
                "Preparando más cuadros"
            } else if paused {
                "Pausado"
            } else {
                "Reproduciendo"
            }
            .into();
            drop(state);
            let _ = wake.try_send(());
            dirty = false;
        }
        let wait = if paused || buffering {
            Duration::from_millis(20)
        } else {
            next.saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20))
        };
        match commands.recv_timeout(wait) {
            Ok(control) => {
                match control {
                    Control::Toggle => paused = !paused,
                    Control::Seek(seconds) => {
                        let target = seek(
                            seek_target.unwrap_or(index),
                            seconds,
                            (duration * FPS as f64).ceil() as usize,
                        );
                        if target < frames || complete {
                            index = target.min(frames - 1);
                            seek_target = None;
                        } else {
                            seek_target = Some(target);
                        }
                    }
                    Control::Restart => {
                        index = 0;
                        paused = false;
                        seek_target = None;
                    }
                }
                next = Instant::now() + Duration::from_secs_f64(1. / FPS as f64);
                dirty = true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout)
                if !paused && !buffering && Instant::now() >= next =>
            {
                if index + 1 < frames {
                    index += 1;
                } else if complete {
                    paused = true;
                }
                next = Instant::now() + Duration::from_secs_f64(1. / FPS as f64);
                dirty = true;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cache_fixture(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(format!(
            "/private/tmp/tw-video-cache-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    }
    fn abandoned(cache: &Path, number: usize, bytes: u64) -> PathBuf {
        let path = cache.join(format!("video-job-1-{number}"));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        let mut marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path.join(".owned"))
            .unwrap();
        marker.write_all(OWNED).unwrap();
        File::create(path.join("generated.png"))
            .unwrap()
            .set_len(bytes)
            .unwrap();
        path
    }
    #[test]
    fn cache_admission_protects_live_lease_and_evicts_only_unlocked_owned_jobs() {
        let cache = cache_fixture("lease");
        let live = Job::new(&cache).unwrap();
        let old = abandoned(&cache, 1, 200);
        assert!(reclaim(&cache, SystemTime::now(), 1024, 900).is_err());
        assert!(live.0.is_dir(), "live job was removed");
        assert!(
            !old.exists(),
            "unlocked owned cache was not reclaimed under pressure"
        );
        drop(live);
        let old = abandoned(&cache, 2, 200);
        reclaim(&cache, SystemTime::now(), 1024, 900).unwrap();
        assert!(!old.exists());
        fs::remove_dir(cache).unwrap();
    }
    #[test]
    fn cache_reclaims_expired_jobs_but_preserves_legacy_unknown_and_symlink_entries() {
        let cache = cache_fixture("stale");
        let outside = cache_fixture("outside");
        fs::write(outside.join("evidence"), b"preserve").unwrap();
        let old = abandoned(&cache, 1, 1);
        let legacy = cache.join("video-job-1-2");
        fs::DirBuilder::new().mode(0o700).create(&legacy).unwrap();
        fs::write(legacy.join(".owned"), b"terminal-workspace-video-job-v1\n").unwrap();
        fs::write(cache.join("unrelated.log"), b"preserve").unwrap();
        std::os::unix::fs::symlink(&outside, cache.join("video-job-1-3")).unwrap();
        reclaim(
            &cache,
            SystemTime::now() + STALE + Duration::from_secs(1),
            CACHE_BUDGET,
            JOB_RESERVATION,
        )
        .unwrap();
        assert!(!old.exists());
        assert!(legacy.is_dir());
        assert_eq!(fs::read(cache.join("unrelated.log")).unwrap(), b"preserve");
        assert_eq!(fs::read(outside.join("evidence")).unwrap(), b"preserve");
        assert!(cache.join("video-job-1-3").is_symlink());
        fs::remove_dir_all(cache).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
    #[test]
    fn cache_admission_excludes_concurrent_creation_and_preserves_unexpected_owned_content() {
        let cache = cache_fixture("guards");
        let admission = File::open(&cache).unwrap();
        assert!(exclusive(&admission));
        assert!(Job::new(&cache).is_err());
        drop(admission);
        let suspicious = abandoned(&cache, 1, 1);
        std::os::unix::fs::symlink("/", suspicious.join("unexpected")).unwrap();
        assert!(Job::new(&cache).is_err());
        assert!(suspicious.is_dir());
        assert!(suspicious.join("unexpected").is_symlink());
        let fifo = cache.join("video-job-1-2");
        fs::DirBuilder::new().mode(0o700).create(&fifo).unwrap();
        let marker =
            std::ffi::CString::new(fifo.join(".owned").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(marker.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert!(owned_marker(&fifo).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        fs::remove_dir_all(cache).unwrap();
    }
    #[test]
    fn seeks_saturate_and_never_leave_decoded_frames() {
        assert_eq!(seek(0, -5, 24), 0);
        assert_eq!(seek(0, 5, 24), 23);
        assert_eq!(seek(120, -5, 360), 60);
        assert_eq!(seek(120, 5, 360), 180);
        assert_eq!(seek(0, i32::MIN, 24), 0);
        assert_eq!(seek(0, i32::MAX, 24), 23);
    }
    #[test]
    fn explicit_decoder_integration_plays_pauses_seeks_restarts_and_cleans_owned_cache() {
        let Some(decoder) = std::env::var_os("TERMINAL_WORKSPACE_VIDEO_TEST_DECODER") else {
            return;
        };
        let decoder = PathBuf::from(decoder);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../Resources/Fixtures")
            .canonicalize()
            .unwrap();
        let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../Resources/Helpers/prepare_video.py");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cache = PathBuf::from(format!(
            "/private/tmp/tw-video-worker-test-{}-{stamp}",
            std::process::id()
        ));
        fs::DirBuilder::new().mode(0o700).create(&cache).unwrap();
        let (wake, _) = async_channel::bounded(1);
        let player = Player::start(
            helper.clone(),
            root.clone(),
            root.join("synthetic-video.mov"),
            cache.clone(),
            decoder.clone(),
            wake,
        )
        .unwrap();
        fn wait(player: &Player, seen: &mut u64, predicate: impl Fn(&Update) -> bool) -> Update {
            let start = Instant::now();
            loop {
                if let Some(update) = player.update(*seen) {
                    *seen = update.revision;
                    if predicate(&update) {
                        return update;
                    }
                    assert!(!update.message.starts_with("Video:"), "{}", update.message);
                }
                assert!(
                    start.elapsed() < Duration::from_secs(15),
                    "player update timed out"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let mut seen = 0;
        let first = wait(&player, &mut seen, |u| u.frame.is_some());
        assert_eq!(first.frame.as_ref().unwrap().dimensions(), (320, 180));
        assert_eq!(first.duration, 2.);
        assert!(!first.paused);
        let next = wait(&player, &mut seen, |u| u.position > first.position);
        assert_ne!(first.frame.unwrap().as_raw(), next.frame.unwrap().as_raw());
        assert!(player.control(Control::Toggle));
        let paused = wait(&player, &mut seen, |u| u.paused);
        std::thread::sleep(Duration::from_millis(220));
        assert!(
            player.update(seen).is_none(),
            "paused player kept publishing frames"
        );
        assert!(player.control(Control::Seek(5)));
        let last = wait(&player, &mut seen, |u| u.position > paused.position);
        assert_eq!(last.position, 23. / 12.);
        assert!(last.paused);
        assert!(player.control(Control::Restart));
        let restart = wait(&player, &mut seen, |u| u.position == 0. && !u.paused);
        assert!(restart.frame.is_some());
        drop(player);
        let start = Instant::now();
        while ACTIVE.load(Ordering::Acquire) {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "player did not stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_dir(&cache).unwrap().count(),
            0,
            "owned job cache was retained"
        );
        for _ in 0..3 {
            let (wake, _) = async_channel::bounded(1);
            let reopened = Player::start(
                helper.clone(),
                root.clone(),
                root.join("synthetic-video.mov"),
                cache.clone(),
                decoder.clone(),
                wake,
            )
            .unwrap();
            let mut seen = 0;
            assert!(
                wait(&reopened, &mut seen, |u| u.frame.is_some())
                    .frame
                    .is_some()
            );
            drop(reopened);
            let started = Instant::now();
            while ACTIVE.load(Ordering::Acquire) {
                assert!(
                    started.elapsed() < Duration::from_secs(3),
                    "reopened player did not stop"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(fs::read_dir(&cache).unwrap().count(), 0);
        }
        // Deliberately slow synthetic producer. The first frame must be visible
        // before completion, a seek must wait for future frames, and closing
        // while the producer is sleeping must stop it before cache removal.
        let slow = cache.join("slow.py");
        fs::write(
            &slow,
            r#"
import json, shutil, sys, time
from pathlib import Path
job, assets = Path(sys.argv[3]), Path(sys.argv[4])
frames = job / 'frames'
frames.mkdir()
def publish(count, final=False):
    name = 'manifest.json' if final else 'progress.json'
    data = {'schema':1,'fps':12,'frames':count,'duration':1.,'cache_bytes':10000}
    part = job / (name + '.tmp')
    part.write_text(json.dumps(data))
    part.replace(job / name)
shutil.copyfile(assets / 'test.png', frames / 'frame-000001.png')
publish(1)
time.sleep(.7)
for i in range(2,13):
    shutil.copyfile(assets / 'test.png', frames / ('frame-%06d.png' % i))
publish(12)
time.sleep(.5)
(assets / 'late-write').write_text('producer completed')
publish(12, True)
"#,
        )
        .unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .save(cache.join("test.png"))
            .unwrap();
        let player_cache = cache.join("player");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&player_cache)
            .unwrap();
        let (wake, _) = async_channel::bounded(1);
        let progressive = Player::start(
            slow.clone(),
            root.clone(),
            root.join("synthetic-video.mov"),
            player_cache.clone(),
            cache.clone(),
            wake,
        )
        .unwrap();
        let mut seen = 0;
        let first = wait(&progressive, &mut seen, |u| u.frame.is_some());
        let job = fs::read_dir(&player_cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(
            !job.join("manifest.json").exists(),
            "first frame waited for completion"
        );
        assert_eq!(first.position, 0.);
        assert_eq!(first.message, "Preparando más cuadros");
        progressive.control(Control::Toggle);
        wait(&progressive, &mut seen, |u| u.paused);
        progressive.control(Control::Seek(5));
        let future = wait(&progressive, &mut seen, |u| u.position > 0.);
        assert_eq!(future.position, 11. / 12.);
        assert!(future.paused, "seeking unpaused the player");
        progressive.control(Control::Restart);
        wait(&progressive, &mut seen, |u| u.position == 0. && !u.paused);
        drop(progressive);
        let started = Instant::now();
        while ACTIVE.load(Ordering::Acquire) {
            assert!(started.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read_dir(&player_cache).unwrap().count(), 0);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            !cache.join("late-write").exists(),
            "cancelled producer kept running"
        );
        fs::remove_dir(player_cache).unwrap();
        fs::remove_file(slow).unwrap();
        fs::remove_file(cache.join("test.png")).unwrap();
        fs::remove_dir(cache).unwrap();
    }
}
