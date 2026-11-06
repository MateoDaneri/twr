//! Recoverable macOS Trash operations. Never purge or replace a destination.
use crate::tree_edit::{cstring, descend, parts, root_fd};
use std::{
    io,
    os::fd::AsRawFd,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Receipt {
    pub root: PathBuf,
    pub original: PathBuf,
    pub trashed: PathBuf,
    pub directory: bool,
    device: u64,
    inode: u64,
}

fn source(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let names = parts(relative)?;
    if names.is_empty() || root.canonicalize().map_err(|e| e.to_string())? != root {
        return Err(
            "La papelera requiere un destino dentro de una raíz explícita sin symlinks".into(),
        );
    }
    let parent = descend(root_fd(root)?, &names[..names.len() - 1], false)?;
    let leaf = cstring(names.last().unwrap())?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(io::Error::last_os_error().to_string());
    }
    let stat = unsafe { stat.assume_init() };
    if !matches!(stat.st_mode & libc::S_IFMT, libc::S_IFREG | libc::S_IFDIR) {
        return Err("La papelera admite archivos regulares y carpetas, sin symlinks".into());
    }
    let path = root.join(relative);
    let canonical = path.canonicalize().map_err(|e| e.to_string())?;
    if canonical != path {
        // Foundation can restore the same APFS filename in decomposed Unicode.
        // Parents were opened with O_NOFOLLOW and the leaf checked with
        // AT_SYMLINK_NOFOLLOW above. Accept only another spelling in that exact
        // parent which still identifies the inspected object, never a new target.
        if canonical.parent() != path.parent() {
            return Err("El destino contiene un symlink".into());
        }
        let resolved = std::fs::symlink_metadata(&canonical).map_err(|e| e.to_string())?;
        if resolved.dev() != stat.st_dev as u64
            || resolved.ino() != stat.st_ino
            || (!resolved.is_file() && !resolved.is_dir())
        {
            return Err("El destino cambió durante la verificación; se conservó".into());
        }
    }
    Ok(canonical)
}

fn trash_with(
    root: &Path,
    relative: &Path,
    backend: impl FnOnce(&Path) -> Result<PathBuf, String>,
) -> Result<Receipt, String> {
    let original = source(root, relative)?;
    let trashed = backend(&original)?;
    if !trashed.is_absolute() || trashed.as_os_str().len() > 4096 {
        return Err(
            "macOS devolvió una ruta de papelera inválida; revisá la papelera del sistema".into(),
        );
    }
    let info = std::fs::symlink_metadata(&trashed).map_err(|e| {
        format!(
            "macOS movió el archivo a {}; no se pudo registrar undo: {e}",
            trashed.display()
        )
    })?;
    if !info.is_file() && !info.is_dir() {
        return Err(format!(
            "Tipo inesperado después del traslado a {}; no se intentó otra operación",
            trashed.display()
        ));
    }
    Ok(Receipt {
        root: root.into(),
        original,
        trashed,
        directory: info.is_dir(),
        device: info.dev(),
        inode: info.ino(),
    })
}

pub fn trash(root: &Path, relative: &Path) -> Result<Receipt, String> {
    trash_with(root, relative, platform::trash)
}

pub struct Batch {
    pub receipts: Vec<Receipt>,
    pub error: Option<String>,
}
pub fn trash_many(root: &Path, relatives: &[PathBuf]) -> Batch {
    trash_many_with(root, relatives, platform::trash)
}
fn trash_many_with(
    root: &Path,
    relatives: &[PathBuf],
    mut backend: impl FnMut(&Path) -> Result<PathBuf, String>,
) -> Batch {
    let mut batch = Batch {
        receipts: Vec::new(),
        error: None,
    };
    let preflight = || -> Result<(), String> {
        if relatives.is_empty() || relatives.len() > 128 {
            return Err("Elegí entre 1 y 128 orígenes para la papelera".into());
        }
        let mut originals: Vec<PathBuf> = Vec::new();
        for relative in relatives {
            let path = source(root, relative)?;
            if originals
                .iter()
                .any(|other| path.starts_with(other) || other.starts_with(&path))
            {
                return Err("La selección contiene padres e hijos o entradas duplicadas; ajustá el rango o las marcas".into());
            }
            originals.push(path);
        }
        Ok(())
    };
    if let Err(error) = preflight() {
        batch.error = Some(error);
        return batch;
    }
    let started = std::time::Instant::now();
    for relative in relatives {
        if started.elapsed() > std::time::Duration::from_secs(10) {
            batch.error = Some(
                "Lote detenido al alcanzar 10 segundos; recibos completados conservados".into(),
            );
            break;
        }
        match trash_with(root, relative, |path| backend(path)) {
            Ok(receipt) => batch.receipts.push(receipt),
            Err(error) => {
                batch.error = Some(format!("{}: {error}", relative.display()));
                break;
            }
        }
    }
    batch
}

pub fn restore(receipt: &Receipt) -> Result<(), String> {
    let relative = receipt
        .original
        .strip_prefix(&receipt.root)
        .map_err(|_| "Destino original fuera del proyecto")?;
    let names = parts(relative)?;
    if names.is_empty() || receipt.root.canonicalize().map_err(|e| e.to_string())? != receipt.root {
        return Err("Raíz original cambió; el archivo sigue en la papelera".into());
    }
    let _destination_parent = descend(root_fd(&receipt.root)?, &names[..names.len() - 1], false)?;
    let info = std::fs::symlink_metadata(&receipt.trashed)
        .map_err(|e| format!("No se pudo verificar el elemento conocido de papelera: {e}"))?;
    if info.dev() != receipt.device
        || info.ino() != receipt.inode
        || (!info.is_file() && !info.is_dir())
    {
        return Err("El elemento de papelera cambió; se conservó".into());
    }
    match std::fs::symlink_metadata(&receipt.original) {
        Ok(_) => return Err("El destino original ya existe; no se sobrescribió".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    platform::restore(&receipt.trashed, &receipt.original)?;
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "native"))]
#[allow(unexpected_cfgs)]
mod platform {
    use super::*;
    use objc::{
        class, msg_send,
        runtime::{BOOL, Object, YES},
        sel, sel_impl,
    };
    use std::ffi::CStr;
    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {}
    struct Pool(*mut Object);
    impl Drop for Pool {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![self.0, drain];
            }
        }
    }
    fn url(path: &Path) -> Result<*mut Object, String> {
        let text = path.to_str().ok_or("La ruta debe ser UTF-8")?;
        unsafe {
            let string: *mut Object = msg_send![class!(NSString), alloc];
            let string: *mut Object =
                msg_send![string,initWithBytes:text.as_ptr() length:text.len() encoding:4usize];
            if string.is_null() {
                return Err("No se pudo construir la ruta de macOS".into());
            }
            let url: *mut Object = msg_send![class!(NSURL),fileURLWithPath:string];
            let _: () = msg_send![string, release];
            if url.is_null() {
                return Err("No se pudo construir la URL de macOS".into());
            }
            Ok(url)
        }
    }
    pub fn restore(from: &Path, to: &Path) -> Result<(), String> {
        unsafe {
            let _pool = Pool(msg_send![class!(NSAutoreleasePool), new]);
            let from_url = url(from)?;
            let to_url = url(to)?;
            let manager: *mut Object = msg_send![class!(NSFileManager), new];
            let manager: *mut Object = msg_send![manager, autorelease];
            let mut error: *mut Object = std::ptr::null_mut();
            let ok: BOOL = msg_send![manager,moveItemAtURL:from_url toURL:to_url error:&mut error];
            if ok != YES {
                if error.is_null() {
                    return Err("macOS rechazó la restauración".into());
                }
                let description: *mut Object = msg_send![error, localizedDescription];
                let bytes: *const std::ffi::c_char = msg_send![description, UTF8String];
                return Err(if bytes.is_null() {
                    "Error de restauración".into()
                } else {
                    CStr::from_ptr(bytes)
                        .to_string_lossy()
                        .chars()
                        .take(512)
                        .collect()
                });
            }
            Ok(())
        }
    }
    pub fn trash(path: &Path) -> Result<PathBuf, String> {
        let text = path.to_str().ok_or("La ruta debe ser UTF-8")?;
        unsafe {
            let pool = Pool(msg_send![class!(NSAutoreleasePool), new]);
            let string: *mut Object = msg_send![class!(NSString), alloc];
            let string: *mut Object =
                msg_send![string,initWithBytes:text.as_ptr() length:text.len() encoding:4usize];
            if string.is_null() {
                return Err("No se pudo construir la ruta de macOS".into());
            }
            let url: *mut Object = msg_send![class!(NSURL),fileURLWithPath:string];
            let _: () = msg_send![string, release];
            let manager: *mut Object = msg_send![class!(NSFileManager), new];
            let manager: *mut Object = msg_send![manager, autorelease];
            let mut result: *mut Object = std::ptr::null_mut();
            let mut error: *mut Object = std::ptr::null_mut();
            let ok: BOOL =
                msg_send![manager,trashItemAtURL:url resultingItemURL:&mut result error:&mut error];
            if ok != YES {
                let detail = if error.is_null() {
                    "macOS rechazó el traslado".into()
                } else {
                    let description: *mut Object = msg_send![error, localizedDescription];
                    let bytes: *const std::ffi::c_char = msg_send![description, UTF8String];
                    if bytes.is_null() {
                        "Error de papelera".into()
                    } else {
                        CStr::from_ptr(bytes)
                            .to_string_lossy()
                            .chars()
                            .take(512)
                            .collect::<String>()
                    }
                };
                return Err(detail);
            }
            if result.is_null() {
                return Err(
                    "macOS completó el traslado sin ruta para undo; revisá la papelera del sistema"
                        .into(),
                );
            }
            let path: *mut Object = msg_send![result, path];
            let bytes: *const std::ffi::c_char = msg_send![path, UTF8String];
            if bytes.is_null() {
                return Err("macOS completó el traslado sin ruta UTF-8 para undo".into());
            }
            let output = PathBuf::from(CStr::from_ptr(bytes).to_string_lossy().as_ref());
            drop(pool);
            Ok(output)
        }
    }
}
#[cfg(not(all(target_os = "macos", feature = "native")))]
mod platform {
    use super::*;
    pub fn restore(from: &Path, to: &Path) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            let from = cstring(from.as_os_str())?;
            let to = cstring(to.as_os_str())?;
            if unsafe {
                libc::renameatx_np(
                    libc::AT_FDCWD,
                    from.as_ptr(),
                    libc::AT_FDCWD,
                    to.as_ptr(),
                    libc::RENAME_EXCL,
                )
            } < 0
            {
                return Err(io::Error::last_os_error().to_string());
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (from, to);
            Err("Restauración disponible solo en macOS".into())
        }
    }
    pub fn trash(_: &Path) -> Result<PathBuf, String> {
        Err("Papelera nativa disponible solo en la app macOS".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn batch_fixture() -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "tw-trash-batch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let bin = root.join("fixture-bin");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&bin).unwrap();
        (root, project, bin)
    }
    #[test]
    fn trash_batch_preflight_rejects_overlap_missing_source_and_excess_without_backend_calls() {
        let (root, project, _) = batch_fixture();
        std::fs::create_dir(project.join("folder")).unwrap();
        std::fs::write(project.join("folder/a"), "A").unwrap();
        for paths in [
            vec![PathBuf::from("folder"), PathBuf::from("folder/a")],
            vec![PathBuf::from("folder/a"), PathBuf::from("missing")],
            vec![PathBuf::from("folder/a"); 129],
        ] {
            let result = trash_many_with(&project, &paths, |_| {
                panic!("preflight must not invoke backend")
            });
            assert!(result.receipts.is_empty() && result.error.is_some());
            assert_eq!(
                std::fs::read_to_string(project.join("folder/a")).unwrap(),
                "A"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn trash_batch_retains_receipts_after_partial_failure_and_each_original_is_restorable() {
        let (root, project, bin) = batch_fixture();
        std::fs::write(project.join("a"), "A界").unwrap();
        std::fs::write(project.join("b"), "Bá").unwrap();
        let paths = [PathBuf::from("a"), PathBuf::from("b")];
        let result = trash_many_with(&project, &paths, |path| {
            if path.ends_with("b") {
                return Err("synthetic backend failure".into());
            }
            let to = bin.join(path.file_name().unwrap());
            std::fs::rename(path, &to).unwrap();
            Ok(to)
        });
        assert!(result.error.is_some());
        assert_eq!(result.receipts.len(), 1);
        assert!(!project.join("a").exists() && project.join("b").exists());
        restore(&result.receipts[0]).unwrap();
        let result = trash_many_with(&project, &paths, |path| {
            let to = bin.join(path.file_name().unwrap());
            std::fs::rename(path, &to).unwrap();
            Ok(to)
        });
        assert!(result.error.is_none());
        assert_eq!(result.receipts.len(), 2);
        assert!(!project.join("a").exists() && !project.join("b").exists());
        for receipt in result.receipts.iter().rev() {
            restore(receipt).unwrap();
        }
        assert_eq!(std::fs::read_to_string(project.join("a")).unwrap(), "A界");
        assert_eq!(std::fs::read_to_string(project.join("b")).unwrap(), "Bá");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn reversible_fixture_move_preserves_content_and_refuses_collision_or_replacement() {
        let root = std::env::temp_dir().join(format!(
            "tw-trash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let bin = root.join("fixture-bin");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(project.join("ñ.txt"), "CONTENIDO").unwrap();
        let receipt = trash_with(&project, Path::new("ñ.txt"), |path| {
            let to = bin.join("renamed.txt");
            std::fs::rename(path, &to).unwrap();
            Ok(to)
        })
        .unwrap();
        assert!(!project.join("ñ.txt").exists());
        assert_eq!(
            std::fs::read_to_string(&receipt.trashed).unwrap(),
            "CONTENIDO"
        );
        std::fs::write(project.join("ñ.txt"), "NEW").unwrap();
        assert!(restore(&receipt).is_err());
        assert_eq!(
            std::fs::read_to_string(project.join("ñ.txt")).unwrap(),
            "NEW"
        );
        std::fs::remove_file(project.join("ñ.txt")).unwrap();
        restore(&receipt).unwrap();
        assert_eq!(
            std::fs::read_to_string(project.join("ñ.txt")).unwrap(),
            "CONTENIDO"
        );
        let receipt = trash_with(&project, Path::new("ñ.txt"), |path| {
            let to = bin.join("changed");
            std::fs::rename(path, &to).unwrap();
            Ok(to)
        })
        .unwrap();
        std::fs::rename(&receipt.trashed, bin.join("preserved")).unwrap();
        std::fs::write(&receipt.trashed, "OTHER").unwrap();
        assert!(restore(&receipt).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn restored_unicode_directory_can_be_trashed_again_without_losing_children() {
        let root = std::env::temp_dir().join(format!(
            "tw-trash-unicode-directory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let bin = root.join("fixture-bin");
        std::fs::create_dir_all(project.join("café")).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(project.join("café/child.txt"), "PRESERVED").unwrap();
        for attempt in 0..2 {
            let expected = project.join("café").canonicalize().unwrap();
            let receipt = trash_with(&project, Path::new("café"), |path| {
                let destination = bin.join(format!("directory-{attempt}"));
                std::fs::rename(path, &destination).unwrap();
                Ok(destination)
            })
            .unwrap();
            assert_eq!(receipt.original, expected);
            assert!(receipt.directory);
            restore(&receipt).unwrap();
            assert_eq!(
                std::fs::read_to_string(project.join("café/child.txt")).unwrap(),
                "PRESERVED"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unsafe_paths_never_reach_trash_backend() {
        let root = std::env::temp_dir().join(format!(
            "tw-trash-guards-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::os::unix::fs::symlink("/private/tmp", root.join("link")).unwrap();
        std::fs::write(root.join("regular"), "same-parent fixture").unwrap();
        std::fs::create_dir(root.join("directory")).unwrap();
        std::fs::write(root.join("directory/child"), "child").unwrap();
        std::os::unix::fs::symlink(root.join("regular"), root.join("leaf-link")).unwrap();
        std::os::unix::fs::symlink(root.join("directory"), root.join("parent-link")).unwrap();
        for relative in [
            "",
            "..",
            "../outside",
            "/absolute",
            "link",
            "link/file",
            "leaf-link",
            "parent-link",
            "parent-link/child",
        ] {
            assert!(
                trash_with(&root, Path::new(relative), |_| panic!(
                    "backend must not run"
                ))
                .is_err()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn directory_restore_keeps_children_and_missing_parent_does_not_recreate_paths() {
        let root = std::env::temp_dir().join(format!(
            "tw-trash-directory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        let bin = root.join("fixture-bin");
        std::fs::create_dir_all(project.join("parent/child")).unwrap();
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(project.join("parent/child/界.txt"), "ñ界").unwrap();
        let receipt = trash_with(&project, Path::new("parent/child"), |path| {
            let to = bin.join("child");
            std::fs::rename(path, &to).unwrap();
            Ok(to)
        })
        .unwrap();
        assert!(receipt.directory);
        std::fs::remove_dir(project.join("parent")).unwrap();
        assert!(restore(&receipt).is_err());
        assert!(!project.join("parent").exists());
        assert_eq!(
            std::fs::read_to_string(receipt.trashed.join("界.txt")).unwrap(),
            "ñ界"
        );
        std::fs::create_dir(project.join("parent")).unwrap();
        restore(&receipt).unwrap();
        assert_eq!(
            std::fs::read_to_string(project.join("parent/child/界.txt")).unwrap(),
            "ñ界"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
