//! Project-relative file operations through directory descriptors. No shell.
use std::{
    ffi::{CString, OsString},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Change {
    pub from: Option<PathBuf>,
    pub to: PathBuf,
    pub directory: bool,
}
pub struct Created {
    pub changes: Vec<Change>,
    pub error: Option<String>,
}
/// Validate the entire expansion before touching disk, then create each path
/// exclusively. A later collision returns the completed paths, never rollback.
pub fn create_many(
    root: &Path,
    base: &Path,
    name: &str,
    directory: bool,
) -> Result<Created, String> {
    let names = crate::tree_expand::names(name)?;
    let base_parts = parts(base)?;
    let mut unique = std::collections::HashSet::new();
    for name in &names {
        let parts = parts(Path::new(name.trim_end_matches('/')))?;
        if parts.is_empty() || parts.len() + base_parts.len() > 64 {
            return Err("Destino expandido fuera de límites".into());
        }
        let relative: PathBuf = parts.iter().collect();
        if !unique.insert(relative) {
            return Err("La expansión repite un destino; no se creó nada".into());
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut changes = Vec::new();
    for name in names {
        let result = if std::time::Instant::now() > deadline {
            Err("Creación detenida por límite de tiempo".into())
        } else {
            create(root, base, &name, directory)
        };
        match result {
            Ok(change) => changes.push(change),
            Err(error) if changes.is_empty() => return Err(error),
            Err(error) => {
                return Ok(Created {
                    changes,
                    error: Some(error),
                });
            }
        }
    }
    Ok(Created {
        changes,
        error: None,
    })
}
pub(super) fn parts(path: &Path) -> Result<Vec<OsString>, String> {
    if path.as_os_str().len() > 4096 {
        return Err("Ruta demasiado larga".into());
    }
    let mut out = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name)
                if !name.as_bytes().iter().any(|b| *b < 32 || *b == 127) && name.len() <= 255 =>
            {
                out.push(name.to_owned())
            }
            _ => return Err("Usá una ruta relativa dentro del proyecto".into()),
        }
    }
    if out.len() > 64 {
        return Err("Demasiados niveles de carpetas".into());
    }
    Ok(out)
}
pub(super) fn cstring(name: &std::ffi::OsStr) -> Result<CString, String> {
    CString::new(name.as_bytes()).map_err(|_| "Nombre inválido".into())
}
pub(super) fn owned(fd: i32) -> Result<OwnedFd, String> {
    if fd < 0 {
        Err(io::Error::last_os_error().to_string())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}
pub(super) fn root_fd(root: &Path) -> Result<OwnedFd, String> {
    let name = cstring(root.as_os_str())?;
    owned(unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    })
}
pub(super) fn descend(
    mut directory: OwnedFd,
    components: &[OsString],
    create: bool,
) -> Result<OwnedFd, String> {
    for component in components {
        let name = cstring(component)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 && create && io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o777) };
            if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
                return Err(io::Error::last_os_error().to_string());
            }
            fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        }
        directory = owned(fd)?;
    }
    Ok(directory)
}
pub fn create(root: &Path, base: &Path, name: &str, directory: bool) -> Result<Change, String> {
    if name.is_empty() || name.len() > 1024 || Path::new(name).is_absolute() {
        return Err("Escribí un nombre relativo".into());
    }
    let directory = directory || name.ends_with('/');
    let trimmed = name.trim_end_matches('/');
    let names = parts(Path::new(trimmed))?;
    if names.is_empty() {
        return Err("El nombre no puede estar vacío".into());
    }
    let mut combined = parts(base)?;
    combined.extend(names);
    if combined.len() > 64 {
        return Err("Demasiados niveles de carpetas".into());
    }
    let parent = descend(root_fd(root)?, &combined[..combined.len() - 1], true)?;
    let leaf = cstring(combined.last().unwrap())?;
    if directory {
        if unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o777) } < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
    } else {
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let _file =
            owned(unsafe { libc::openat(parent.as_raw_fd(), leaf.as_ptr(), flags, 0o666) })?;
    }
    let relative: PathBuf = combined.iter().collect();
    Ok(Change {
        from: None,
        to: root.join(relative),
        directory,
    })
}
pub fn rename(root: &Path, source: &Path, name: &str) -> Result<Change, String> {
    let components = parts(source)?;
    let target = parts(Path::new(name))?;
    if components.is_empty() || target.len() != 1 || name.len() > 255 {
        return Err("Renombrar requiere un solo nombre dentro de la misma carpeta".into());
    }
    let parent = descend(root_fd(root)?, &components[..components.len() - 1], false)?;
    let from = cstring(components.last().unwrap())?;
    let to = cstring(&target[0])?;
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            from.as_ptr(),
            info.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    let mode = unsafe { info.assume_init() }.st_mode & libc::S_IFMT;
    if mode != libc::S_IFREG && mode != libc::S_IFDIR {
        return Err("No se renombran enlaces ni archivos especiales".into());
    }
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let result = {
        let _ = (&from, &to);
        return Err("Renombrado exclusivo disponible en macOS".into());
    };
    if result < 0 {
        return Err(io::Error::last_os_error().to_string());
    }
    let destination = source.parent().unwrap_or(Path::new("")).join(&target[0]);
    Ok(Change {
        from: Some(root.join(source)),
        to: root.join(destination),
        directory: mode == libc::S_IFDIR,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "tw-tree-edit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn create_nested_unicode_names_and_refuse_existing_destination() {
        let root = temp();
        let a = create(&root, Path::new(""), "src/ñ界.rs", false).unwrap();
        assert_eq!(std::fs::read(&a.to).unwrap(), b"");
        std::fs::write(&a.to, "conservar").unwrap();
        assert!(create(&root, Path::new("src"), "ñ界.rs", false).is_err());
        assert_eq!(std::fs::read_to_string(a.to).unwrap(), "conservar");
        assert!(
            create(&root, Path::new(""), "docs/", false)
                .unwrap()
                .directory
        );
        assert!(
            create(&root, Path::new(""), "more", true)
                .unwrap()
                .directory
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn paths_cannot_escape_through_parent_components_or_symlinks() {
        let root = temp();
        let outside = temp();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        for name in ["../outside", "/absolute", "link/file", "", "a\nb"] {
            assert!(create(&root, Path::new(""), name, false).is_err(), "{name}");
        }
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        assert!(create(&root, Path::new("../"), "file", false).is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
    #[test]
    fn batch_creation_validates_all_names_and_reports_partial_collisions() {
        let root = temp();
        assert!(create_many(&root, Path::new(""), "{valid,../escape}", false).is_err());
        assert!(!root.join("valid").exists());
        assert!(create_many(&root, Path::new(""), "{a,a}", false).is_err());
        assert!(create_many(&root, Path::new(""), "{1..257}", false).is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let created = create_many(&root, Path::new(""), "{ñ,界}/{01..03}.txt", false).unwrap();
        assert_eq!(created.changes.len(), 6);
        assert!(created.error.is_none());
        assert!(root.join("界/03.txt").is_file());
        std::fs::write(root.join("existing"), "retain").unwrap();
        let partial = create_many(&root, Path::new(""), "{new,existing,later}", false).unwrap();
        assert_eq!(partial.changes.len(), 1);
        assert!(partial.error.is_some());
        assert!(!root.join("later").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("existing")).unwrap(),
            "retain"
        );
        let directories = create_many(&root, Path::new(""), "dirs/{a,b}/", false).unwrap();
        assert!(directories.changes.iter().all(|c| c.directory));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn exclusive_rename_preserves_existing_files_and_works_for_directories() {
        let root = temp();
        std::fs::write(root.join("a"), "A").unwrap();
        std::fs::write(root.join("b"), "B").unwrap();
        assert!(rename(&root, Path::new("a"), "b").is_err());
        assert_eq!(std::fs::read_to_string(root.join("a")).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(root.join("b")).unwrap(), "B");
        rename(&root, Path::new("a"), "ñ").unwrap();
        assert!(!root.join("a").exists());
        assert_eq!(std::fs::read_to_string(root.join("ñ")).unwrap(), "A");
        std::fs::create_dir(root.join("dir")).unwrap();
        rename(&root, Path::new("dir"), "renamed").unwrap();
        assert!(root.join("renamed").is_dir());
        std::os::unix::fs::symlink(root.join("b"), root.join("link")).unwrap();
        assert!(rename(&root, Path::new("link"), "no").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
