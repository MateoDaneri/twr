//! Private tmux server, argv-only commands and bounded background metadata.
//! Dropping the frontend kills its attach client, never the server/panes.
use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Session {
    pub binary: PathBuf,
    pub socket: PathBuf,
    pub name: String,
    pub root: PathBuf,
}
/// Identity captured after creating a terminal on the private server. This
/// record is not authentication by itself: callers must persist it in their
/// own protected state and revalidate it before attaching or sending input.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdditionalTerminalIdentity {
    name: String,
    session_id: String,
    server_pid: u32,
    socket_device: u64,
    socket_inode: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: String,
    pub active_pane: String,
    pub index: usize,
    pub name: String,
    pub active: bool,
}
/// Only treat an unambiguous change of active window or pane as a target change.
/// Polling is observational and cannot guarantee that no change occurred
/// between the sample and an asynchronous file read.
pub fn observed_active_target_changed(before: &[WindowInfo], after: &[WindowInfo]) -> bool {
    fn active(windows: &[WindowInfo]) -> Option<(&str, &str)> {
        let mut active = windows.iter().filter(|window| window.active);
        let first = active.next()?;
        active
            .next()
            .is_none()
            .then_some((first.id.as_str(), first.active_pane.as_str()))
    }
    matches!((active(before), active(after)), (Some(a), Some(b)) if a != b)
}
#[derive(Clone, Debug)]
pub enum Action {
    Normal(bool),
    Resume(u64),
    ResumeInput(u64, Vec<u8>),
    Barrier(u64),
    /// An input-replay action whose destination must be observed before the
    /// next event is released. The wrapped action cannot itself be tracked.
    TrackedTarget(u64, String, String, Box<Action>),
    CaptureCopyMark(u64),
    InspectSelection {
        token: u64,
        window: String,
        pane: String,
    },
    VisualScreenMotion {
        token: u64,
        window: String,
        pane: String,
        bottom: bool,
        count: usize,
        scrolloff: usize,
    },
    VisualLine {
        token: u64,
        window: String,
        pane: String,
        count: usize,
    },
    RestoreCopyMark(u64, CopyMark),
    RestoreCopyMarkInSession(u64, CopyMark),
    Motion(String, usize),
    Select(String),
    Resize(char, u16),
    PaneKey(&'static str),
    Scroll(&'static str),
    Copy,
    CopyNormal,
    CopyTracked {
        token: u64,
        window: String,
        pane: String,
        keep_mode: bool,
    },
    SelectAll,
    Find,
    EndSearch,
    Search(bool),
    SearchQuery {
        window: String,
        pane: String,
        text: String,
        forward: bool,
        count: usize,
    },
    Clear,
    Cycle(bool),
    Equalize,
    SelectIndex(usize),
    New,
    AgentExample,
    Split,
    SplitVertical,
    Zoom,
    Direction(char),
}
pub enum Event {
    Resumed(u64, Result<(), String>),
    InputDelivered(u64, Result<(), String>),
    Barrier(u64),
    TargetActionDone(u64, Result<Vec<WindowInfo>, String>),
    CopyMarkCaptured(u64, Result<CopyMark, String>),
    SelectionInspected(u64, Result<bool, String>),
    VisualScreenMotionDone(u64, RestoreCopyMark),
    VisualLineDone(u64, Result<(), String>),
    CopyMarkRestored(u64, RestoreCopyMark),
    Ready(Session, Result<Vec<u32>, String>),
    ReadyAdditional(
        Session,
        AdditionalTerminalIdentity,
        crate::terminal::Terminal,
    ),
    ReusedAdditional(Session, AdditionalTerminalIdentity, u32),
    Clipboard(String),
    CopyTracked(u64, Result<Option<String>, String>),
    SearchPrompt,
    Windows(Vec<WindowInfo>),
    Error(String),
}
pub struct Backend {
    pub events: Receiver<Event>,
    commands: SyncSender<Action>,
    accepting: Mutex<bool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectBarrier {
    Queued,
    Disconnected,
}
enum AdditionalStart {
    Attach(AdditionalTerminalIdentity, bool),
    Reuse(AdditionalTerminalIdentity, u32),
}

/// Decide without dropping a live client whose pending input cannot be
/// acknowledged. PID membership is checked against this exact tmux session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectClient {
    Attach,
    Reuse,
    PreserveUnverified,
}
pub fn reconnect_client(
    existing: Option<(Option<u32>, bool)>,
    clients: &Result<Vec<u32>, String>,
) -> ReconnectClient {
    match existing {
        None => ReconnectClient::Attach,
        Some((Some(pid), false)) if clients.as_ref().is_ok_and(|pids| pids.contains(&pid)) => {
            ReconnectClient::Reuse
        }
        Some((Some(pid), true)) if clients.as_ref().is_ok_and(|pids| !pids.contains(&pid)) => {
            ReconnectClient::Attach
        }
        Some(_) => ReconnectClient::PreserveUnverified,
    }
}
impl Action {
    pub fn changes_input_target(&self) -> bool {
        matches!(
            self,
            Self::Normal(true)
                | Self::Select(_)
                | Self::Cycle(_)
                | Self::SelectIndex(_)
                | Self::New
                | Self::AgentExample
                | Self::Split
                | Self::SplitVertical
                | Self::Direction(_)
        )
    }
    /// Entering normal/copy mode is itself the desired destination. Resuming
    /// input afterward would immediately undo that transition.
    pub fn resumes_input_after_target_change(&self, input_focused: bool) -> bool {
        input_focused && self.changes_input_target() && !matches!(self, Self::Normal(true))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ResumeTarget {
    window: String,
    pane: String,
}
impl ResumeTarget {
    fn parse(text: &str) -> Result<Self, String> {
        let (window, pane) = text
            .trim()
            .split_once('\t')
            .ok_or("Destino tmux inválido")?;
        let id = |s: &str, prefix: char| {
            s.len() > 1
                && s.len() <= 24
                && s.starts_with(prefix)
                && s.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        };
        if !id(window, '@') || !id(pane, '%') {
            return Err("IDs tmux inválidos".into());
        }
        Ok(Self {
            window: window.into(),
            pane: pane.into(),
        })
    }
}

#[derive(PartialEq, Eq)]
struct SelectionSnapshot {
    target: ResumeTarget,
    session: String,
    server: u32,
    height: usize,
    active: bool,
    present: bool,
}

/// Metadata only. A tmux pane can change its selection while a copy request is
/// waiting in the worker queue, even when its identity and focus are unchanged.
#[derive(Clone, PartialEq, Eq)]
struct CopySelectionSnapshot {
    target: ResumeTarget,
    session: String,
    server: u32,
    active: bool,
    present: bool,
    search_present: bool,
    rectangle: u8,
    cursor_x: u64,
    cursor_y: u64,
    scroll: u64,
    width: u64,
    height: u64,
    history: u64,
    endpoints: Option<[u64; 4]>,
}

impl CopySelectionSnapshot {
    fn condition(&self) -> String {
        let equals = |field: &str, value: String| format!("#{{==:#{{{field}}},{value}}}");
        let mut predicates = vec![
            equals("window_id", self.target.window.clone()),
            equals("pane_id", self.target.pane.clone()),
            equals("session_id", self.session.clone()),
            equals("pid", self.server.to_string()),
            equals("pane_mode", "copy-mode".into()),
            equals("selection_active", u8::from(self.active).to_string()),
            equals("selection_present", u8::from(self.present).to_string()),
            equals("rectangle_toggle", self.rectangle.to_string()),
            equals("copy_cursor_x", self.cursor_x.to_string()),
            equals("copy_cursor_y", self.cursor_y.to_string()),
            equals("scroll_position", self.scroll.to_string()),
            equals("pane_width", self.width.to_string()),
            equals("pane_height", self.height.to_string()),
            equals("history_size", self.history.to_string()),
        ];
        if let Some(endpoints) = self.endpoints {
            predicates.extend([
                equals("selection_start_x", endpoints[0].to_string()),
                equals("selection_start_y", endpoints[1].to_string()),
                equals("selection_end_x", endpoints[2].to_string()),
                equals("selection_end_y", endpoints[3].to_string()),
            ]);
        }
        predicates
            .into_iter()
            .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
            .expect("fixed nonempty predicate list")
    }
}

/// Only selections initiated by this worker may be treated as Visual Line.
/// tmux exposes the selected endpoints but not its internal line-mode flag.
struct LineSelection {
    session: String,
    server: u32,
    socket_device: u64,
    socket_inode: u64,
    anchor_row: u64,
}

type LineSelections = BTreeMap<(String, String), LineSelection>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CopyFrame {
    row: usize,
    column: usize,
    scroll: usize,
    height: usize,
    width: usize,
    history: usize,
}

/// A coordinate in one specific copy-mode screen. It contains a digest of the
/// target row, not terminal text. No restore is performed by capturing it.
#[derive(Clone, PartialEq, Eq)]
pub struct CopyMark {
    target: ResumeTarget,
    session_id: String,
    server_pid: u32,
    socket_device: u64,
    socket_inode: u64,
    frame: CopyFrame,
    /// Some only when the separate copy grid demonstrably exceeds base history.
    backing_history: Option<usize>,
    row_digest: [u8; 32],
    /// A nonempty, unique text row and its neighbors in a bounded copy grid.
    /// Only a digest is kept; ambiguous or oversized grids have no identity.
    resize_row_digest: Option<[u8; 32]>,
}

impl std::fmt::Debug for CopyMark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CopyMark([redacted])")
    }
}

/// A tmux request can time out after execution; callers must never retry an
/// uncertain movement automatically or assume the old cursor is still active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreCopyMark {
    Restored,
    RejectedBeforeMove,
    UncertainAfterMove,
}

impl CopyMark {
    /// Stable tmux identifiers captured with the mark; no terminal text.
    pub fn target_ids(&self) -> (&str, &str) {
        (&self.target.window, &self.target.pane)
    }
    /// Zero-based physical row in the bounded tmux history plus visible pane.
    pub fn absolute_row(&self) -> Option<usize> {
        self.frame
            .history
            .max(self.backing_history.unwrap_or(0))
            .checked_add(self.frame.row)?
            .checked_sub(self.frame.scroll)
    }
    pub fn column(&self) -> usize {
        self.frame.column
    }
    pub fn jump_location(&self, buffer: u64) -> Option<workspace_core::editor_jumplist::Location> {
        if buffer == 0 {
            return None;
        }
        Some(workspace_core::editor_jumplist::Location {
            buffer,
            row: self.absolute_row()?,
            column: self.frame.column,
            // The terminal-specific view is owned by this mark, not the
            // generic editor viewport restoration.
            view: workspace_core::editor_jumplist::View::default(),
        })
    }
}

// FNV-1a is stable across builds; IDs contain no path, session names or shell text.
pub fn project_id(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let hash = path
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        });
    format!("p-{hash:016x}")
}
fn hex_bytes(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}
fn additional_terminal_prefix(root: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let digest = Sha256::digest(root.as_os_str().as_bytes());
    format!("twr-terminal-{}-", hex_bytes(&digest[..16]))
}
/// Validate syntax and workspace association only; this does not establish
/// that a session exists or that the app owns it.
pub fn valid_additional_terminal_name(root: &Path, name: &str) -> bool {
    root.is_absolute()
        && name
            .strip_prefix(&additional_terminal_prefix(root))
            .is_some_and(|id| {
                id.len() == 32
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
}
fn output_bytes(cmd: Command) -> Result<Vec<u8>, String> {
    output_bytes_with_timeout(cmd, Duration::from_secs(3))
}
fn output_bytes_with_timeout(mut cmd: Command, timeout: Duration) -> Result<Vec<u8>, String> {
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    fn read(r: impl Read + Send + 'static) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            r.take(262145).read_to_end(&mut b)?;
            Ok(b)
        })
    }
    let out = read(out);
    let err = read(err);
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err("tmux command timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = out
        .join()
        .map_err(|_| "No se pudo leer la respuesta de tmux".to_owned())?
        .map_err(|e| format!("No se pudo leer la respuesta de tmux: {e}"))?;
    let err = err
        .join()
        .map_err(|_| "No se pudieron leer errores de tmux".to_owned())?
        .map_err(|e| format!("No se pudieron leer errores de tmux: {e}"))?;
    if out.len() > 262144 || err.len() > 262144 {
        return Err("tmux response exceeds limit".into());
    }
    if !status.success() {
        return Err(String::from_utf8_lossy(&err).trim().to_owned());
    }
    Ok(out)
}
fn output(cmd: Command) -> Result<String, String> {
    output_bytes(cmd).map(|out| String::from_utf8_lossy(&out).into_owned())
}
impl Session {
    /// A separate terminal has an opaque name scoped to the workspace root.
    /// Generate the identity before starting tmux so an uncertain command
    /// outcome can be reconciled by discovery without retrying creation.
    pub fn additional_terminal_name(&self) -> Result<String, String> {
        let mut bytes = [0u8; 16];
        fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|e| format!("No se pudo generar identidad de terminal: {e}"))?;
        Ok(format!(
            "{}{}",
            self.additional_terminal_prefix(),
            hex_bytes(&bytes)
        ))
    }

    fn additional_terminal_prefix(&self) -> String {
        additional_terminal_prefix(&self.root)
    }

    fn is_additional_terminal_name(&self, name: &str) -> bool {
        valid_additional_terminal_name(&self.root, name)
    }

    /// Construct a candidate from persisted metadata. It must still pass
    /// recover_additional_identity before it can be attached.
    pub fn additional_candidate(&self, name: &str) -> Result<Self, String> {
        if !self.is_additional_terminal_name(name) {
            return Err("Identidad de terminal inválida".into());
        }
        Ok(Self {
            name: name.into(),
            ..self.clone()
        })
    }

    /// Only use a caller-retained, freshly generated name. `new-session`
    /// rejects collisions; it must never adopt or replace an existing one.
    /// A timeout or configuration failure may leave the session alive: the
    /// caller must reconcile this exact name, not automatically retry.
    pub fn create_additional_terminal(
        &self,
        name: &str,
        personal_shell: bool,
    ) -> Result<(Self, AdditionalTerminalIdentity), String> {
        if !self.is_additional_terminal_name(name) {
            return Err("Identidad de terminal inválida".into());
        }
        let original_server = self.additional_terminal_server_stamp()?;
        let mut cmd = self.command();
        cmd.arg("-N")
            .args(["new-session", "-d", "-s", name, "-n", "terminal", "-c"])
            .arg(&self.root)
            .arg(if personal_shell {
                "/bin/zsh -l"
            } else {
                "/bin/sh -i"
            });
        output(cmd)
            .map_err(|e| format!("Creación de {name}: {e}; verificar antes de reintentar"))?;
        let session = Self {
            name: name.into(),
            ..self.clone()
        };
        let target = format!("={name}:");
        for (option, value) in [
            (
                "default-command",
                if personal_shell {
                    "/bin/zsh -l"
                } else {
                    "/bin/sh -i"
                },
            ),
            ("destroy-unattached", "off"),
        ] {
            let mut cmd = session.command();
            cmd.arg("-N")
                .args(["set-option", "-t", &target, option, value]);
            output(cmd).map_err(|e| {
                format!("Configuración de {name}: {e}; la sesión puede seguir activa")
            })?;
        }
        let identity = session
            .additional_terminal_identity()
            .map_err(|e| format!("Verificación de {name}: {e}; la sesión puede seguir activa"))?;
        let server_after = self
            .additional_terminal_server_stamp()
            .map_err(|e| format!("Verificación de {name}: {e}; la sesión puede seguir activa"))?;
        if (
            identity.server_pid,
            identity.socket_device,
            identity.socket_inode,
        ) != original_server
            || server_after != original_server
        {
            return Err(format!(
                "Servidor tmux cambió; la sesión {name} puede seguir activa"
            ));
        }
        Ok((session, identity))
    }

    fn additional_terminal_server_stamp(&self) -> Result<(u32, u64, u64), String> {
        use std::os::unix::fs::FileTypeExt;
        let socket = fs::symlink_metadata(&self.socket).map_err(|e| e.to_string())?;
        if !socket.file_type().is_socket() {
            return Err("Socket tmux inválido".into());
        }
        let target = self.resume_target()?;
        let pid = self.server_pid(&target)?;
        let after = fs::symlink_metadata(&self.socket).map_err(|e| e.to_string())?;
        if !after.file_type().is_socket()
            || socket.dev() != after.dev()
            || socket.ino() != after.ino()
        {
            return Err("Socket tmux cambió durante la verificación".into());
        }
        Ok((pid, socket.dev(), socket.ino()))
    }

    /// Names only, from the already-running private server. Never start a new
    /// server or inspect terminal output while discovering surviving sessions.
    pub fn discover_additional_terminals(&self) -> Result<Vec<Self>, String> {
        let mut cmd = self.command();
        cmd.arg("-N")
            .args(["list-sessions", "-F", "#{session_name}"]);
        let names = output(cmd)?;
        let mut sessions: Vec<_> = names
            .lines()
            .filter(|name| self.is_additional_terminal_name(name))
            .map(|name| Self {
                name: name.into(),
                ..self.clone()
            })
            .collect();
        sessions.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(sessions)
    }

    /// Inspect only tmux metadata and the private socket, twice. This is an
    /// observation, not a lock: a client with access to the server can still
    /// change the session between this check and a later attach or command.
    pub fn additional_terminal_identity(&self) -> Result<AdditionalTerminalIdentity, String> {
        use std::os::unix::fs::FileTypeExt;
        if !self.is_additional_terminal_name(&self.name) {
            return Err("Identidad de terminal inválida".into());
        }
        let observe = || -> Result<AdditionalTerminalIdentity, String> {
            let socket = fs::symlink_metadata(&self.socket).map_err(|e| e.to_string())?;
            if !socket.file_type().is_socket() {
                return Err("Socket tmux inválido".into());
            }
            let target = format!("={}:", self.name);
            let mut cmd = self.command();
            cmd.arg("-N").args([
                "display-message",
                "-p",
                "-t",
                &target,
                "#{session_id}\t#{pid}\t#{session_name}",
            ]);
            let response = output(cmd)?;
            let fields: Vec<_> = response.trim_end_matches('\n').split('\t').collect();
            if fields.len() != 3 || fields[2] != self.name {
                return Err("Identidad de sesión tmux cambió".into());
            }
            let session_id = fields[0];
            if !session_id.starts_with('$')
                || session_id.len() < 2
                || session_id.len() > 24
                || !session_id.as_bytes()[1..].iter().all(u8::is_ascii_digit)
            {
                return Err("ID de sesión tmux inválido".into());
            }
            let server_pid = fields[1].parse::<u32>().map_err(|_| "PID tmux inválido")?;
            if server_pid == 0 {
                return Err("PID tmux inválido".into());
            }
            Ok(AdditionalTerminalIdentity {
                name: self.name.clone(),
                session_id: session_id.into(),
                server_pid,
                socket_device: socket.dev(),
                socket_inode: socket.ino(),
            })
        };
        let before = observe()?;
        if observe()? != before {
            return Err("Servidor o sesión tmux cambió durante la verificación".into());
        }
        Ok(before)
    }

    pub fn verify_additional_terminal(
        &self,
        expected: &AdditionalTerminalIdentity,
    ) -> Result<(), String> {
        if &self.additional_terminal_identity()? != expected {
            return Err("La sesión de terminal ya no coincide con la identidad guardada".into());
        }
        Ok(())
    }

    fn additional_identity_path(&self) -> Result<PathBuf, String> {
        if !self.is_additional_terminal_name(&self.name) {
            return Err("Identidad de terminal inválida".into());
        }
        let dir = self.socket.parent().ok_or("Socket tmux sin directorio")?;
        let meta = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        let uid = unsafe { libc::geteuid() };
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o777 != 0o700 {
            return Err("Directorio de tmux no es privado".into());
        }
        Ok(dir.join(format!("{}.identity.json", self.name)))
    }

    /// Persist only a verified identity. Never overwrite or remove a record,
    /// including after an interrupted write; an uncertain result needs review.
    pub fn persist_additional_identity(
        &self,
        identity: &AdditionalTerminalIdentity,
    ) -> Result<(), String> {
        self.verify_additional_terminal(identity)?;
        let path = self.additional_identity_path()?;
        let bytes = serde_json::to_vec(identity).map_err(|e| e.to_string())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::File::open(path.parent().ok_or("Directorio inválido")?)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| e.to_string())?;
        self.verify_additional_terminal(identity)
            .map_err(|e| format!("Registro conservado, pero {e}"))
    }

    /// Load a bounded private record and match it to this live session. Never
    /// infer ownership from discovery by name or overwrite an invalid record.
    pub fn recover_additional_identity(&self) -> Result<AdditionalTerminalIdentity, String> {
        let path = self.additional_identity_path()?;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        // SAFETY: geteuid has no arguments or memory-safety preconditions.
        let uid = unsafe { libc::geteuid() };
        if !meta.is_file()
            || meta.uid() != uid
            || meta.nlink() != 1
            || meta.mode() & 0o777 != 0o600
            || meta.len() > 4096
        {
            return Err("Registro de terminal inválido o no privado".into());
        }
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 4096 {
            return Err("Registro de terminal demasiado grande".into());
        }
        let identity: AdditionalTerminalIdentity =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        self.verify_additional_terminal(&identity)?;
        Ok(identity)
    }
    /// Validate the tmux window and pane around a background operation. The
    /// caller must still check its native pane/generation on completion.
    pub fn while_target_stable<T>(
        &self,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let before = self.resume_target()?;
        let result = operation()?;
        if self.resume_target()? != before {
            return Err("Apertura cancelada: cambió el destino de tmux".into());
        }
        Ok(result)
    }
    fn copy_frame(&self, target: &ResumeTarget) -> Result<CopyFrame, String> {
        let text = self.run(&[
            "display-message",
            "-p",
            "-t",
            &target.pane,
            "#{pane_mode}\t#{copy_cursor_y}\t#{copy_cursor_x}\t#{scroll_position}\t#{pane_height}\t#{selection_present}\t#{pane_width}\t#{history_size}\t#{selection_active}",
        ])?;
        let parts = text.trim_end_matches('\n').split('\t').collect::<Vec<_>>();
        if parts.len() != 9 || parts[0] != "copy-mode" || parts[5] != "0" || parts[8] != "0" {
            return Err("La terminal no está en modo copia sin selección".into());
        }
        let parse = |value: &str| {
            value
                .parse::<usize>()
                .map_err(|_| "Estado de copia inválido".to_owned())
        };
        let frame = CopyFrame {
            row: parse(parts[1])?,
            column: parse(parts[2])?,
            scroll: parse(parts[3])?,
            height: parse(parts[4])?,
            width: parse(parts[6])?,
            history: parse(parts[7])?,
        };
        if frame.height == 0
            || frame.height > 10_000
            || frame.row >= frame.height
            || frame.width == 0
            || frame.width > 10_000
            || frame.column >= frame.width
            || frame.history > 1_000_000
            || frame.scroll > 1_000_000
        {
            return Err("Estado de copia fuera de límites".into());
        }
        Ok(frame)
    }

    /// When copy-mode retains rows absent from the live pane, determine its
    /// history from a bounded physical-row capture. The inclusive end row 0
    /// makes zero and one history row distinguishable, even if lines match.
    fn copy_backing_history(&self, target: &ResumeTarget, scroll: usize) -> Result<usize, String> {
        const MAX_ROWS: usize = 1024;
        let mut requested = scroll.max(1);
        loop {
            if requested > MAX_ROWS {
                return Err("Historial de copia demasiado grande para verificar".into());
            }
            let start = format!("-{requested}");
            let capture = self.run(&[
                "capture-pane",
                "-p",
                "-M",
                "-S",
                &start,
                "-E",
                "0",
                "-t",
                &target.pane,
            ])?;
            if !capture.ends_with('\n') {
                return Err("Historial de copia incompleto".into());
            }
            let rows = capture.bytes().filter(|byte| *byte == b'\n').count();
            if rows == 0 || rows > requested + 1 {
                return Err("Historial de copia inválido".into());
            }
            if rows <= requested {
                let history = rows - 1;
                return if history >= scroll {
                    Ok(history)
                } else {
                    Err("Historial de copia cambió".into())
                };
            }
            requested = requested
                .checked_mul(2)
                .ok_or("Historial de copia fuera de límites")?;
        }
    }

    fn server_pid(&self, target: &ResumeTarget) -> Result<u32, String> {
        let pid = self
            .run(&["display-message", "-p", "-t", &target.pane, "#{pid}"])?
            .trim()
            .parse::<u32>()
            .map_err(|_| "Servidor tmux inválido".to_owned())?;
        if pid == 0 {
            return Err("Servidor tmux inválido".into());
        }
        Ok(pid)
    }

    fn copy_row_digest(&self, target: &ResumeTarget, relative: i64) -> Result<[u8; 32], String> {
        let position = relative.to_string();
        let mut command = self.command();
        command.args([
            "capture-pane",
            "-p",
            "-M",
            "-N",
            "-e",
            "-S",
            &position,
            "-E",
            &position,
            "-t",
            &target.pane,
        ]);
        // Keep invalid UTF-8 distinct in row identity; do not log the bytes.
        let line = output_bytes(command)?;
        Ok(Sha256::digest(&line).into())
    }

    /// Identify the current row across a reflow only when the row and its
    /// immediate neighbors form a unique sequence in the bounded copy grid.
    /// The UTF-8 byte length conservatively bounds cell width for non-control
    /// text: tmux stores each Unicode character in at most two cells. Reject
    /// wrapped, tabbed and otherwise ambiguous content.
    fn resize_row_digest(&self, target: &ResumeTarget, frame: &CopyFrame) -> Option<[u8; 32]> {
        const MAX_ROWS: usize = 1024;
        if frame.height > MAX_ROWS || frame.history > MAX_ROWS {
            return None;
        }
        let mut command = self.command();
        command.args([
            "capture-pane",
            "-p",
            "-M",
            "-T",
            "-N",
            "-S",
            "-",
            "-E",
            "-",
            "-t",
            &target.pane,
        ]);
        // Hash exact bytes: lossy decoding can make distinct invalid
        // neighbors indistinguishable. Never retain or log captured text.
        let screen = output_bytes(command).ok()?;
        let rows: Vec<_> = screen
            .strip_suffix(b"\n")?
            .split(|byte| *byte == b'\n')
            .collect();
        let history = rows.len().checked_sub(frame.height)?;
        if history > MAX_ROWS || frame.scroll > history {
            return None;
        }
        let index = history.checked_add(frame.row)?.checked_sub(frame.scroll)?;
        let row = *rows.get(index)?;
        let text = std::str::from_utf8(row).ok()?;
        if row.is_empty()
            || row.len() >= frame.width
            || frame.column > row.len().saturating_add(1)
            || text.chars().any(|ch| ch.is_control())
            || !text.chars().any(|ch| !ch.is_whitespace())
        {
            return None;
        }
        // `-T -N` and `-J` use the same cell serialization in tmux, but
        // `-J` omits a separator after a wrapped row. Compare three physical
        // rows to ensure neither the preceding nor current row wraps. The
        // final transport newline is present regardless of the third row's
        // wrap flag. Keep captures transient and reject clipped/changed data.
        let neighbors = rows.get(index.checked_sub(1)?..=index.checked_add(1)?)?;
        // Repeated prompts are common. Their surrounding rows can still
        // identify exactly one location without retaining terminal text.
        if rows
            .windows(3)
            .filter(|window| *window == neighbors)
            .count()
            != 1
        {
            return None;
        }
        let relative = i64::try_from(index).ok()? - i64::try_from(history).ok()?;
        let start = (relative - 1).to_string();
        let end = (relative + 1).to_string();
        let mut joined_command = self.command();
        joined_command.args([
            "capture-pane",
            "-p",
            "-M",
            "-J",
            "-S",
            &start,
            "-E",
            &end,
            "-t",
            &target.pane,
        ]);
        let joined = output_bytes(joined_command).ok()?;
        let mut rest = joined.as_slice();
        for neighbor in neighbors {
            rest = rest.strip_prefix(*neighbor)?.strip_prefix(b"\n")?;
        }
        if !rest.is_empty() {
            return None;
        }
        let mut digest = Sha256::new();
        for neighbor in [index.checked_sub(1), Some(index), index.checked_add(1)] {
            if let Some(text) = neighbor.and_then(|i| rows.get(i)) {
                digest.update([1]);
                digest.update((text.len() as u64).to_be_bytes());
                digest.update(text);
            } else {
                digest.update([0]);
            }
        }
        Some(digest.finalize().into())
    }

    /// Take a read-only mark only when tmux, its copy cursor and the target
    /// row agree across two observations. This does not yet restore marks;
    /// identical row text is not proof against every history rewrite.
    pub fn capture_copy_mark(&self) -> Result<CopyMark, String> {
        let socket_before = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        let target = self.resume_target()?;
        let session_id = self.session_id()?;
        let pid = self.server_pid(&target)?;
        let frame = self.copy_frame(&target)?;
        let backing_history = if frame.scroll > frame.history {
            Some(self.copy_backing_history(&target, frame.scroll)?)
        } else {
            None
        };
        let relative = frame.row as i64 - frame.scroll as i64;
        let first = self.copy_row_digest(&target, relative)?;
        let second = self.copy_row_digest(&target, relative)?;
        let resize_row_digest = self.resize_row_digest(&target, &frame);
        let socket_after = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        let pid_after = self.server_pid(&target)?;
        if first != second
            || socket_before.dev() != socket_after.dev()
            || socket_before.ino() != socket_after.ino()
            || pid != pid_after
            || self.session_id()? != session_id
            || self.resume_target()? != target
            || self.copy_frame(&target)? != frame
            || if let Some(history) = backing_history {
                self.copy_backing_history(&target, frame.scroll)? != history
            } else {
                false
            }
        {
            return Err("Destino o historial cambió; marca cancelada".into());
        }
        Ok(CopyMark {
            target,
            session_id,
            server_pid: pid,
            socket_device: socket_before.dev(),
            socket_inode: socket_before.ino(),
            frame,
            backing_history,
            row_digest: first,
            resize_row_digest,
        })
    }

    /// tmux repositions its current copy cursor when resizing. Accept that
    /// position only when both observations identify the same unique row,
    /// column and exact terminal identity. Never issue a movement here.
    fn resized_copy_cursor_matches(&self, mark: &CopyMark, inactive: bool) -> Result<bool, String> {
        let Some(expected_row) = mark.resize_row_digest else {
            return Ok(false);
        };
        let socket = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        let target = &mark.target;
        let matches_target = || {
            if inactive {
                self.mark_target_in_session(mark)
            } else {
                self.resume_target().map(|current| current == *target)
            }
        };
        if !matches_target()?
            || self.session_id()? != mark.session_id
            || self.server_pid(target)? != mark.server_pid
            || socket.dev() != mark.socket_device
            || socket.ino() != mark.socket_inode
        {
            return Ok(false);
        }
        let before = self.copy_frame(target)?;
        if (before.width, before.height) == (mark.frame.width, mark.frame.height)
            || before.column != mark.frame.column
            || self.resize_row_digest(target, &before) != Some(expected_row)
            || self.resize_row_digest(target, &before) != Some(expected_row)
        {
            return Ok(false);
        }
        let after_socket = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        Ok(self.copy_frame(target)? == before
            && matches_target()?
            && self.session_id()? == mark.session_id
            && self.server_pid(target)? == mark.server_pid
            && after_socket.dev() == mark.socket_device
            && after_socket.ino() == mark.socket_inode)
    }

    /// Validate the original row while allowing the copy cursor to have moved.
    /// Equal digests cannot prove that an identical row wasn't substituted.
    pub fn copy_mark_matches(&self, mark: &CopyMark) -> Result<bool, String> {
        self.validated_copy_mark_frame(mark)
            .map(|frame| frame.is_some())
    }

    fn validated_copy_mark_frame(&self, mark: &CopyMark) -> Result<Option<CopyFrame>, String> {
        self.validated_copy_mark_frame_with_target(mark, false)
    }

    /// For an explicitly addressed inactive window, the full target is first
    /// resolved inside the original session; it is never selected as a side
    /// effect of validating the mark.
    fn mark_target_in_session(&self, mark: &CopyMark) -> Result<bool, String> {
        let target = format!("{}:{}.{}", self.name, mark.target.window, mark.target.pane);
        let resolved = self.run(&[
            "display-message",
            "-p",
            "-t",
            &target,
            "#{session_id}\t#{window_id}\t#{pane_id}",
        ])?;
        let expected = format!(
            "{}\t{}\t{}",
            mark.session_id, mark.target.window, mark.target.pane
        );
        Ok(resolved.trim_end_matches('\n') == expected)
    }

    fn validated_copy_mark_frame_with_target(
        &self,
        mark: &CopyMark,
        inactive: bool,
    ) -> Result<Option<CopyFrame>, String> {
        let socket = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        let target = &mark.target;
        let target_matches = if inactive {
            self.mark_target_in_session(mark)?
        } else {
            self.resume_target()? == *target
        };
        if !target_matches
            || self.session_id()? != mark.session_id
            || socket.dev() != mark.socket_device
            || socket.ino() != mark.socket_inode
            || self.server_pid(target)? != mark.server_pid
        {
            return Ok(None);
        }
        let before = self.copy_frame(target)?;
        if before.height != mark.frame.height
            || before.width != mark.frame.width
            || before.history != mark.frame.history
        {
            return Ok(None);
        }
        if let Some(history) = mark.backing_history
            && self.copy_backing_history(target, mark.frame.scroll)? != history
        {
            return Ok(None);
        }
        let relative = mark.frame.row as i64 - mark.frame.scroll as i64;
        let first = self.copy_row_digest(target, relative)?;
        let second = self.copy_row_digest(target, relative)?;
        let after_socket = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        let valid = first == mark.row_digest
            && second == first
            && self.copy_frame(target)? == before
            && if let Some(history) = mark.backing_history {
                self.copy_backing_history(target, mark.frame.scroll)? == history
            } else {
                true
            }
            && (if inactive {
                self.mark_target_in_session(mark)?
            } else {
                self.resume_target()? == *target
            })
            && self.session_id()? == mark.session_id
            && self.server_pid(target)? == mark.server_pid
            && after_socket.dev() == mark.socket_device
            && after_socket.ino() == mark.socket_inode;
        Ok(valid.then_some(before))
    }

    /// Restore within tmux copy mode only, never through the terminal PTY.
    /// The row digest cannot detect replacement by identical content or an
    /// away-and-back race; this experimental primitive is not wired to UI.
    pub fn restore_copy_mark(&self, mark: &CopyMark) -> RestoreCopyMark {
        self.restore_copy_mark_with_target(mark, false)
    }

    /// Restore an explicitly addressed pane in the captured session without
    /// selecting its window. This is not yet connected to the UI.
    pub fn restore_copy_mark_in_session(&self, mark: &CopyMark) -> RestoreCopyMark {
        self.restore_copy_mark_with_target(mark, true)
    }

    fn restore_copy_mark_with_target(&self, mark: &CopyMark, inactive: bool) -> RestoreCopyMark {
        // A tmux cursor-right step can cross multiple cells (wide characters
        // and tabs). Until there is a bounded server-side loop, avoid building
        // an unbounded command string for unusually wide panes.
        const MAX_RIGHT_STEPS: usize = 512;
        if mark.frame.column > MAX_RIGHT_STEPS
            || self.name.is_empty()
            || self.name.len() > 128
            || !self
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return RestoreCopyMark::RejectedBeforeMove;
        }
        if self
            .resized_copy_cursor_matches(mark, inactive)
            .unwrap_or(false)
        {
            return RestoreCopyMark::Restored;
        }
        let Some(before) = self
            .validated_copy_mark_frame_with_target(mark, inactive)
            .ok()
            .flatten()
        else {
            return RestoreCopyMark::RejectedBeforeMove;
        };
        // The validated cursor may already be at the captured position. In
        // particular, a freshly opened buffer often leaves copy mode exactly
        // where it was. Replaying goto-line and horizontal movement can change
        // an otherwise correct cursor (for example, at the end of a prompt).
        // The validation above checked the target, row digest and full frame;
        // no tmux command is needed when that same frame is still current.
        if before == mark.frame {
            return RestoreCopyMark::Restored;
        }
        let equals = |field: &str, value: String| format!("#{{==:#{{{field}}},{value}}}");
        let predicates = [
            equals("pid", mark.server_pid.to_string()),
            equals("session_id", mark.session_id.clone()),
            equals("window_id", mark.target.window.clone()),
            equals("pane_id", mark.target.pane.clone()),
            equals("pane_mode", "copy-mode".into()),
            equals("selection_present", "0".into()),
            equals("selection_active", "0".into()),
            equals("pane_width", before.width.to_string()),
            equals("pane_height", before.height.to_string()),
            equals("history_size", before.history.to_string()),
            equals("scroll_position", before.scroll.to_string()),
            equals("copy_cursor_y", before.row.to_string()),
            equals("copy_cursor_x", before.column.to_string()),
        ];
        let condition = predicates
            .into_iter()
            .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
            .expect("fixed nonempty predicate list");
        let pane = &mark.target.pane;
        let context = if inactive {
            format!("{}:{}.{}", self.name, mark.target.window, mark.target.pane)
        } else {
            format!("{}:", self.name)
        };
        let mut commands = vec![format!(
            "send-keys -t {pane} -X goto-line {}",
            mark.frame.scroll
        )];
        if before.row != mark.frame.row {
            let direction = if before.row < mark.frame.row {
                "cursor-down"
            } else {
                "cursor-up"
            };
            commands.push(format!(
                "send-keys -t {pane} -X -N {} {direction}",
                before.row.abs_diff(mark.frame.row)
            ));
        }
        commands.push(format!("send-keys -t {pane} -X start-of-line"));
        if mark.frame.column > 0 {
            let right_checks = [
                equals("pid", mark.server_pid.to_string()),
                equals("session_id", mark.session_id.clone()),
                equals("window_id", mark.target.window.clone()),
                equals("pane_id", pane.clone()),
                equals("pane_mode", "copy-mode".into()),
                equals("selection_present", "0".into()),
                equals("selection_active", "0".into()),
                equals("pane_width", before.width.to_string()),
                equals("pane_height", before.height.to_string()),
                equals("history_size", before.history.to_string()),
                equals("scroll_position", mark.frame.scroll.to_string()),
                equals("copy_cursor_y", mark.frame.row.to_string()),
                // tmux's plain < compares strings; cursor columns are numeric.
                format!("#{{e|<:#{{copy_cursor_x}},{}}}", mark.frame.column),
            ];
            let right_condition = right_checks
                .into_iter()
                .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
                .expect("fixed nonempty predicate list");
            for _ in 0..mark.frame.column {
                commands.push(format!(
                    "if-shell -F -t {context} '{right_condition}' 'send-keys -t {pane} -X cursor-right'"
                ));
            }
        }
        commands.push(format!("display-message -p -t {pane} TWR_MARK_OK"));
        let decision = self.run(&[
            "if-shell",
            "-F",
            "-t",
            &context,
            &condition,
            &commands.join(" ; "),
            "display-message -p TWR_MARK_REJECT",
        ]);
        match decision.as_deref().map(str::trim) {
            Ok("TWR_MARK_REJECT") => RestoreCopyMark::RejectedBeforeMove,
            Ok("TWR_MARK_OK") => {
                if self
                    .validated_copy_mark_frame_with_target(mark, inactive)
                    .ok()
                    .flatten()
                    .as_ref()
                    == Some(&mark.frame)
                {
                    RestoreCopyMark::Restored
                } else {
                    RestoreCopyMark::UncertainAfterMove
                }
            }
            _ => RestoreCopyMark::UncertainAfterMove,
        }
    }

    fn session_id(&self) -> Result<String, String> {
        let id = self.run(&[
            "display-message",
            "-p",
            "-t",
            &format!("{}:", self.name),
            "#{session_id}",
        ])?;
        let id = id.trim();
        if id.len() < 2
            || id.len() > 24
            || !id.starts_with('$')
            || !id.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        {
            return Err("ID de sesión tmux inválido".into());
        }
        Ok(id.to_owned())
    }

    /// Capture a bounded number of physical copy-mode rows without moving the
    /// cursor. Tabs are rejected because their historical cell widths cannot
    /// be recovered safely from captured text or the current tab stops. This
    /// Erase-to-end and erase-character can also produce indistinguishable
    /// padding and written spaces, so even accepted text is not a Neovim yank.
    /// This primitive is not wired to editor registers or user-facing yank.
    pub fn capture_normal_lines(&self, count: usize) -> Result<String, String> {
        if !(1..=32).contains(&count) {
            return Err("Conteo de captura fuera de límites".into());
        }
        let target = self.resume_target()?;
        let before = self.copy_frame(&target)?;
        if count > before.height - before.row + before.scroll {
            return Err("La captura excede el historial visible".into());
        }
        let mut result = String::new();
        for offset in 0..count {
            let row = before.row as i64 - before.scroll as i64 + offset as i64;
            let position = row.to_string();
            let line = self.run(&[
                "capture-pane",
                "-p",
                "-M",
                "-J",
                "-S",
                &position,
                "-E",
                &position,
                "-t",
                &target.pane,
            ])?;
            if !line.ends_with('\n')
                || line[..line.len() - 1].chars().any(char::is_control)
                || result.len().saturating_add(line.len()) > 262_144
            {
                return Err("Captura de terminal no representable de forma segura".into());
            }
            result.push_str(&line);
        }
        if self.resume_target()? != target || self.copy_frame(&target)? != before {
            return Err("El destino o el cursor de copia cambió; captura cancelada".into());
        }
        Ok(result)
    }

    fn resume_target(&self) -> Result<ResumeTarget, String> {
        let text = self.run(&[
            "display-message",
            "-p",
            "-t",
            &format!("{}:", self.name),
            "#{window_id}\t#{pane_id}",
        ])?;
        ResumeTarget::parse(&text)
    }
    fn resume(&self) -> Result<ResumeTarget, String> {
        let target = self.resume_target()?;
        if self
            .run(&[
                "display-message",
                "-p",
                "-t",
                &target.pane,
                "#{pane_in_mode}",
            ])?
            .trim()
            != "0"
        {
            self.run(&["send-keys", "-t", &target.pane, "-X", "cancel"])?;
        }
        if self.resume_target()? != target
            || self
                .run(&[
                    "display-message",
                    "-p",
                    "-t",
                    &target.pane,
                    "#{pane_in_mode}",
                ])?
                .trim()
                != "0"
        {
            return Err("Destino cambió o no salió de copy-mode; entrada cancelada".into());
        }
        Ok(target)
    }
    fn resume_input(&self, target: &ResumeTarget, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty()
            || bytes.len() > 4096
            || bytes
                .iter()
                .any(|b| *b < 32 && !matches!(*b, 8 | 9 | 10 | 13))
            || bytes.contains(&127)
        {
            return Err("Entrada de transición fuera de límites".into());
        }
        if self.resume_target()? != *target
            || self
                .run(&[
                    "display-message",
                    "-p",
                    "-t",
                    &target.pane,
                    "#{pane_in_mode}",
                ])?
                .trim()
                != "0"
        {
            return Err("Pane o ventana cambió; entrada pendiente cancelada".into());
        }
        // One argv command, bounded to 4 KiB. Hex arguments cannot become tmux
        // options or commands. Explicit pane ID prevents retargeting even if an
        // external client changes the active pane after the check above.
        let mut c = self.command();
        c.args(["send-keys", "-t", &target.pane, "-H"]);
        c.args(bytes.iter().map(|b| format!("{b:02x}")));
        output(c).map(|_| ())
    }
    fn command(&self) -> Command {
        let mut c = Command::new(&self.binary);
        // Control replies must retain UTF-8 and literal tab separators even when
        // the app inherits LC_ALL=C. This does not change the pane's shell locale.
        c.args(["-u", "-S"])
            .arg(&self.socket)
            .env_remove("TMUX")
            .env_remove("ENV")
            .env_remove("BASH_ENV")
            .env("COLORTERM", "truecolor")
            .env(
                "LANG",
                std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".into()),
            );
        c
    }
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut c = self.command();
        c.args(args);
        output(c)
    }
    pub fn prepare(root: &Path, directory: &Path, personal_shell: bool) -> Result<Self, String> {
        let uid = output(Command::new("/usr/bin/id").tap_args(&["-u"]))?
            .trim()
            .parse::<u32>()
            .map_err(|e| e.to_string())?;
        if !directory.exists() {
            let mut b = fs::DirBuilder::new();
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700).create(directory).map_err(|e| e.to_string())?;
        }
        let m = fs::symlink_metadata(directory).map_err(|e| e.to_string())?;
        if !m.is_dir() || m.uid() != uid || m.permissions().mode() & 0o777 != 0o700 {
            return Err("tmux state directory must be owned by you, real and mode 0700".into());
        }
        let binary = [
            "/opt/homebrew/bin/tmux",
            "/usr/local/bin/tmux",
            "/usr/bin/tmux",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or("tmux executable not found")?;
        #[cfg(test)]
        let binary = if let Some(path) = std::env::var_os("TERMINAL_WORKSPACE_TEST_TMUX_BINARY") {
            let path = PathBuf::from(path);
            let m = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !path.is_absolute()
                || !m.is_file()
                || m.uid() != uid
                || m.nlink() != 1
                || m.mode() & 0o022 != 0
            {
                return Err("Unsafe explicitly selected test tmux binary".into());
            }
            path
        } else {
            binary
        };
        let session = Self {
            binary,
            socket: directory.join("tmux.sock"),
            name: project_id(root),
            root: root.to_owned(),
        };
        if session.socket.as_os_str().len() > 100 {
            return Err("tmux socket path too long".into());
        }
        if fs::symlink_metadata(&session.socket)
            .is_ok_and(|m| m.file_type().is_symlink() || m.uid() != uid)
        {
            return Err("unsafe tmux socket".into());
        }
        let conf = directory.join("navigation.conf");
        let contents = include_str!("../../../Resources/Fixtures/native-tmux.conf");
        match fs::symlink_metadata(&conf) {
            Ok(m) if !m.is_file() || m.uid() != uid || m.nlink() != 1 || m.mode() & 0o022 != 0 => {
                return Err("unsafe tmux config path".into());
            }
            Ok(_) => {
                if fs::read_to_string(&conf).map_err(|e| e.to_string())? != contents {
                    return Err("Private tmux config differs from the bundled contract".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                use std::{io::Write, os::unix::fs::OpenOptionsExt};
                let mut out = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&conf)
                    .map_err(|e| e.to_string())?;
                out.write_all(contents.as_bytes())
                    .map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(e.to_string()),
        }
        // -f is used only when this private server starts. Never source user config.
        if session.run(&["has-session", "-t", &session.name]).is_ok() {
            session.configure_terminal_protocols()?;
            return Ok(session);
        }
        let mut c = session.command();
        c.arg("-f")
            .arg(conf)
            .args([
                "new-session",
                "-d",
                "-s",
                &session.name,
                "-n",
                "terminal",
                "-c",
            ])
            .arg(root);
        c.arg(if personal_shell {
            "/bin/zsh -l"
        } else {
            "/bin/sh -i"
        });
        output(c)?;
        session.run(&[
            "set-option",
            "-t",
            &session.name,
            "default-command",
            if personal_shell {
                "/bin/zsh -l"
            } else {
                "/bin/sh -i"
            },
        ])?;
        session.run(&[
            "set-option",
            "-t",
            &session.name,
            "destroy-unattached",
            "off",
        ])?;
        session.configure_terminal_protocols()?;
        Ok(session)
    }
    fn configure_terminal_protocols(&self) -> Result<(), String> {
        // Only this app-owned socket. `on` passes escapes from visible panes;
        // `all` would also pass output from hidden panes and is not used.
        #[cfg(feature = "ghostty")]
        self.run(&["set-option", "-g", "-p", "allow-passthrough", "on"])?;
        Ok(())
    }
    pub fn attach(&self) -> CommandBuilder {
        let mut c = CommandBuilder::new(&self.binary);
        c.args(["-S"]);
        c.arg(&self.socket);
        c.args(["attach-session", "-t", &self.name]);
        c.env_remove("TMUX");
        c.env("TERM", "xterm-256color");
        c
    }
    /// Attach to the verified session ID, then verify the client and server
    /// again after launching. A name alone can be reused by another session.
    pub fn attach_additional(
        &self,
        identity: &AdditionalTerminalIdentity,
    ) -> Result<CommandBuilder, String> {
        self.verify_additional_terminal(identity)?;
        let mut c = CommandBuilder::new(&self.binary);
        c.args(["-N", "-S"]);
        c.arg(&self.socket);
        c.args(["attach-session", "-t", &identity.session_id]);
        c.env_remove("TMUX");
        c.env("TERM", "xterm-256color");
        Ok(c)
    }
    pub fn client_pids(&self) -> Result<Vec<u32>, String> {
        let output = self.run(&["list-clients", "-t", &self.name, "-F", "#{client_pid}"])?;
        output
            .lines()
            .map(|s| {
                s.parse::<u32>()
                    .ok()
                    .filter(|pid| *pid > 1)
                    .ok_or_else(|| "invalid tmux client PID".into())
            })
            .collect()
    }
    pub fn windows(&self) -> Result<Vec<WindowInfo>, String> {
        let text = self.run(&[
            "list-windows",
            "-t",
            &self.name,
            "-F",
            "#{window_id}\t#{window_index}\t#{window_active}\t#{pane_id}\t#{window_name}",
        ])?;
        text.lines()
            .map(|line| {
                let v = line.splitn(5, '\t').collect::<Vec<_>>();
                if v.len() != 5 || !v[0].starts_with('@') || !v[3].starts_with('%') {
                    return Err("invalid tmux window metadata".into());
                }
                Ok(WindowInfo {
                    id: v[0].into(),
                    active_pane: v[3].into(),
                    index: v[1].parse().map_err(|_| "invalid window index")?,
                    active: v[2] == "1",
                    name: v[4].chars().filter(|c| !c.is_control()).take(160).collect(),
                })
            })
            .collect()
    }
    fn copy_selection(&self) -> Result<String, String> {
        self.copy_selection_with_mode(false)
    }
    /// A newly started one-cell selection is active even when tmux reports
    /// selection_present=0 because both endpoints are equal.
    fn copy_selection_exists(&self, target: &str) -> Result<bool, String> {
        let state = self.run(&[
            "display-message",
            "-p",
            "-t",
            target,
            "#{pane_mode}\t#{selection_active}\t#{selection_present}",
        ])?;
        match state
            .trim_end_matches('\n')
            .split('\t')
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["copy-mode", "0", "0"] => Ok(false),
            ["copy-mode", "0" | "1", "1"] | ["copy-mode", "1", "0"] => Ok(true),
            _ => Err("Estado de selección de terminal no disponible".into()),
        }
    }
    /// Observe selection geometry in a single tmux format expansion. This
    /// deliberately does not read the text selected in the user's terminal.
    fn copy_selection_snapshot(&self) -> Result<CopySelectionSnapshot, String> {
        let state = self.run(&[
            "display-message",
            "-p",
            "-t",
            &format!("{}:", self.name),
            "#{window_id}\t#{pane_id}\t#{session_id}\t#{pid}\t#{pane_mode}\t#{selection_active}\t#{selection_present}\t#{rectangle_toggle}\t#{copy_cursor_x}\t#{copy_cursor_y}\t#{scroll_position}\t#{pane_width}\t#{pane_height}\t#{history_size}\t#{selection_start_x}\t#{selection_start_y}\t#{selection_end_x}\t#{selection_end_y}\t#{search_present}",
        ])?;
        let fields: Vec<_> = state.trim_end_matches('\n').split('\t').collect();
        let [
            window,
            pane,
            session,
            server,
            "copy-mode",
            active,
            present,
            rectangle,
            cursor_x,
            cursor_y,
            scroll,
            width,
            height,
            history,
            start_x,
            start_y,
            end_x,
            end_y,
            search_present,
        ] = fields.as_slice()
        else {
            return Err("Estado de selección de terminal no disponible".into());
        };
        let target = ResumeTarget::parse(&format!("{window}\t{pane}"))?;
        if session.len() < 2
            || session.len() > 24
            || !session.starts_with('$')
            || !session.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        {
            return Err("ID de sesión tmux inválido".into());
        }
        let number = |value: &str| {
            value
                .parse::<u64>()
                .map_err(|_| "Geometría de selección inválida".to_owned())
        };
        let flag = |value: &str| match value {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err("Estado de selección de terminal inválido".to_owned()),
        };
        let active = flag(active)?;
        let present = flag(present)?;
        let rectangle = u8::from(flag(rectangle)?);
        let server = server
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid != 0)
            .ok_or("Servidor tmux inválido")?;
        let endpoints = if active || present {
            Some([
                number(start_x)?,
                number(start_y)?,
                number(end_x)?,
                number(end_y)?,
            ])
        } else {
            None
        };
        Ok(CopySelectionSnapshot {
            target,
            session: session.to_string(),
            server,
            active,
            present,
            search_present: flag(search_present)?,
            rectangle,
            cursor_x: number(cursor_x)?,
            cursor_y: number(cursor_y)?,
            scroll: number(scroll)?,
            width: number(width)?,
            height: number(height)?,
            history: number(history)?,
            endpoints,
        })
    }

    /// Track Visual Line only when this worker actually started it. tmux does
    /// not expose its line-mode flag, so an unknown selection must not be
    /// classified as linewise solely from its endpoints.
    fn toggle_visual_line(
        &self,
        lines: &mut LineSelections,
        window: &str,
        pane: &str,
        count: usize,
    ) -> Result<(), String> {
        if !(1..=9999).contains(&count) {
            return Err("Conteo fuera de límites".into());
        }
        let socket = fs::metadata(&self.socket).map_err(|error| error.to_string())?;
        let before = self.copy_selection_snapshot()?;
        let confirmed = self.copy_selection_snapshot()?;
        let after = fs::metadata(&self.socket).map_err(|error| error.to_string())?;
        if before != confirmed
            || before.target.window != window
            || before.target.pane != pane
            || socket.dev() != after.dev()
            || socket.ino() != after.ino()
        {
            return Err("Cambió la selección; acción cancelada".into());
        }
        let key = (before.target.window.clone(), before.target.pane.clone());
        let is_line = lines.get(&key).is_some_and(|line| {
            (before.active || before.present)
                && before.rectangle == 0
                && before.session == line.session
                && before.server == line.server
                && socket.dev() == line.socket_device
                && socket.ino() == line.socket_inode
                && before
                    .endpoints
                    .is_some_and(|ends| ends[1] == line.anchor_row || ends[3] == line.anchor_row)
        });
        // A stale or uncertain record must never classify a later selection.
        lines.remove(&key);
        // For an existing character or rectangle selection, select-line must
        // start at the fixed end, not at the current cursor. tmux's other-end
        // moves there without touching the user's mark or copying any text.
        // The relative row difference is independent of backing history size.
        let mut start = before.clone();
        let mut return_rows = 0_i64;
        if !is_line && (before.active || before.present) {
            if !before.active || count != 1 {
                return Err("No se puede convertir esta selección con ese conteo".into());
            }
            let command = format!(
                "send-keys -t {} -X other-end ; display-message -p TWR_ANCHOR_OK",
                before.target.pane
            );
            let result = self.run(&[
                "if-shell",
                "-F",
                "-t",
                &format!("{}:", self.name),
                &before.condition(),
                &command,
                "display-message -p TWR_ANCHOR_REJECT",
            ])?;
            if result.trim() != "TWR_ANCHOR_OK" {
                return Err("Cambió la selección; acción cancelada".into());
            }
            start = self.copy_selection_snapshot()?;
            let socket_start = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
            let ends = before.endpoints.ok_or("Selección sin extremos")?;
            let row_before = i128::from(before.cursor_y) - i128::from(before.scroll);
            let row_start = i128::from(start.cursor_y) - i128::from(start.scroll);
            let delta = row_before - row_start;
            let x_matches = (before.cursor_x == ends[0] && start.cursor_x == ends[2])
                || (before.cursor_x == ends[2] && start.cursor_x == ends[0]);
            if start.target != before.target
                || start.session != before.session
                || start.server != before.server
                || start.width != before.width
                || start.height != before.height
                || start.history != before.history
                || start.active != before.active
                || start.present != before.present
                || start.rectangle != before.rectangle
                || start.endpoints != before.endpoints
                || !x_matches
                || delta.unsigned_abs() != u128::from(ends[1].abs_diff(ends[3]))
                || delta.unsigned_abs() > 9999
                || socket_start.dev() != socket.dev()
                || socket_start.ino() != socket.ino()
            {
                return Err("No se pudo confirmar el origen de la selección".into());
            }
            return_rows = delta as i64;
        }
        let command = if is_line {
            format!("send-keys -t {} -X clear-selection", before.target.pane)
        } else {
            let mut command = format!(
                "send-keys -t {} -X -N {count} select-line",
                before.target.pane
            );
            if return_rows != 0 {
                let direction = if return_rows > 0 {
                    "cursor-down"
                } else {
                    "cursor-up"
                };
                command.push_str(&format!(
                    " ; send-keys -t {} -X -N {} {direction}",
                    before.target.pane,
                    return_rows.unsigned_abs()
                ));
            }
            command
        };
        let command = format!("{command} ; display-message -p TWR_VISUAL_OK");
        let result = self.run(&[
            "if-shell",
            "-F",
            "-t",
            &format!("{}:", self.name),
            &start.condition(),
            &command,
            "display-message -p TWR_VISUAL_REJECT",
        ])?;
        match result.trim() {
            "TWR_VISUAL_REJECT" => return Err("Cambió la selección; acción cancelada".into()),
            "TWR_VISUAL_OK" => {}
            _ => return Err("No se pudo confirmar la selección".into()),
        }
        let mut post = self.copy_selection_snapshot()?;
        let socket_after = fs::metadata(&self.socket).map_err(|error| error.to_string())?;
        if post.target != before.target
            || post.session != before.session
            || post.server != before.server
            || socket_after.dev() != socket.dev()
            || socket_after.ino() != socket.ino()
        {
            return Err("Destino cambió después de la selección".into());
        }
        if !is_line && (before.active || before.present) {
            let original_row = i128::from(before.cursor_y) - i128::from(before.scroll);
            let current_row = i128::from(post.cursor_y) - i128::from(post.scroll);
            if current_row != original_row {
                return Err("No se pudo confirmar la línea del cursor".into());
            }
        }
        if is_line {
            if post.active || post.present {
                return Err("No se pudo confirmar el cierre de la selección".into());
            }
        } else if (post.active || post.present) && post.rectangle == 0 {
            // select-line or a vertical movement can place the cursor at
            // either edge. Restore toward its original display column without
            // changing an existing tmux mark. Wide characters can move more
            // than one cell per step; check the live column before each step.
            // Limit command size; extremely wide panes retain tmux's previous
            // behavior rather than enqueueing an unbounded command list.
            const MAX_RESTORE_STEPS: u64 = 256;
            let desired = before.cursor_x;
            let steps = post.cursor_x.abs_diff(desired);
            if steps > 0 && steps <= MAX_RESTORE_STEPS {
                let fixed_x = format!("#{{==:#{{copy_cursor_x}},{}}}", post.cursor_x);
                // Plain comparison modifiers compare strings, not coordinates.
                let (comparison, movement) = if post.cursor_x > desired {
                    (">", "cursor-left")
                } else {
                    ("<", "cursor-right")
                };
                let live_x = format!("#{{e|{comparison}:#{{copy_cursor_x}},{desired}}}");
                let condition = post.condition();
                if !condition.contains(&fixed_x) {
                    return Err("No se pudo preparar la posición del cursor".into());
                }
                let step_condition = condition.replacen(&fixed_x, &live_x, 1);
                let step = format!(
                    "if-shell -F -t {session}: '{step_condition}' 'send-keys -t {pane} -X {movement}' 'display-message -p TWR_COLUMN_STEP_SKIP'",
                    pane = post.target.pane,
                    session = self.name,
                );
                let mut steps_command = vec![step; steps as usize];
                steps_command.push("display-message -p TWR_COLUMN_OK".into());
                let outcome = self.run(&[
                    "if-shell",
                    "-F",
                    "-t",
                    &format!("{}:", self.name),
                    &post.condition(),
                    &steps_command.join(" ; "),
                    "display-message -p TWR_COLUMN_REJECT",
                ])?;
                if !outcome
                    .lines()
                    .last()
                    .is_some_and(|line| line == "TWR_COLUMN_OK")
                {
                    return Err("Cambió la selección antes de restaurar el cursor".into());
                }
                let restored = self.copy_selection_snapshot()?;
                let socket_restored = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
                if restored.target != post.target
                    || restored.session != post.session
                    || restored.server != post.server
                    || restored.cursor_y != post.cursor_y
                    || restored.scroll != post.scroll
                    || restored.width != post.width
                    || restored.height != post.height
                    || restored.history != post.history
                    || restored.active != post.active
                    || restored.present != post.present
                    || restored.rectangle != post.rectangle
                    || restored.endpoints != post.endpoints
                    || (movement == "cursor-left" && restored.cursor_x > desired)
                    || (movement == "cursor-right" && restored.cursor_x != desired)
                    || socket_restored.dev() != socket.dev()
                    || socket_restored.ino() != socket.ino()
                {
                    return Err(format!(
                        "No se pudo confirmar la posición del cursor ({}, esperado hasta {})",
                        restored.cursor_x, desired
                    ));
                }
                post = restored;
            }
            let ends = post.endpoints.ok_or("Selección sin extremos")?;
            // Closed tmux panes need not send a notification to this worker.
            // Bound retained markers; an evicted selection is treated as unknown.
            if lines.len() >= 1024 {
                lines.clear();
            }
            lines.insert(
                key,
                LineSelection {
                    session: post.session,
                    server: post.server,
                    socket_device: socket.dev(),
                    socket_inode: socket.ino(),
                    anchor_row: ends[1],
                },
            );
        } else {
            return Err("No se pudo confirmar la selección de línea".into());
        }
        Ok(())
    }

    fn forget_visual_line(&self, lines: &mut LineSelections) {
        if let Ok(target) = self.resume_target() {
            lines.remove(&(target.window, target.pane));
        } else {
            // Without a current target, no retained marker is trustworthy.
            lines.clear();
        }
    }
    /// One expansion binds the selected target, server and selection to the
    /// same observation. It contains only identifiers and metadata, no text.
    fn selection_snapshot(&self) -> Result<SelectionSnapshot, String> {
        let result = self.run(&[
                "display-message",
                "-p",
                "-t",
                &format!("{}:", self.name),
                "#{window_id}\t#{pane_id}\t#{session_id}\t#{pid}\t#{pane_mode}\t#{selection_active}\t#{selection_present}\t#{pane_height}",
            ])?;
        let fields: Vec<_> = result.trim_end_matches('\n').split('\t').collect();
        let [
            window_id,
            pane_id,
            session_id,
            server,
            "copy-mode",
            active,
            present,
            height,
        ] = fields.as_slice()
        else {
            return Err("Estado de selección de terminal no disponible".into());
        };
        let valid_id = |id: &str, prefix: char| {
            id.len() > 1
                && id.len() <= 24
                && id.starts_with(prefix)
                && id.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        };
        if !valid_id(window_id, '@') || !valid_id(pane_id, '%') || !valid_id(session_id, '$') {
            return Err("Identidad de terminal inválida".into());
        }
        let server = server
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid != 0)
            .ok_or("Servidor tmux inválido")?;
        let flag = |value: &str| match value {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err("Estado de selección de terminal inválido".to_owned()),
        };
        let height = height
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=10_000).contains(value))
            .ok_or("Altura de terminal inválida")?;
        Ok(SelectionSnapshot {
            target: ResumeTarget {
                window: window_id.to_string(),
                pane: pane_id.to_string(),
            },
            session: session_id.to_string(),
            server,
            height,
            active: flag(active)?,
            present: flag(present)?,
        })
    }
    /// Read selection state in worker order, while binding the reply to the
    /// expected pane and server. The caller must still fence later input and
    /// validate the frontend focus and attachment before acting on the reply.
    fn inspect_selection(&self, window: &str, pane: &str) -> Result<bool, String> {
        // Repeated complete observations and the socket check catch some,
        // but not all, changes caused by concurrent tmux clients.
        let socket = fs::metadata(&self.socket).map_err(|error| error.to_string())?;
        let first = self.selection_snapshot()?;
        let second = self.selection_snapshot()?;
        let after = fs::metadata(&self.socket).map_err(|error| error.to_string())?;
        if first != second
            || first.target.window != window
            || first.target.pane != pane
            || socket.dev() != after.dev()
            || socket.ino() != after.ino()
        {
            return Err("Cambió el destino o la selección de terminal".into());
        }
        Ok(first.active || first.present)
    }

    /// Infer two wrap boundaries only if exactly one byte representation fits.
    /// Captured terminal contents remain transient and never become commands.
    fn visual_wrap_flags(
        &self,
        pane: &str,
        rows: &[&[u8]],
        history: usize,
        index: usize,
        deadline: Instant,
    ) -> Option<(bool, bool)> {
        let neighbors = rows.get(index.checked_sub(1)?..=index.checked_add(1)?)?;
        let relative = i64::try_from(index).ok()? - i64::try_from(history).ok()?;
        let start = (relative - 1).to_string();
        let end = (relative + 1).to_string();
        let mut joined = self.command();
        joined.args([
            "capture-pane",
            "-p",
            "-M",
            "-J",
            "-S",
            &start,
            "-E",
            &end,
            "-t",
            pane,
        ]);
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let joined = output_bytes_with_timeout(joined, remaining).ok()?;
        let matches = |previous: bool, current: bool| -> Option<()> {
            let mut rest = joined.as_slice().strip_prefix(neighbors[0])?;
            if !previous {
                rest = rest.strip_prefix(b"\n")?;
            }
            rest = rest.strip_prefix(neighbors[1])?;
            if !current {
                rest = rest.strip_prefix(b"\n")?;
            }
            rest = rest.strip_prefix(neighbors[2])?.strip_prefix(b"\n")?;
            rest.is_empty().then_some(())
        };
        let mut result = None;
        for previous in [false, true] {
            for current in [false, true] {
                if matches(previous, current).is_some() {
                    if result.is_some() {
                        return None;
                    }
                    result = Some((previous, current));
                }
            }
        }
        result
    }

    /// Choose one vi motion when the logical line can be identified safely.
    /// The boolean requires the actual post-screen-motion cursor at column zero.
    fn visual_first_nonblank_motion(
        &self,
        state: &CopySelectionSnapshot,
        screen_row: usize,
    ) -> Option<(&'static str, bool)> {
        const MAX_ROWS: usize = 1024;
        let deadline = Instant::now() + Duration::from_millis(250);
        let height = usize::try_from(state.height).ok()?;
        let scroll = usize::try_from(state.scroll).ok()?;
        if height == 0 || height > MAX_ROWS || screen_row >= height || scroll > MAX_ROWS {
            return None;
        }
        let mut capture = self.command();
        capture.args([
            "capture-pane",
            "-p",
            "-M",
            "-T",
            "-N",
            "-S",
            "-",
            "-E",
            "-",
            "-t",
            &state.target.pane,
        ]);
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let screen = output_bytes_with_timeout(capture, remaining).ok()?;
        let rows: Vec<_> = screen
            .strip_suffix(b"\n")?
            .split(|byte| *byte == b'\n')
            .collect();
        let history = rows.len().checked_sub(height)?;
        if history > MAX_ROWS || scroll > history {
            return None;
        }
        let index = history.checked_add(screen_row)?.checked_sub(scroll)?;
        // Follow a bounded chain of wrapped rows back to its logical start,
        // sharing one deadline across all captures.
        const MAX_WRAP_STEPS: usize = 8;
        let (mut previous_wrapped, current_wrapped) =
            self.visual_wrap_flags(&state.target.pane, &rows, history, index, deadline)?;
        let mut logical_start = index;
        for _ in 0..MAX_WRAP_STEPS {
            if !previous_wrapped {
                break;
            }
            logical_start = logical_start.checked_sub(1)?;
            previous_wrapped = self
                .visual_wrap_flags(&state.target.pane, &rows, history, logical_start, deadline)?
                .0;
        }
        if previous_wrapped {
            return None;
        }
        let row = *rows.get(logical_start)?;
        let indentation = row
            .iter()
            .take_while(|byte| matches!(**byte, b' ' | b'\t'))
            .count();
        let (motion, require_column_zero) = match row.get(indentation) {
            None if logical_start == index && !current_wrapped => ("start-of-line", true),
            None => return None,
            Some(byte) if *byte > b' ' && *byte != 127 && indentation == 0 => {
                ("start-of-line", false)
            }
            Some(byte) if *byte > b' ' && *byte != 127 && logical_start == index => {
                ("next-space", true)
            }
            _ => return None,
        };
        Some((motion, require_column_zero))
    }

    /// Request a selection-guarded movement on the exact pane. Concurrent
    /// tmux clients may still change state; an ambiguous reply must not be
    /// retried automatically. Visual equivalence is not yet established.
    fn visual_screen_motion(
        &self,
        window: &str,
        pane: &str,
        bottom: bool,
        count: usize,
        scrolloff: usize,
    ) -> RestoreCopyMark {
        let expected = ResumeTarget {
            window: window.into(),
            pane: pane.into(),
        };
        let before = (|| -> Result<_, String> {
            let socket = fs::metadata(&self.socket).map_err(|error| error.to_string())?;
            let state = self.copy_selection_snapshot()?;
            if state.target != expected
                || !(state.active || state.present)
                || !(1..=10_000).contains(&state.height)
                || count > 10_000
                || scrolloff > 10_000
            {
                return Err("Movimiento de terminal fuera de límites".into());
            }
            Ok((socket.dev(), socket.ino(), state))
        })();
        let Ok((device, inode, state)) = before else {
            return RestoreCopyMark::RejectedBeforeMove;
        };
        // Anchor the command to the observed cursor, selection endpoints,
        // rectangle mode and geometry as well as target/server identity.
        // A selection can change without changing its presence flag.
        let search_condition = format!(
            "#{{==:#{{search_present}},{}}}",
            u8::from(state.search_present)
        );
        let condition = format!("#{{&&:{},{search_condition}}}", state.condition());
        let session = state.session.clone();
        let server = state.server;
        let height = state.height as usize;
        let context = format!("{}:", self.name);
        let motion = if bottom { "bottom-line" } else { "top-line" };
        let margin = scrolloff.min((height - 1) / 2);
        let offset = count.saturating_sub(1).max(margin).min(height - 1 - margin);
        let direction = if bottom { "cursor-up" } else { "cursor-down" };
        let destination_row = if bottom { height - 1 - offset } else { offset };
        let preserve_search = state
            .search_present
            .then(|| self.visual_first_nonblank_motion(&state, destination_row))
            .flatten();
        let mut commands = vec![format!("send-keys -t {pane} -X {motion}")];
        if offset != 0 {
            commands.push(format!("send-keys -t {pane} -X -N {offset} {direction}"));
        }
        if let Some((motion, require_column_zero)) = preserve_search {
            // In vi copy mode these single motions preserve search marks. For
            // indentation and blank lines equivalence requires column zero; a
            // linewise selection may have moved the cursor.
            let mut guards = vec![
                "#{==:#{mode-keys},vi}".to_owned(),
                format!("#{{==:#{{copy_cursor_y}},{destination_row}}}"),
                format!("#{{==:#{{scroll_position}},{}}}", state.scroll),
            ];
            if require_column_zero {
                guards.push("#{==:#{copy_cursor_x},0}".to_owned());
            }
            let still_at_row = guards
                .into_iter()
                .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
                .expect("fixed nonempty predicate list");
            commands.push(format!(
                "if-shell -F -t {pane} '{still_at_row}' 'send-keys -t {pane} -X {motion}' 'send-keys -t {pane} -X back-to-indentation'"
            ));
        } else {
            commands.push(format!("send-keys -t {pane} -X back-to-indentation"));
        }
        commands.push(format!("display-message -p -t {pane} TWR_VISUAL_MOVE_OK"));
        let command = commands.join(" ; ");
        let outcome = self.run(&[
            "if-shell",
            "-F",
            "-t",
            &context,
            &condition,
            &command,
            "display-message -p TWR_VISUAL_MOVE_REJECT",
        ]);
        match outcome.as_deref().map(str::trim) {
            Ok("TWR_VISUAL_MOVE_REJECT") => RestoreCopyMark::RejectedBeforeMove,
            Ok("TWR_VISUAL_MOVE_OK") => {
                let after = (|| -> Result<bool, String> {
                    let state = self.selection_snapshot()?;
                    let socket = fs::metadata(&self.socket).map_err(|error| error.to_string())?;
                    Ok(socket.dev() == device
                        && socket.ino() == inode
                        && state.target == expected
                        && state.session == session
                        && state.server == server
                        && state.height == height
                        && (state.active || state.present))
                })();
                if after == Ok(true) {
                    RestoreCopyMark::Restored
                } else {
                    RestoreCopyMark::UncertainAfterMove
                }
            }
            _ => RestoreCopyMark::UncertainAfterMove,
        }
    }
    fn copy_selection_with_mode(&self, keep_mode: bool) -> Result<String, String> {
        let target = format!("{}:", self.name);
        if !self.copy_selection_exists(&target)? {
            return Err("No hay selección en el pane; arrastrá para seleccionar".into());
        }
        self.run(&[
            "send-keys",
            "-t",
            &target,
            "-X",
            if keep_mode {
                "copy-selection"
            } else {
                "copy-selection-and-cancel"
            },
        ])?;
        if keep_mode {
            self.run(&["send-keys", "-t", &target, "-X", "clear-selection"])?;
        }
        self.run(&["show-buffer"])
    }
    /// Copy from an explicitly identified pane into an automatically named
    /// buffer with a fresh random prefix. Never read tmux's global top buffer.
    fn copy_selection_tracked(
        &self,
        window: &str,
        pane: &str,
        keep_mode: bool,
    ) -> Result<Option<String>, String> {
        let expected = ResumeTarget {
            window: window.into(),
            pane: pane.into(),
        };
        if self.resume_target()? != expected {
            return Err("Cambió el destino de la terminal; copia cancelada".into());
        }
        let socket = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        let selection = self.copy_selection_snapshot()?;
        let confirmed = self.copy_selection_snapshot()?;
        let observed_socket = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        if selection != confirmed
            || selection.target != expected
            || socket.dev() != observed_socket.dev()
            || socket.ino() != observed_socket.ino()
        {
            return Err("Cambió el destino o la selección; copia cancelada".into());
        }
        if !selection.active && !selection.present {
            return Ok(None);
        }
        let mut random = [0_u8; 16];
        fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|_| "No se pudo identificar la copia".to_owned())?;
        let prefix = format!("twr-copy-{}-", hex_bytes(&random));
        selection.endpoints.ok_or("Selección sin extremos")?;
        let condition = selection.condition();
        let action = if keep_mode {
            "copy-selection"
        } else {
            "copy-selection-and-cancel"
        };
        // IDs and prefix contain only validated ASCII digits or generated hex.
        // Evaluate the metadata predicate and enqueue the copy within tmux,
        // reducing (but not eliminating) races with other clients.
        let command =
            format!("send-keys -t {pane} -X {action} -C {prefix} ; display-message -p TWR_COPY_OK");
        let decision = self.run(&[
            "if-shell",
            "-F",
            "-t",
            &format!("{}:", self.name),
            &condition,
            &command,
            "display-message -p TWR_COPY_REJECT",
        ])?;
        match decision.trim() {
            "TWR_COPY_OK" => {}
            "TWR_COPY_REJECT" => return Err("Cambió la selección; copia cancelada".into()),
            _ => return Err("No se pudo confirmar la copia de terminal".into()),
        }
        let names = self.run(&["list-buffers", "-F", "#{buffer_name}"])?;
        let mut matching = names.lines().filter(|name| {
            name.strip_prefix(&prefix).is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
            })
        });
        let name = matching
            .next()
            .ok_or("La copia no generó un buffer verificable")?;
        if matching.next().is_some() {
            return Err("La copia produjo buffers ambiguos".into());
        }
        let mut command = self.command();
        command.args(["show-buffer", "-b", name]);
        // Decode strictly, preserving a literal U+FFFD while refusing malformed
        // UTF-8 instead of silently changing the selection on the clipboard.
        let text = String::from_utf8(output_bytes(command)?)
            .map_err(|_| "La selección no pudo decodificarse sin pérdida".to_owned())?;
        let after = fs::metadata(&self.socket).map_err(|e| e.to_string())?;
        if self.resume_target()? != expected
            || self.session_id()? != selection.session
            || self.server_pid(&expected)? != selection.server
            || socket.dev() != after.dev()
            || socket.ino() != after.ino()
        {
            return Err("Cambió el destino de la terminal; copia cancelada".into());
        }
        Ok(Some(text))
    }
    fn action(&self, a: Action) -> Result<(), String> {
        self.action_with_target(a, None)
    }
    /// For a replayed target change, bind pane-relative actions to the pane
    /// that was inspected rather than resolving the session's current pane a
    /// second time after the check. Other clients can still race the action.
    fn action_with_target(&self, a: Action, expected: Option<&ResumeTarget>) -> Result<(), String> {
        let target = format!("{}:", self.name);
        let pane_target = expected.map_or_else(|| target.clone(), |target| target.pane.clone());
        let args = match a {
            Action::Resume(_)
            | Action::ResumeInput(_, _)
            | Action::Barrier(_)
            | Action::TrackedTarget(_, _, _, _)
            | Action::CaptureCopyMark(_)
            | Action::InspectSelection { .. }
            | Action::VisualScreenMotion { .. }
            | Action::VisualLine { .. }
            | Action::RestoreCopyMark(_, _)
            | Action::RestoreCopyMarkInSession(_, _) => {
                return Err("Resume requires explicit acknowledgement".into());
            }
            Action::Normal(enter) => {
                if enter {
                    // Returning to an already-normal terminal must keep its
                    // copy cursor and selection instead of restarting mode.
                    let mode =
                        self.run(&["display-message", "-p", "-t", &target, "#{pane_mode}"])?;
                    if mode.trim() == "copy-mode" {
                        return Ok(());
                    }
                    if !mode.trim().is_empty() {
                        return Err("La terminal está en otro modo de tmux".into());
                    }
                    return self.run(&["copy-mode", "-t", &target]).map(|_| ());
                }
                if self
                    .run(&["display-message", "-p", "-t", &target, "#{pane_in_mode}"])?
                    .trim()
                    == "0"
                {
                    return Ok(());
                }
                return self
                    .run(&["send-keys", "-t", &target, "-X", "cancel"])
                    .map(|_| ());
            }
            Action::Motion(key, count) => {
                let command = normal_command(&key).ok_or("Movimiento normal no soportado")?;
                if !(1..=9999).contains(&count) {
                    return Err("Conteo fuera de límites".into());
                }
                self.run(&["copy-mode", "-t", &target])?;
                if key == "<Esc>" {
                    return self
                        .run(&["send-keys", "-t", &target, "-X", "clear-selection"])
                        .map(|_| ());
                }
                if key == "v" {
                    let command = if self.copy_selection_exists(&target)? {
                        "clear-selection"
                    } else {
                        "begin-selection"
                    };
                    return self
                        .run(&["send-keys", "-t", &target, "-X", command])
                        .map(|_| ());
                }
                if key == "<C-v>" {
                    self.run(&["send-keys", "-t", &target, "-X", "begin-selection"])?;
                }
                vec![
                    "send-keys".into(),
                    "-t".into(),
                    target,
                    "-X".into(),
                    "-N".into(),
                    count.to_string(),
                    command.into(),
                ]
            }
            Action::Copy | Action::CopyNormal | Action::CopyTracked { .. } => {
                return Err("Copy requires explicit clipboard response".into());
            }
            Action::Resize(d, n)
                if matches!(d, 'L' | 'R' | 'U' | 'D') && (1..=100).contains(&n) =>
            {
                vec![
                    "resize-pane".into(),
                    "-t".into(),
                    target,
                    format!("-{d}"),
                    n.to_string(),
                ]
            }
            Action::Resize(_, _) => return Err("Invalid pane resize".into()),
            Action::PaneKey(key) if matches!(key, "C-a" | "C-e" | "C-u") => {
                vec!["send-keys".into(), "-t".into(), target, key.into()]
            }
            Action::PaneKey(_) => return Err("Invalid pane key".into()),
            Action::Scroll(action) => {
                if !matches!(
                    action,
                    "history-top" | "history-bottom" | "page-up" | "page-down"
                ) {
                    return Err("Invalid history action".into());
                }
                self.run(&["copy-mode", "-t", &target])?;
                vec![
                    "send-keys".into(),
                    "-t".into(),
                    target,
                    "-X".into(),
                    action.into(),
                ]
            }
            Action::SelectAll => {
                self.run(&["copy-mode", "-t", &target])?;
                for command in [
                    "history-top",
                    "start-of-line",
                    "begin-selection",
                    "history-bottom",
                    "end-of-line",
                ] {
                    self.run(&["send-keys", "-t", &target, "-X", command])?;
                }
                return Ok(());
            }
            Action::Find => {
                return self.run(&["copy-mode", "-t", &target]).map(|_| ());
            }
            Action::EndSearch => {
                // The prompt belongs to the attached client, not to the pane.
                // Cancel its input before leaving copy-mode. Never send a key to
                // another frontend if an explicit CLI attach makes this ambiguous.
                let clients =
                    self.run(&["list-clients", "-t", &self.name, "-F", "#{client_name}"])?;
                let clients = clients.lines().collect::<Vec<_>>();
                if clients.len() != 1 {
                    return Err("Search requires one attached terminal client".into());
                }
                let in_mode =
                    self.run(&["display-message", "-p", "-t", &target, "#{pane_in_mode}"])?;
                if in_mode.trim() == "0" {
                    return Ok(());
                }
                self.run(&["send-keys", "-K", "-c", clients[0], "Escape"])?;
                vec![
                    "send-keys".into(),
                    "-t".into(),
                    target,
                    "-X".into(),
                    "cancel".into(),
                ]
            }
            Action::Search(forward) => vec![
                "send-keys".into(),
                "-t".into(),
                target,
                "-X".into(),
                if forward {
                    "search-again"
                } else {
                    "search-reverse"
                }
                .into(),
            ],
            Action::SearchQuery {
                window,
                pane,
                text,
                forward,
                count,
            } => {
                let expected = ResumeTarget::parse(&format!("{window}\t{pane}"))?;
                if !(1..=9999).contains(&count)
                    || text.len() > 4096
                    || text.chars().any(char::is_control)
                {
                    return Err("Búsqueda de terminal fuera de límites".into());
                }
                let observed = self.run(&[
                    "display-message",
                    "-p",
                    "-t",
                    &expected.pane,
                    "#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_mode}",
                ])?;
                let expected_state = format!(
                    "{}\t{}\t{}\tcopy-mode",
                    self.name, expected.window, expected.pane
                );
                if observed.trim_end() != expected_state {
                    return Err(
                        "Cambió el destino o modo de la terminal; búsqueda cancelada".into(),
                    );
                }
                let mut command = vec![
                    "send-keys".into(),
                    "-t".into(),
                    expected.pane,
                    "-X".into(),
                    "-N".into(),
                    count.to_string(),
                ];
                if text.is_empty() {
                    command.push("search-again".into());
                } else {
                    command.push(
                        if forward {
                            "search-forward"
                        } else {
                            "search-backward"
                        }
                        .into(),
                    );
                    command.push("--".into());
                    // tmux's argv parser treats a final semicolon as a command
                    // separator even when the OS passed one argument. Escape
                    // only that final character; tmux removes the added slash.
                    command.push(if let Some(prefix) = text.strip_suffix(';') {
                        format!("{prefix}\\;")
                    } else {
                        text
                    });
                }
                command
            }
            Action::Clear => {
                // The tmux client itself uses an alternate screen, so query
                // the active pane, not the outer terminal's screen state.
                // Ghostty leaves alternate-screen applications untouched;
                // copy mode also owns its selection and must not receive C-l.
                let active = self.resume_target()?;
                let equals = |field: &str, value: &str| format!("#{{==:#{{{field}}},{value}}}");
                let condition = format!(
                    "#{{&&:{},{}}}",
                    equals("window_id", &active.window),
                    equals("pane_id", &active.pane)
                );
                // ResumeTarget::parse accepts only bounded @digits/%digits;
                // no session or pane content is interpolated as commands.
                let guarded = format!(
                    "if-shell -F -t {pane} '#{{||:#{{alternate_on}},#{{pane_in_mode}}}}' \
                     'display-message -p TWR_CLEAR_SKIP' \
                     'clear-history -t {pane} ; send-keys -t {pane} C-l ; display-message -p TWR_CLEAR_OK'",
                    pane = active.pane
                );
                let result = self.run(&[
                    "if-shell",
                    "-F",
                    "-t",
                    &target,
                    &condition,
                    &guarded,
                    "display-message -p TWR_CLEAR_REJECT",
                ])?;
                return match result.trim() {
                    "TWR_CLEAR_OK" | "TWR_CLEAR_SKIP" => Ok(()),
                    _ => Err("Cambió el panel de terminal; limpieza cancelada".into()),
                };
            }
            Action::Cycle(next) => vec![
                if next {
                    "next-window"
                } else {
                    "previous-window"
                }
                .into(),
                "-t".into(),
                target,
            ],
            Action::Equalize => {
                let panes = self.run(&["list-panes", "-t", &target, "-F", "#{pane_id}"])?;
                let panes = panes.lines().collect::<Vec<_>>();
                if panes.is_empty()
                    || panes.len() > 64
                    || panes.iter().any(|p| {
                        !p.strip_prefix('%').is_some_and(|id| {
                            !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit())
                        })
                    })
                {
                    return Err("Unsupported pane list for equalize".into());
                }
                // -E spreads the first unequal ancestor only. Repeating over
                // all leaves reaches nested parents while preserving topology.
                let mut previous =
                    self.run(&["display-message", "-p", "-t", &target, "#{window_layout}"])?;
                let mut batch = Vec::new();
                for pane in &panes {
                    if !batch.is_empty() {
                        batch.push(";");
                    }
                    batch.extend(["select-layout", "-E", "-t", pane]);
                }
                for _ in 0..=panes.len() {
                    self.run(&batch)?;
                    let current =
                        self.run(&["display-message", "-p", "-t", &target, "#{window_layout}"])?;
                    if current == previous {
                        return Ok(());
                    }
                    previous = current;
                }
                return Err("Nested pane equalize did not settle".into());
            }
            Action::AgentExample => {
                if let Some(w) = self.windows()?.iter().find(|w| w.name == "agente-ejemplo") {
                    return self.run(&["select-window", "-t", &w.id]).map(|_| ());
                }
                vec![
                    "new-window".into(),
                    "-t".into(),
                    format!("{}:", self.name),
                    "-n".into(),
                    "agente-ejemplo".into(),
                    "-c".into(),
                    self.root.display().to_string(),
                ]
            }
            Action::Select(id) => vec!["select-window".into(), "-t".into(), id],
            Action::SelectIndex(index) => {
                if !(1..=9).contains(&index) {
                    return Err("La ventana debe estar entre 1 y 9".into());
                }
                vec![
                    "select-window".into(),
                    "-t".into(),
                    format!("{}:{index}", self.name),
                ]
            }
            Action::New => vec![
                "new-window".into(),
                "-t".into(),
                format!("{}:", self.name),
                "-c".into(),
                self.root.display().to_string(),
            ],
            Action::Split => vec![
                "split-window".into(),
                "-h".into(),
                "-t".into(),
                pane_target.clone(),
                "-c".into(),
                "#{pane_current_path}".into(),
            ],
            Action::SplitVertical => vec![
                "split-window".into(),
                "-v".into(),
                "-t".into(),
                pane_target.clone(),
                "-c".into(),
                "#{pane_current_path}".into(),
            ],
            Action::Zoom => vec![
                "resize-pane".into(),
                "-Z".into(),
                "-t".into(),
                format!("{}:", self.name),
            ],
            Action::Direction(d) => vec![
                "select-pane".into(),
                format!("-{d}"),
                "-t".into(),
                pane_target,
            ],
        };
        self.run(&args.iter().map(String::as_str).collect::<Vec<_>>())
            .map(|_| ())
    }
}

// Build a Command as a value without ever assembling shell strings.
trait Args {
    fn tap_args(self, args: &[&str]) -> Self;
}
impl Args for Command {
    fn tap_args(mut self, args: &[&str]) -> Self {
        self.args(args);
        self
    }
}
/// Read-only copy-mode commands, never shell input or arbitrary tmux commands.
fn normal_command(key: &str) -> Option<&'static str> {
    Some(match key {
        "h" | "<Left>" => "cursor-left",
        "j" | "<Down>" => "cursor-down",
        "k" | "<Up>" => "cursor-up",
        "l" | "<Right>" => "cursor-right",
        "w" => "next-word",
        "b" => "previous-word",
        "e" => "next-word-end",
        "W" => "next-space",
        "B" => "previous-space",
        "E" => "next-space-end",
        "0" => "start-of-line",
        "^" => "back-to-indentation",
        "$" => "end-of-line",
        "gg" => "history-top",
        "G" => "history-bottom",
        "v" => "begin-selection",
        "V" => "select-line",
        "<C-v>" => "rectangle-toggle",
        "<Esc>" => "clear-selection",
        "<C-u>" => "halfpage-up",
        "<C-d>" => "halfpage-down",
        "<C-b>" => "page-up",
        "<C-f>" => "page-down",
        "n" => "search-again",
        "N" => "search-reverse",
        _ => return None,
    })
}
impl Backend {
    pub fn start(root: PathBuf, directory: PathBuf, personal_shell: bool) -> Self {
        Self::start_notifying(root, directory, personal_shell, None)
    }
    pub fn start_notifying(
        root: PathBuf,
        directory: PathBuf,
        personal_shell: bool,
        wake: Option<async_channel::Sender<()>>,
    ) -> Self {
        Self::start_with(wake, move || {
            Session::prepare(&root, &directory, personal_shell).map(|session| (session, None))
        })
    }

    /// Create and persist one additional terminal entirely on the worker.
    /// The preallocated name is retained by the caller to reconcile an
    /// uncertain outcome. No retry, cleanup or adoption happens implicitly.
    pub fn start_additional_notifying(
        primary: Session,
        name: String,
        personal_shell: bool,
        wake: Option<async_channel::Sender<()>>,
    ) -> Self {
        Self::start_with(wake, move || {
            let (session, identity) = primary.create_additional_terminal(&name, personal_shell)?;
            session
                .persist_additional_identity(&identity)
                .map_err(|error| {
                    format!("Terminal {name} creada; registro no confirmado: {error}")
                })?;
            Ok((session, Some(AdditionalStart::Attach(identity, false))))
        })
    }

    /// Reattach only a session with a private record that still matches the
    /// exact live server and session. Discovery by name is not sufficient.
    pub fn start_recovered_additional_notifying(
        session: Session,
        wake: Option<async_channel::Sender<()>>,
    ) -> Self {
        Self::start_with(wake, move || {
            let identity = session.recover_additional_identity()?;
            Ok((session, Some(AdditionalStart::Attach(identity, true))))
        })
    }

    /// Verify an existing client instead of opening a second PTY. The caller
    /// must retain the client and reject the result if its own state changed.
    pub fn start_reusing_additional_notifying(
        session: Session,
        pid: u32,
        wake: Option<async_channel::Sender<()>>,
    ) -> Self {
        Self::start_with(wake, move || {
            if pid < 2 {
                return Err("PID de terminal inválido".into());
            }
            let identity = session.recover_additional_identity()?;
            Ok((session, Some(AdditionalStart::Reuse(identity, pid))))
        })
    }

    fn start_with(
        wake: Option<async_channel::Sender<()>>,
        create: impl FnOnce() -> Result<(Session, Option<AdditionalStart>), String> + Send + 'static,
    ) -> Self {
        let terminal_wake = wake.clone();
        let (tx, events) = crate::events::sync_channel(8, wake);
        let (commands, rx) = mpsc::sync_channel(32);
        std::thread::spawn(move || {
            let (session, identity) = match create() {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(Event::Error(e));
                    return;
                }
            };
            let guard = identity.as_ref().map(|start| match start {
                AdditionalStart::Attach(identity, _) | AdditionalStart::Reuse(identity, _) => {
                    identity.clone()
                }
            });
            let ready = match identity {
                Some(AdditionalStart::Attach(identity, require_detached)) => {
                    let attach = session.attach_additional(&identity).and_then(|command| {
                        if require_detached && !session.client_pids()?.is_empty() {
                            return Err(
                                "Ya hay un cliente conectado; se conserva sin duplicarlo".into()
                            );
                        }
                        let terminal =
                            crate::terminal::Terminal::spawn_notifying(command, terminal_wake)?;
                        let pid = terminal.process_id().ok_or("PTY sin PID verificable")?;
                        let deadline = Instant::now() + Duration::from_secs(3);
                        loop {
                            session.verify_additional_terminal(&identity)?;
                            if session.client_pids()?.contains(&pid) {
                                break;
                            }
                            if Instant::now() >= deadline {
                                return Err("Cliente tmux no confirmado".into());
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        session.verify_additional_terminal(&identity)?;
                        Ok(terminal)
                    });
                    match attach {
                        Ok(terminal) => Event::ReadyAdditional(session.clone(), identity, terminal),
                        Err(error) => {
                            let _ = tx.send(Event::Error(format!(
                                "Terminal {} conservada: {error}",
                                session.name
                            )));
                            return;
                        }
                    }
                }
                Some(AdditionalStart::Reuse(identity, pid)) => {
                    let verified = session
                        .verify_additional_terminal(&identity)
                        .and_then(|_| session.client_pids())
                        .and_then(|clients| {
                            if clients.contains(&pid) {
                                Ok(())
                            } else {
                                Err("Cliente de terminal no confirmado".into())
                            }
                        })
                        .and_then(|_| session.verify_additional_terminal(&identity));
                    if let Err(error) = verified {
                        let _ = tx.send(Event::Error(format!(
                            "Terminal {} conservada: {error}",
                            session.name
                        )));
                        return;
                    }
                    Event::ReusedAdditional(session.clone(), identity, pid)
                }
                None => Event::Ready(session.clone(), session.client_pids()),
            };
            if tx.send(ready).is_err() {
                return;
            }
            let mut previous = Vec::new();
            let mut resume = None;
            let mut resume_error = None;
            let mut visual_lines = LineSelections::new();
            loop {
                match session.windows() {
                    Ok(ws) if ws != previous => {
                        previous = ws.clone();
                        if tx.send(Event::Windows(ws)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        if tx.try_send(Event::Error(e)).is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
                let request = rx.recv_timeout(Duration::from_millis(400));
                if let (Some(expected), Ok(action)) = (&guard, &request)
                    && let Err(error) = session.verify_additional_terminal(expected)
                {
                    let event = match action {
                        Action::Resume(token) => Event::Resumed(*token, Err(error)),
                        Action::ResumeInput(token, _) => Event::InputDelivered(*token, Err(error)),
                        Action::TrackedTarget(token, _, _, _) => {
                            Event::TargetActionDone(*token, Err(error))
                        }
                        Action::CaptureCopyMark(token) => {
                            Event::CopyMarkCaptured(*token, Err(error))
                        }
                        Action::InspectSelection { token, .. } => {
                            Event::SelectionInspected(*token, Err(error))
                        }
                        Action::VisualScreenMotion { token, .. } => Event::VisualScreenMotionDone(
                            *token,
                            RestoreCopyMark::RejectedBeforeMove,
                        ),
                        Action::VisualLine { token, .. } => {
                            Event::VisualLineDone(*token, Err(error))
                        }
                        Action::RestoreCopyMark(token, _)
                        | Action::RestoreCopyMarkInSession(token, _) => {
                            Event::CopyMarkRestored(*token, RestoreCopyMark::RejectedBeforeMove)
                        }
                        _ => Event::Error(error),
                    };
                    let _ = tx.send(event);
                    break;
                }
                match request {
                    Ok(Action::Barrier(token)) => {
                        let _ = tx.send(Event::Barrier(token));
                    }
                    Ok(Action::TrackedTarget(token, expected_window, expected_pane, action)) => {
                        resume = None;
                        let result = (|| -> Result<Vec<WindowInfo>, String> {
                            if !action.changes_input_target()
                                || matches!(*action, Action::Normal(_))
                            {
                                return Err("Acción de destino inválida".into());
                            }
                            let socket =
                                fs::metadata(&session.socket).map_err(|e| e.to_string())?;
                            let session_id = session.session_id()?;
                            let before = session.resume_target()?;
                            if before.window != expected_window || before.pane != expected_pane {
                                return Err("Cambió el destino antes de la acción".into());
                            }
                            let server = session.server_pid(&before)?;
                            let same_window = matches!(
                                *action,
                                Action::Direction(_) | Action::Split | Action::SplitVertical
                            );
                            let new_pane = matches!(*action, Action::Split | Action::SplitVertical);
                            let requested_window = match action.as_ref() {
                                Action::Select(id) => Some(id.clone()),
                                _ => None,
                            };
                            let requested_index = match action.as_ref() {
                                Action::SelectIndex(index) => Some(*index),
                                _ => None,
                            };
                            let cycle_target = if let Action::Cycle(forward) = action.as_ref() {
                                let mut windows = session.windows()?;
                                windows.sort_by_key(|window| window.index);
                                if windows.is_empty()
                                    || windows
                                        .windows(2)
                                        .any(|pair| pair[0].index == pair[1].index)
                                {
                                    return Err("Lista de ventanas ambigua".into());
                                }
                                let mut active = windows
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, window)| window.active);
                                let (index, current) =
                                    active.next().ok_or("Ventana activa no disponible")?;
                                if active.next().is_some()
                                    || current.id != before.window
                                    || current.active_pane != before.pane
                                    || session.resume_target()? != before
                                {
                                    return Err(
                                        "Cambió el destino antes de recorrer ventanas".into()
                                    );
                                }
                                let next = if *forward {
                                    (index + 1) % windows.len()
                                } else {
                                    (index + windows.len() - 1) % windows.len()
                                };
                                Some((windows[next].id.clone(), windows[next].index))
                            } else {
                                None
                            };
                            let created = if matches!(*action, Action::New) {
                                let target = format!("{}:", session.name);
                                let directory = session.root.display().to_string();
                                let output = session.run(&[
                                    "new-window",
                                    "-P",
                                    "-F",
                                    "#{window_id}\t#{pane_id}",
                                    "-t",
                                    &target,
                                    "-c",
                                    &directory,
                                ])?;
                                Some(ResumeTarget::parse(&output)?)
                            } else {
                                session.action_with_target(*action, Some(&before))?;
                                None
                            };
                            let target = session.resume_target()?;
                            if (same_window && target.window != before.window)
                                || (new_pane && target.pane == before.pane)
                                || created.as_ref().is_some_and(|created| created != &target)
                            {
                                return Err("La acción no confirmó el panel esperado".into());
                            }
                            let windows = session.windows()?;
                            let mut active = windows.iter().filter(|window| window.active);
                            if !active.next().is_some_and(|window| {
                                window.id == target.window
                                    && window.active_pane == target.pane
                                    && requested_window.as_ref().is_none_or(|id| &window.id == id)
                                    && requested_index.is_none_or(|index| window.index == index)
                                    && cycle_target.as_ref().is_none_or(|(id, index)| {
                                        &window.id == id && window.index == *index
                                    })
                            }) || active.next().is_some()
                                || session.resume_target()? != target
                                || session.session_id()? != session_id
                                || session.server_pid(&target)? != server
                            {
                                return Err("Destino tmux no confirmado".into());
                            }
                            let after = fs::metadata(&session.socket).map_err(|e| e.to_string())?;
                            if socket.dev() != after.dev() || socket.ino() != after.ino() {
                                return Err("Servidor tmux cambió durante la acción".into());
                            }
                            Ok(windows)
                        })();
                        resume_error = result.as_ref().err().cloned();
                        let _ = tx.send(Event::TargetActionDone(token, result));
                    }
                    Ok(Action::Resume(token)) => {
                        session.forget_visual_line(&mut visual_lines);
                        let result = match resume_error.take() {
                            Some(error) => Err(error),
                            None => session.resume(),
                        };
                        resume = result.as_ref().ok().cloned().map(|target| (token, target));
                        let _ = tx.send(Event::Resumed(token, result.map(|_| ())));
                    }
                    Ok(Action::ResumeInput(token, bytes)) => {
                        let result = match &resume {
                            Some((id, target)) if *id == token => {
                                session.resume_input(target, &bytes)
                            }
                            _ => Err("Transición obsoleta; entrada cancelada".into()),
                        };
                        if result.is_err() {
                            resume = None;
                        }
                        let _ = tx.send(Event::InputDelivered(token, result));
                    }
                    Ok(Action::CaptureCopyMark(token)) => {
                        let _ =
                            tx.send(Event::CopyMarkCaptured(token, session.capture_copy_mark()));
                    }
                    Ok(Action::InspectSelection {
                        token,
                        window,
                        pane,
                    }) => {
                        let _ = tx.send(Event::SelectionInspected(
                            token,
                            session.inspect_selection(&window, &pane),
                        ));
                    }
                    Ok(Action::VisualScreenMotion {
                        token,
                        window,
                        pane,
                        bottom,
                        count,
                        scrolloff,
                    }) => {
                        let _ = tx.send(Event::VisualScreenMotionDone(
                            token,
                            session.visual_screen_motion(&window, &pane, bottom, count, scrolloff),
                        ));
                    }
                    Ok(Action::RestoreCopyMark(token, mark)) => {
                        let _ = tx.send(Event::CopyMarkRestored(
                            token,
                            session.restore_copy_mark(&mark),
                        ));
                    }
                    Ok(Action::RestoreCopyMarkInSession(token, mark)) => {
                        let _ = tx.send(Event::CopyMarkRestored(
                            token,
                            session.restore_copy_mark_in_session(&mark),
                        ));
                    }
                    Ok(Action::Find) => {
                        let event = match session.action(Action::Find) {
                            Ok(()) => Event::SearchPrompt,
                            Err(e) => Event::Error(e),
                        };
                        let _ = tx.try_send(event);
                    }
                    Ok(action @ (Action::Copy | Action::CopyNormal)) => {
                        session.forget_visual_line(&mut visual_lines);
                        let result = if matches!(action, Action::CopyNormal) {
                            session.copy_selection_with_mode(true)
                        } else {
                            session.copy_selection()
                        };
                        let event = match result {
                            Ok(text) => Event::Clipboard(text),
                            Err(e) => Event::Error(e),
                        };
                        let _ = tx.try_send(event);
                    }
                    Ok(Action::CopyTracked {
                        token,
                        window,
                        pane,
                        keep_mode,
                    }) => {
                        visual_lines.remove(&(window.clone(), pane.clone()));
                        let result = session.copy_selection_tracked(&window, &pane, keep_mode);
                        let _ = tx.send(Event::CopyTracked(token, result));
                    }
                    Ok(Action::VisualLine {
                        token,
                        window,
                        pane,
                        count,
                    }) => {
                        let result =
                            session.toggle_visual_line(&mut visual_lines, &window, &pane, count);
                        let _ = tx.send(Event::VisualLineDone(token, result));
                    }
                    Ok(a) => {
                        if matches!(&a, Action::Motion(key, _) if matches!(key.as_str(), "v" | "<C-v>" | "<Esc>"))
                            || matches!(&a, Action::Normal(false))
                        {
                            session.forget_visual_line(&mut visual_lines);
                        }
                        let changed = a.changes_input_target();
                        if changed {
                            resume = None;
                        }
                        let result = session.action(a);
                        if changed {
                            resume_error = result.as_ref().err().cloned();
                        }
                        if let Err(e) = result {
                            let _ = tx.try_send(Event::Error(e));
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        Self {
            events,
            commands,
            accepting: Mutex::new(true),
        }
    }
    pub fn send(&self, a: Action) -> Result<(), String> {
        let accepting = self.accepting.try_lock().map_err(|_| "Backend ocupado")?;
        if !*accepting || matches!(a, Action::Barrier(_)) {
            return Err("Reconexión en curso; reintentá la acción".into());
        }
        self.commands.try_send(a).map_err(|e| e.to_string())
    }
    /// Quiesce the caller's command stream and acknowledge earlier accepted
    /// actions in FIFO order before a backend can be replaced.
    pub fn begin_reconnect_barrier(&self, token: u64) -> Result<ReconnectBarrier, String> {
        let mut accepting = self.accepting.try_lock().map_err(|_| "Backend ocupado")?;
        if !*accepting {
            return Err("Reconexión ya en curso".into());
        }
        *accepting = false;
        match self.commands.try_send(Action::Barrier(token)) {
            Ok(()) => Ok(ReconnectBarrier::Queued),
            Err(mpsc::TrySendError::Full(_)) => {
                *accepting = true;
                Err("Cola de comandos llena; reintentá".into())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Ok(ReconnectBarrier::Disconnected),
        }
    }
    pub fn cancel_reconnect_barrier(&self) {
        if let Ok(mut accepting) = self.accepting.lock() {
            *accepting = true;
        }
    }
    // The synthetic GPUI harness adds this feature to its temporary manifest.
    #[cfg(feature = "native-test")]
    pub fn disconnected_for_test() -> Self {
        let (commands, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let (sender, events) = mpsc::sync_channel(1);
        let _ = sender.send(Event::Error("fallo sintético previo".into()));
        drop(sender);
        Self {
            events,
            commands,
            accepting: Mutex::new(true),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reconnect_barrier_distinguishes_full_queue_from_disconnected_worker() {
        use super::*;
        let (commands, receiver) = mpsc::sync_channel(1);
        let (_event_tx, events) = mpsc::channel();
        let backend = Backend {
            events,
            commands,
            accepting: Mutex::new(true),
        };
        backend.send(Action::New).unwrap();
        assert!(backend.begin_reconnect_barrier(1).is_err());
        assert!(matches!(receiver.recv().unwrap(), Action::New));
        assert_eq!(
            backend.begin_reconnect_barrier(2).unwrap(),
            ReconnectBarrier::Queued
        );
        assert!(matches!(receiver.recv().unwrap(), Action::Barrier(2)));
        backend.cancel_reconnect_barrier();
        drop(receiver);
        assert_eq!(
            backend.begin_reconnect_barrier(3).unwrap(),
            ReconnectBarrier::Disconnected
        );
        assert!(backend.send(Action::New).is_err());
    }
    #[test]
    fn reconnect_keeps_a_verified_live_client_and_fails_closed_for_unknown() {
        use super::{ReconnectClient::*, reconnect_client};
        let clients = Ok(vec![17, 23]);
        assert_eq!(reconnect_client(None, &clients), Attach);
        assert_eq!(reconnect_client(Some((Some(17), false)), &clients), Reuse);
        assert_eq!(
            reconnect_client(Some((Some(19), false)), &clients),
            PreserveUnverified
        );
        assert_eq!(
            reconnect_client(Some((None, false)), &clients),
            PreserveUnverified
        );
        assert_eq!(
            reconnect_client(Some((Some(17), false)), &Err("timeout".into())),
            PreserveUnverified
        );
        assert_eq!(
            reconnect_client(Some((Some(17), true)), &clients),
            PreserveUnverified
        );
        assert_eq!(reconnect_client(Some((Some(19), true)), &clients), Attach);
        assert_eq!(
            reconnect_client(Some((None, true)), &clients),
            PreserveUnverified
        );
    }
    #[test]
    fn entering_normal_mode_cancels_the_old_target_without_resuming_input() {
        use super::Action;
        assert!(Action::Normal(true).changes_input_target());
        assert!(!Action::Normal(true).resumes_input_after_target_change(true));
        assert!(!Action::Normal(true).resumes_input_after_target_change(false));
        assert!(Action::Direction('L').resumes_input_after_target_change(true));
        assert!(!Action::Direction('L').resumes_input_after_target_change(false));
        assert!(!Action::Resize('L', 1).resumes_input_after_target_change(true));
    }
    #[test]
    fn observed_active_window_change_ignores_initial_and_ambiguous_metadata() {
        use super::{WindowInfo, observed_active_target_changed};
        let window = |id: &str, active| WindowInfo {
            id: id.into(),
            active_pane: "%1".into(),
            index: 0,
            name: "synthetic".into(),
            active,
        };
        let before = [window("@1", true), window("@2", false)];
        let after = [window("@1", false), window("@2", true)];
        assert!(observed_active_target_changed(&before, &after));
        assert!(!observed_active_target_changed(&[], &after));
        assert!(!observed_active_target_changed(
            &before,
            &[window("@1", true), window("@2", true)]
        ));
        let mut renamed = before.clone();
        renamed[0].name = "renamed".into();
        assert!(!observed_active_target_changed(&before, &renamed));
        renamed[0].active_pane = "%2".into();
        assert!(observed_active_target_changed(&before, &renamed));
    }
    use super::*;
    use crate::terminal::Terminal;
    #[test]
    fn additional_backend_creates_persists_and_routes_on_its_own_worker() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = root.join(format!("tw-worker-{nonce}"));
        let primary = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let _cleanup = Cleanup(primary.clone());
        let name = primary.additional_terminal_name().unwrap();
        let backend =
            Backend::start_additional_notifying(primary.clone(), name.clone(), false, None);
        let (session, identity, terminal) = loop {
            match backend
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Event::ReadyAdditional(session, identity, terminal) => {
                    assert_eq!(session.name, name);
                    assert!(
                        session
                            .client_pids()
                            .unwrap()
                            .contains(&terminal.process_id().unwrap())
                    );
                    break (session, identity, terminal);
                }
                Event::Error(error) => panic!("additional worker: {error}"),
                _ => {}
            }
        };
        assert_eq!(session.recover_additional_identity().unwrap(), identity);
        let before = primary.windows().unwrap().len();
        backend.send(Action::New).unwrap();
        backend.begin_reconnect_barrier(42).unwrap();
        assert!(backend.send(Action::New).is_err());
        loop {
            match backend
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Event::Barrier(42) => break,
                Event::Error(error) => panic!("barrier failed: {error}"),
                _ => {}
            }
        }
        assert_eq!(session.windows().unwrap().len(), 2);
        backend.cancel_reconnect_barrier();
        backend.begin_reconnect_barrier(43).unwrap();
        loop {
            match backend
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Event::Barrier(43) => break,
                Event::Error(error) => panic!("second barrier failed: {error}"),
                _ => {}
            }
        }
        backend.cancel_reconnect_barrier();
        let deadline = Instant::now() + Duration::from_secs(8);
        while session.windows().unwrap().len() != 2 {
            assert!(
                Instant::now() < deadline,
                "additional window was not created"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(primary.windows().unwrap().len(), before);
        let old_pid = terminal.process_id().unwrap();
        let prior_clients = session.client_pids().unwrap();
        let reuse = Backend::start_reusing_additional_notifying(session.clone(), old_pid, None);
        match reuse.events.recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::ReusedAdditional(recovered, recovered_identity, pid) => {
                assert_eq!(recovered.name, session.name);
                assert_eq!(recovered_identity, identity);
                assert_eq!(pid, old_pid);
            }
            _ => panic!("verified client was not reused"),
        }
        assert_eq!(session.client_pids().unwrap(), prior_clients);
        drop(reuse);
        let wrong = Backend::start_reusing_additional_notifying(session.clone(), u32::MAX, None);
        assert!(matches!(
            wrong.events.recv_timeout(Duration::from_secs(10)).unwrap(),
            Event::Error(_)
        ));
        assert_eq!(session.client_pids().unwrap(), prior_clients);
        let duplicate = Backend::start_recovered_additional_notifying(session.clone(), None);
        match duplicate
            .events
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Event::Error(error) => assert!(error.contains("sin duplicarlo")),
            _ => panic!("recovery duplicated a live client"),
        }
        assert!(session.client_pids().unwrap().contains(&old_pid));
        drop(backend);
        drop(terminal);
        let deadline = Instant::now() + Duration::from_secs(8);
        while session.client_pids().unwrap().contains(&old_pid) {
            assert!(Instant::now() < deadline, "old client did not detach");
            std::thread::sleep(Duration::from_millis(20));
        }
        let backend = Backend::start_recovered_additional_notifying(session.clone(), None);
        let recovered_terminal = loop {
            match backend
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Event::ReadyAdditional(recovered, recovered_identity, terminal) => {
                    assert_eq!(recovered.name, name);
                    assert_eq!(recovered_identity, identity);
                    assert_ne!(terminal.process_id(), Some(old_pid));
                    break terminal;
                }
                Event::Error(error) => panic!("recovery worker: {error}"),
                _ => {}
            }
        };
        assert_eq!(session.windows().unwrap().len(), 2);
        assert!(recovered_terminal.process_id().is_some());
        primary
            .run(&["kill-session", "-t", &format!("={name}:")])
            .unwrap();
        let (replacement, _) = primary.create_additional_terminal(&name, false).unwrap();
        backend.send(Action::New).unwrap();
        loop {
            if let Event::Error(_) = backend
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                break;
            }
        }
        assert_eq!(replacement.windows().unwrap().len(), 1);

        // A conflicting record must not be overwritten, and the worker must
        // surface the failure while preserving the newly created tmux session.
        let failed_name = primary.additional_terminal_name().unwrap();
        let failed_session = Session {
            name: failed_name.clone(),
            ..primary.clone()
        };
        let record_path = failed_session.additional_identity_path().unwrap();
        fs::write(&record_path, b"synthetic existing record").unwrap();
        let failed =
            Backend::start_additional_notifying(primary.clone(), failed_name.clone(), false, None);
        match failed.events.recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Error(error) => assert!(error.contains(&failed_name)),
            _ => panic!("unverified session became ready"),
        }
        assert_eq!(
            fs::read(&record_path).unwrap(),
            b"synthetic existing record"
        );
        assert!(
            primary
                .discover_additional_terminals()
                .unwrap()
                .iter()
                .any(|s| s.name == failed_name)
        );
    }
    #[test]
    fn additional_terminal_sessions_are_unique_discoverable_and_never_replaced() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = root.join(format!("tw-extra-{nonce}"));
        let primary = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let _cleanup = Cleanup(primary.clone());
        let first_name = primary.additional_terminal_name().unwrap();
        let second_name = primary.additional_terminal_name().unwrap();
        assert_ne!(first_name, second_name);
        let (first, first_record) = primary
            .create_additional_terminal(&first_name, false)
            .unwrap();
        let (second, second_record) = primary
            .create_additional_terminal(&second_name, false)
            .unwrap();
        let restored: AdditionalTerminalIdentity =
            serde_json::from_slice(&serde_json::to_vec(&first_record).unwrap()).unwrap();
        assert_eq!(restored, first_record);
        first.verify_additional_terminal(&restored).unwrap();
        assert!(first.verify_additional_terminal(&second_record).is_err());
        first.persist_additional_identity(&first_record).unwrap();
        assert_eq!(first.recover_additional_identity().unwrap(), first_record);
        assert!(first.persist_additional_identity(&first_record).is_err());
        let second_path = second.additional_identity_path().unwrap();
        std::os::unix::fs::symlink(first.additional_identity_path().unwrap(), &second_path)
            .unwrap();
        assert!(second.recover_additional_identity().is_err());
        assert!(second.persist_additional_identity(&second_record).is_err());
        fs::remove_file(&second_path).unwrap();
        fs::hard_link(first.additional_identity_path().unwrap(), &second_path).unwrap();
        assert!(first.recover_additional_identity().is_err());
        fs::remove_file(&second_path).unwrap();
        assert_eq!(first.recover_additional_identity().unwrap(), first_record);
        let identity = |session: &Session| {
            session
                .run(&[
                    "display-message",
                    "-p",
                    "-t",
                    &format!("={}:", session.name),
                    "#{session_id}:#{pane_id}",
                ])
                .unwrap()
        };
        let before = identity(&first);
        assert_ne!(before, identity(&second));
        assert!(
            primary
                .create_additional_terminal(&first_name, false)
                .is_err()
        );
        assert_eq!(before, identity(&first));
        assert!(
            primary
                .create_additional_terminal("unrelated", false)
                .is_err()
        );
        let discovered = primary.discover_additional_terminals().unwrap();
        assert_eq!(discovered.len(), 2);
        assert!(discovered.iter().any(|s| s.name == first_name));
        assert!(discovered.iter().any(|s| s.name == second_name));
        primary
            .run(&["kill-session", "-t", &format!("={first_name}:")])
            .unwrap();
        let (replacement, replacement_record) = primary
            .create_additional_terminal(&first_name, false)
            .unwrap();
        assert_ne!(replacement_record, first_record);
        assert!(
            replacement
                .verify_additional_terminal(&first_record)
                .is_err()
        );
        assert!(replacement.recover_additional_identity().is_err());
        replacement
            .verify_additional_terminal(&replacement_record)
            .unwrap();
        let other_workspace = Session {
            root: dir.clone(),
            ..primary.clone()
        };
        assert!(
            other_workspace
                .discover_additional_terminals()
                .unwrap()
                .is_empty()
        );
        assert!(
            Session {
                root: dir.clone(),
                ..first.clone()
            }
            .verify_additional_terminal(&first_record)
            .is_err()
        );
        primary.run(&["kill-server"]).unwrap();
        assert!(primary.discover_additional_terminals().is_err());
        assert!(
            primary
                .create_additional_terminal(&primary.additional_terminal_name().unwrap(), false)
                .is_err()
        );
        // tmux may leave the socket path behind after the server exits.
        // Discovery verifies that failed creation did not start a new server.
        assert!(primary.discover_additional_terminals().is_err());
    }
    #[test]
    fn named_client_keys_match_attached_pty_in_bundled_tmux_config() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let paths = [
            root.join(format!("tw-kp-{nonce}")),
            root.join(format!("tw-kn-{nonce}")),
        ];
        let sessions = [
            Session::prepare(&root, &paths[0], false).unwrap(),
            Session::prepare(&root, &paths[1], false).unwrap(),
        ];
        struct Cleanup([Session; 2]);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for session in &self.0 {
                    let _ = session.run(&["kill-server"]);
                }
            }
        }
        let _cleanup = Cleanup(sessions.clone());
        let outputs = [paths[0].join("bytes"), paths[1].join("bytes")];
        for (i, session) in sessions.iter().enumerate() {
            session
                .run(&["set-option", "-s", "escape-time", "20"])
                .unwrap();
            let ready = paths[i].join("ready");
            let command = format!(
                "stty raw -echo; printf READY > '{}'; exec /bin/cat > '{}'",
                ready.display(),
                outputs[i].display()
            );
            session
                .run(&["send-keys", "-t", &session.name, "-l", &command])
                .unwrap();
            session
                .run(&["send-keys", "-t", &session.name, "Enter"])
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.is_file() || !outputs[i].is_file() {
                assert!(Instant::now() < deadline, "synthetic cat did not start");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let terminals = [
            Terminal::spawn_command(sessions[0].attach()).unwrap(),
            Terminal::spawn_command(sessions[1].attach()).unwrap(),
        ];
        let mut client_names = Vec::new();
        for (session, terminal) in sessions.iter().zip(&terminals) {
            let pid = terminal.process_id().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let client = loop {
                let rows = session
                    .run(&[
                        "list-clients",
                        "-t",
                        &session.name,
                        "-F",
                        "#{client_pid}\t#{client_name}",
                    ])
                    .unwrap();
                let matches: Vec<_> = rows
                    .lines()
                    .filter_map(|line| {
                        let (candidate, name) = line.split_once('\t')?;
                        (candidate.parse::<u32>().ok() == Some(pid)).then(|| name.to_owned())
                    })
                    .collect();
                if matches.len() == 1 {
                    break matches[0].clone();
                }
                assert!(Instant::now() < deadline, "synthetic client not attached");
                std::thread::sleep(Duration::from_millis(10));
            };
            client_names.push(client);
        }
        // A successful send-keys -K command can return while the target
        // client's command queue is blocked and before the key reaches cat.
        // The gate is a synthetic file and is released even on a test panic.
        struct Release(PathBuf);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = fs::write(&self.0, b"release");
            }
        }
        let blocked = paths[1].join("blocked");
        let gate = paths[1].join("gate");
        let _release = Release(gate.clone());
        let wait_command = format!(
            "printf BLOCKED > '{}'; while [ ! -e '{}' ]; do /bin/sleep 0.05; done",
            blocked.display(),
            gate.display()
        );
        sessions[1]
            .run(&["bind-key", "-n", "F12", "run-shell", &wait_command])
            .unwrap();
        sessions[1]
            .run(&["send-keys", "-K", "-c", &client_names[1], "F12"])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !blocked.is_file() {
            assert!(
                Instant::now() < deadline,
                "synthetic client queue did not block"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        sessions[1]
            .run(&["send-keys", "-K", "-c", &client_names[1], "x"])
            .unwrap();
        assert_eq!(fs::read(&outputs[1]).unwrap(), b"");
        fs::write(&gate, b"release").unwrap();
        terminals[0].input(b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read(&outputs[0]).unwrap() != b"x" || fs::read(&outputs[1]).unwrap() != b"x" {
            assert!(
                Instant::now() < deadline,
                "synthetic queued key did not arrive"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let cases: &[(&[u8], &str)] = &[
            (b"\x03", "C-c"),
            (b"\x04", "C-d"),
            (b"\x15", "C-u"),
            (b"\x1c", "C-\\"),
            (b"\x1a", "C-z"),
            (b"\x1e", "C-^"),
            (b"\x7f", "BSpace"),
            (b"\x1b", "Escape"),
            ("ñ".as_bytes(), "ñ"),
        ];
        let mut expected = b"x".to_vec();
        for (physical, named) in cases {
            terminals[0].input(physical).unwrap();
            terminals[0].wait_for_writer().unwrap();
            sessions[1]
                .run(&["send-keys", "-K", "-c", &client_names[1], named])
                .unwrap();
            // Wait for a lone Escape to reach both cats before the sentinel;
            // a fixed delay alone could merge it with the next key under load.
            if *physical == b"\x1b" {
                let deadline = Instant::now() + Duration::from_secs(5);
                while fs::metadata(&outputs[0]).unwrap().len() < (expected.len() + 1) as u64
                    || fs::metadata(&outputs[1]).unwrap().len() < (expected.len() + 1) as u64
                {
                    assert!(
                        Instant::now() < deadline,
                        "standalone Escape did not arrive"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            terminals[0].input(b"!").unwrap();
            sessions[1]
                .run(&["send-keys", "-K", "-c", &client_names[1], "!"])
                .unwrap();
            expected.extend_from_slice(physical);
            expected.push(b'!');
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let first = fs::read(&outputs[0]).unwrap();
                let second = fs::read(&outputs[1]).unwrap();
                if first.len() >= expected.len() && second.len() >= expected.len() {
                    assert_eq!(first, expected, "physical {named}");
                    assert_eq!(second, expected, "named {named}");
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out on {named}: {first:?} / {second:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // TWR's bundled tmux prefix is C-a. Both paths must invoke the
        // client's prefix table, rather than deliver the bytes to cat.
        // Include the next command in the same burst to check the ordering
        // between the split binding and subsequent input.
        terminals[0].input(b"\x01%echo TWR_BOUNDARY_OK\r").unwrap();
        let mut key_args = vec![
            "send-keys".to_string(),
            "-K".into(),
            "-c".into(),
            client_names[1].clone(),
            "C-a".into(),
            "%".into(),
        ];
        key_args.extend("echo TWR_BOUNDARY_OK".chars().map(|c| {
            if c == ' ' {
                "Space".into()
            } else {
                c.to_string()
            }
        }));
        key_args.push("Enter".into());
        sessions[1]
            .run(&key_args.iter().map(String::as_str).collect::<Vec<_>>())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let panes = sessions
                .iter()
                .map(|s| {
                    s.run(&["list-panes", "-t", &s.name, "-F", "#{pane_id}"])
                        .unwrap()
                        .lines()
                        .count()
                })
                .collect::<Vec<_>>();
            if panes == [2, 2] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "prefix split mismatch: {panes:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        for session in &sessions {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let panes = session
                    .run(&[
                        "list-panes",
                        "-t",
                        &session.name,
                        "-F",
                        "#{pane_index}\t#{pane_id}",
                    ])
                    .unwrap();
                let right = panes
                    .lines()
                    .filter_map(|line| line.split_once('\t'))
                    .find_map(|(index, id)| (index == "2").then_some(id))
                    .unwrap();
                let capture = session.run(&["capture-pane", "-p", "-t", right]).unwrap();
                if capture.contains("TWR_BOUNDARY_OK") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "input after split did not reach pane 2: {capture:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // The bundled config maps Alt-Shift-H to the pane on the left.
        // Compare a physical escape sequence with the named client event.
        terminals[0].input(b"\x1bH").unwrap();
        sessions[1]
            .run(&["send-keys", "-K", "-c", &client_names[1], "M-H"])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let active = sessions
                .iter()
                .map(|s| {
                    s.run(&[
                        "list-panes",
                        "-t",
                        &s.name,
                        "-F",
                        "#{pane_index}\t#{pane_active}",
                    ])
                    .unwrap()
                    .lines()
                    .filter_map(|line| line.split_once('\t'))
                    .find_map(|(index, active)| (active == "1").then(|| index.to_owned()))
                })
                .collect::<Vec<_>>();
            if active == [Some("1".to_owned()), Some("1".to_owned())] {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Alt-Shift-H mismatch: {active:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read(&outputs[0]).unwrap(), expected);
        assert_eq!(fs::read(&outputs[1]).unwrap(), expected);
    }
    #[test]
    fn verified_client_survives_reconnect_and_parking_with_queued_input() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-keep-client-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let session = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir.clone());
        let terminal = Terminal::spawn_command(session.attach()).unwrap();
        let client_pid = terminal.process_id().unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let clients = session.client_pids().unwrap();
            if clients.contains(&client_pid) {
                break;
            }
            assert!(Instant::now() < deadline, "client did not attach");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Restart the metadata backend exactly as reconnect does, while the
        // existing attach client remains alive.
        let backend = Backend::start(root.clone(), dir.clone(), false);
        let clients = loop {
            match backend.events.recv_timeout(Duration::from_secs(8)).unwrap() {
                Event::Ready(new_session, clients) => {
                    assert_eq!(new_session.socket, session.socket);
                    assert_eq!(new_session.name, session.name);
                    break clients.unwrap();
                }
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        };
        assert!(clients.contains(&client_pid));
        let server_pid: i32 = session
            .run(&["display-message", "-p", "-t", &session.name, "#{pid}"])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(server_pid > 1 && server_pid as u32 != std::process::id());
        struct Resume(i32);
        impl Drop for Resume {
            fn drop(&mut self) {
                unsafe {
                    libc::kill(self.0, libc::SIGCONT);
                }
            }
        }
        assert_eq!(unsafe { libc::kill(server_pid, libc::SIGSTOP) }, 0);
        let resume = Resume(server_pid);
        let marker = dir.join("reconnect-input");
        let path = marker.to_str().unwrap();
        assert!(
            path.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'))
        );
        terminal
            .input(format!("printf %s RETAINED > '{path}'\r").as_bytes())
            .unwrap();
        terminal.wait_for_writer().unwrap();
        assert_eq!(
            reconnect_client(Some((Some(client_pid), false)), &Ok(clients)),
            ReconnectClient::Reuse
        );
        let mut parked = crate::terminal_parking::Parked::default();
        assert!(parked.park(1, terminal).is_ok());
        drop(resume);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !std::fs::read_to_string(&marker).is_ok_and(|text| text == "RETAINED") {
            assert!(Instant::now() < deadline, "queued input not observed");
            std::thread::sleep(Duration::from_millis(20));
        }
        let terminal = parked.take(1).expect("client must remain parked");
        assert_eq!(terminal.process_id(), Some(client_pid));
        assert!(session.client_pids().unwrap().contains(&client_pid));
    }
    #[test]
    fn full_backend_queue_does_not_consume_detached_input() {
        use crate::editor_panes::{Content, Panes};
        use crate::terminal_resume::{Context, Deliveries, Fence, detach_with_delivery};
        use workspace_core::editor_panes::Axis;
        let (commands, rx) = mpsc::sync_channel(1);
        let (_event_tx, events) = mpsc::channel();
        let backend = Backend {
            events,
            commands,
            accepting: Mutex::new(true),
        };
        backend.send(Action::New).unwrap();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let now = Instant::now();
        let context = Context {
            project: 1,
            attachment: 4,
        };
        let token = fence.begin(context, now);
        fence.push(context, b"synthetic").unwrap();
        let mut panes = Panes::terminal(context.attachment);
        let terminal_pane = panes.layout.active();
        let file_pane = panes.layout.split(Axis::Row, true).unwrap();
        panes
            .set_content(Content::File("/synthetic/example".into()))
            .unwrap();
        panes.layout.focus(terminal_pane).unwrap();
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, id, bytes| {
                backend.send(Action::ResumeInput(id, bytes.to_vec()))
            })
            .is_err()
        );
        assert!(fence.active());
        assert_eq!(panes.layout.active(), terminal_pane);
        assert_eq!(
            panes.content(terminal_pane),
            Some(&Content::Terminal(context.attachment))
        );
        assert!(!deliveries.busy(1));
        assert!(matches!(rx.recv().unwrap(), Action::New));
        assert_eq!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, id, bytes| {
                backend.send(Action::ResumeInput(id, bytes.to_vec()))
            }),
            Ok(true)
        );
        assert!(
            matches!(rx.recv().unwrap(), Action::ResumeInput(id, bytes) if id == token && bytes == b"synthetic")
        );
        panes.layout.focus(file_pane).unwrap();
        assert_eq!(
            panes.content(file_pane),
            Some(&Content::File("/synthetic/example".into()))
        );
        assert_eq!(
            panes.content(terminal_pane),
            Some(&Content::Terminal(context.attachment))
        );
        assert!(!fence.active());
        assert!(deliveries.busy(1));
    }
    #[test]
    fn external_window_change_is_reported_by_private_tmux_backend() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-tmux-observed-change-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let backend = Backend::start(root, dir.clone(), false);
        let session = loop {
            match backend.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Ready(session, _) => break session,
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        };
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir);
        let before = loop {
            match backend.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Windows(windows) => break windows,
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        };
        // This acts directly on the synthetic server, bypassing Backend::send.
        session.action(Action::New).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let after = loop {
            assert!(Instant::now() < deadline, "external window event deadline");
            match backend.events.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::Windows(windows))
                    if observed_active_target_changed(&before, &windows) =>
                {
                    break windows;
                }
                Ok(Event::Error(error)) => panic!("{error}"),
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("{error}"),
            }
        };
        session.action(Action::Split).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "external pane event deadline");
            match backend.events.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::Windows(windows)) if observed_active_target_changed(&after, &windows) => {
                    let old = after.iter().find(|window| window.active).unwrap();
                    let new = windows.iter().find(|window| window.active).unwrap();
                    assert_eq!(old.id, new.id);
                    assert_ne!(old.active_pane, new.active_pane);
                    break;
                }
                Ok(Event::Error(error)) => panic!("{error}"),
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(
            session.while_target_stable(|| Ok("synthetic file")),
            Ok("synthetic file")
        );
        let changed = session.while_target_stable(|| {
            session.action(Action::New)?;
            Ok("must not open")
        });
        assert!(changed.is_err(), "changing the target must cancel the read");
    }

    #[test]
    fn backend_serializes_copy_mark_capture_and_restore_with_acknowledgements() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-jump-backend-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let backend = Backend::start(root, dir.clone(), false);
        let session = loop {
            match backend.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Ready(session, _) => break session,
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        };
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir);
        session
            .run(&[
                "send-keys",
                "-t",
                &format!("{}:", session.name),
                "printf 'synthetic-abcdefghij\\n'",
                "Enter",
            ])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let output = session
                .run(&["capture-pane", "-p", "-t", &format!("{}:", session.name)])
                .unwrap();
            if output.lines().any(|line| line == "synthetic-abcdefghij") {
                break;
            }
            assert!(Instant::now() < deadline, "synthetic output not ready");
            std::thread::sleep(Duration::from_millis(20));
        }
        backend.send(Action::Normal(true)).unwrap();
        backend.send(Action::Motion("gg".into(), 1)).unwrap();
        backend.send(Action::CaptureCopyMark(71)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mark = loop {
            assert!(Instant::now() < deadline, "synthetic capture deadline");
            match backend.events.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::CopyMarkCaptured(71, Ok(mark))) => break mark,
                Ok(Event::CopyMarkCaptured(71, Err(error))) | Ok(Event::Error(error)) => {
                    panic!("{error}")
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("{error}"),
            }
        };
        backend.send(Action::Motion("l".into(), 2)).unwrap();
        backend
            .send(Action::RestoreCopyMark(72, mark.clone()))
            .unwrap();
        loop {
            assert!(Instant::now() < deadline, "synthetic restore deadline");
            match backend.events.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::CopyMarkRestored(72, outcome)) => {
                    assert_eq!(outcome, RestoreCopyMark::Restored);
                    break;
                }
                Ok(Event::Error(error)) => panic!("{error}"),
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(
            session
                .copy_frame(&session.resume_target().unwrap())
                .unwrap(),
            mark.frame
        );
        // Move the original synthetic copy cursor, then select another
        // synthetic window. The backend must restore without selecting it.
        session.action(Action::Motion("l".into(), 2)).unwrap();
        let other = session
            .run(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                &session.name,
                "/bin/cat",
            ])
            .unwrap();
        session.run(&["select-window", "-t", other.trim()]).unwrap();
        let selected = session.resume_target().unwrap();
        backend
            .send(Action::RestoreCopyMarkInSession(73, mark.clone()))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                Instant::now() < deadline,
                "inactive synthetic restore deadline"
            );
            match backend.events.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::CopyMarkRestored(73, outcome)) => {
                    assert_eq!(outcome, RestoreCopyMark::Restored);
                    break;
                }
                Ok(Event::Error(error)) => panic!("{error}"),
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("{error}"),
            }
        }
        assert_eq!(session.copy_frame(&mark.target).unwrap(), mark.frame);
        assert_eq!(session.resume_target().unwrap(), selected);
    }
    #[test]
    fn control_formats_keep_tabs_and_unicode_with_c_locale() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-tmux-locale-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let session = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                // Retain the private fixture and protocol evidence on failure.
            }
        }
        let _cleanup = Cleanup(session.clone());
        session
            .run(&["rename-window", "-t", &session.name, "terminal-ñ"])
            .unwrap();
        let mut command = session.command();
        command.env("LC_ALL", "C").args([
            "list-windows",
            "-t",
            &session.name,
            "-F",
            "#{window_id}\t#{window_index}\t#{window_active}\t#{window_name}",
        ]);
        let windows = output(command).unwrap();
        std::fs::write(dir.join("windows.txt"), &windows).unwrap();
        assert_eq!(windows, "@0\t1\t1\tterminal-ñ\n", "{}", dir.display());
        let mut command = session.command();
        command.env("LC_ALL", "C").args([
            "display-message",
            "-p",
            "-t",
            &format!("{}:", session.name),
            "#{window_id}\t#{pane_id}",
        ]);
        let target = output(command).unwrap();
        std::fs::write(dir.join("target.txt"), &target).unwrap();
        assert_eq!(target, "@0\t%0\n", "{}", dir.display());
    }
    #[test]
    fn held_control_and_earlier_text_are_not_replayed_after_real_resume_ack() {
        use crate::terminal_resume::{Context, Fence};
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-held-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let backend = Backend::start(root, dir, false);
        let session = loop {
            match backend.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Ready(session, _) => break session,
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        };
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let _cleanup = Cleanup(session.clone());
        let now = Instant::now();
        let context = Context {
            project: 0,
            attachment: 1,
        };
        let mut fence = Fence::default();
        backend.send(Action::Normal(true)).unwrap();
        let token = fence.begin(context, now);
        backend.send(Action::Resume(token)).unwrap();
        fence.push(context, b"SYNTHETIC_NOT_SENT\r").unwrap();
        assert!(fence.push(context, b"\x02%").is_err());
        fence.retain_rejected(b"\x02%".to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline);
            match backend.events.recv_timeout(Duration::from_millis(500)) {
                Ok(Event::Resumed(id, result)) if id == token => {
                    result.unwrap();
                    assert!(
                        fence
                            .ack(context, token, true, true, Instant::now())
                            .is_none()
                    );
                    break;
                }
                Ok(Event::Error(error)) => panic!("{error}"),
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("{error}"),
            }
        }
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("SYNTHETIC_NOT_SENT\r"));
        assert_eq!(fence.prefix_hex_if_guarded(&guard).as_deref(), Some("0225"));
        let target = session.resume_target().unwrap();
        let capture = session
            .run(&["capture-pane", "-p", "-t", &target.pane])
            .unwrap();
        assert!(!capture.contains("SYNTHETIC_NOT_SENT"));
        let panes = session
            .run(&["list-panes", "-t", &target.window, "-F", "#{pane_id}"])
            .unwrap();
        assert_eq!(panes.lines().count(), 1);
    }
    #[test]
    fn resume_ack_orders_immediate_text_and_rejects_changed_pane_or_window() {
        let dir = std::env::temp_dir().join(format!("tw-resume-{}", std::process::id()));
        let root = std::env::temp_dir().canonicalize().unwrap();
        let backend = Backend::start(root, dir.clone(), false);
        let session = loop {
            match backend.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Ready(s, _) => break s,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        };
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir);
        backend.send(Action::Normal(true)).unwrap();
        let mut fence = crate::terminal_resume::Fence::default();
        let context = crate::terminal_resume::Context {
            project: 0,
            attachment: 1,
        };
        let token = fence.begin(context, Instant::now());
        backend.send(Action::Resume(token)).unwrap();
        // No wait between i and text. The only output path is the ack chain.
        fence
            .push(context, b"printf 'SYNTHETIC-RESUME-FAST\\n'\r")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut delivered = false;
        while fence.active() {
            assert!(Instant::now() < deadline);
            let (id, ready, result) =
                match backend.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                    Event::Resumed(id, r) => (id, true, r),
                    Event::InputDelivered(id, r) => {
                        delivered = true;
                        (id, false, r)
                    }
                    Event::Error(e) => panic!("{e}"),
                    _ => continue,
                };
            result.unwrap();
            if let Some(bytes) = fence.ack(context, id, ready, true, Instant::now()) {
                backend.send(Action::ResumeInput(id, bytes)).unwrap();
            }
        }
        assert!(delivered);
        let target = session.resume_target().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let captured = session
                .run(&["capture-pane", "-p", "-t", &target.pane])
                .unwrap();
            if captured
                .lines()
                .any(|line| line.trim() == "SYNTHETIC-RESUME-FAST")
            {
                break;
            }
            assert!(Instant::now() < deadline, "{captured}");
            std::thread::sleep(Duration::from_millis(10));
        }
        // A focus departure can queue the remaining batch immediately after
        // Resume; the backend must process it against the resolved pane in FIFO
        // order even before the UI receives the ready event.
        let blur_token = fence.begin(context, Instant::now());
        backend.send(Action::Normal(true)).unwrap();
        backend.send(Action::Resume(blur_token)).unwrap();
        fence.push(context, b"printf 'BLUR-FIFO-OK\\n'\r").unwrap();
        let detached = fence.detach().unwrap();
        assert_eq!(detached.token, blur_token);
        backend
            .send(Action::ResumeInput(blur_token, detached.bytes))
            .unwrap();
        // A window switch requested immediately afterward must be queued
        // behind the input, even before the UI has received either ack.
        backend.send(Action::New).unwrap();
        let mut resumed = false;
        loop {
            match backend.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Resumed(id, result) if id == blur_token => {
                    result.unwrap();
                    resumed = true;
                }
                Event::InputDelivered(id, result) if id == blur_token => {
                    assert!(resumed);
                    result.unwrap();
                    break;
                }
                Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let captured = session
                .run(&["capture-pane", "-p", "-t", &target.pane])
                .unwrap();
            if captured.lines().any(|line| line.trim() == "BLUR-FIFO-OK") {
                break;
            }
            assert!(Instant::now() < deadline, "{captured}");
            std::thread::sleep(Duration::from_millis(10));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while session.resume_target().unwrap().window == target.window {
            assert!(Instant::now() < deadline, "window change was not processed");
            std::thread::sleep(Duration::from_millis(10));
        }
        session.action(Action::Split).unwrap();
        assert_ne!(session.resume_target().unwrap().pane, target.pane);
        assert!(
            session
                .resume_input(&target, b"SHOULD-NOT-ENTER\r")
                .is_err()
        );
        let target = session.resume().unwrap();
        assert!(session.resume_input(&target, b"\x01").is_err());
        assert!(session.resume_input(&target, b"\x1b[200~paste").is_err());
        session.action(Action::New).unwrap();
        assert!(
            session
                .resume_input(&target, b"SHOULD-NOT-ENTER\r")
                .is_err()
        );
        // Target-changing backend actions invalidate even a previously acked token.
        backend.send(Action::New).unwrap();
        backend
            .send(Action::ResumeInput(token, b"SHOULD-NOT-ENTER\r".to_vec()))
            .unwrap();
        loop {
            if let Event::InputDelivered(id, result) =
                backend.events.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                assert_eq!(id, token);
                assert!(result.is_err());
                break;
            }
        }
        backend.send(Action::Select("@9999999999".into())).unwrap();
        let failed_token = fence.begin(context, Instant::now());
        fence.push(context, b"RETAINED-NOT-SENT").unwrap();
        backend.send(Action::Resume(failed_token)).unwrap();
        loop {
            if let Event::Resumed(id, result) =
                backend.events.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                assert_eq!(id, failed_token);
                assert!(result.is_err(), "failed selection must not resume old pane");
                assert_eq!(fence.ack(context, id, true, false, Instant::now()), None);
                let guard = fence
                    .retained_guard()
                    .expect("queued text must survive a failed ready ack");
                assert_eq!(fence.text_if_guarded(&guard), Some("RETAINED-NOT-SENT"));
                break;
            }
        }
        backend
            .send(Action::ResumeInput(
                failed_token,
                b"SHOULD-NOT-ENTER\r".to_vec(),
            ))
            .unwrap();
        loop {
            if let Event::InputDelivered(id, result) =
                backend.events.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                assert_eq!(id, failed_token);
                assert!(result.is_err());
                break;
            }
        }
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("RETAINED-NOT-SENT"));
        assert!(fence.cancel_guarded(&guard));

        // The UI may leave the pane before the ready acknowledgement. The
        // backend then rejects the queued batch; keep it for manual review.
        let mut deliveries = crate::terminal_resume::Deliveries::default();
        let detached_token = fence.begin(context, Instant::now());
        fence.push(context, b"DETACHED-RECOVERY-ONLY").unwrap();
        backend.send(Action::Select("@9999999999".into())).unwrap();
        backend.send(Action::Resume(detached_token)).unwrap();
        assert_eq!(
            crate::terminal_resume::detach_with_delivery(
                &mut fence,
                &mut deliveries,
                Instant::now(),
                |_, id, bytes| { backend.send(Action::ResumeInput(id, bytes.to_vec())) }
            ),
            Ok(true)
        );
        loop {
            if let Event::InputDelivered(id, result) =
                backend.events.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                assert_eq!(id, detached_token);
                assert!(result.is_err());
                assert_eq!(
                    deliveries.ack(context.project, id, false),
                    crate::terminal_resume::DeliveryAck::Failed
                );
                break;
            }
        }
        let guard = deliveries
            .failed_guard()
            .expect("rejected detached batch must remain recoverable");
        assert_eq!(
            deliveries.failed_text_if_guarded(guard),
            Some("DETACHED-RECOVERY-ONLY")
        );
        let captured = session
            .run(&[
                "capture-pane",
                "-p",
                "-t",
                &session.resume_target().unwrap().pane,
            ])
            .unwrap();
        assert!(!captured.contains("DETACHED-RECOVERY-ONLY"));
        assert!(deliveries.discard_failed_guarded(guard));

        // Hold a real backend ready reply until after a simulated deadline.
        // The synthetic text must stay in the fence, not be sent on late ack.
        let started = Instant::now();
        let delayed_token = fence.begin(context, started);
        fence.push(context, b"TIMEOUT-MUST-NOT-SEND").unwrap();
        backend.send(Action::Resume(delayed_token)).unwrap();
        let reply = loop {
            if let Event::Resumed(id, result) =
                backend.events.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                assert_eq!(id, delayed_token);
                break result;
            }
        };
        assert!(reply.is_ok());
        assert_eq!(
            fence.mark_expired(started + Duration::from_secs(16)),
            crate::terminal_resume::ResumeTimeout::Retained
        );
        assert_eq!(
            fence.ack(
                context,
                delayed_token,
                true,
                true,
                started + Duration::from_secs(16)
            ),
            None
        );
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("TIMEOUT-MUST-NOT-SEND"));
        let captured = session
            .run(&[
                "capture-pane",
                "-p",
                "-t",
                &session.resume_target().unwrap().pane,
            ])
            .unwrap();
        assert!(!captured.contains("TIMEOUT-MUST-NOT-SEND"));
        assert!(fence.cancel_guarded(&guard));
    }
    #[test]
    fn frozen_copy_rows_preserve_spaces_and_reject_tabs_without_moving_cursor() {
        let root = std::env::temp_dir();
        let dir = root.join(format!(
            "tw-copy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let session = Session {
            binary: [
                "/opt/homebrew/bin/tmux",
                "/usr/local/bin/tmux",
                "/usr/bin/tmux",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .unwrap(),
            socket: dir.join("s"),
            name: "synthetic".into(),
            root: root.clone(),
        };
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir);
        session.run(&[
            "-f", "/dev/null", "new-session", "-d", "-s", "synthetic", "-x", "40", "-y", "10",
            "printf 'alpha   \\nbeta   \\none\\ttwo\\n'; sleep 1; printf 'later\\n'; exec /bin/cat",
        ]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(session.capture_normal_lines(1).is_err());
        session.action(Action::Normal(true)).unwrap();
        session.action(Action::Motion("gg".into(), 1)).unwrap();
        let target = session.resume_target().unwrap();
        let before = session.copy_frame(&target).unwrap();
        assert_eq!(
            session.capture_normal_lines(2).unwrap(),
            "alpha   \nbeta   \n"
        );
        assert_eq!(session.copy_frame(&target).unwrap(), before);
        assert!(session.capture_normal_lines(0).is_err());
        assert!(session.capture_normal_lines(33).is_err());
        std::thread::sleep(Duration::from_millis(1100));
        assert!(
            session
                .run(&["capture-pane", "-p", "-t", &target.pane])
                .unwrap()
                .contains("later")
        );
        assert_eq!(
            session.capture_normal_lines(2).unwrap(),
            "alpha   \nbeta   \n"
        );
        session.action(Action::Motion("j".into(), 2)).unwrap();
        assert!(session.capture_normal_lines(1).is_err());
        session.action(Action::Motion("v".into(), 1)).unwrap();
        assert!(session.capture_normal_lines(1).is_err());
        session
            .run(&[
                "new-window",
                "-t",
                "synthetic",
                "-n",
                "history",
                "for i in $(seq 1 30); do printf 'line_%02d   \\n' \"$i\"; done; exec /bin/cat",
            ])
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        session.action(Action::Normal(true)).unwrap();
        session.action(Action::Motion("gg".into(), 1)).unwrap();
        let history_target = session.resume_target().unwrap();
        assert!(session.copy_frame(&history_target).unwrap().scroll > 0);
        assert_eq!(
            session.capture_normal_lines(2).unwrap(),
            "line_01   \nline_02   \n"
        );
        // The wrapper changes the synthetic copy cursor after the capture but
        // before the validation query, simulating another client moving it.
        let wrapper = session.socket.parent().unwrap().join("tmux-wrapper");
        let script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do last=\"$arg\"; done\nfor arg in \"$@\"; do\n  if [ \"$arg\" = capture-pane ]; then\n    '{0}' \"$@\" || exit $?\n    '{0}' -S \"$3\" send-keys -t \"$last\" -X cursor-down >/dev/null\n    exit $?\n  fi\ndone\nexec '{0}' \"$@\"\n",
            session.binary.display()
        );
        std::fs::write(&wrapper, script).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut racing = session.clone();
        racing.binary = wrapper;
        assert!(racing.capture_normal_lines(1).is_err());

        // Written trailing spaces and erased cells can have identical tmux
        // captures even though Neovim's linewise yanks distinguish them.
        session
            .run(&[
                "new-window",
                "-t",
                "synthetic",
                "-n",
                "erase-ambiguity",
                r"printf 'xy    \n123456\rxy\033[K\n'; exec /bin/cat",
            ])
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        session.action(Action::Normal(true)).unwrap();
        session.action(Action::Motion("gg".into(), 1)).unwrap();
        assert_eq!(session.capture_normal_lines(2).unwrap(), "xy    \nxy    \n");
    }

    #[test]
    fn copy_marks_capture_stable_coordinates_and_exact_target_without_moving() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-terminal.json"
        ))
        .unwrap();
        assert_eq!(reference["after_back"]["terminal"], true);
        assert_eq!(reference["after_back"]["mode"], "nt");
        assert_eq!(reference["before"]["cursor"], serde_json::json!([2, 1]));
        assert_eq!(
            reference["after_back"]["cursor"],
            reference["before"]["cursor"]
        );
        assert_eq!(reference["after_forward"]["terminal"], false);
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-jump-mark-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let session = Session {
            binary: [
                "/opt/homebrew/bin/tmux",
                "/usr/local/bin/tmux",
                "/usr/bin/tmux",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .unwrap(),
            socket: dir.join("s"),
            name: "synthetic".into(),
            root,
        };
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir);
        session.run(&["-u", "-f", "/dev/null", "new-session", "-d", "-s", "synthetic", "-x", "40", "-y", "8",
            "for i in $(seq 1 50); do printf 'fixture-%03d-abcdefghij\\n' \"$i\"; done; exec /bin/cat"]).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(session.capture_copy_mark().is_err());
        session.action(Action::Normal(true)).unwrap();
        session.action(Action::Motion("gg".into(), 1)).unwrap();
        let top = session.capture_copy_mark().unwrap();
        assert_eq!(top.absolute_row(), Some(0));
        assert!(session.capture_copy_mark().unwrap() == top);
        session
            .run(&["send-keys", "-t", "synthetic:", "-X", "goto-line", "10"])
            .unwrap();
        session.action(Action::Motion("j".into(), 2)).unwrap();
        session.action(Action::Motion("l".into(), 6)).unwrap();
        let mark = session.capture_copy_mark().unwrap();
        assert_eq!(mark.frame.scroll, 10);
        assert_eq!(mark.frame.row, 2);
        assert_eq!(mark.column(), 6);
        assert_eq!(mark.absolute_row(), Some(mark.frame.history + 2 - 10));
        assert!(session.capture_copy_mark().unwrap() == mark);
        assert!(session.copy_mark_matches(&mark).unwrap());
        session.action(Action::Motion("k".into(), 1)).unwrap();
        assert!(session.copy_mark_matches(&mark).unwrap());
        session
            .run(&["send-keys", "-t", "synthetic:", "-X", "goto-line", "5"])
            .unwrap();
        assert!(session.copy_mark_matches(&mark).unwrap());
        assert_eq!(session.restore_copy_mark(&mark), RestoreCopyMark::Restored);
        assert_eq!(
            session
                .copy_frame(&session.resume_target().unwrap())
                .unwrap(),
            mark.frame
        );
        for key in ["gg", "G"] {
            session.action(Action::Motion(key.into(), 1)).unwrap();
            assert_eq!(
                session.restore_copy_mark(&mark),
                RestoreCopyMark::Restored,
                "{key}"
            );
            assert_eq!(
                session
                    .copy_frame(&session.resume_target().unwrap())
                    .unwrap(),
                mark.frame
            );
        }
        assert_eq!(session.restore_copy_mark(&top), RestoreCopyMark::Restored);
        assert_eq!(
            session
                .copy_frame(&session.resume_target().unwrap())
                .unwrap(),
            top.frame
        );
        assert_eq!(session.restore_copy_mark(&mark), RestoreCopyMark::Restored);
        session.action(Action::Motion("k".into(), 1)).unwrap();
        let before_race = session
            .copy_frame(&session.resume_target().unwrap())
            .unwrap();
        let gate_wrapper = session.socket.parent().unwrap().join("jump-gate-wrapper");
        let gate_script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = if-shell ]; then\n    '{0}' -S \"$3\" send-keys -t synthetic: -X cursor-down >/dev/null\n    break\n  fi\ndone\nexec '{0}' \"$@\"\n",
            session.binary.display()
        );
        std::fs::write(&gate_wrapper, gate_script).unwrap();
        std::fs::set_permissions(&gate_wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut gate_race = session.clone();
        gate_race.binary = gate_wrapper;
        assert_eq!(
            gate_race.restore_copy_mark(&mark),
            RestoreCopyMark::RejectedBeforeMove
        );
        let after_race = session
            .copy_frame(&session.resume_target().unwrap())
            .unwrap();
        assert_eq!(after_race.row, before_race.row + 1);
        assert_eq!(after_race.scroll, before_race.scroll);
        assert_eq!(after_race.column, before_race.column);
        let uncertain_wrapper = session.socket.parent().unwrap().join("jump-result-wrapper");
        let uncertain_script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = if-shell ]; then\n    '{0}' \"$@\" || exit $?\n    '{0}' -S \"$3\" send-keys -t synthetic: -X cursor-down >/dev/null\n    exit $?\n  fi\ndone\nexec '{0}' \"$@\"\n",
            session.binary.display()
        );
        std::fs::write(&uncertain_wrapper, uncertain_script).unwrap();
        std::fs::set_permissions(&uncertain_wrapper, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let mut uncertain = session.clone();
        uncertain.binary = uncertain_wrapper;
        assert_eq!(
            uncertain.restore_copy_mark(&mark),
            RestoreCopyMark::UncertainAfterMove
        );
        assert_eq!(
            session
                .copy_frame(&session.resume_target().unwrap())
                .unwrap()
                .row,
            mark.frame.row + 1
        );
        assert_eq!(session.restore_copy_mark(&mark), RestoreCopyMark::Restored);
        // Move only this private synthetic cursor between capture and the
        // final observation, emulating another client racing the mark.
        let wrapper = session.socket.parent().unwrap().join("jump-mark-wrapper");
        let script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do last=\"$arg\"; done\nfor arg in \"$@\"; do\n  if [ \"$arg\" = capture-pane ]; then\n    '{0}' \"$@\" || exit $?\n    '{0}' -S \"$3\" send-keys -t \"$last\" -X cursor-down >/dev/null\n    exit $?\n  fi\ndone\nexec '{0}' \"$@\"\n",
            session.binary.display()
        );
        std::fs::write(&wrapper, script).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut racing = session.clone();
        racing.binary = wrapper;
        assert!(racing.capture_copy_mark().is_err());
        assert!(!racing.copy_mark_matches(&mark).unwrap_or(false));
        session.action(Action::Motion("v".into(), 1)).unwrap();
        session.action(Action::Motion("l".into(), 1)).unwrap();
        assert!(session.capture_copy_mark().is_err());
        assert!(!session.copy_mark_matches(&mark).unwrap_or(false));
        assert_eq!(
            session.restore_copy_mark(&mark),
            RestoreCopyMark::RejectedBeforeMove
        );
        session
            .run(&["send-keys", "-t", "synthetic:", "-X", "clear-selection"])
            .unwrap();
        session.action(Action::Motion("k".into(), 1)).unwrap();
        let other = session
            .run(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                "synthetic",
                "/bin/cat",
            ])
            .unwrap();
        session.run(&["select-window", "-t", other.trim()]).unwrap();
        assert!(!session.copy_mark_matches(&mark).unwrap());
        assert_eq!(
            session.restore_copy_mark(&mark),
            RestoreCopyMark::RejectedBeforeMove
        );
        let current = session.resume_target().unwrap();
        let before_other = session.copy_frame(&mark.target).unwrap();
        assert_ne!(before_other, mark.frame);
        let inactive_gate = session
            .socket
            .parent()
            .unwrap()
            .join("inactive-gate-wrapper");
        let script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = if-shell ]; then\n    '{0}' -S \"$3\" send-keys -t '{1}' -X cursor-down >/dev/null\n    break\n  fi\ndone\nexec '{0}' \"$@\"\n",
            session.binary.display(),
            mark.target.pane
        );
        std::fs::write(&inactive_gate, script).unwrap();
        std::fs::set_permissions(&inactive_gate, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut raced = session.clone();
        raced.binary = inactive_gate;
        assert_eq!(
            raced.restore_copy_mark_in_session(&mark),
            RestoreCopyMark::RejectedBeforeMove
        );
        assert_eq!(session.resume_target().unwrap(), current);
        assert_eq!(
            session.restore_copy_mark_in_session(&mark),
            RestoreCopyMark::Restored
        );
        assert_eq!(session.copy_frame(&mark.target).unwrap(), mark.frame);
        assert_eq!(session.resume_target().unwrap(), current);
        let inactive_result = session
            .socket
            .parent()
            .unwrap()
            .join("inactive-result-wrapper");
        let script = format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = if-shell ]; then\n    '{0}' \"$@\" || exit $?\n    '{0}' -S \"$3\" send-keys -t '{1}' -X cursor-down >/dev/null\n    exit $?\n  fi\ndone\nexec '{0}' \"$@\"\n",
            session.binary.display(),
            mark.target.pane
        );
        std::fs::write(&inactive_result, script).unwrap();
        std::fs::set_permissions(&inactive_result, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut uncertain = session.clone();
        uncertain.binary = inactive_result;
        assert_eq!(
            uncertain.restore_copy_mark_in_session(&mark),
            RestoreCopyMark::UncertainAfterMove
        );
        assert_eq!(session.resume_target().unwrap(), current);
        let mut wrong_session = mark.clone();
        wrong_session.session_id = "$9999999".into();
        assert_eq!(
            session.restore_copy_mark_in_session(&wrong_session),
            RestoreCopyMark::RejectedBeforeMove
        );
        assert_eq!(session.resume_target().unwrap(), current);
        // This window belongs only to the isolated test server. Once removed,
        // its saved mark cannot be retargeted to the still-active window.
        session
            .run(&["kill-window", "-t", &mark.target.window])
            .unwrap();
        assert_eq!(
            session.restore_copy_mark_in_session(&mark),
            RestoreCopyMark::RejectedBeforeMove
        );
        assert_eq!(session.resume_target().unwrap(), current);

        // New output must invalidate a mark when tmux history grows.
        let signal = session.socket.parent().unwrap().join("emit-more");
        let quoted = signal.to_str().unwrap().replace('\'', "'\\''");
        let command = format!(
            "for i in $(seq 1 40); do printf 'before-%03d\\n' \"$i\"; done; \
            while [ ! -f '{quoted}' ]; do sleep .05; done; \
            for i in $(seq 1 40); do printf 'after-%03d\\n' \"$i\"; done; exec /bin/cat"
        );
        let output_window = session
            .run(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                "synthetic",
                &command,
            ])
            .unwrap();
        session
            .run(&["select-window", "-t", output_window.trim()])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if session
                .run(&["capture-pane", "-p", "-t", "synthetic:"])
                .unwrap()
                .contains("before-040")
            {
                break;
            }
            assert!(Instant::now() < deadline, "synthetic output did not arrive");
            std::thread::sleep(Duration::from_millis(20));
        }
        session.action(Action::Normal(true)).unwrap();
        session.action(Action::Motion("gg".into(), 1)).unwrap();
        let old = session.capture_copy_mark().unwrap();
        std::fs::write(&signal, b"go").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if session
                .copy_frame(&session.resume_target().unwrap())
                .unwrap()
                .history
                != old.frame.history
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic history did not change"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!session.copy_mark_matches(&old).unwrap());
        assert_eq!(
            session.restore_copy_mark(&old),
            RestoreCopyMark::RejectedBeforeMove
        );

        // A wide glyph and a tab can each advance more than one screen cell.
        let unicode = session
            .run(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-t",
                "synthetic",
                "printf 'a\\347\\225\\214b\\tcd\\n'; exec /bin/cat",
            ])
            .unwrap();
        session
            .run(&["select-window", "-t", unicode.trim()])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if session
                .run(&["capture-pane", "-p", "-t", "synthetic:"])
                .unwrap()
                .contains("a界b")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic UTF-8 output did not arrive"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        session.action(Action::Normal(true)).unwrap();
        session.action(Action::Motion("gg".into(), 1)).unwrap();
        session.action(Action::Motion("l".into(), 4)).unwrap();
        let unicode_mark = session.capture_copy_mark().unwrap();
        assert_eq!(unicode_mark.column(), 8);
        session.action(Action::Motion("0".into(), 1)).unwrap();
        assert_eq!(
            session.restore_copy_mark(&unicode_mark),
            RestoreCopyMark::Restored
        );
        assert_eq!(
            session
                .copy_frame(&session.resume_target().unwrap())
                .unwrap(),
            unicode_mark.frame
        );
        let mut invalid_name = session.clone();
        invalid_name.name = "synthetic; display-message injection".into();
        assert_eq!(
            invalid_name.restore_copy_mark(&unicode_mark),
            RestoreCopyMark::RejectedBeforeMove
        );
        assert_eq!(
            session
                .copy_frame(&session.resume_target().unwrap())
                .unwrap(),
            unicode_mark.frame
        );
        let mut too_far = unicode_mark.clone();
        too_far.frame.column = 513;
        assert_eq!(
            session.restore_copy_mark(&too_far),
            RestoreCopyMark::RejectedBeforeMove
        );
    }

    #[cfg(feature = "ghostty")]
    #[test]
    fn private_copy_redraw_without_indicator_retains_grapheme_presence_with_tabs() {
        let root = std::env::temp_dir();
        let dir = root.join(format!(
            "tw-copy-presence-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let session = Session {
            binary: [
                "/opt/homebrew/bin/tmux",
                "/usr/local/bin/tmux",
                "/usr/bin/tmux",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .unwrap(),
            socket: dir.join("s"),
            name: "synthetic".into(),
            root,
        };
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir);
        session
            .run(&[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "synthetic",
                "-x",
                "80",
                "-y",
                "24",
                r"printf 'xy    \n123456\rxy\033[K\none\ttwo  \n'; exec /bin/cat",
            ])
            .unwrap();
        session
            .run(&["set-option", "-t", "synthetic", "status", "off"])
            .unwrap();
        session
            .run(&[
                "set-window-option",
                "-t",
                "synthetic:",
                "copy-mode-position-format",
                "",
            ])
            .unwrap();
        let terminal = crate::terminal::Terminal::spawn_command(session.attach()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let initial = loop {
            if let Some(frame) = terminal.snapshot()
                && frame.cells[0][0].text == "x"
                && frame.cells[1][0].text == "x"
            {
                break frame;
            }
            assert!(Instant::now() < deadline, "synthetic attach did not settle");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(initial.cells[0][2..6].iter().all(|c| c.has_grapheme));
        assert!(initial.cells[1][2..6].iter().all(|c| !c.has_grapheme));
        session.action(Action::Normal(true)).unwrap();
        session.action(Action::Motion("gg".into(), 1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let copy = loop {
            if let Some(frame) = terminal.snapshot()
                && frame.revision > initial.revision
                && frame.cursor.0 == 0
            {
                break frame;
            }
            assert!(Instant::now() < deadline, "copy-mode redraw did not settle");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(copy.cells[0][2..6].iter().all(|c| c.has_grapheme));
        assert!(copy.cells[1][2..6].iter().all(|c| !c.has_grapheme));
        assert_eq!(
            copy.cells[2][0..13]
                .iter()
                .map(|c| c.text.as_str())
                .collect::<String>(),
            "one     two  "
        );
        assert!(copy.cells[2][3..8].iter().all(|c| c.has_grapheme));
        assert!(copy.cells[2][11..13].iter().all(|c| c.has_grapheme));
        assert!(!copy.cells[2][13].has_grapheme);
    }

    #[test]
    fn private_normal_motions_select_text_and_return_without_shell_input() {
        let dir = std::env::temp_dir().join(format!("tw-tmux-normal-{}", std::process::id()));
        let root = std::env::temp_dir().canonicalize().unwrap();
        let session = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir);
        session
            .run(&[
                "send-keys",
                "printf 'SYNTHETIC-NAV\\nsecond row\\n'",
                "Enter",
            ])
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        session.action(Action::Normal(true)).unwrap();
        let state = || {
            session
                .run(&[
                    "display-message",
                    "-p",
                    "#{pane_in_mode}:#{selection_present}:#{copy_cursor_y}:#{copy_cursor_x}",
                ])
                .unwrap()
        };
        assert!(state().starts_with("1:0:"));
        session.action(Action::Motion("gg".into(), 1)).unwrap();
        session.action(Action::Motion("0".into(), 1)).unwrap();
        session.action(Action::Motion("j".into(), 1)).unwrap();
        let before = state();
        session.action(Action::Motion("v".into(), 1)).unwrap();
        session.action(Action::Motion("l".into(), 3)).unwrap();
        assert!(state().starts_with("1:1:"));
        assert_ne!(before, state());
        assert!(!session.copy_selection_with_mode(true).unwrap().is_empty());
        assert!(state().starts_with("1:0:"));
        session.action(Action::Motion("l".into(), 1)).unwrap();
        assert!(state().starts_with("1:0:"));
        session.action(Action::Motion("v".into(), 1)).unwrap();
        session.action(Action::Motion("<Esc>".into(), 1)).unwrap();
        assert!(state().starts_with("1:0:"));
        assert!(session.action(Action::Motion("delete".into(), 1)).is_err());
        assert!(session.action(Action::Motion("j".into(), 10000)).is_err());
        session.action(Action::Normal(false)).unwrap();
        assert!(state().starts_with("0:"));
    }

    #[cfg(feature = "ghostty")]
    #[test]
    #[ignore = "Deferred tmux graphics: initial image transmission is intermittently lost; explicit --ignored diagnostics only"]
    fn real_private_tmux_passes_chunked_graphics_and_quiet_animation_to_the_adapter() {
        for fixture in ["rgba", "png", "animation", "small-animation"] {
            let animation = fixture.ends_with("animation");
            let small = fixture == "small-animation";
            let dir = std::env::temp_dir()
                .join(format!("tw-tmux-images-{}-{fixture}", std::process::id()));
            let root = std::env::temp_dir().canonicalize().unwrap();
            let session = Session::prepare(&root, &dir, false).unwrap();
            struct Cleanup(Session, PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = self.0.run(&["kill-server"]);
                    let _ = std::fs::remove_dir_all(&self.1);
                }
            }
            let _cleanup = Cleanup(session.clone(), dir);
            assert_eq!(
                session
                    .run(&["show-options", "-g", "-p", "-v", "allow-passthrough"])
                    .unwrap()
                    .trim(),
                "on"
            );
            let capture = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut terminal =
                Terminal::spawn_capturing(session.attach(), capture.clone()).unwrap();
            terminal
                .resize_cells(
                    24,
                    80,
                    if small { 8 } else { 9 },
                    if small { 21 } else { 18 },
                )
                .unwrap();
            // Establish an output barrier after attach/resize before emitting
            // passthrough images, which tmux does not replay from pane history.
            // Octal input avoids matching the echoed command as readiness.
            terminal.input(b"/usr/bin/printf '\\124\\127\\055\\101\\124\\124\\101\\103\\110\\055\\122\\105\\101\\104\\131\\n'\r").unwrap();
            let ready_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if terminal
                    .snapshot()
                    .is_some_and(|frame| frame.text().contains("TW-ATTACH-READY"))
                {
                    break;
                }
                assert!(
                    Instant::now() < ready_deadline,
                    "private tmux attach readiness deadline for {fixture}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join(if animation {
                "../../Resources/Helpers/terminal_animation_demo.py"
            } else {
                "../../Resources/Helpers/terminal_graphics_demo.py"
            });
            let quoted = helper.to_str().unwrap().replace('\'', "'\\''");
            let extra_args = match fixture {
                "png" => "--png",
                "animation" => "--once --frame-ms 100 --raster square",
                "small-animation" => "--once --frame-ms 100 --raster small",
                _ => "",
            };
            terminal
                .input(
                    format!("/usr/bin/python3 -I -S '{quoted}' --tmux {extra_args}\r").as_bytes(),
                )
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut first_color: Option<[u8; 4]> = None;
            loop {
                if let Some(frame) = terminal.snapshot() {
                    assert!(frame.error.is_none(), "{:?}", frame.error);
                    // tmux text readiness can be observed before all passthrough
                    // placements. Keep the existing deadline and pixel checks.
                    if frame.text().contains("FRAME-READY") && !frame.images.is_empty() {
                        assert!(frame.images.len() <= 8);
                        assert!(!frame.images_limited);
                        assert!(
                            frame
                                .images
                                .iter()
                                .all(|p| p.layer == 1
                                    && p.pixels.width == if small { 16 } else { 64 })
                        );
                        if animation {
                            let color: [u8; 4] =
                                frame.images[0].pixels.bgra[..4].try_into().unwrap();
                            if let Some(first) = first_color {
                                if first != color {
                                    break;
                                }
                            } else {
                                first_color = Some(color);
                            }
                        } else {
                            let red = frame.images.iter().any(|p| {
                                p.pixels.key.crop[1] == 0 && p.pixels.bgra[..4] == [0, 0, 255, 255]
                            });
                            let blue = frame.images.iter().any(|p| {
                                p.pixels.key.crop[1] >= 32 && p.pixels.bgra[..4] == [255, 0, 0, 255]
                            });
                            if red && blue {
                                break;
                            }
                        }
                    }
                }
                if Instant::now() >= deadline {
                    let frame = terminal.snapshot();
                    let evidence = std::env::temp_dir().join(format!(
                        "tw-tmux-graphics-failure-{}-{fixture}-{}",
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                    ));
                    use std::os::unix::fs::DirBuilderExt;
                    if std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&evidence)
                        .is_ok()
                    {
                        if let Ok(bytes) = capture.lock() {
                            let _ = std::fs::write(evidence.join("pty-output.bin"), &*bytes);
                        }
                        eprintln!("Synthetic PTY evidence: {}", evidence.display());
                    }
                    panic!(
                        "private tmux graphics deadline for {fixture}; revision={:?} images={:?} limited={:?} ready={} error={:?}",
                        frame.as_ref().map(|f| f.revision),
                        frame.as_ref().map(|f| f.images.len()),
                        frame.as_ref().map(|f| f.images_limited),
                        frame
                            .as_ref()
                            .is_some_and(|f| f.text().contains("FRAME-READY")),
                        frame.as_ref().map(|f| &f.error)
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    #[test]
    fn synthetic_prompt_glyphs_survive_tmux_and_terminal_parser() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-prompt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let session = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let _cleanup = Cleanup(session.clone());
        let terminal = Terminal::spawn_command(session.attach()).unwrap();
        terminal.input(b"printf '\\033[1;32m\\342\\235\\257\\033[0m TWR_PROMPT_OK\\n'; printf '\\033[1;31m\\342\\234\\227\\033[0m TWR_PROMPT_ERR\\n'\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(snapshot) = terminal.snapshot() {
                let mut found = Vec::new();
                for (glyph, marker) in [("❯", "TWR_PROMPT_OK"), ("✗", "TWR_PROMPT_ERR")] {
                    if let Some(cell) = snapshot.cells.iter().find_map(|row| {
                        let text: String = row.iter().map(|cell| cell.text.as_str()).collect();
                        if text.contains(marker) {
                            row.iter().find(|cell| cell.text == glyph)
                        } else {
                            None
                        }
                    }) {
                        found.push(cell.clone());
                    }
                }
                if found.len() == 2 {
                    assert!(found[0].bold && found[1].bold);
                    assert_ne!(found[0].fg, found[1].fg);
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "private tmux prompt did not reach the parser"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    fn reconnect_preserves_visible_shell_output_without_trailing_newline() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-no-newline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let session = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let _cleanup = Cleanup(session.clone());
        let first = Terminal::spawn_command(session.attach()).unwrap();
        // Octal bytes avoid confusing the shell's command echo with its output.
        first.input(b"/usr/bin/printf '\\116\\117\\055\\116\\105\\127\\114\\111\\116\\105\\055\\117\\113'\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(frame) = first.snapshot()
                && frame.text().contains("NO-NEWLINE-OK")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "original terminal did not render output"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // Match the file-focus and return-to-input transition before a second
        // command. Neither command prints a trailing line feed.
        session.action(Action::Normal(true)).unwrap();
        session.action(Action::Normal(false)).unwrap();
        first
            .input(b"/usr/bin/printf '\\123\\105\\103\\117\\116\\104\\055\\117\\113'\r")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let before = loop {
            if let Some(frame) = first.snapshot()
                && frame.text().contains("SECOND-OK")
            {
                break frame.text();
            }
            assert!(Instant::now() < deadline, "second command did not render");
            std::thread::sleep(Duration::from_millis(20));
        };
        let target = session.resume_target().unwrap();
        let pane_before = session
            .run(&["capture-pane", "-p", "-t", &target.pane])
            .unwrap();
        std::fs::write(dir.join("before-parser.txt"), &before).unwrap();
        std::fs::write(dir.join("before-pane.txt"), &pane_before).unwrap();
        drop(first);
        let second = Terminal::spawn_command(session.attach()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let after = loop {
            if let Some(frame) = second.snapshot()
                && frame.revision > 0
                && frame.text().contains("sh-")
            {
                break frame.text();
            }
            assert!(
                Instant::now() < deadline,
                "reattached terminal did not render shell"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let pane_after = session
            .run(&["capture-pane", "-p", "-t", &target.pane])
            .unwrap();
        std::fs::write(dir.join("after-parser.txt"), &after).unwrap();
        std::fs::write(dir.join("after-pane.txt"), &pane_after).unwrap();
        for (stage, text) in [
            ("parser before", &before),
            ("pane before", &pane_before),
            ("parser after", &after),
            ("pane after", &pane_after),
        ] {
            assert!(
                text.contains("NO-NEWLINE-OK") && text.contains("SECOND-OK"),
                "output missing in {stage}; synthetic evidence: {}",
                dir.display()
            );
        }
    }
    #[test]
    fn wiped_terminal_view_can_reopen_same_private_tmux_session() {
        use crate::buffer_numbers::{BufferTracking, LookupError};
        use crate::buffer_registry::{Buffer, BufferRegistry};
        use crate::editor_panes::{Panes, stage_vim_buffer_delete_target};
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-wipe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!dir.exists());
        let session = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir.clone());
        let pid = session
            .run(&["display-message", "-p", "-t", &session.name, "#{pane_pid}"])
            .unwrap();
        let terminal = Terminal::spawn_command(session.attach()).unwrap();
        let old = Buffer::Terminal(41);
        let file = Buffer::File("/synthetic/a".into());
        let mut registry = BufferRegistry::default();
        registry.register(old.clone());
        registry.register(file.clone());
        let mut numbers = BufferTracking::default();
        numbers.register(1, old.clone());
        let old_number = numbers.number_for(1, &old).unwrap();
        let panes = Panes::terminal(41);
        let (mut panes, mut registry, _) =
            stage_vim_buffer_delete_target(&panes, &registry, &old, Some(file.clone()))
                .unwrap()
                .unwrap();
        assert!(numbers.wipe_owned(1, &old));
        assert_eq!(numbers.resolve(1, old_number), Err(LookupError::Missing));
        assert!(!registry.knows(&old));
        let renewed = Buffer::Terminal(42);
        let pane = panes
            .reveal_terminal_with_registry(&mut registry, 42)
            .unwrap();
        numbers.register(1, renewed.clone());
        assert_ne!(numbers.number_for(1, &renewed).unwrap(), old_number);
        assert_eq!(registry.alternate(pane, &renewed), Some(file));
        let marker = dir.join("reopened");
        let marker_path = marker.to_str().unwrap();
        assert!(
            marker_path
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'/' | b'-' | b'_' | b'.'))
        );
        terminal
            .input(format!("printf '%s' REOPENED > '{marker_path}'\r").as_bytes())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !std::fs::read_to_string(&marker).is_ok_and(|text| text == "REOPENED") {
            assert!(
                Instant::now() < deadline,
                "reopened terminal did not execute command"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            session
                .run(&["display-message", "-p", "-t", &session.name, "#{pane_pid}"])
                .unwrap(),
            pid
        );
    }
    #[test]
    fn detached_client_exits_and_private_tmux_panes_survive_reattach() {
        exercise_detach(false);
    }
    #[test]
    #[ignore = "known race: PTY queue acceptance is not a tmux delivery acknowledgment"]
    fn input_accepted_immediately_before_client_drop_is_delivered() {
        exercise_detach(true);
    }
    fn exercise_detach(race: bool) {
        fn wait_for(label: &str, mut condition: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !condition() {
                assert!(Instant::now() < deadline, "timed out waiting for {label}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        fn pane_state(session: &Session) -> Vec<String> {
            let result = session
                .run(&[
                    "list-panes",
                    "-s",
                    "-t",
                    &session.name,
                    "-F",
                    "#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}",
                ])
                .unwrap();
            let mut rows: Vec<_> = result.lines().map(str::to_owned).collect();
            rows.sort();
            rows
        }
        fn client_count(session: &Session) -> usize {
            session
                .run(&["list-clients", "-F", "#{client_pid}"])
                .unwrap()
                .lines()
                .count()
        }
        fn marker_input(terminal: &Terminal, path: &Path, marker: &str) {
            let path = path.to_str().unwrap();
            assert!(
                path.bytes().all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'/' | b'-' | b'_' | b'.'))
            );
            terminal
                .input(format!("printf '%s' {marker} > '{path}'\r").as_bytes())
                .unwrap();
        }
        let root = std::env::temp_dir().canonicalize().unwrap();
        let dir = root.join(format!(
            "tw-detach-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!dir.exists());
        let session = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(session.clone(), dir.clone());
        session.action(Action::New).unwrap();
        session.action(Action::Split).unwrap();
        let before = pane_state(&session);
        assert_eq!(before.len(), 3);
        assert!(before.iter().all(|row| row.ends_with("\t0")));
        let terminal = Terminal::spawn_command(session.attach()).unwrap();
        wait_for("first client", || client_count(&session) == 1);
        let first = dir.join("before");
        marker_input(&terminal, &first, "BEFORE");
        wait_for("first command", || {
            std::fs::read_to_string(&first).is_ok_and(|value| value == "BEFORE")
        });
        let pending = dir.join("pending");
        marker_input(&terminal, &pending, "PENDING");
        if !race {
            wait_for("command observed before disconnect", || {
                std::fs::read_to_string(&pending).is_ok_and(|value| value == "PENDING")
            });
        }
        let worker = terminal.worker_lifetime();
        drop(terminal);
        wait_for("client worker to exit and disconnect", || {
            worker.upgrade().is_none() && client_count(&session) == 0
        });
        wait_for("queued command", || {
            std::fs::read_to_string(&pending).is_ok_and(|value| value == "PENDING")
        });
        assert_eq!(pane_state(&session), before);
        let reattached = Terminal::spawn_command(session.attach()).unwrap();
        wait_for("reattached client", || client_count(&session) == 1);
        let after = dir.join("after");
        marker_input(&reattached, &after, "AFTER");
        wait_for("command after reattach", || {
            std::fs::read_to_string(&after).is_ok_and(|value| value == "AFTER")
        });
        assert_eq!(pane_state(&session), before);
        let worker = reattached.worker_lifetime();
        drop(reattached);
        wait_for("reattached worker to exit", || {
            worker.upgrade().is_none() && client_count(&session) == 0
        });
    }
    #[test]
    fn real_tmux_windows_splits_and_process_survive_frontend_drop() {
        let dir = std::env::temp_dir().join(format!("tw-tmux-test-{}", std::process::id()));
        let root = std::env::temp_dir().canonicalize().unwrap();
        let s = Session::prepare(&root, &dir, false).unwrap();
        struct Cleanup(Session, PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
                let _ = std::fs::remove_dir_all(&self.1);
            }
        }
        let _cleanup = Cleanup(s.clone(), dir);
        let first = s.windows().unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].index, 1);
        let target = format!("{}:", s.name);
        let pid = s
            .run(&["display-message", "-p", "-t", &target, "#{pane_pid}"])
            .unwrap();
        let terminal = Terminal::spawn_command(s.attach()).unwrap();
        terminal.input(b"printf 'TMUX-NATIVE-OK\\n'\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let text = terminal
                .snapshot()
                .map(|s| {
                    s.cells
                        .iter()
                        .map(|r| r.iter().map(|c| c.text.clone()).collect::<String>())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if text.contains("TMUX-NATIVE-OK") {
                break;
            }
            assert!(Instant::now() < deadline, "PTY did not render tmux output");
            std::thread::sleep(Duration::from_millis(20));
        }
        terminal.input(b"i=0; while [ $i -lt 150 ]; do printf 'scroll-line-%s\\n' $i; i=$((i+1)); done; printf 'SCROLL-DONE\\n'\r").unwrap();
        let ready = Instant::now() + Duration::from_secs(3);
        while !terminal
            .snapshot()
            .is_some_and(|f| f.text().contains("scroll-line-149"))
        {
            assert!(Instant::now() < ready, "scroll fixture output deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        terminal
            .input(&crate::terminal::mouse_wheel(3., 5, 5, 3))
            .unwrap();
        let ready = Instant::now() + Duration::from_secs(3);
        while s
            .run(&["display-message", "-p", "-t", &target, "#{pane_in_mode}"])
            .unwrap()
            .trim()
            != "1"
        {
            assert!(Instant::now() < ready, "wheel did not enter tmux copy-mode");
            std::thread::sleep(Duration::from_millis(20));
        }
        s.action(Action::SelectAll).unwrap();
        let copied = s.copy_selection().unwrap();
        assert!(copied.contains("scroll-line-149"));
        assert!(
            s.copy_selection().is_err(),
            "must never copy an old buffer without a selection"
        );
        s.action(Action::Find).unwrap();
        terminal.input(b"/").unwrap();
        let ready = Instant::now() + Duration::from_secs(3);
        while !terminal
            .snapshot()
            .is_some_and(|f| f.text().contains("(search down)"))
        {
            assert!(
                Instant::now() < ready,
                "search prompt not rendered: {}",
                terminal.snapshot().map(|s| s.text()).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        terminal.input(b"scroll-line-120\r").unwrap();
        std::thread::sleep(Duration::from_millis(80));
        s.action(Action::Search(false)).unwrap();
        s.action(Action::Search(true)).unwrap();
        s.action(Action::EndSearch).unwrap();
        assert_eq!(
            s.run(&["display-message", "-p", "-t", &target, "#{pane_in_mode}"])
                .unwrap()
                .trim(),
            "0"
        );
        s.action(Action::Find).unwrap();
        terminal.input(b"/").unwrap();
        let ready = Instant::now() + Duration::from_secs(3);
        while !terminal
            .snapshot()
            .is_some_and(|f| f.text().contains("(search down)"))
        {
            assert!(Instant::now() < ready, "empty search prompt deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        s.action(Action::EndSearch).unwrap();
        let ready = Instant::now() + Duration::from_secs(3);
        while terminal
            .snapshot()
            .is_some_and(|f| f.text().contains("(search down)"))
        {
            assert!(
                Instant::now() < ready,
                "empty search prompt was not cancelled"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(terminal);
        assert_eq!(
            s.run(&["display-message", "-p", "-t", &target, "#{pane_pid}"])
                .unwrap(),
            pid
        );
        let second = Session::prepare(&root, &_cleanup.1, false).unwrap();
        assert_eq!(second.name, s.name);
        s.action(Action::New).unwrap();
        assert_eq!(s.windows().unwrap().len(), 2);
        s.action(Action::SelectIndex(1)).unwrap();
        assert!(
            s.windows()
                .unwrap()
                .iter()
                .any(|w| w.index == 1 && w.active)
        );
        s.action(Action::SelectIndex(2)).unwrap();
        assert!(
            s.windows()
                .unwrap()
                .iter()
                .any(|w| w.index == 2 && w.active)
        );
        assert!(s.action(Action::SelectIndex(9)).is_err());
        assert!(
            s.windows()
                .unwrap()
                .iter()
                .any(|w| w.index == 2 && w.active)
        );
        assert!(s.action(Action::SelectIndex(0)).is_err());
        s.run(&["rename-window", "-t", &target, "build test"])
            .unwrap();
        assert!(s.windows().unwrap().iter().any(|w| w.name == "build test"));
        s.action(Action::Split).unwrap();
        assert_eq!(
            s.run(&["list-panes", "-t", &target, "-F", "#{pane_id}"])
                .unwrap()
                .lines()
                .count(),
            2
        );
        let width = s
            .run(&["display-message", "-p", "-t", &target, "#{pane_width}"])
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        s.action(Action::Resize('L', 3)).unwrap();
        let resized = s
            .run(&["display-message", "-p", "-t", &target, "#{pane_width}"])
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        assert!(resized > width);
        s.action(Action::Equalize).unwrap();
        let widths = s
            .run(&["list-panes", "-t", &target, "-F", "#{pane_width}"])
            .unwrap()
            .lines()
            .map(|v| v.parse::<usize>().unwrap())
            .collect::<Vec<_>>();
        assert!(
            widths[0].abs_diff(widths[1]) <= 1,
            "pane widths after equalize: {widths:?}"
        );
        // Unequal four-pane grid: equalize must reach both nested parents
        // and the root divider without replacing the user's split topology.
        s.action(Action::Resize('L', 5)).unwrap();
        s.action(Action::SplitVertical).unwrap();
        s.action(Action::Direction('L')).unwrap();
        s.action(Action::SplitVertical).unwrap();
        s.action(Action::Resize('U', 3)).unwrap();
        s.action(Action::Equalize).unwrap();
        let dimensions = s
            .run(&[
                "list-panes",
                "-t",
                &target,
                "-F",
                "#{pane_width} #{pane_height}",
            ])
            .unwrap();
        let sizes = dimensions
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .map(|v| v.parse::<usize>().unwrap())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(sizes.len(), 4);
        assert!(
            sizes
                .iter()
                .map(|s| s[0])
                .max()
                .unwrap()
                .abs_diff(sizes.iter().map(|s| s[0]).min().unwrap())
                <= 1,
            "grid widths: {sizes:?}"
        );
        assert!(
            sizes
                .iter()
                .map(|s| s[1])
                .max()
                .unwrap()
                .abs_diff(sizes.iter().map(|s| s[1]).min().unwrap())
                <= 1,
            "grid heights: {sizes:?}"
        );
        s.action(Action::Zoom).unwrap();
        assert_eq!(
            s.run(&[
                "display-message",
                "-p",
                "-t",
                &target,
                "#{window_zoomed_flag}"
            ])
            .unwrap()
            .trim(),
            "1"
        );
        let reattached = Terminal::spawn_command(s.attach()).unwrap();
        drop(reattached);
        assert_eq!(s.windows().unwrap().len(), 2);
    }
}
