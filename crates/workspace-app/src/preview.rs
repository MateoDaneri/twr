//! Local image/document previews. Only explicit project files enter a renderer.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const JOB_MARKER: &str = "terminal-workspace-preview-v1\n";

/// The source image is borrowed; only a freshly created renderer job is owned.
#[derive(Debug)]
pub struct DisplayImage {
    path: PathBuf,
    _job: Option<PreviewJob>,
}
impl DisplayImage {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn project(root: &Path, path: &Path) -> Result<Self, String> {
        Ok(Self {
            path: check_image(root, path)?,
            _job: None,
        })
    }
}
#[derive(Debug)]
struct PreviewJob {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl PreviewJob {
    fn create(path: PathBuf) -> Result<Self, String> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| e.to_string())?;
        let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
        let job = Self {
            path,
            device: meta.dev(),
            inode: meta.ino(),
        };
        let mut marker = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(job.path.join(".owned-preview"))
            .map_err(|e| e.to_string())?;
        marker
            .write_all(JOB_MARKER.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(job)
    }
    fn cleanup(path: &Path, device: u64, inode: u64) {
        // Never sweep the runtime directory or follow a replacement job/marker.
        let Ok(meta) = path.symlink_metadata() else {
            return;
        };
        if !meta.is_dir()
            || meta.dev() != device
            || meta.ino() != inode
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
        {
            return;
        }
        let Ok(mut marker) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path.join(".owned-preview"))
        else {
            return;
        };
        let Ok(meta) = marker.metadata() else { return };
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || meta.len() != JOB_MARKER.len() as u64
        {
            return;
        }
        let mut contents = String::new();
        if marker.read_to_string(&mut contents).is_ok() && contents == JOB_MARKER {
            let _ = fs::remove_dir_all(path);
        }
    }
}
impl Drop for PreviewJob {
    fn drop(&mut self) {
        let path = self.path.clone();
        let device = self.device;
        let inode = self.inode;
        // TeX may produce several files; close and stale-result handling stay off UI.
        let _ = std::thread::Builder::new()
            .name("preview-cleanup".into())
            .spawn(move || {
                PreviewJob::cleanup(&path, device, inode);
            });
    }
}
pub fn check_image(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if path.canonicalize().map_err(|e| e.to_string())? != path || !path.starts_with(root) {
        return Err("Imagen fuera del proyecto o symlink".into());
    }
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
        return Err("Imagen supera 16 MiB".into());
    }
    let (w, h) = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    if w == 0 || h == 0 || w as u64 * h as u64 > 40_000_000 {
        return Err("Imagen supera 40 megapíxeles".into());
    }
    Ok(path.into())
}
pub fn document(
    helper: &Path,
    state: &Path,
    root: &Path,
    path: &Path,
    kind: &str,
) -> Result<DisplayImage, String> {
    if !matches!(kind, "html" | "tex") {
        return Err("Tipo de preview desconocido".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let actual = path.canonicalize().map_err(|e| e.to_string())?;
    if actual != path
        || !path.starts_with(&root)
        || path.metadata().map_err(|e| e.to_string())?.len() > 2 * 1024 * 1024
    {
        return Err("Documento fuera de límites".into());
    }
    let out = state.join(format!(
        "preview-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let job = PreviewJob::create(out.clone())?;
    let output = helper_output(
        helper,
        &[
            kind.as_ref(),
            path.as_os_str(),
            root.as_os_str(),
            out.as_os_str(),
        ],
        100,
    )?;
    let png = PathBuf::from(output.trim());
    if png != out.join("preview.png") {
        return Err("Ruta de salida inválida".into());
    }
    Ok(DisplayImage {
        path: check_image(&out, &png)?,
        _job: Some(job),
    })
}
pub fn prepare_tex(helper: &Path, state: &Path) -> Result<PathBuf, String> {
    let output = helper_output(helper, &[state.as_os_str()], 270)?;
    let assets = PathBuf::from(output.trim());
    if assets.parent() != Some(state.join("tex-assets").as_path())
        || !assets
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("assets-"))
        || assets.canonicalize().map_err(|e| e.to_string())? != assets
    {
        return Err("Ruta de recursos inválida".into());
    }
    Ok(assets)
}
fn helper_output(helper: &Path, args: &[&std::ffi::OsStr], seconds: u64) -> Result<String, String> {
    let mut child = Command::new("/usr/bin/python3")
        .args(["-I", "-S"])
        .arg(helper)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    fn read(stream: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = stream.take(65537).read_to_end(&mut b);
            b
        })
    }
    let stdout = read(child.stdout.take().unwrap());
    let stderr = read(child.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Helper excedió {seconds} segundos"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = stdout.join().unwrap_or_default();
    let error = stderr.join().unwrap_or_default();
    if !status.success() {
        return Err(String::from_utf8_lossy(&error).chars().take(1000).collect());
    }
    String::from_utf8(output).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tw-preview-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        path.canonicalize().unwrap()
    }
    fn wait_removed(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !path.exists(),
            "owned preview was retained: {}",
            path.display()
        );
    }
    #[test]
    fn discarded_generated_result_removes_only_owned_job() {
        let root = fixture();
        let job_path = root.join("preview-job");
        let job = PreviewJob::create(job_path.clone()).unwrap();
        fs::write(job_path.join("preview.png"), b"generated fixture").unwrap();
        fs::write(root.join("unrelated"), b"preserve").unwrap();
        let result: Result<DisplayImage, String> = Ok(DisplayImage {
            path: job_path.join("preview.png"),
            _job: Some(job),
        });
        drop(result); // Same path as an obsolete asynchronous result or closed view.
        wait_removed(&job_path);
        assert_eq!(fs::read(root.join("unrelated")).unwrap(), b"preserve");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn image_source_is_never_owned() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../Resources/Demo/projects/workspace")
            .canonicalize()
            .unwrap();
        let path = root.join("docs/palette.png");
        let before = fs::read(&path).unwrap();
        drop(DisplayImage::project(&root, &path).unwrap());
        assert_eq!(fs::read(path).unwrap(), before);
    }
    #[test]
    fn missing_or_symlink_marker_preserves_directory() {
        let root = fixture();
        fs::write(root.join("outside-marker"), JOB_MARKER).unwrap();
        for symlink in [false, true] {
            let path = root.join(if symlink { "linked" } else { "unmarked" });
            let job = PreviewJob::create(path.clone()).unwrap();
            fs::remove_file(path.join(".owned-preview")).unwrap();
            if symlink {
                std::os::unix::fs::symlink(
                    root.join("outside-marker"),
                    path.join(".owned-preview"),
                )
                .unwrap();
            }
            PreviewJob::cleanup(&path, job.device, job.inode);
            assert!(path.is_dir());
            // Retain ownership until this test removes its own fixtures.
            std::mem::drop(job);
        }
        assert_eq!(
            fs::read_to_string(root.join("outside-marker")).unwrap(),
            JOB_MARKER
        );
        // Both asynchronous cleanups validate markers before touching any entries.
        std::thread::sleep(Duration::from_millis(30));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn renderer_failure_removes_owned_job_and_preserves_source() {
        let root = fixture();
        let state = root.join("state");
        fs::create_dir(&state).unwrap();
        let source = root.join("input.html");
        fs::write(&source, b"synthetic unchanged").unwrap();
        let helper = root.join("fail.py");
        fs::write(&helper, "import sys\nsys.exit(2)\n").unwrap();
        assert!(document(&helper, &state, &root, &source, "html").is_err());
        let deadline = Instant::now() + Duration::from_secs(3);
        while fs::read_dir(&state).unwrap().next().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(fs::read_dir(&state).unwrap().next().is_none());
        assert_eq!(fs::read(source).unwrap(), b"synthetic unchanged");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_escape_and_oversized_dimensions() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../Resources/Demo/projects/workspace")
            .canonicalize()
            .unwrap();
        assert!(check_image(&root, &root.join("../../../../Contracts/keymaps.json")).is_err());
        let png = root.join("docs/palette.png");
        assert_eq!(check_image(&root, &png).unwrap(), png);
    }
}
