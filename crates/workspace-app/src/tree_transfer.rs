//! Bounded, fd-relative tree copies and exclusive moves inside one project.
use crate::tree_edit::{Change, cstring, descend, owned, parts, root_fd};
use std::{
    ffi::{CStr, OsStr, OsString},
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, IntoRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn error() -> String {
    std::io::Error::last_os_error().to_string()
}
fn source_at(parent: &OwnedFd, name: &OsStr) -> Result<File, String> {
    let name = cstring(name)?;
    // NONBLOCK prevents a special file swapped into place from hanging open.
    let fd = owned(unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    })?;
    let file = File::from(fd);
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() && !meta.is_dir() {
        return Err("No se transfieren enlaces ni archivos especiales".into());
    }
    Ok(file)
}
fn publish(
    from_parent: &OwnedFd,
    from: &OsStr,
    to_parent: &OwnedFd,
    to: &OsStr,
) -> Result<(), String> {
    let (from, to) = (cstring(from)?, cstring(to)?);
    #[cfg(target_os = "macos")]
    {
        if unsafe {
            libc::renameatx_np(
                from_parent.as_raw_fd(),
                from.as_ptr(),
                to_parent.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        } < 0
        {
            return Err(error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (from_parent, to_parent, from, to);
        Err("Transferencia exclusiva disponible en macOS".into())
    }
}
fn endpoints(
    root: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(OwnedFd, OwnedFd, OsString, OsString, bool), String> {
    let from = parts(source)?;
    let to = parts(destination)?;
    if from.is_empty() || to.is_empty() || source == destination || destination.starts_with(source)
    {
        return Err("Elegí otro destino relativo dentro del proyecto".into());
    }
    let source_parent = descend(root_fd(root)?, &from[..from.len() - 1], false)?;
    let target_parent = descend(root_fd(root)?, &to[..to.len() - 1], false)?;
    let target_name = cstring(to.last().unwrap())?;
    let mut target_info = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            target_parent.as_raw_fd(),
            target_name.as_ptr(),
            target_info.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err("El destino ya existe; se conservó".into());
    }
    if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
        return Err(error());
    }
    let source_name = from.last().unwrap().clone();
    let directory = source_at(&source_parent, &source_name)?
        .metadata()
        .map_err(|e| e.to_string())?
        .is_dir();
    Ok((
        source_parent,
        target_parent,
        source_name,
        to.last().unwrap().clone(),
        directory,
    ))
}
pub fn move_path(root: &Path, source: &Path, destination: &Path) -> Result<Change, String> {
    let (from_parent, to_parent, from, to, directory) = endpoints(root, source, destination)?;
    publish(&from_parent, &from, &to_parent, &to)?;
    Ok(Change {
        from: Some(root.join(source)),
        to: root.join(destination),
        directory,
    })
}
struct Budget {
    entries: usize,
    bytes: u64,
    started: Instant,
}
impl Budget {
    fn check(&mut self, depth: usize) -> Result<(), String> {
        self.entries += 1;
        if depth > 64 || self.entries > 5000 || self.started.elapsed() > Duration::from_secs(10) {
            return Err("Copia excede 5000 entradas, 64 niveles o 10 segundos".into());
        }
        Ok(())
    }
}
struct Entries(*mut libc::DIR);
impl Drop for Entries {
    fn drop(&mut self) {
        unsafe {
            libc::closedir(self.0);
        }
    }
}
fn children(file: &File) -> Result<Vec<OsString>, String> {
    let cloned = file.try_clone().map_err(|e| e.to_string())?;
    let fd = cloned.into_raw_fd();
    let pointer = unsafe { libc::fdopendir(fd) };
    if pointer.is_null() {
        unsafe {
            libc::close(fd);
        }
        return Err(error());
    }
    let entries = Entries(pointer);
    let mut names = Vec::new();
    loop {
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(entries.0) };
        if entry.is_null() {
            if std::io::Error::last_os_error().raw_os_error().unwrap_or(0) != 0 {
                return Err(error());
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if names.len() == 5000 {
            return Err("Demasiadas entradas en una carpeta".into());
        }
        names.push(OsStr::from_bytes(name).to_owned());
    }
    Ok(names)
}
fn copy_entry(
    parent: &OwnedFd,
    name: &OsStr,
    target: &OwnedFd,
    leaf: &OsStr,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), String> {
    budget.check(depth)?;
    let mut input = source_at(parent, name)?;
    let metadata = input.metadata().map_err(|e| e.to_string())?;
    let leaf = cstring(leaf)?;
    if metadata.is_dir() {
        if unsafe { libc::mkdirat(target.as_raw_fd(), leaf.as_ptr(), 0o700) } < 0 {
            return Err(error());
        }
        let output = descend(
            target.try_clone().map_err(|e| e.to_string())?,
            &[OsStr::from_bytes(leaf.as_bytes()).to_owned()],
            false,
        )?;
        let source_directory = input.try_clone().map_err(|e| e.to_string())?.into();
        for child in children(&input)? {
            copy_entry(
                &source_directory,
                &child,
                &output,
                &child,
                depth + 1,
                budget,
            )?;
        }
        if unsafe { libc::fchmod(output.as_raw_fd(), metadata.mode() as libc::mode_t & 0o777) } < 0
        {
            return Err(error());
        }
    } else {
        if metadata.len() > 64 * 1024 * 1024 - budget.bytes {
            return Err("Copia excede 64 MiB totales".into());
        }
        let fd = owned(unsafe {
            libc::openat(
                target.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        let mut output = File::from(fd);
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if budget.started.elapsed() > Duration::from_secs(10) {
                return Err("Copia excede 10 segundos".into());
            }
            let read = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            budget.bytes += read as u64;
            if budget.bytes > 64 * 1024 * 1024 {
                return Err("Copia excede 64 MiB totales".into());
            }
            output
                .write_all(&buffer[..read])
                .map_err(|e| e.to_string())?;
        }
        output.sync_all().map_err(|e| e.to_string())?;
        if unsafe { libc::fchmod(output.as_raw_fd(), metadata.mode() as libc::mode_t & 0o777) } < 0
        {
            return Err(error());
        }
    }
    Ok(())
}
pub fn copy_path(root: &Path, source: &Path, destination: &Path) -> Result<Change, String> {
    copy_with_budget(
        root,
        source,
        destination,
        &mut Budget {
            entries: 0,
            bytes: 0,
            started: Instant::now(),
        },
    )
}
fn copy_with_budget(
    root: &Path,
    source: &Path,
    destination: &Path,
    budget: &mut Budget,
) -> Result<Change, String> {
    let (from_parent, to_parent, from, to, directory) = endpoints(root, source, destination)?;
    // Stage under a private sibling. Publish only after every entry succeeds.
    let name = format!(
        ".terminal-workspace-copy-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let staging = cstring(OsStr::new(&name))?;
    if unsafe { libc::mkdirat(to_parent.as_raw_fd(), staging.as_ptr(), 0o700) } < 0 {
        return Err(error());
    }
    let stage = descend(
        to_parent.try_clone().map_err(|e| e.to_string())?,
        &[OsString::from(&name)],
        false,
    )?;
    let result = copy_entry(
        &from_parent,
        &from,
        &stage,
        OsStr::new("payload"),
        0,
        budget,
    )
    .and_then(|()| publish(&stage, OsStr::new("payload"), &to_parent, &to));
    match result {
        Ok(()) => {
            // Only the empty staging directory is removed, never copied data.
            unsafe {
                libc::unlinkat(to_parent.as_raw_fd(), staging.as_ptr(), libc::AT_REMOVEDIR);
            }
            Ok(Change {
                from: None,
                to: root.join(destination),
                directory,
            })
        }
        Err(e) => {
            let partial: PathBuf = destination.parent().unwrap_or(Path::new("")).join(name);
            Err(format!(
                "{e}. Destino sin publicar; copia parcial privada en {}",
                root.join(partial).display()
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    /// Paths are relative to the explicitly chosen project root.
    pub source: PathBuf,
    pub destination: PathBuf,
    pub cut: bool,
}
pub struct Batch {
    pub changes: Vec<Change>,
    pub error: Option<String>,
}
/// Reject predictable collisions before publishing anything. Races still fail
/// exclusively at publication. A runtime failure preserves completed transfers.
pub fn transfer_many(root: &Path, transfers: &[Transfer]) -> Batch {
    let mut batch = Batch {
        changes: Vec::new(),
        error: None,
    };
    let preflight = || -> Result<(), String> {
        if transfers.is_empty() || transfers.len() > 128 {
            return Err("Elegí entre 1 y 128 archivos o carpetas".into());
        }
        for (i, transfer) in transfers.iter().enumerate() {
            endpoints(root, &transfer.source, &transfer.destination)?;
            for other in &transfers[..i] {
                if transfer.source.starts_with(&other.source)
                    || other.source.starts_with(&transfer.source)
                    || transfer.destination.starts_with(&other.destination)
                    || other.destination.starts_with(&transfer.destination)
                    || transfer.destination.starts_with(&other.source)
                    || other.source.starts_with(&transfer.destination)
                    || other.destination.starts_with(&transfer.source)
                    || transfer.source.starts_with(&other.destination)
                {
                    return Err(
                        "El lote contiene orígenes o destinos superpuestos; ajustá las marcas"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    };
    if let Err(error) = preflight() {
        batch.error = Some(error);
        return batch;
    }
    let mut budget = Budget {
        entries: 0,
        bytes: 0,
        started: Instant::now(),
    };
    for transfer in transfers {
        let result = budget.check(0).and_then(|()| {
            if transfer.cut {
                move_path(root, &transfer.source, &transfer.destination)
            } else {
                copy_with_budget(root, &transfer.source, &transfer.destination, &mut budget)
            }
        });
        match result {
            Ok(change) => batch.changes.push(change),
            Err(error) => {
                batch.error = Some(format!("{}: {error}", transfer.source.display()));
                break;
            }
        }
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        for _ in 0..32 {
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "tw-transfer-{}-{}-{sequence}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            match std::fs::create_dir(&root) {
                Ok(()) => return root,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private test fixture: {error}"),
            }
        }
        panic!("test fixture name budget exhausted; existing paths preserved");
    }
    #[test]
    fn batch_preflights_every_destination_and_rejects_overlapping_sources() {
        let root = temp();
        std::fs::create_dir(root.join("dest")).unwrap();
        for name in ["a", "b"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        let plan = vec![
            Transfer {
                source: "a".into(),
                destination: "dest/a".into(),
                cut: true,
            },
            Transfer {
                source: "b".into(),
                destination: "dest/b".into(),
                cut: false,
            },
        ];
        std::fs::write(root.join("dest/b"), "preservar").unwrap();
        let result = transfer_many(&root, &plan);
        assert!(result.error.is_some() && result.changes.is_empty());
        assert!(root.join("a").exists());
        assert!(!root.join("dest/a").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("dest/b")).unwrap(),
            "preservar"
        );
        std::fs::remove_file(root.join("dest/b")).unwrap();
        let result = transfer_many(&root, &plan);
        assert!(result.error.is_none());
        assert_eq!(result.changes.len(), 2);
        assert!(!root.join("a").exists() && root.join("b").exists());
        assert_eq!(std::fs::read_to_string(root.join("dest/a")).unwrap(), "a");
        assert_eq!(std::fs::read_to_string(root.join("dest/b")).unwrap(), "b");
        let overlapping = vec![
            Transfer {
                source: "dest".into(),
                destination: "copy".into(),
                cut: false,
            },
            Transfer {
                source: "dest/a".into(),
                destination: "other".into(),
                cut: false,
            },
        ];
        let result = transfer_many(&root, &overlapping);
        assert!(result.changes.is_empty() && result.error.is_some());
        assert!(!root.join("copy").exists());
        let duplicate = vec![
            Transfer {
                source: "dest/a".into(),
                destination: "same".into(),
                cut: false,
            },
            Transfer {
                source: "b".into(),
                destination: "same".into(),
                cut: false,
            },
        ];
        assert!(transfer_many(&root, &duplicate).changes.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn batch_runtime_error_reports_completed_transfers_without_rolling_back() {
        let root = temp();
        std::fs::write(root.join("a"), "A").unwrap();
        std::fs::create_dir(root.join("bad")).unwrap();
        std::os::unix::fs::symlink("../a", root.join("bad/link")).unwrap();
        let result = transfer_many(
            &root,
            &[
                Transfer {
                    source: "a".into(),
                    destination: "copied".into(),
                    cut: false,
                },
                Transfer {
                    source: "bad".into(),
                    destination: "bad-copy".into(),
                    cut: false,
                },
            ],
        );
        assert_eq!(result.changes.len(), 1);
        assert!(result.error.is_some());
        assert!(root.join("a").exists() && root.join("copied").exists());
        assert!(!root.join("bad-copy").exists());
        assert!(
            std::fs::symlink_metadata(root.join("bad/link"))
                .unwrap()
                .is_symlink()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn move_between_parents_and_copy_unicode_tree_without_replacing() {
        let root = temp();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir(root.join("dest")).unwrap();
        std::fs::write(root.join("src/nested/ñ.txt"), "texto界").unwrap();
        let c = copy_path(&root, Path::new("src"), Path::new("dest/copy")).unwrap();
        assert!(c.from.is_none() && c.directory);
        assert_eq!(
            std::fs::read_to_string(root.join("dest/copy/nested/ñ.txt")).unwrap(),
            "texto界"
        );
        assert!(copy_path(&root, Path::new("src"), Path::new("dest/copy")).is_err());
        assert!(move_path(&root, Path::new("src"), Path::new("dest/copy")).is_err());
        move_path(
            &root,
            Path::new("src/nested/ñ.txt"),
            Path::new("dest/moved.txt"),
        )
        .unwrap();
        assert!(!root.join("src/nested/ñ.txt").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("dest/moved.txt")).unwrap(),
            "texto界"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_symlinks_special_files_self_descendants_and_missing_parents() {
        let root = temp();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/a"), "A").unwrap();
        std::os::unix::fs::symlink("a", root.join("src/link")).unwrap();
        assert!(copy_path(&root, Path::new("src"), Path::new("copy")).is_err());
        assert!(!root.join("copy").exists());
        assert!(copy_path(&root, Path::new("src/link"), Path::new("linkcopy")).is_err());
        for dest in ["src/child", "../escape", "/absolute", "missing/file"] {
            assert!(move_path(&root, Path::new("src"), Path::new(dest)).is_err());
        }
        let fifo = cstring(root.join("fifo").as_os_str()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(copy_path(&root, Path::new("fifo"), Path::new("fifocopy")).is_err());
        assert_eq!(std::fs::read_to_string(root.join("src/a")).unwrap(), "A");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn oversized_file_is_not_published_and_partial_stage_stays_private() {
        let root = temp();
        File::create(root.join("large"))
            .unwrap()
            .set_len(65 * 1024 * 1024)
            .unwrap();
        let error = copy_path(&root, Path::new("large"), Path::new("target")).unwrap_err();
        assert!(error.contains("64 MiB") && error.contains("parcial privada"));
        assert!(!root.join("target").exists());
        let stage = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".terminal-workspace-copy-")
            })
            .unwrap();
        assert_eq!(std::fs::metadata(stage).unwrap().mode() & 0o777, 0o700);
        std::fs::remove_dir_all(root).unwrap();
    }
}
