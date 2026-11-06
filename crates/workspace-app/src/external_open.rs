//! Explicit document handoff. No shell, no default-association changes.
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Application {
    Browser,
    Excel,
    Default,
}
pub fn application(path: &Path) -> Application {
    match path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => Application::Browser,
        "xlsx" | "xls" | "xlsm" | "xlsb" | "xltx" | "xltm" => Application::Excel,
        _ => Application::Default,
    }
}
pub fn checked_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    if !root.is_absolute()
        || root.canonicalize().map_err(|e| e.to_string())? != root
        || !path.is_absolute()
        || !path.starts_with(root)
        || path.canonicalize().map_err(|e| e.to_string())? != path
    {
        return Err("Elegí un archivo dentro de la carpeta explícita, sin symlinks".into());
    }
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("La apertura externa requiere un archivo regular".into());
    }
    Ok(path.to_owned())
}
/// Only the exact bundled synthetic documents may leave a validation profile.
pub fn validation_fixture(root: &Path, path: &Path) -> bool {
    let Ok(path) = checked_path(root, path) else {
        return false;
    };
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let expected: &[u8] = match relative.to_str() {
        Some("external-open/page.html") => {
            include_bytes!("../../../Resources/Demo/projects/workspace/external-open/page.html")
        }
        Some("external-open/sheet.xlsx") => {
            include_bytes!("../../../Resources/Demo/projects/workspace/external-open/sheet.xlsx")
        }
        _ => return false,
    };
    path.metadata()
        .is_ok_and(|m| m.len() == expected.len() as u64)
        && std::fs::read(path).is_ok_and(|bytes| bytes == expected)
}
pub fn open(root: &Path, path: &Path) -> Result<(), String> {
    let path = checked_path(root, path)?;
    let mut command = Command::new("/usr/bin/open");
    match application(&path) {
        Application::Excel => {
            command.args(["-b", "com.microsoft.Excel"]);
        }
        Application::Browser => {
            command.arg("-a").arg(browser()?);
        }
        Application::Default => {}
    }
    command
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(match application(&path) {
                        Application::Excel => {
                            "No se pudo abrir en Microsoft Excel. Comprobá que esté instalada."
                        }
                        _ => "macOS no pudo abrir el archivo en su aplicación.",
                    }
                    .into())
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("La apertura externa no respondió en ocho segundos".into());
            }
        }
    }
}
#[cfg(not(all(target_os = "macos", feature = "native")))]
fn browser() -> Result<PathBuf, String> {
    Err("El navegador del sistema requiere la app nativa de macOS".into())
}
#[cfg(all(target_os = "macos", feature = "native"))]
#[allow(unexpected_cfgs)]
fn browser() -> Result<PathBuf, String> {
    use objc::{class, msg_send, runtime::Object, sel, sel_impl};
    use std::ffi::CStr;
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    struct Pool(*mut Object);
    impl Drop for Pool {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![self.0, drain];
            }
        }
    }
    unsafe {
        let _pool = Pool(msg_send![class!(NSAutoreleasePool), new]);
        // Resolve the HTTPS handler only. This does not navigate or fetch a URL.
        let value = b"https://example.invalid/";
        let string: *mut Object = msg_send![class!(NSString), alloc];
        let string: *mut Object =
            msg_send![string, initWithBytes:value.as_ptr() length:value.len() encoding:4usize];
        let url: *mut Object = msg_send![class!(NSURL), URLWithString:string];
        let _: () = msg_send![string, release];
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut Object = msg_send![workspace, URLForApplicationToOpenURL:url];
        if app.is_null() {
            return Err("macOS no tiene un navegador HTTPS asociado".into());
        }
        let path: *mut Object = msg_send![app, path];
        let bytes: *const std::ffi::c_char = msg_send![path, UTF8String];
        if bytes.is_null() {
            return Err("macOS no devolvió la ruta del navegador".into());
        }
        Ok(PathBuf::from(
            CStr::from_ptr(bytes).to_string_lossy().into_owned(),
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_destinations_and_project_boundaries() {
        assert_eq!(application(Path::new("Budget.XLSX")), Application::Excel);
        assert_eq!(application(Path::new("page.HTML")), Application::Browser);
        assert_eq!(application(Path::new("notes.md")), Application::Default);
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tw-external-doc-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("quote ' and $(literal) ñ.xlsx");
        std::fs::write(&file, b"synthetic").unwrap();
        assert_eq!(checked_path(&root, &file).unwrap(), file);
        let link = root.join("link.xlsx");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(checked_path(&root, &link).is_err());
        assert!(checked_path(&root, &root).is_err());
        assert!(checked_path(&root, Path::new("relative.xlsx")).is_err());
        assert!(checked_path(&root, &root.parent().unwrap().join("outside.xlsx")).is_err());
        let fixtures = root.join("external-open");
        std::fs::create_dir(&fixtures).unwrap();
        let html = fixtures.join("page.html");
        let bytes =
            include_bytes!("../../../Resources/Demo/projects/workspace/external-open/page.html");
        std::fs::write(&html, bytes).unwrap();
        assert!(validation_fixture(&root, &html));
        std::fs::write(&html, b"modified fixture").unwrap();
        assert!(!validation_fixture(&root, &html));
        assert!(!validation_fixture(&root, &file));
        std::fs::remove_dir_all(root).unwrap();
    }
}
