//! App-owned state only. Project roots are remembered solely after explicit choice.
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
const MAX_ROOTS: usize = 128;
const MAX_REGISTRY_BYTES: usize = 32768;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn validate_roots(roots: &[PathBuf]) -> Result<(), String> {
    if roots.len() > MAX_ROOTS || roots.iter().any(|p| !p.is_absolute()) {
        return Err("Invalid explicit project registry".into());
    }
    Ok(())
}
fn registry_temp(state: &Path) -> Result<(PathBuf, fs::File), String> {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = state.join(format!(
            "projects-{}-{time}-{sequence}.tmp",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("No se pudo crear un temporal exclusivo para el registro".into())
}
/// Durable user state is separate from the short-lived Unix socket namespace.
/// Resolving paths does not inspect or migrate any existing user state.
pub struct Layout {
    pub state: PathBuf,
    pub runtime: PathBuf,
}
impl Layout {
    pub fn validation_instance(mut self, instance: &str) -> Result<Self, String> {
        if self.state != self.runtime
            || instance.len() != 8
            || !instance
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("Instancia de validación inválida".into());
        }
        self.runtime.set_file_name(format!(
            "{}-{instance}",
            self.runtime
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));
        self.state = self.runtime.clone();
        Ok(self)
    }
    pub fn resolve(home: Option<&Path>, username: &str, validation: bool) -> Result<Self, String> {
        let username: String = username
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if username.is_empty() {
            return Err("No se pudo determinar el usuario para el socket privado".into());
        }
        let runtime = PathBuf::from("/private/tmp").join(format!(
            "terminal-workspace-{}-{username}",
            if validation { "validation" } else { "native" }
        ));
        let state = if validation {
            runtime.clone()
        } else {
            let home = home
                .filter(|p| {
                    p.is_absolute()
                        && !p.components().any(|c| {
                            matches!(
                                c,
                                std::path::Component::ParentDir | std::path::Component::CurDir
                            )
                        })
                })
                .ok_or("HOME absoluto requerido para guardar el estado durable")?;
            home.join("Library/Application Support/Terminal Workspace Rust")
        };
        Ok(Self { state, runtime })
    }
}
pub fn private_directory(path: &Path) -> Result<(), String> {
    let uid = std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| e.to_string())?;
    let uid = String::from_utf8_lossy(&uid.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|e| e.to_string())?;
    if let Err(e) = fs::DirBuilder::new().mode(0o700).create(path)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(e.to_string());
    }
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !m.is_dir() || m.uid() != uid || m.mode() & 0o777 != 0o700 {
        return Err("El directorio de estado debe ser privado, propio y sin symlinks".into());
    }
    Ok(())
}
pub fn initialize(source: Option<&Path>, state: &Path) -> Result<Vec<PathBuf>, String> {
    private_directory(state)?;
    fn copy(source: &Path, dest: &Path, depth: usize) -> Result<(), String> {
        if depth > 8 {
            return Err("Demo directory depth exceeds limit".into());
        }
        if !fs::symlink_metadata(dest)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("Unsafe demo directory".into());
        }
        for entry in fs::read_dir(source).map_err(|e| e.to_string())?.take(100) {
            let e = entry.map_err(|e| e.to_string())?;
            let ty = e.file_type().map_err(|e| e.to_string())?;
            let target = dest.join(e.file_name());
            if ty.is_dir() {
                if !target.exists() {
                    fs::create_dir(&target).map_err(|e| e.to_string())?;
                }
                copy(&e.path(), &target, depth + 1)?;
            } else if ty.is_file() && !target.exists() {
                let bytes = fs::read(e.path()).map_err(|e| e.to_string())?;
                let mut out = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(target)
                    .map_err(|e| e.to_string())?;
                out.write_all(&bytes).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    if let Some(source) = source {
        let demo = state.join("demo");
        if !demo.exists() {
            fs::create_dir(&demo).map_err(|e| e.to_string())?;
        }
        copy(source, &demo, 0)?;
    }
    let path = state.join("projects.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let m = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !m.is_file() || m.nlink() != 1 || m.len() > MAX_REGISTRY_BYTES as u64 {
        return Err("Unsafe project registry".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_REGISTRY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_REGISTRY_BYTES {
        return Err("Project registry too large".into());
    }
    let roots: Vec<PathBuf> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate_roots(&roots)?;
    Ok(roots)
}
pub fn remember(state: &Path, roots: &[PathBuf]) -> Result<(), String> {
    private_directory(state)?;
    validate_roots(roots)?;
    let bytes = serde_json::to_vec(roots).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_REGISTRY_BYTES {
        return Err("Project registry too large".into());
    }
    let (path, mut out) = registry_temp(state)?;
    let result = (|| {
        out.write_all(&bytes).map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&path, state.join("projects.json")).map_err(|e| e.to_string())?;
        fs::File::open(state)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_registry_temp_does_not_block_later_writes_or_get_deleted() {
        let dir =
            std::env::temp_dir().join(format!("tw-storage-temp-collision-{}", std::process::id()));
        private_directory(&dir).unwrap();
        let interrupted = dir.join(format!("projects-{}.tmp", std::process::id()));
        fs::write(&interrupted, "synthetic interrupted evidence").unwrap();
        let roots = vec![PathBuf::from("/explicit/new")];
        assert!(
            remember(&dir, &roots).is_ok(),
            "a preserved PID temp blocks all later registry writes"
        );
        assert_eq!(initialize(None, &dir).unwrap(), roots);
        assert_eq!(
            fs::read_to_string(interrupted).unwrap(),
            "synthetic interrupted evidence"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn relative_registry_roots_cannot_replace_last_readable_registry() {
        let dir =
            std::env::temp_dir().join(format!("tw-storage-invalid-roots-{}", std::process::id()));
        private_directory(&dir).unwrap();
        let roots = vec![PathBuf::from("/explicit/old")];
        remember(&dir, &roots).unwrap();
        let before = fs::read(dir.join("projects.json")).unwrap();
        let result = remember(&dir, &[PathBuf::from("relative")]);
        assert!(
            result.is_err(),
            "registry writer accepts relative roots rejected on startup"
        );
        assert_eq!(fs::read(dir.join("projects.json")).unwrap(), before);
        assert_eq!(initialize(None, &dir).unwrap(), roots);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn registry_limits_and_repeated_writes_leave_only_complete_private_json() {
        let dir = std::env::temp_dir().join(format!("tw-storage-limits-{}", std::process::id()));
        private_directory(&dir).unwrap();
        let roots: Vec<PathBuf> = (0..MAX_ROOTS)
            .map(|i| format!("/explicit/{i}").into())
            .collect();
        remember(&dir, &roots).unwrap();
        assert_eq!(initialize(None, &dir).unwrap(), roots);
        let before = fs::read(dir.join("projects.json")).unwrap();
        assert!(remember(&dir, &vec![PathBuf::from("/explicit/a"); MAX_ROOTS + 1]).is_err());
        assert!(
            remember(
                &dir,
                &[PathBuf::from(format!(
                    "/{}",
                    "x".repeat(MAX_REGISTRY_BYTES)
                ))]
            )
            .is_err()
        );
        assert_eq!(fs::read(dir.join("projects.json")).unwrap(), before);
        for i in 0..8 {
            let roots = vec![PathBuf::from(format!("/explicit/round-{i}"))];
            remember(&dir, &roots).unwrap();
            assert_eq!(initialize(None, &dir).unwrap(), roots);
        }
        let m = fs::metadata(dir.join("projects.json")).unwrap();
        assert_eq!(m.mode() & 0o077, 0);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn overlapping_writes_use_distinct_temps_and_publish_one_complete_submission() {
        let dir = std::env::temp_dir().join(format!("tw-storage-overlap-{}", std::process::id()));
        private_directory(&dir).unwrap();
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|i| {
                    let dir = &dir;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        remember(dir, &[PathBuf::from(format!("/explicit/submission-{i}"))])
                    })
                })
                .collect();
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
        });
        let roots = initialize(None, &dir).unwrap();
        assert_eq!(roots.len(), 1);
        assert!((0..4).any(|i| roots[0] == Path::new(&format!("/explicit/submission-{i}"))));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn persistent_state_and_socket_paths_are_separate_without_migration() {
        let home = Path::new("/Users/synthetic");
        let layout = Layout::resolve(Some(home), "synthetic", false).unwrap();
        assert_eq!(
            layout.state,
            home.join("Library/Application Support/Terminal Workspace Rust")
        );
        assert_eq!(
            layout.runtime,
            Path::new("/private/tmp/terminal-workspace-native-synthetic")
        );
        let validation = Layout::resolve(None, "synthetic", true).unwrap();
        assert_eq!(validation.state, validation.runtime);
        let validation = validation.validation_instance("abcd1234").unwrap();
        assert!(
            validation
                .state
                .ends_with("terminal-workspace-validation-synthetic-abcd1234")
        );
        assert_eq!(validation.state, validation.runtime);
        assert!(
            Layout::resolve(None, "synthetic", true)
                .unwrap()
                .validation_instance("../bad")
                .is_err()
        );
        assert!(Layout::resolve(None, "synthetic", false).is_err());
        assert!(Layout::resolve(Some(Path::new("relative")), "synthetic", false).is_err());
    }
    #[test]
    fn normal_initialization_never_seeds_demo_or_discards_explicit_roots() {
        let dir = std::env::temp_dir().join(format!("tw-storage-normal-{}", std::process::id()));
        assert!(initialize(None, &dir).unwrap().is_empty());
        assert!(!dir.join("demo").exists());
        let roots = vec![PathBuf::from("/explicit/example")];
        remember(&dir, &roots).unwrap();
        assert_eq!(initialize(None, &dir).unwrap(), roots);
        assert!(!dir.join("demo").exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn remembers_explicit_roots_and_keeps_demo_edits_outside_bundle() {
        let dir = std::env::temp_dir().join(format!("tw-storage-test-{}", std::process::id()));
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Resources/Demo/projects");
        assert!(initialize(Some(&source), &dir).unwrap().is_empty());
        let file = dir.join("demo/workspace/src/main.rs");
        fs::write(&file, "local edit").unwrap();
        let roots = vec![PathBuf::from("/explicit/example")];
        remember(&dir, &roots).unwrap();
        assert_eq!(initialize(Some(&source), &dir).unwrap(), roots);
        assert_eq!(fs::read_to_string(file).unwrap(), "local edit");
        assert_ne!(
            fs::read_to_string(source.join("workspace/src/main.rs")).unwrap(),
            "local edit"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
