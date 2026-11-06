//! Opt-in LSP over private stdio. No server command execution or workspace edits.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Clangd,
    Lua,
    Tex,
}
impl Kind {
    pub fn for_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(|s| s.to_str()) {
            Some("c" | "h" | "cpp" | "cc" | "hpp") => Some(Self::Clangd),
            Some("lua") => Some(Self::Lua),
            Some("tex" | "bib" | "sty" | "cls") => Some(Self::Tex),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Clangd => "clangd C/C++",
            Self::Lua => "Lua Language Server",
            Self::Tex => "texlab LaTeX/BibTeX",
        }
    }
    pub fn language(self, path: &Path) -> Option<&'static str> {
        if Self::for_path(path) != Some(self) {
            return None;
        }
        Some(match path.extension().and_then(|s| s.to_str()) {
            Some("c" | "h") => "c",
            Some("lua") => "lua",
            Some("bib") => "bibtex",
            Some("tex" | "sty" | "cls") => "latex",
            _ => "cpp",
        })
    }
    pub fn executable(self, home: Option<&Path>) -> Option<PathBuf> {
        let candidates = match self {
            Self::Clangd => vec![PathBuf::from("/usr/bin/clangd")],
            Self::Lua => [
                Some(PathBuf::from("/opt/homebrew/bin/lua-language-server")),
                home.map(|p| p.join(".local/share/nvim/mason/bin/lua-language-server")),
            ]
            .into_iter()
            .flatten()
            .collect(),
            Self::Tex => [
                Some(PathBuf::from("/opt/homebrew/bin/texlab")),
                home.map(|p| p.join(".local/share/nvim/mason/bin/texlab")),
            ]
            .into_iter()
            .flatten()
            .collect(),
        };
        candidates.into_iter().find(|p| p.is_file())
    }
}

fn private_profile(runtime: &Path, prefix: &str) -> Result<PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt;
    crate::storage::private_directory(runtime)?;
    let profile = runtime.join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&profile)
        .map_err(|e| e.to_string())?;
    crate::storage::private_directory(&profile)?;
    Ok(profile)
}

fn configuration_reply(settings: &Value, items: &Value) -> Result<Vec<Value>, String> {
    let items = items
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or("LSP configuration requires at most100 items")?;
    items
        .iter()
        .map(|item| {
            let section = match item.get("section") {
                None | Some(Value::Null) => return Ok(settings.clone()),
                Some(Value::String(s)) if s.len() <= 128 && s.split('.').count() <= 8 => s,
                _ => return Err("Invalid LSP configuration section".into()),
            };
            if section.is_empty() {
                return Ok(settings.clone());
            }
            Ok(section
                .split('.')
                .try_fold(settings, |value, part| value.get(part))
                .cloned()
                .unwrap_or(Value::Null))
        })
        .collect()
}

fn tex_settings() -> Value {
    json!({"texlab": {
        "build": {"onSave": false,"forwardSearchAfter":false},
        "chktex": {"onOpenAndSave": false,"onEdit":false},
        "forwardSearch": {"executable":null},
        "latexFormatter":"none", "bibtexFormatter":"none",
        "hover":{"symbols":"glyph"},"experimental":{"followPackageLinks":false}
    }})
}

#[derive(Default)]
pub struct Codec {
    bytes: Vec<u8>,
}
impl Codec {
    pub fn push(&mut self, input: &[u8]) -> Result<Vec<Value>, String> {
        if self.bytes.len() + input.len() > 4 * 1024 * 1024 + 8192 {
            return Err("LSP supera 4 MiB".into());
        }
        self.bytes.extend_from_slice(input);
        let mut out = Vec::new();
        loop {
            let Some(end) = self.bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                if self.bytes.len() > 8192 {
                    return Err("Header LSP demasiado grande".into());
                }
                break;
            };
            if end > 8192 {
                return Err("Header LSP demasiado grande".into());
            }
            let header = std::str::from_utf8(&self.bytes[..end]).map_err(|e| e.to_string())?;
            let mut lengths = header
                .lines()
                .filter_map(|l| l.split_once(':'))
                .filter(|(k, _)| k.eq_ignore_ascii_case("Content-Length"));
            let len = lengths
                .next()
                .ok_or("Content-Length ausente")?
                .1
                .trim()
                .parse::<usize>()
                .map_err(|e| e.to_string())?;
            if lengths.next().is_some() || len > 4 * 1024 * 1024 {
                return Err("Content-Length inválido".into());
            }
            if self.bytes.len() < end + 4 + len {
                break;
            }
            let value = serde_json::from_slice(&self.bytes[end + 4..end + 4 + len])
                .map_err(|e| e.to_string())?;
            self.bytes.drain(..end + 4 + len);
            out.push(value);
        }
        Ok(out)
    }
}
fn send(w: &mut impl Write, value: Value) -> Result<(), String> {
    let body = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())
        .and_then(|_| w.write_all(&body))
        .and_then(|_| w.flush())
        .map_err(|e| e.to_string())
}
pub fn uri(path: &Path) -> String {
    let mut s = "file://".to_owned();
    for b in path.to_string_lossy().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(b) {
            s.push(*b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}
pub fn path(uri: &str) -> Option<PathBuf> {
    let s = uri.strip_prefix("file:///")?;
    let b = s.as_bytes();
    let mut out = vec![b'/'];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(out).ok()?;
    (!s.contains('\0')).then(|| PathBuf::from(s))
}
pub fn position(text: &[char], cursor: usize) -> Value {
    let cursor = cursor.min(text.len());
    let start = text[..cursor]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |p| p + 1);
    json!({"line":text[..start].iter().filter(|c|**c=='\n').count(),"character":text[start..cursor].iter().map(|c|c.len_utf16()).sum::<usize>()})
}
pub fn offset(text: &[char], pos: &Value) -> Option<usize> {
    let row = pos["line"].as_u64()? as usize;
    let col = pos["character"].as_u64()? as usize;
    let start = if row == 0 {
        0
    } else {
        text.iter()
            .enumerate()
            .filter(|(_, c)| **c == '\n')
            .nth(row - 1)?
            .0
            + 1
    };
    let mut units = 0;
    for (i, c) in text.iter().enumerate().skip(start) {
        if units == col {
            return Some(i);
        }
        if *c == '\n' {
            return None;
        }
        units += c.len_utf16();
        if units > col {
            return None;
        }
    }
    (units == col).then_some(text.len())
}
#[derive(Clone)]
pub struct Document {
    pub path: PathBuf,
    pub text: String,
    pub version: u64,
    pub language: String,
}
pub enum Event {
    Ready,
    Diagnostics(PathBuf, u64, Vec<Value>),
    Response(PathBuf, u64, String, Value, Value),
    Error(String),
}
enum Work {
    Sync(Document),
    Request(Document, String, Value),
}
pub struct Server {
    tx: SyncSender<Work>,
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
}
impl Server {
    pub fn clangd(root: &Path) -> Result<Self, String> {
        Self::clangd_notifying(root, None)
    }
    pub fn clangd_notifying(
        root: &Path,
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        let mut c = Command::new("/usr/bin/clangd");
        c.args([
            "--background-index=false",
            "--clang-tidy=false",
            "--log=error",
        ]);
        Self::start_notifying(c, root, wake)
    }
    pub fn lua_notifying(
        root: &Path,
        runtime: &Path,
        binary: &Path,
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        use std::{fs, os::unix::fs::OpenOptionsExt};
        // Exclusive fresh profile, never reuse a server's logs/config or the user's HOME.
        let profile = private_profile(runtime, "lua-lsp")?;
        let config = profile.join("settings.json");
        let settings = json!({"runtime.version":"LuaJIT","runtime.plugin":"","workspace.checkThirdParty":"Disable","workspace.userThirdParty":[],"workspace.library":[],"workspace.maxPreload":5000,"workspace.preloadFileSize":512,"diagnostics.globals":["vim"],"completion.callSnippet":"Replace","completion.keywordSnippet":"Replace"});
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&config)
            .map_err(|e| e.to_string())?;
        file.write_all(
            serde_json::to_string(&settings)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        let mut command = Command::new(binary);
        command
            .arg(format!("--configpath={}", config.display()))
            .arg(format!("--logpath={}", profile.join("log").display()))
            .arg(format!("--metapath={}", profile.join("meta").display()))
            .arg("--loglevel=error")
            .env("HOME", &profile)
            .env("TMPDIR", &profile)
            .env("XDG_CONFIG_HOME", &profile)
            .env("XDG_CACHE_HOME", profile.join("cache"))
            .env("XDG_DATA_HOME", profile.join("data"))
            .env("XDG_STATE_HOME", profile.join("state"))
            .env_remove("BASH_ENV")
            .env_remove("ENV")
            .env_remove("LUA_INIT")
            .env_remove("LUA_PATH")
            .env_remove("LUA_CPATH");
        Self::start_notifying(command, root, wake)
    }
    pub fn tex_notifying(
        root: &Path,
        runtime: &Path,
        binary: &Path,
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        let profile = private_profile(runtime, "tex-lsp")?;
        let mut command = Command::new(binary);
        command
            .args(["--quiet", "run"])
            .env("HOME", &profile)
            .env("TMPDIR", &profile)
            .env("XDG_RUNTIME_DIR", &profile)
            .env("XDG_CONFIG_HOME", &profile)
            .env("XDG_CACHE_HOME", &profile)
            .env("XDG_DATA_HOME", &profile)
            .env("XDG_STATE_HOME", &profile)
            .env_remove("BASH_ENV")
            .env_remove("ENV");
        // skipDistro avoids invoking distribution discovery. texlab's own IPC
        // socket is confined to the fresh private runtime directory, not shared.
        Self::start_configured(
            command,
            root,
            wake,
            tex_settings(),
            json!({"skipDistro":true}),
        )
    }
    pub fn start(command: Command, root: &Path) -> Result<Self, String> {
        Self::start_notifying(command, root, None)
    }
    pub fn start_notifying(
        command: Command,
        root: &Path,
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        Self::start_configured(command, root, wake, Value::Null, Value::Null)
    }
    fn start_configured(
        mut command: Command,
        root: &Path,
        wake: Option<async_channel::Sender<()>>,
        settings: Value,
        initialization: Value,
    ) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let mut child = command
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(8);
        let (events_tx, events) = crate::events::sync_channel(64, wake);
        let (wire_tx, wire_rx) = mpsc::sync_channel(8);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::spawn(move || {
            let mut codec = Codec::default();
            let mut b = [0; 8192];
            loop {
                match output.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => match codec.push(&b[..n]) {
                        Ok(messages) => {
                            for m in messages {
                                if wire_tx.send(Ok(m)).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = wire_tx.send(Err(e));
                            return;
                        }
                    },
                }
            }
        });
        std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                send(
                    &mut input,
                    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":std::process::id(),"rootUri":uri(&root),"initializationOptions":initialization,"capabilities":{"workspace":{"configuration":!settings.is_null()},"general":{"positionEncodings":["utf-16"]},"textDocument":{"completion":{"completionItem":{"snippetSupport":true}},"publishDiagnostics":{"versionSupport":true}}}}}),
                )?;
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut ready = false;
                let mut next = 2u64;
                let mut documents: BTreeMap<PathBuf, u64> = BTreeMap::new();
                let mut pending: BTreeMap<u64, (PathBuf, u64, String, Instant, Value)> =
                    BTreeMap::new();
                while !stopped.load(Ordering::Relaxed) {
                    while let Ok(m) = wire_rx.try_recv() {
                        let m = m?;
                        if m.get("method").is_some() && m.get("id").is_some() {
                            let method = m["method"].as_str().unwrap_or("");
                            let reply = if method == "workspace/configuration" {
                                match configuration_reply(&settings, &m["params"]["items"]) {
                                    Ok(values) => {
                                        json!({"jsonrpc":"2.0","id":m["id"],"result":values})
                                    }
                                    Err(error) => {
                                        json!({"jsonrpc":"2.0","id":m["id"],"error":{"code":-32602,"message":error}})
                                    }
                                }
                            } else {
                                json!({"jsonrpc":"2.0","id":m["id"],"error":{"code":-32601,"message":"Client has no workspace edit or command execution support"}})
                            };
                            send(&mut input, reply)?;
                            continue;
                        }
                        if m["id"] == 1 {
                            if m.get("error").is_some() {
                                return Err("LSP rechazó initialize".into());
                            }
                            if m["result"]["capabilities"]["positionEncoding"]
                                .as_str()
                                .is_some_and(|s| s != "utf-16")
                            {
                                return Err(
                                    "LSP requiere una codificación de posiciones no soportada"
                                        .into(),
                                );
                            }
                            send(
                                &mut input,
                                json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
                            )?;
                            if !settings.is_null() {
                                send(
                                    &mut input,
                                    json!({"jsonrpc":"2.0","method":"workspace/didChangeConfiguration","params":{"settings":settings}}),
                                )?;
                            }
                            ready = true;
                            let _ = events_tx.try_send(Event::Ready);
                        } else if m["method"] == "textDocument/publishDiagnostics" {
                            let p = &m["params"];
                            if let Some(path) = p["uri"].as_str().and_then(path)
                                && let Some(v) = documents.get(&path)
                            {
                                let version = p["version"].as_u64().unwrap_or(*v);
                                let data = p["diagnostics"]
                                    .as_array()
                                    .map(|a| a.iter().take(500).cloned().collect())
                                    .unwrap_or_default();
                                let _ = events_tx.try_send(Event::Diagnostics(path, version, data));
                            }
                        } else if let Some(id) = m["id"].as_u64()
                            && let Some((path, version, method, _, position)) = pending.remove(&id)
                        {
                            if m.get("error").is_some() {
                                let _ = events_tx.try_send(Event::Error(
                                    "LSP no pudo completar la consulta".into(),
                                ));
                            } else {
                                let _ = events_tx.try_send(Event::Response(
                                    path,
                                    version,
                                    method,
                                    position,
                                    m["result"].clone(),
                                ));
                            }
                        }
                    }
                    if !ready {
                        if Instant::now() > deadline {
                            return Err("LSP initialize excedió 10 segundos".into());
                        }
                    } else {
                        for _ in 0..8 {
                            let Ok(work) = rx.try_recv() else {
                                break;
                            };
                            let (doc, request) = match work {
                                Work::Sync(d) => (d, None),
                                Work::Request(d, m, p) => (d, Some((m, p))),
                            };
                            if !doc.path.starts_with(&root) || doc.text.len() > 2 * 1024 * 1024 {
                                continue;
                            }
                            if documents.get(&doc.path) != Some(&doc.version) {
                                let params = if documents.contains_key(&doc.path) {
                                    json!({"textDocument":{"uri":uri(&doc.path),"version":doc.version},"contentChanges":[{"text":doc.text}]})
                                } else {
                                    json!({"textDocument":{"uri":uri(&doc.path),"languageId":doc.language,"version":doc.version,"text":doc.text}})
                                };
                                let method = if documents.contains_key(&doc.path) {
                                    "textDocument/didChange"
                                } else {
                                    "textDocument/didOpen"
                                };
                                send(
                                    &mut input,
                                    json!({"jsonrpc":"2.0","method":method,"params":params}),
                                )?;
                                documents.insert(doc.path.clone(), doc.version);
                            }
                            if let Some((method, position)) = request {
                                if pending.len() >= 16 {
                                    let _ = events_tx.try_send(Event::Error(
                                        "Demasiadas consultas LSP pendientes".into(),
                                    ));
                                    continue;
                                }
                                let mut params = json!({"textDocument":{"uri":uri(&doc.path)},"position":position.clone()});
                                if method == "textDocument/references" {
                                    params["context"] = json!({"includeDeclaration":true});
                                }
                                send(
                                    &mut input,
                                    json!({"jsonrpc":"2.0","id":next,"method":method,"params":params}),
                                )?;
                                pending.insert(
                                    next,
                                    (doc.path, doc.version, method, Instant::now(), position),
                                );
                                next += 1;
                            }
                        }
                        let expired = pending
                            .iter()
                            .filter(|(_, (_, _, _, t, _))| t.elapsed() > Duration::from_secs(8))
                            .map(|(id, _)| *id)
                            .collect::<Vec<_>>();
                        for id in expired {
                            pending.remove(&id);
                            send(
                                &mut input,
                                json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}}),
                            )?;
                            let _ = events_tx.try_send(Event::Error(
                                "Consulta LSP cancelada después de 8 segundos".into(),
                            ));
                        }
                    }
                    if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                        return Err("LSP terminó".into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                for path in documents.keys() {
                    let _ = send(
                        &mut input,
                        json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":uri(path)}}}),
                    );
                }
                let _ = send(
                    &mut input,
                    json!({"jsonrpc":"2.0","id":next,"method":"shutdown","params":null}),
                );
                let end = Instant::now() + Duration::from_millis(300);
                while Instant::now() < end {
                    if let Ok(Ok(m)) = wire_rx.recv_timeout(Duration::from_millis(20))
                        && m["id"] == next
                    {
                        break;
                    }
                }
                let _ = send(&mut input, json!({"jsonrpc":"2.0","method":"exit"}));
                Ok(())
            })();
            if let Err(e) = result {
                let _ = events_tx.try_send(Event::Error(e));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
        Ok(Self { tx, events, stop })
    }
    pub fn sync(&self, doc: Document) -> Result<(), String> {
        self.tx
            .try_send(Work::Sync(doc))
            .map_err(|_| "LSP ocupado".into())
    }
    pub fn request(&self, doc: Document, method: &str, position: Value) -> Result<(), String> {
        if !matches!(
            method,
            "textDocument/hover"
                | "textDocument/definition"
                | "textDocument/references"
                | "textDocument/completion"
        ) {
            return Err("Consulta LSP no permitida".into());
        }
        self.tx
            .try_send(Work::Request(doc, method.into(), position))
            .map_err(|_| "LSP ocupado".into())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_framing_and_utf16_roundtrip() {
        let v = json!({"jsonrpc":"2.0","id":1,"result":"ñ 🦀"});
        let mut wire = Vec::new();
        send(&mut wire, v.clone()).unwrap();
        let mut c = Codec::default();
        let mut out = Vec::new();
        for b in wire {
            out.extend(c.push(&[b]).unwrap());
        }
        assert_eq!(out, vec![v]);
        let t = "ñ🦀x\na".chars().collect::<Vec<_>>();
        assert_eq!(position(&t, 2), json!({"line":0,"character":3}));
        assert_eq!(offset(&t, &position(&t, 2)), Some(2));
        assert_eq!(offset(&t, &json!({"line":0,"character":2})), None);
        let p = PathBuf::from("/tmp/ñ space#?.c");
        assert_eq!(path(&uri(&p)), Some(p));
        assert!(path("file://remote/tmp/x").is_none());
    }
    #[test]
    fn language_families_and_discovery_do_not_cross_servers_or_search_projects() {
        assert_eq!(Kind::for_path(Path::new("init.lua")), Some(Kind::Lua));
        assert_eq!(Kind::Clangd.language(Path::new("init.lua")), None);
        assert_eq!(Kind::Lua.language(Path::new("init.lua")), Some("lua"));
        assert_eq!(Kind::for_path(Path::new("main.rs")), None);
        assert_eq!(Kind::Clangd.language(Path::new("header.hpp")), Some("cpp"));
        assert_eq!(Kind::for_path(Path::new("sample.tex")), Some(Kind::Tex));
        assert_eq!(Kind::Tex.language(Path::new("sample.bib")), Some("bibtex"));
        assert_eq!(Kind::Tex.language(Path::new("sample.lua")), None);
    }
    #[test]
    fn configuration_queries_are_bounded_and_use_only_explicit_settings() {
        let settings = tex_settings();
        let result = configuration_reply(
            &settings,
            &json!([
                {"section":"texlab"},{"section":"texlab.build.onSave"},
                {"section":"unrecognized"},{"section":null}
            ]),
        )
        .unwrap();
        assert_eq!(result[0]["chktex"]["onEdit"], false);
        assert_eq!(result[0]["hover"]["symbols"], "glyph");
        assert_eq!(result[1], false);
        assert_eq!(result[2], Value::Null);
        assert_eq!(result[3], settings);
        assert!(configuration_reply(&settings, &json!([{"section":12}])).is_err());
        assert!(configuration_reply(&settings, &json!(vec![json!({}); 101])).is_err());
        assert!(configuration_reply(&settings, &json!([{"section":"x".repeat(129)}])).is_err());
    }
    #[test]
    fn actual_texlab_labels_definition_references_completion_and_private_runtime() {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let Some(binary) = Kind::Tex.executable(home.as_deref()) else {
            eprintln!("texlab unavailable; real-server test skipped");
            return;
        };
        // A short parent also lets texlab bind its optional Unix IPC socket on macOS.
        let temp = PathBuf::from("/private/tmp").join(format!(
            "twtex-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = temp.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let marker = root.join("LATEXMK_RAN");
        std::fs::write(
            root.join(".latexmkrc"),
            format!(
                "open(my $f, '>', '{}'); print $f 'executed'; close($f);",
                marker.display()
            ),
        )
        .unwrap();
        let path = root.join("sample.tex");
        let text = include_str!("../../../Resources/Demo/projects/workspace/src/sample.tex");
        std::fs::write(&path, text).unwrap();
        let runtime = temp.join("runtime");
        let server = Server::tex_notifying(&root, &runtime, &binary, None).unwrap();
        let doc = Document {
            path: path.clone(),
            text: text.into(),
            version: 1,
            language: "latex".into(),
        };
        assert!(
            server
                .request(
                    doc.clone(),
                    "textDocument/build",
                    json!({"line":0,"character":0})
                )
                .is_err()
        );
        for method in [
            "textDocument/definition",
            "textDocument/references",
            "textDocument/completion",
        ] {
            server
                .request(doc.clone(), method, json!({"line":4,"character":14}))
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut response = None;
            while Instant::now() < deadline {
                match server.events.recv_timeout(Duration::from_millis(100)) {
                    Ok(Event::Response(_, 1, m, _, data)) if m == method => {
                        response = Some(data);
                        break;
                    }
                    Ok(Event::Error(error)) => panic!("texlab: {error}"),
                    _ => {}
                }
            }
            let response = response.unwrap_or_else(|| panic!("texlab {method} not received"));
            if method == "textDocument/completion" {
                assert!(
                    response
                        .as_array()
                        .or_else(|| response["items"].as_array())
                        .is_some_and(|a| a.iter().any(|item| item["label"]
                            .as_str()
                            .is_some_and(|s| s.contains("sec:inicio")))),
                    "texlab completion: {response}"
                );
            } else {
                assert!(
                    response.as_array().is_some_and(|a| !a.is_empty()),
                    "texlab {method}: {response}"
                );
            }
        }
        assert!(!marker.exists(), "texlab executed project latexmk config");
        let profiles = std::fs::read_dir(&runtime)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(profiles.len(), 1);
        assert!(
            profiles[0].join("texlab.sock").exists(),
            "texlab IPC must stay in private runtime"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&profiles[0])
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(server);
        std::thread::sleep(Duration::from_millis(350));
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn actual_lua_hover_completion_and_explicit_config_disables_workspace_plugin() {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let Some(binary) = Kind::Lua.executable(home.as_deref()) else {
            eprintln!("LuaLS unavailable; real-server test skipped");
            return;
        };
        let temp = std::env::temp_dir().join(format!(
            "workspace-lua-lsp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = temp.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let plugin_marker = root.join("PLUGIN_RAN");
        let plugin = root.join("plugin.lua");
        std::fs::write(
            &plugin,
            format!(
                "local f=io.open({:?},'w');if f then f:write('executed');f:close() end;return {{}}",
                plugin_marker.to_str().unwrap()
            ),
        )
        .unwrap();
        std::fs::write(
            root.join(".luarc.json"),
            serde_json::to_string(&json!({"runtime.plugin":plugin})).unwrap(),
        )
        .unwrap();
        let path = root.join("test.lua");
        let text = "---@type integer\nlocal answer = 42\nlocal result = answer\n";
        std::fs::write(&path, text).unwrap();
        let server = Server::lua_notifying(&root, &temp.join("runtime"), &binary, None).unwrap();
        server
            .request(
                Document {
                    path: path.clone(),
                    text: text.into(),
                    version: 1,
                    language: "lua".into(),
                },
                "textDocument/hover",
                json!({"line":2,"character":16}),
            )
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let mut hover = false;
        while Instant::now() < until {
            match server.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Response(_, 1, method, _, v)) if method == "textDocument/hover" => {
                    hover = !v.is_null();
                    break;
                }
                Ok(Event::Error(e)) => panic!("LuaLS: {e}"),
                _ => {}
            }
        }
        assert!(hover, "LuaLS hover not received");
        let text = "---@type integer\nlocal answer=42\nlocal result=ans\n";
        let doc = Document {
            path: path.clone(),
            text: text.into(),
            version: 2,
            language: "lua".into(),
        };
        let position = position(
            &text.chars().collect::<Vec<_>>(),
            text.rfind("ans").unwrap() + 3,
        );
        server
            .request(doc.clone(), "textDocument/completion", position.clone())
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let mut complete = false;
        while Instant::now() < until {
            match server.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Response(_, 2, method, _, v)) if method == "textDocument/completion" => {
                    complete = v
                        .as_array()
                        .or_else(|| v["items"].as_array())
                        .is_some_and(|a| {
                            a.iter()
                                .any(|i| i["label"].as_str().is_some_and(|s| s.contains("answer")))
                        });
                    if complete {
                        break;
                    }
                    // LuaLS explicitly returns null while workspace.isReady is false.
                    std::thread::sleep(Duration::from_millis(100));
                    server
                        .request(doc.clone(), "textDocument/completion", position.clone())
                        .unwrap();
                }
                Ok(Event::Error(e)) => panic!("LuaLS: {e}"),
                _ => {}
            }
        }
        assert!(complete, "LuaLS answer completion not received");
        let text = "---@param left string\n---@param right string\nlocal function combine(left, right)\n return left .. right\nend\ncombi\n";
        let doc = Document {
            path: path.clone(),
            text: text.into(),
            version: 3,
            language: "lua".into(),
        };
        let origin = json!({"line":5,"character":5});
        server
            .request(doc.clone(), "textDocument/completion", origin.clone())
            .unwrap();
        let until = Instant::now() + Duration::from_secs(8);
        let mut snippet = None;
        while Instant::now() < until {
            match server.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Response(_, 3, method, _, v)) if method == "textDocument/completion" => {
                    snippet = v
                        .as_array()
                        .or_else(|| v["items"].as_array())
                        .and_then(|items| {
                            items.iter().find(|item| {
                                item["insertTextFormat"] == 2
                                    && item["label"]
                                        .as_str()
                                        .is_some_and(|label| label.contains("combine"))
                            })
                        })
                        .cloned();
                    if snippet.is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    server
                        .request(doc.clone(), "textDocument/completion", origin.clone())
                        .unwrap();
                }
                Ok(Event::Error(error)) => panic!("LuaLS snippet: {error}"),
                _ => {}
            }
        }
        let snippet = snippet.expect("LuaLS function snippet not received");
        let body = snippet["textEdit"]["newText"]
            .as_str()
            .or_else(|| snippet["insertText"].as_str())
            .unwrap();
        let mut editor = crate::editor::Editor::new(text.into());
        editor.goto(5, 5);
        editor.key("i");
        let range = json!({"start":{"line":5,"character":0},"end":origin});
        editor
            .apply_lsp_snippet(&range, body, editor.version)
            .unwrap();
        assert!(
            editor.snippet_active(),
            "LuaLS snippet has no fields: {body}"
        );
        editor.paste("\"A\"");
        editor.key("<Tab>");
        editor.paste("\"B\"");
        editor.key("<Tab>");
        editor.key("<Esc>");
        assert!(editor.contents().contains("combine(\"A\", \"B\")"));
        editor.key("u");
        assert_eq!(editor.contents(), text);
        assert!(
            !plugin_marker.exists(),
            "Workspace Lua plugin executed despite explicit override"
        );
        drop(server);
        std::thread::sleep(Duration::from_millis(350));
        std::fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn actual_clangd_hover_completion_and_diagnostics() {
        let root = std::env::temp_dir().join(format!("workspace-lsp-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("test.c");
        let text = "int answer = 42;\nint main(void) { return answer; }\n";
        std::fs::write(&path, text).unwrap();
        let server = Server::clangd(&root).unwrap();
        let doc = Document {
            path: path.clone(),
            text: text.into(),
            version: 1,
            language: "c".into(),
        };
        server
            .request(doc, "textDocument/hover", json!({"line":0,"character":5}))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut hover = false;
        let mut diagnostics = false;
        while Instant::now() < deadline {
            match server.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Response(_, 1, _, _, v)) => hover = !v.is_null(),
                Ok(Event::Diagnostics(p, 1, _)) => diagnostics = p == path,
                Ok(Event::Error(e)) => panic!("{e}"),
                _ => {}
            }
            if hover && diagnostics {
                break;
            }
        }
        assert!(hover && diagnostics);
        let broken = "int answer=42;\nint main(void) { return ans; }\n";
        let pos = position(
            &broken.chars().collect::<Vec<_>>(),
            broken.rfind("ans;").unwrap() + 3,
        );
        server
            .request(
                Document {
                    path: path.clone(),
                    text: broken.into(),
                    version: 2,
                    language: "c".into(),
                },
                "textDocument/completion",
                pos,
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut completed = false;
        let mut error_seen = false;
        while Instant::now() < deadline {
            match server.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Response(_, 2, method, _, v)) if method == "textDocument/completion" => {
                    completed = v
                        .as_array()
                        .or_else(|| v["items"].as_array())
                        .is_some_and(|a| {
                            a.iter()
                                .any(|i| i["label"].as_str().is_some_and(|s| s.contains("answer")))
                        });
                }
                Ok(Event::Diagnostics(_, 2, data)) => {
                    error_seen = data.iter().any(|d| d["severity"] == 1)
                }
                Ok(Event::Error(e)) => panic!("{e}"),
                _ => {}
            }
            if completed && error_seen {
                break;
            }
        }
        assert!(
            completed && error_seen,
            "completion={completed}, diagnostic={error_seen}"
        );
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }
}
