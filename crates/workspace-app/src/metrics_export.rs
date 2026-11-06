//! Explicit, exclusive exports of aggregate reports to the app's private state.
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub fn export(
    root: &Path,
    report: &serde_json::Value,
    source: Option<&str>,
) -> Result<PathBuf, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let meta = root.symlink_metadata().map_err(|e| e.to_string())?;
    if !root.is_absolute() || !meta.is_dir() || meta.file_type().is_symlink()
        // SAFETY: geteuid queries only this process and has no memory arguments.
        || meta.uid()!=unsafe {libc::geteuid()} || meta.mode()&0o777!=0o700
    {
        return Err("Directorio de métricas debe ser privado (0700), propio y sin symlink".into());
    }
    let source = source.filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()));
    let output = serde_json::json!({"schema_version":1,"build_source_sha256":source,"debug_assertions":cfg!(debug_assertions),"metrics":report});
    let bytes = serde_json::to_vec_pretty(&output).map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("Reporte de métricas supera 64 KiB".into());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = root.join(format!("metrics-{stamp}.json"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|e| e.to_string())?;
    // Use the open descriptor; never chmod by pathname after creation.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("Reporte parcial conservado en {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(not(unix))]
pub fn export(_: &Path, _: &serde_json::Value, _: Option<&str>) -> Result<PathBuf, String> {
    Err("Exportación privada de métricas no implementada en esta plataforma".into())
}

/// Read only this app bundle's public build identifier, never user profiles.
pub fn bundled_source() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let resources = exe
        .parent()?
        .parent()?
        .join("Resources/build-manifest.json");
    let meta = resources.symlink_metadata().ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 256 * 1024 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(resources).ok()?).ok()?;
    let source = value["source_sha256"].as_str()?;
    (source.len() == 64 && source.bytes().all(|c| c.is_ascii_hexdigit())).then(|| source.into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    #[test]
    fn reports_are_private_distinct_bounded_and_reject_symlink_or_public_root() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("tw-metrics-export-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let report = serde_json::json!({"resident_bytes":42,"input_events":0});
        let a = export(&root, &report, Some(&"a".repeat(64))).unwrap();
        let b = export(&root, &report, Some("invalid")).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.metadata().unwrap().mode() & 0o777, 0o600);
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&a).unwrap()).unwrap();
        assert_eq!(value["build_source_sha256"], "a".repeat(64));
        assert_eq!(value["metrics"], report);
        assert!(export(&root, &serde_json::json!({"large":"x".repeat(65536)}), None).is_err());
        let link = root.join("alias");
        symlink(&root, &link).unwrap();
        assert!(export(&link, &report, None).is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(export(&root, &report, None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
