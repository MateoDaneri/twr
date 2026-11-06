//! Opaque in-memory buffer IDs and exclusive first writes. No file at the ID.
use std::{
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
/// Resolve the common Neovim edit forms without consulting shell expansion.
pub fn edit_path(cwd: &Path, home: Option<&Path>, name: &str) -> Result<PathBuf, String> {
    // Ex escapes protect path metacharacters; preserve unknown escapes so a
    // literal backslash in a filename is not silently removed.
    let mut decoded = String::new();
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && chars
                .peek()
                .is_some_and(|next| matches!(next, '#' | '%' | ' ' | '\\'))
        {
            decoded.push(chars.next().expect("peeked character"));
        } else {
            decoded.push(c);
        }
    }
    let name = decoded.as_str();
    if name == "~" || name.starts_with("~/") {
        let home = home.ok_or("No se encontró HOME")?;
        return Ok(if name == "~" {
            home.to_path_buf()
        } else {
            home.join(&name[2..])
        });
    }
    let path = PathBuf::from(name);
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}
pub fn id(root: &Path) -> PathBuf {
    root.join(format!(
        ".twr-unnamed-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
/// Validate and normalize before checking the open-buffer registry or writing.
pub fn destination(root: &Path, name: &str) -> Result<PathBuf, String> {
    let relative = Path::new(name);
    let relative = if relative.is_absolute() {
        relative
            .strip_prefix(root)
            .map_err(|_| "Destino fuera de la raíz")?
    } else {
        relative
    };
    let parts = crate::tree_edit::parts(relative)?;
    if parts.is_empty() || !root.is_absolute() {
        return Err("Destino fuera de límites".into());
    }
    Ok(root.join(parts.iter().collect::<PathBuf>()))
}
/// Resolve a named edit target without creating anything. A dangling symlink is
/// considered existing so a later exclusive write can never replace it.
pub fn edit_target(path: &Path) -> Result<(PathBuf, bool), String> {
    let leaf = path.file_name().ok_or("Ruta sin nombre de archivo")?;
    let parent = path.parent().ok_or("Ruta sin carpeta")?;
    let parent = match parent.canonicalize() {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A missing directory may be created later by the user. Resolve
            // the existing prefix now and keep only unambiguous components.
            if !parent.is_absolute()
                || parent
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err("Ruta ambigua con carpeta inexistente".into());
            }
            let mut ancestor = parent;
            let mut suffix = Vec::new();
            loop {
                match std::fs::symlink_metadata(ancestor) {
                    Ok(_) => {
                        let mut resolved = ancestor.canonicalize().map_err(|e| e.to_string())?;
                        for part in suffix.iter().rev() {
                            resolved.push(part);
                        }
                        break resolved;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        suffix.push(
                            ancestor
                                .file_name()
                                .ok_or("Ruta sin carpeta")?
                                .to_os_string(),
                        );
                        ancestor = ancestor.parent().ok_or("Ruta sin carpeta")?;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
        }
        Err(error) => return Err(error.to_string()),
    };
    let target = destination(&parent, &leaf.to_string_lossy())?;
    let missing = match std::fs::symlink_metadata(&target) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.to_string()),
    };
    Ok((target, missing))
}

/// Publish complete bytes at a new destination without replacing any entry.
pub fn write(root: &Path, name: &str, text: &str) -> Result<PathBuf, String> {
    let target = destination(root, name)?;
    let relative = target
        .strip_prefix(root)
        .map_err(|_| "Destino fuera de la raíz")?;
    let parts = crate::tree_edit::parts(relative)?;
    if text.len() > 8 * 1024 * 1024 {
        return Err("Destino o texto fuera de límites".into());
    }
    let parent = crate::tree_edit::descend(
        crate::tree_edit::root_fd(root)?,
        &parts[..parts.len() - 1],
        false,
    )?;
    let leaf = crate::tree_edit::cstring(parts.last().unwrap())?;
    let temp_name = format!(
        ".twr-write-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let temp = crate::tree_edit::cstring(std::ffi::OsStr::new(&temp_name))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let result = (|| {
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(text.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        if unsafe {
            libc::linkat(
                parent.as_raw_fd(),
                temp.as_ptr(),
                parent.as_raw_fd(),
                leaf.as_ptr(),
                0,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(target)
    })();
    unsafe {
        libc::unlinkat(parent.as_raw_fd(), temp.as_ptr(), 0);
    }
    if result.is_ok() {
        std::fs::File::from(parent).sync_all().map_err(|e| {
            format!("Archivo publicado, pero falló sincronizar el directorio: {e}. El buffer se conserva")
        })?;
    }
    result
}

/// Write a separate new file without renaming or marking the source buffer
/// clean. Existing files and symlinks are never replaced by this operation.
pub fn write_copy(
    cwd: &Path,
    home: Option<&Path>,
    name: &str,
    text: &str,
) -> Result<PathBuf, String> {
    write_copy_bounded(cwd, home, name, text, None)
}

/// When an allowed root is supplied, anchor the final write to that directory
/// and walk its children without following symlinks, including across races.
pub fn write_copy_bounded(
    cwd: &Path,
    home: Option<&Path>,
    name: &str,
    text: &str,
    allowed_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let requested = edit_path(cwd, home, name)?;
    let (target, missing) = edit_target(&requested)?;
    if !missing {
        return Err("E13: El archivo ya existe".into());
    }
    if let Some(root) = allowed_root {
        let relative = target
            .strip_prefix(root)
            .map_err(|_| "Validación: archivos limitados a fixtures")?;
        let relative = relative.to_str().ok_or("Ruta inválida")?;
        return write(root, relative, text);
    }
    let parent = target.parent().ok_or("Ruta sin carpeta")?;
    let leaf = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("Ruta inválida")?;
    write(parent, leaf, text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copy_write_creates_new_target_and_preserves_existing_files() {
        let root = std::env::temp_dir().join(format!("tw-copy-write-{}", std::process::id()));
        crate::storage::private_directory(&root).unwrap();
        let source = root.join("source.txt");
        std::fs::write(&source, "original").unwrap();
        let target = write_copy(&root, Some(&root), "~/copy\\ file.txt", "nuevo ñ\n").unwrap();
        assert_eq!(target, root.join("copy file.txt"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "nuevo ñ\n");
        assert!(write_copy(&root, Some(&root), "source.txt", "overwrite").is_err());
        assert_eq!(std::fs::read_to_string(source).unwrap(), "original");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn save_as_from_missing_source_does_not_create_or_replace_original() {
        let root = std::env::temp_dir().join(format!("tw-saveas-missing-{}", std::process::id()));
        crate::storage::private_directory(&root).unwrap();
        let original = root.join("missing.txt");
        let (_, missing) = edit_target(&original).unwrap();
        assert!(missing);
        write_copy(&root, Some(&root), "copy.txt", "changed\n").unwrap();
        assert!(!original.exists());
        assert!(edit_target(&original).unwrap().1);
        std::fs::write(&original, "created by another process\n").unwrap();
        assert!(write(&root, "missing.txt", "replacement\n").is_err());
        assert_eq!(
            std::fs::read_to_string(&original).unwrap(),
            "created by another process\n"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn first_save_accepts_absolute_path_outside_project_without_overwriting() {
        let root = std::env::temp_dir().join(format!("tw-first-absolute-{}", std::process::id()));
        crate::storage::private_directory(&root).unwrap();
        let project = root.join("project");
        let other = root.join("other");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&other).unwrap();
        let target = other.join("first file.txt");
        let result = write_copy(
            &project,
            Some(&root),
            target.to_str().unwrap(),
            "synthetic\n",
        )
        .unwrap();
        assert_eq!(result, target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "synthetic\n");
        assert!(write_copy(&project, Some(&root), target.to_str().unwrap(), "replace\n").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "synthetic\n");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bounded_copy_cannot_escape_via_absolute_path_or_symlink() {
        let root = std::env::temp_dir().join(format!("tw-bounded-copy-{}", std::process::id()));
        crate::storage::private_directory(&root).unwrap();
        let fixtures = root.join("fixtures");
        let outside = root.join("outside");
        std::fs::create_dir(&fixtures).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, fixtures.join("link")).unwrap();
        assert!(
            write_copy_bounded(
                &fixtures,
                Some(&fixtures),
                outside.join("x").to_str().unwrap(),
                "x",
                Some(&fixtures)
            )
            .is_err()
        );
        assert!(
            write_copy_bounded(&fixtures, Some(&fixtures), "link/x", "x", Some(&fixtures)).is_err()
        );
        assert!(!outside.join("x").exists());
        assert!(
            write_copy_bounded(
                &fixtures,
                Some(&fixtures),
                "safe.txt",
                "ok",
                Some(&fixtures)
            )
            .is_ok()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn edit_paths_expand_home_and_keep_relative_paths_in_cwd() {
        let cwd = Path::new("/synthetic/project");
        let home = Path::new("/synthetic/home");
        assert_eq!(
            edit_path(cwd, Some(home), "~/notes/a b.md").unwrap(),
            home.join("notes/a b.md")
        );
        assert_eq!(
            edit_path(cwd, Some(home), "a.md").unwrap(),
            cwd.join("a.md")
        );
        assert_eq!(
            edit_path(cwd, Some(home), "/other/a.md").unwrap(),
            PathBuf::from("/other/a.md")
        );
        assert!(edit_path(cwd, None, "~/a.md").is_err());
        assert_eq!(edit_path(cwd, None, "\\#").unwrap(), cwd.join("#"));
        assert_eq!(
            edit_path(cwd, None, "file\\ name").unwrap(),
            cwd.join("file name")
        );
        assert_eq!(
            edit_path(cwd, None, "back\\q").unwrap(),
            cwd.join("back\\q")
        );
    }
    #[test]
    fn concurrent_first_writes_publish_one_complete_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("tw-first-race-{}", std::process::id()));
        crate::storage::private_directory(&root).unwrap();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
        let workers = (0..4)
            .map(|i| {
                let root = root.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    let text = format!("writer-{i}:{}", "ñ".repeat(65536));
                    gate.wait();
                    (text.clone(), write(&root, "same.txt", &text))
                })
            })
            .collect::<Vec<_>>();
        let results = workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|(_, r)| r.is_ok()).count(), 1);
        let winner = results.iter().find(|(_, r)| r.is_ok()).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("same.txt")).unwrap(),
            winner.0
        );
        assert_eq!(
            std::fs::metadata(root.join("same.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert!(write(&root, "missing/new.txt", "preserve").is_err());
        assert!(!root.join("missing").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn named_edit_does_not_create_or_overwrite_a_racing_file() {
        let root = std::env::temp_dir().join(format!("tw-edit-new-test-{}", std::process::id()));
        crate::storage::private_directory(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let target = root.join("nuevo ñ.txt");
        assert_eq!(edit_target(&target).unwrap(), (target.clone(), true));
        assert!(!target.exists());
        std::fs::write(&target, "external").unwrap();
        assert!(write(&root, "nuevo ñ.txt", "buffer").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "external");
        assert_eq!(edit_target(&target).unwrap(), (target.clone(), false));
        let link = root.join("dangling");
        std::os::unix::fs::symlink(root.join("absent"), &link).unwrap();
        assert_eq!(edit_target(&link).unwrap(), (link, false));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_parent_keeps_a_buffer_without_creating_directories_or_following_new_links() {
        let root = std::env::temp_dir().join(format!("tw-edit-parent-test-{}", std::process::id()));
        crate::storage::private_directory(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let target = root.join("missing/nested/file.txt");
        assert_eq!(edit_target(&target).unwrap(), (target.clone(), true));
        assert!(!root.join("missing").exists());
        let name = target.to_str().unwrap();
        assert!(write(Path::new("/"), name, "synthetic").is_err());
        assert!(!root.join("missing").exists());
        let outside = root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(outside.join("nested")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("missing")).unwrap();
        assert!(write(Path::new("/"), name, "synthetic").is_err());
        assert!(!outside.join("nested/file.txt").exists());
        std::fs::remove_file(root.join("missing")).unwrap();
        std::fs::create_dir_all(root.join("missing/nested")).unwrap();
        write(Path::new("/"), name, "synthetic").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "synthetic");
        assert!(write(Path::new("/"), name, "overwrite").is_err());
        let dangling = root.join("dangling");
        std::os::unix::fs::symlink(root.join("absent"), &dangling).unwrap();
        assert!(edit_target(&dangling.join("file")).is_err());
        assert!(edit_target(&root.join("absent/../file")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn ids_never_create_files_and_writes_preserve_conflicts_and_links() {
        let root = std::env::temp_dir().join(format!("tw-unnamed-test-{}", std::process::id()));
        crate::storage::private_directory(&root).unwrap();
        let a = id(&root);
        let b = id(&root);
        assert_ne!(a, b);
        assert!(!a.exists());
        assert_eq!(
            destination(&root, "nested/./file.txt").unwrap(),
            root.join("nested/file.txt")
        );
        assert!(destination(&root, "").is_err());
        assert!(destination(&root, "/outside/file.txt").is_err());
        let path = write(&root, "new.txt", "hello ñ\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello ñ\n");
        assert!(write(&root, "new.txt", "replace").is_err());
        assert!(write(&root, "../outside", "x").is_err());
        std::os::unix::fs::symlink(&path, root.join("link")).unwrap();
        assert!(write(&root, "link", "replace").is_err());
        std::os::unix::fs::symlink(&root, root.join("dirlink")).unwrap();
        assert!(write(&root, "dirlink/escape", "replace").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "hello ñ\n");
        std::fs::remove_dir_all(root).unwrap();
    }
}
