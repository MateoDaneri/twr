//! Serialized, private recovery snapshots. A single worker orders all writes.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    time::Duration,
};
const MAX_BUFFERS: usize = 128;
const MAX_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct Buffer {
    pub path: PathBuf,
    pub root: PathBuf,
    #[serde(default)]
    pub owners: Vec<PathBuf>,
    pub original: String,
    pub text: String,
    pub cursor: usize,
    #[serde(default)]
    pub unnamed: bool,
}
/// A listed terminal is only a request to verify a private tmux identity at
/// startup; the name is never itself authority to adopt a session.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Terminal {
    pub root: PathBuf,
    pub name: String,
}
/// Keep both in-memory edits and the older recovered text when startup races
/// with an edit of the same file. The synthetic ID never names a real file.
pub fn separate_conflicting_buffer(
    buffer: &mut Buffer,
    conflict: bool,
    occupied: impl Fn(&std::path::Path) -> bool,
) -> bool {
    if !conflict {
        return false;
    }
    loop {
        let id = crate::unnamed::id(&buffer.root);
        if !occupied(&id) {
            buffer.path = id;
            buffer.unnamed = true;
            return true;
        }
    }
}
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct State {
    pub buffers: Vec<Buffer>,
    #[serde(default)]
    pub terminals: Vec<Terminal>,
    #[serde(default)]
    pub theme: crate::appearance::Theme,
    #[serde(default)]
    pub typography: crate::appearance::Typography,
    pub sidebar: Option<workspace_core::agents::ViewState>,
    pub right: bool,
    /// One-time adoption of the compact right sidebar; later explicit placement persists.
    #[serde(default)]
    pub compact_layout: bool,
    pub buffer_bar: bool,
    /// Explicit local metadata opt-in only. Remote destinations are never restored.
    #[serde(default)]
    pub remember_local_agents: bool,
}
impl State {
    pub fn compact_sidebar_right(&self) -> bool {
        !self.compact_layout || self.right
    }
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    schema_version: u32,
    state: State,
}
pub enum Event {
    Loaded(State),
    Written,
    Error(String),
}
enum Command {
    Write(State),
    Flush(State, Sender<Result<(), String>>),
}
pub struct Store {
    tx: SyncSender<Command>,
    pub events: Receiver<Event>,
}
fn validate(state: &State) -> Result<(), String> {
    if state.buffers.len() > MAX_BUFFERS
        || state.buffers.iter().any(|b| {
            !b.root.is_absolute()
                || !b.path.is_absolute()
                || !b.path.starts_with(&b.root)
                || b.owners.len() > MAX_BUFFERS
                || b.owners.iter().any(|owner| !owner.is_absolute())
                || b.text.len() > MAX_BUFFER_BYTES
        })
    {
        return Err("Snapshot de recuperación fuera de límites".into());
    }
    if state.terminals.len() > MAX_BUFFERS
        || state.terminals.iter().any(|terminal| {
            !crate::tmux::valid_additional_terminal_name(&terminal.root, &terminal.name)
        })
        || state
            .terminals
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != state.terminals.len()
    {
        return Err("Terminales de recuperación inválidas o duplicadas".into());
    }
    Ok(())
}
fn write(dir: &std::path::Path, state: &State) -> Result<(), String> {
    // Never replace the last readable snapshot with state our loader rejects.
    // Validate before serializing/cloning buffers or creating a temporary file.
    validate(state)?;
    let bytes = serde_json::to_vec(&Snapshot {
        schema_version: 1,
        state: state.clone(),
    })
    .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err("Recuperación supera 16 MiB; guardá algunos buffers".into());
    }
    let temp = dir.join(format!(
        "recovery-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temp, dir.join("recovery.json")).map_err(|e| e.to_string())?;
        fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn load(dir: &std::path::Path) -> Result<State, String> {
    let path = dir.join("recovery.json");
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(e) => return Err(e.to_string()),
    };
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
        || meta.len() > MAX_SNAPSHOT_BYTES as u64
    {
        return Err("Archivo de recuperación inválido o no privado".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_SNAPSHOT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err("Archivo de recuperación supera 16 MiB".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let state: State = if value.get("schema_version").is_some() {
        let snapshot: Snapshot = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if snapshot.schema_version != 1 {
            return Err(
                "Versión de recuperación desconocida; se conserva el archivo original".into(),
            );
        }
        snapshot.state
    } else {
        // Read the initial unversioned development format without rewriting on load.
        serde_json::from_value(value).map_err(|e| e.to_string())?
    };
    validate(&state)?;
    Ok(state)
}
impl Store {
    pub fn start(dir: PathBuf) -> Self {
        Self::start_notifying(dir, None)
    }
    pub fn start_notifying(dir: PathBuf, wake: Option<async_channel::Sender<()>>) -> Self {
        let (tx, rx) = mpsc::sync_channel(2);
        let (events_tx, events) = crate::events::channel(wake);
        std::thread::spawn(move || {
            let load_error = match load(&dir) {
                Ok(s) => {
                    let _ = events_tx.send(Event::Loaded(s));
                    None
                }
                Err(e) => {
                    let _ = events_tx.send(Event::Error(e.clone()));
                    Some(e)
                }
            };
            while let Ok(command) = rx.recv() {
                let (state, ack) = match command {
                    Command::Write(s) => (s, None),
                    Command::Flush(s, tx) => (s, Some(tx)),
                };
                // Quit also flushes. Preserve an unreadable snapshot on every path,
                // independently of whether the UI has processed the load error.
                let result = match &load_error {
                    Some(e) => Err(format!("Recuperación en solo lectura: {e}")),
                    None => write(&dir, &state),
                };
                if let Some(tx) = ack {
                    let _ = tx.send(result.clone());
                }
                let _ = events_tx.send(match result {
                    Ok(()) => Event::Written,
                    Err(e) => Event::Error(e),
                });
            }
        });
        Self { tx, events }
    }
    pub fn schedule(&self, state: State) -> Result<(), String> {
        self.tx
            .try_send(Command::Write(state))
            .map_err(|e| e.to_string())
    }
    pub fn flush(&self, state: State) -> impl FnOnce() + Send + 'static {
        let tx = self.tx.clone();
        move || {
            let (ack, rx) = mpsc::channel();
            if tx.send(Command::Flush(state, ack)).is_ok() {
                let _ = rx.recv_timeout(Duration::from_secs(3));
            }
        }
    }
    /// A view that has not incorporated the loaded snapshot must never
    /// replace it with its incomplete in-memory state during shutdown.
    pub fn flush_if_loaded(&self, state: Option<State>) -> Option<impl FnOnce() + Send + 'static> {
        state.map(|state| self.flush(state))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_recovery_is_bounded_workspace_scoped_and_backward_compatible() {
        let root = PathBuf::from("/synthetic/workspace");
        let session = crate::tmux::Session {
            binary: "/bin/false".into(),
            socket: "/synthetic/socket".into(),
            name: "synthetic-primary".into(),
            root: root.clone(),
        };
        let terminal = Terminal {
            root: root.clone(),
            name: session.additional_terminal_name().unwrap(),
        };
        let state = State {
            terminals: vec![terminal.clone()],
            ..State::default()
        };
        validate(&state).unwrap();
        let decoded: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(decoded.terminals, vec![terminal.clone()]);
        let mut legacy = serde_json::to_value(State::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("terminals");
        assert!(
            serde_json::from_value::<State>(legacy)
                .unwrap()
                .terminals
                .is_empty()
        );
        assert!(
            validate(&State {
                terminals: vec![terminal.clone(), terminal.clone()],
                ..State::default()
            })
            .is_err()
        );
        let wrong_root = Terminal {
            root: "/synthetic/other".into(),
            ..terminal.clone()
        };
        assert!(
            validate(&State {
                terminals: vec![wrong_root],
                ..State::default()
            })
            .is_err()
        );
        assert!(
            validate(&State {
                terminals: vec![Terminal {
                    root,
                    name: "../escape".into()
                }],
                ..State::default()
            })
            .is_err()
        );
    }
    fn synthetic_buffer() -> Buffer {
        Buffer {
            path: "/explicit/demo/a".into(),
            root: "/explicit/demo".into(),
            owners: Vec::new(),
            original: "saved".into(),
            text: "dirty".into(),
            cursor: 0,
            unnamed: false,
        }
    }
    #[test]
    fn quit_before_load_preserves_previous_snapshot() {
        let dir =
            std::env::temp_dir().join(format!("tw-recovery-early-quit-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let original = State {
            buffers: vec![synthetic_buffer()],
            ..State::default()
        };
        write(&dir, &original).unwrap();
        let store = Store::start(dir.clone());
        assert!(store.flush_if_loaded(None).is_none());
        let event = store.events.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(matches!(event, Event::Loaded(_)));
        assert_eq!(load(&dir).unwrap().buffers[0].text, "dirty");
        store.flush_if_loaded(Some(State::default())).unwrap()();
        assert!(load(&dir).unwrap().buffers.is_empty());
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn startup_conflict_keeps_recovered_content_as_distinct_unnamed_buffer() {
        let mut buffer = synthetic_buffer();
        let original = buffer.path.clone();
        assert!(!separate_conflicting_buffer(&mut buffer, false, |_| false));
        assert_eq!(buffer.path, original);
        assert!(separate_conflicting_buffer(&mut buffer, true, |_| false));
        assert_ne!(buffer.path, original);
        assert!(buffer.path.starts_with(&buffer.root));
        assert!(buffer.unnamed);
        assert_eq!(buffer.text, "dirty");
        assert_eq!(buffer.original, "saved");
        validate(&State {
            buffers: vec![buffer],
            ..State::default()
        })
        .unwrap();
    }
    #[test]
    fn conflicting_live_and_recovered_edits_survive_next_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "tw-recovery-conflict-roundtrip-{}",
            std::process::id()
        ));
        crate::storage::private_directory(&dir).unwrap();
        let mut recovered = synthetic_buffer();
        let mut live = synthetic_buffer();
        live.text = "live edit".into();
        assert!(separate_conflicting_buffer(&mut recovered, true, |path| {
            path == live.path
        }));
        let restored_editor = crate::editor::Editor::from_recovery(
            recovered.original.clone(),
            recovered.text.clone(),
            recovered.cursor,
        );
        let live_editor = crate::editor::Editor::from_recovery(
            live.original.clone(),
            live.text.clone(),
            live.cursor,
        );
        assert!(restored_editor.dirty() && live_editor.dirty());
        recovered.text = restored_editor.contents();
        live.text = live_editor.contents();
        let state = State {
            buffers: vec![live, recovered],
            ..State::default()
        };
        write(&dir, &state).unwrap();
        let saved = load(&dir).unwrap();
        assert_eq!(saved.buffers.len(), 2);
        assert_eq!(saved.buffers[0].text, "live edit");
        assert_eq!(saved.buffers[1].text, "dirty");
        assert!(!saved.buffers[0].unnamed && saved.buffers[1].unnamed);
        assert_ne!(saved.buffers[0].path, saved.buffers[1].path);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn owners_roundtrip_and_legacy_snapshots_default_to_no_owners() {
        let mut buffer = synthetic_buffer();
        buffer.owners = vec!["/workspace/a".into(), "/workspace/b".into()];
        let encoded = serde_json::to_value(&buffer).unwrap();
        let decoded: Buffer = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded.owners, buffer.owners);
        let mut legacy = encoded.as_object().unwrap().clone();
        legacy.remove("owners");
        let decoded: Buffer = serde_json::from_value(legacy.into()).unwrap();
        assert!(decoded.owners.is_empty());
        let mut invalid = State {
            buffers: vec![buffer],
            ..State::default()
        };
        invalid.buffers[0].owners.push("relative".into());
        assert!(validate(&invalid).is_err());
    }
    #[test]
    fn external_file_and_multiple_workspace_owners_survive_disk_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tw-recovery-external-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let buffer = Buffer {
            path: "/synthetic/external/note.txt".into(),
            root: "/synthetic/external".into(),
            owners: vec!["/synthetic/project-a".into(), "/synthetic/project-b".into()],
            original: "disk".into(),
            text: "unsaved".into(),
            cursor: 3,
            unnamed: false,
        };
        write(
            &dir,
            &State {
                buffers: vec![buffer],
                ..State::default()
            },
        )
        .unwrap();
        let restored = load(&dir).unwrap().buffers.remove(0);
        assert_eq!(restored.path, PathBuf::from("/synthetic/external/note.txt"));
        assert_eq!(
            restored.owners,
            vec![
                PathBuf::from("/synthetic/project-a"),
                PathBuf::from("/synthetic/project-b")
            ]
        );
        assert_eq!(restored.text, "unsaved");
        assert_eq!(restored.cursor, 3);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn compact_layout_adopts_right_once_and_preserves_later_placement() {
        let mut state = State::default();
        assert!(state.compact_sidebar_right());
        state.compact_layout = true;
        assert!(!state.compact_sidebar_right());
        state.right = true;
        assert!(state.compact_sidebar_right());
    }
    #[test]
    fn local_agents_opt_in_is_explicit_and_backward_compatible() {
        let old = serde_json::to_value(State::default()).unwrap();
        let mut old = old.as_object().unwrap().clone();
        old.remove("remember_local_agents");
        let restored: State = serde_json::from_value(serde_json::Value::Object(old)).unwrap();
        assert!(!restored.remember_local_agents);
        let enabled = State {
            remember_local_agents: true,
            ..State::default()
        };
        let restored: State =
            serde_json::from_slice(&serde_json::to_vec(&enabled).unwrap()).unwrap();
        assert!(restored.remember_local_agents);
    }
    #[test]
    fn unnamed_snapshot_roundtrip_preserves_marker_without_creating_a_source_file() {
        let dir = std::env::temp_dir().join(format!("tw-unnamed-recovery-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let mut buffer = synthetic_buffer();
        buffer.unnamed = true;
        buffer.path = crate::unnamed::id(&dir);
        buffer.root = dir.clone();
        let path = buffer.path.clone();
        write(
            &dir,
            &State {
                buffers: vec![buffer],
                ..State::default()
            },
        )
        .unwrap();
        let restored = load(&dir).unwrap();
        assert!(restored.buffers[0].unnamed);
        assert!(!path.exists());
        let mut legacy = serde_json::to_value(synthetic_buffer()).unwrap();
        legacy.as_object_mut().unwrap().remove("unnamed");
        assert!(!serde_json::from_value::<Buffer>(legacy).unwrap().unnamed);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rejected_runtime_state_cannot_replace_last_readable_snapshot() {
        let dir =
            std::env::temp_dir().join(format!("tw-recovery-prevalidate-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        write(&dir, &State::default()).unwrap();
        let before = fs::read(dir.join("recovery.json")).unwrap();
        let oversized = State {
            buffers: (0..129)
                .map(|i| Buffer {
                    path: format!("/explicit/demo/{i}.txt").into(),
                    root: "/explicit/demo".into(),
                    owners: Vec::new(),
                    original: "saved".into(),
                    text: "dirty".into(),
                    cursor: 0,
                    unnamed: false,
                })
                .collect(),
            ..State::default()
        };
        let result = write(&dir, &oversized);
        assert!(
            result.is_err(),
            "writer accepted129buffers; next-load rejects={}",
            load(&dir).is_err()
        );
        assert_eq!(fs::read(dir.join("recovery.json")).unwrap(), before);
        assert!(load(&dir).unwrap().buffers.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn all_unreadable_state_shapes_preserve_prior_bytes_and_leave_no_temp_files() {
        let dir = std::env::temp_dir().join(format!("tw-recovery-shapes-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let good = State {
            buffers: vec![synthetic_buffer()],
            ..State::default()
        };
        write(&dir, &good).unwrap();
        let before = fs::read(dir.join("recovery.json")).unwrap();
        let mut cases = Vec::new();
        for (root, path) in [
            ("relative", "/explicit/demo/a"),
            ("/explicit/demo", "relative"),
            ("/explicit/demo", "/other/a"),
        ] {
            let mut bad = good.clone();
            bad.buffers[0].root = root.into();
            bad.buffers[0].path = path.into();
            cases.push(bad);
        }
        let mut bad = good.clone();
        bad.buffers[0].text = "x".repeat(MAX_BUFFER_BYTES + 1);
        cases.push(bad);
        // Text can pass its own budget but escaped JSON still exceed the total.
        let mut bad = good.clone();
        bad.buffers[0].text = "\0".repeat(MAX_BUFFER_BYTES / 2);
        cases.push(bad);
        for state in cases {
            assert!(write(&dir, &state).is_err());
            assert_eq!(fs::read(dir.join("recovery.json")).unwrap(), before);
            assert_eq!(load(&dir).unwrap().buffers[0].text, "dirty");
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn worker_errors_preserve_last_good_and_allow_later_valid_flush() {
        let dir =
            std::env::temp_dir().join(format!("tw-recovery-worker-limits-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let mut good = State {
            buffers: vec![synthetic_buffer()],
            ..State::default()
        };
        write(&dir, &good).unwrap();
        let before = fs::read(dir.join("recovery.json")).unwrap();
        let store = Store::start(dir.clone());
        assert!(matches!(
            store.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Loaded(_)
        ));
        let mut bad = good.clone();
        bad.buffers = vec![synthetic_buffer(); MAX_BUFFERS + 1];
        store.schedule(bad.clone()).unwrap();
        store.flush(bad)();
        for _ in 0..2 {
            assert!(matches!(
                store.events.recv_timeout(Duration::from_secs(2)).unwrap(),
                Event::Error(_)
            ));
        }
        assert_eq!(fs::read(dir.join("recovery.json")).unwrap(), before);
        good.buffers[0].text = "later valid 🦀".into();
        store.flush(good)();
        assert!(matches!(
            store.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            Event::Written
        ));
        assert_eq!(load(&dir).unwrap().buffers[0].text, "later valid 🦀");
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn exact_buffer_count_boundary_still_roundtrips() {
        let dir = std::env::temp_dir().join(format!("tw-recovery-boundary-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let state = State {
            buffers: vec![synthetic_buffer(); MAX_BUFFERS],
            ..State::default()
        };
        write(&dir, &state).unwrap();
        assert_eq!(load(&dir).unwrap().buffers.len(), MAX_BUFFERS);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn recovery_roundtrip_confidential_mode_and_flush_order() {
        let dir = std::env::temp_dir().join(format!("tw-recovery-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let mut state = State::default();
        state.buffers.push(Buffer {
            path: "/explicit/demo/a".into(),
            root: "/explicit/demo".into(),
            owners: Vec::new(),
            original: "old".into(),
            text: "dirty 🦀".into(),
            cursor: 3,
            unnamed: false,
        });
        let store = Store::start(dir.clone());
        state.theme = crate::appearance::Theme::NightOwl;
        state.typography = state
            .typography
            .apply("jetbrains-mono")
            .unwrap()
            .apply("18")
            .unwrap();
        store.schedule(state.clone()).unwrap();
        store.flush(state.clone())();
        assert_eq!(load(&dir).unwrap().buffers[0].text, "dirty 🦀");
        assert_eq!(
            load(&dir).unwrap().theme,
            crate::appearance::Theme::NightOwl
        );
        store.flush(State::default())();
        assert!(load(&dir).unwrap().buffers.is_empty());
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn unknown_or_corrupt_snapshots_survive_scheduled_and_quit_writes() {
        let dir = std::env::temp_dir().join(format!("tw-recovery-invalid-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let path = dir.join("recovery.json");
        for original in [br#"{"schema_version":2,"state":{"buffers":[],"sidebar":null,"right":false,"buffer_bar":false}}"#.as_slice(), b"{broken"] {
            let mut file = fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&path).unwrap();
            file.write_all(original).unwrap();
            drop(file);
            let store = Store::start(dir.clone());
            assert!(matches!(store.events.recv_timeout(Duration::from_secs(2)).unwrap(), Event::Error(_)));
            store.schedule(State::default()).unwrap();
            store.flush(State::default())();
            assert_eq!(fs::read(&path).unwrap(), original);
            drop(store);
        }
        // Snapshots from before appearance preferences still load with system theme.
        fs::write(&path, br#"{"schema_version":1,"state":{"buffers":[],"sidebar":null,"right":false,"buffer_bar":false}}"#).unwrap();
        assert_eq!(load(&dir).unwrap().theme, crate::appearance::Theme::System);
        // The prior development format remains readable and migrates on next write.
        fs::write(&path, serde_json::to_vec(&State::default()).unwrap()).unwrap();
        assert!(load(&dir).unwrap().buffers.is_empty());
        write(&dir, &State::default()).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["schema_version"], 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
