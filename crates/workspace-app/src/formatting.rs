//! Formatter processes are bounded and run only over caller-supplied buffer text.
use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub fn format(path: &Path, text: &str) -> Result<String, String> {
    if text.len() > 8 * 1024 * 1024 {
        return Err("Buffer supera el límite de formateo".into());
    }
    let (binary, args) = match path.extension().and_then(|e| e.to_str()) {
        Some("json") => ("/usr/bin/jq", vec!["--indent", "2", "."]),
        Some("xml") => {
            let lower = text.to_ascii_lowercase();
            if lower.contains("<!doctype") || lower.contains("<!entity") {
                return Err("XML con DTD/entidades requiere revisión explícita; no se resuelven recursos externos".into());
            }
            ("/usr/bin/xmllint", vec!["--nonet", "--format", "-"])
        }
        _ => return Err("No hay formatter configurado para este tipo de archivo".into()),
    };
    if !Path::new(binary).is_file() {
        return Err(format!("Formatter no disponible: {binary}"));
    }
    let mut child = Command::new(binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("XML_CATALOG_FILES")
        .env_remove("SGML_CATALOG_FILES")
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut input = child.stdin.take().unwrap();
    let text = text.as_bytes().to_vec();
    let writer = std::thread::spawn(move || input.write_all(&text));
    fn reader(stream: impl Read + Send + 'static, limit: u64) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = stream.take(limit + 1).read_to_end(&mut b);
            b
        })
    }
    let out = reader(child.stdout.take().unwrap(), 8 * 1024 * 1024);
    let err = reader(child.stderr.take().unwrap(), 65536);
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = writer.join();
            return Err("Formatter excedió 8 segundos".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = writer.join();
    let bytes = out.join().unwrap_or_default();
    let error = err.join().unwrap_or_default();
    if !status.success() {
        return Err(String::from_utf8_lossy(&error).chars().take(1000).collect());
    }
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Resultado de formatter demasiado grande".into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_jq_is_strict_and_xml_is_nonet_without_entity_resolution() {
        assert_eq!(
            format(Path::new("a.json"), "{\"b\":1}").unwrap(),
            "{\n  \"b\": 1\n}\n"
        );
        assert!(format(Path::new("a.json"), "{ // comment\n}").is_err());
        assert!(
            format(Path::new("a.xml"), "<root><a/></root>")
                .unwrap()
                .contains("  <a/>")
        );
        assert!(format(Path::new("a.xml"), "<!DOCTYPE x SYSTEM 'file:///fake'><x/>").is_err());
        assert!(format(Path::new("a.jsonc"), "{}").is_err());
    }
}
