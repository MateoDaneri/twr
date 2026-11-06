//! Opt-in read-only Codex metadata transport. No turns, messages, approvals or tools.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};
use workspace_core::{
    agents::Agent,
    codex::{self, JsonLines, StatusOverrides},
};
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub project: Option<String>,
}
pub enum Event {
    Connected,
    Snapshot(Vec<Agent>, BTreeMap<String, Metadata>),
    Error(String),
    Disconnected,
    Refreshed,
}
pub struct Provider {
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
    refresh: Arc<AtomicBool>,
}
/// SSH destination and Codex app identity are distinct; never infer one from the other.
#[derive(Clone, Debug)]
pub struct Remote {
    ssh_host: String,
    codex_host_id: String,
}
impl Remote {
    pub fn new(ssh_host: &str, codex_host_id: &str) -> Result<Self, String> {
        if ssh_host.is_empty()
            || ssh_host.len() > 200
            || matches!(ssh_host, "local" | "all")
            || ssh_host.starts_with('-')
            || !ssh_host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_@".contains(&b))
        {
            return Err("Host SSH inválido".into());
        }
        if codex_host_id.is_empty()
            || codex_host_id.len() > 256
            || codex_host_id == "local"
            || codex_host_id
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err("Indicá el ID remoto de Codex; no puede ser local".into());
        }
        Ok(Self {
            ssh_host: ssh_host.into(),
            codex_host_id: codex_host_id.into(),
        })
    }
}
/// Recovery may finish after a user clicks. A current choice wins over saved state.
#[derive(Default)]
pub struct LocalPreference {
    remembered: bool,
    chosen: bool,
}
impl LocalPreference {
    pub fn remembered(&self) -> bool {
        self.remembered
    }
    pub fn choose_connection(&mut self) {
        self.chosen = true;
    }
    pub fn toggle(&mut self) {
        self.chosen = true;
        self.remembered = !self.remembered;
    }
    pub fn forget(&mut self) {
        self.chosen = true;
        self.remembered = false;
    }
    /// True only when restoring the explicit local opt-in should connect now.
    pub fn restore(&mut self, saved: bool) -> bool {
        if self.chosen {
            return false;
        }
        self.remembered = saved;
        saved
    }
}
#[derive(Default)]
pub struct Registry {
    slots: BTreeMap<String, Slot>,
    wake: Option<async_channel::Sender<()>>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    Connecting,
    Loading,
    Ready,
    Disconnected,
}
struct Slot {
    provider: Option<Provider>,
    agents: Vec<Agent>,
    metadata: BTreeMap<String, Metadata>,
    connection: Connection,
    error: Option<String>,
    refreshed: Option<Instant>,
}
impl Registry {
    pub fn notifying(wake: async_channel::Sender<()>) -> Self {
        Self {
            slots: BTreeMap::new(),
            wake: Some(wake),
        }
    }
    pub fn connected(&self, key: &str) -> bool {
        self.slots.get(key).is_some_and(|s| s.provider.is_some())
    }
    pub fn count(&self) -> usize {
        self.slots.values().filter(|s| s.provider.is_some()).count()
    }
    fn insert(&mut self, key: String, provider: Provider) -> Result<(), String> {
        if self.connected(&key) {
            return Err("Proveedor ya conectado".into());
        }
        if self.slots.len() >= 8 && !self.slots.contains_key(&key) {
            return Err("Límite de ocho proveedores por ejecución".into());
        }
        if let Some(slot) = self.slots.get_mut(&key) {
            slot.provider = Some(provider);
            slot.connection = Connection::Connecting;
            slot.error = None;
        } else {
            self.slots.insert(
                key,
                Slot {
                    provider: Some(provider),
                    agents: Vec::new(),
                    metadata: BTreeMap::new(),
                    connection: Connection::Connecting,
                    error: None,
                    refreshed: None,
                },
            );
        }
        Ok(())
    }
    pub fn connect_local(&mut self, validation: bool) -> Result<(), String> {
        if self.connected("local") {
            return Err("Proveedor local ya conectado".into());
        }
        if self.slots.len() >= 8 && !self.slots.contains_key("local") {
            return Err("Límite de ocho proveedores por ejecución".into());
        }
        let provider = if validation {
            Provider::fixture_notifying(None, self.wake.clone())?
        } else {
            match Provider::local_notifying(self.wake.clone()) {
                Ok(provider) => provider,
                Err(error) => {
                    self.start_failed("local", &error);
                    return Err(error);
                }
            }
        };
        self.insert("local".into(), provider)
    }
    fn start_failed(&mut self, key: &str, error: &str) {
        let slot = self.slots.entry(key.into()).or_insert_with(|| Slot {
            provider: None,
            agents: Vec::new(),
            metadata: BTreeMap::new(),
            connection: Connection::Disconnected,
            error: None,
            refreshed: None,
        });
        Self::stop(slot);
        slot.error = Some(error.into());
    }
    pub fn connect_remote(&mut self, remote: Remote, validation: bool) -> Result<(), String> {
        if self.connected(&remote.ssh_host) {
            return Err("Host ya conectado".into());
        }
        if self.slots.len() >= 8 && !self.slots.contains_key(&remote.ssh_host) {
            return Err("Límite de ocho proveedores".into());
        }
        let provider = if validation {
            if remote.ssh_host != "fixture" {
                return Err("Validación: solo host fixture, sin red".into());
            }
            Provider::fixture_notifying(Some(remote.clone()), self.wake.clone())?
        } else {
            Provider::remote_notifying(&remote, self.wake.clone())?
        };
        self.insert(remote.ssh_host, provider)
    }
    pub fn disconnect(&mut self, key: &str) -> Result<(), String> {
        if key == "all" {
            for slot in self.slots.values_mut() {
                Self::stop(slot);
            }
        } else {
            let slot = self.slots.get_mut(key).ok_or("Proveedor no registrado")?;
            Self::stop(slot);
        }
        Ok(())
    }
    fn stop(slot: &mut Slot) {
        slot.provider = None;
        slot.connection = Connection::Disconnected;
        for agent in &mut slot.agents {
            agent.connection_state = Some("disconnected".into());
            agent.coordination_waiting = false;
            agent.coordination_targets = 0;
        }
    }
    pub fn poll(&mut self) -> (bool, Vec<String>) {
        let mut changed = false;
        let mut messages = Vec::new();
        for (key, slot) in &mut self.slots {
            let mut closed = false;
            let mut events = Vec::new();
            if let Some(provider) = &slot.provider {
                loop {
                    match provider.events.try_recv() {
                        Ok(event) => events.push(event),
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            closed = true;
                            break;
                        }
                    }
                }
            }
            for event in events {
                changed = true;
                match event {
                    Event::Connected => {
                        slot.connection = Connection::Loading;
                        messages.push(format!("Agents {key} conectado · solo metadata"))
                    }
                    Event::Snapshot(agents, metadata) => {
                        slot.agents = agents;
                        slot.metadata = metadata;
                    }
                    Event::Refreshed => {
                        slot.connection = Connection::Ready;
                        slot.error = None;
                        slot.refreshed = Some(Instant::now());
                    }
                    Event::Error(error) => {
                        slot.error = Some(error.clone());
                        messages.push(format!("Agents {key}: {error}"));
                    }
                    Event::Disconnected => {
                        Self::stop(slot);
                        messages.push(format!("Agents {key} desconectado · estado conservado"));
                    }
                }
            }
            if closed && slot.provider.is_some() {
                Self::stop(slot);
                changed = true;
                messages.push(format!("Agents {key} desconectado · estado conservado"));
            }
        }
        (changed, messages)
    }
    /// Queue one refresh without overlapping an in-flight metadata page sequence.
    pub fn refresh(&self) {
        for slot in self.slots.values() {
            if let Some(provider) = &slot.provider {
                provider.refresh.store(true, Ordering::Release);
            }
        }
    }
    /// Persistent UI status. Starting a process does not prove a usable connection.
    pub fn status_lines(&self) -> Vec<String> {
        if self.slots.is_empty() {
            return vec!["Local · desconectado. Conectá para ver tus tareas de Codex.".into()];
        }
        self.slots
            .iter()
            .map(|(key, slot)| {
                let state = match slot.connection {
                    Connection::Connecting => "conectando",
                    Connection::Loading => "consultando metadata",
                    Connection::Ready => "conectado",
                    Connection::Disconnected => "desconectado",
                };
                let mut text = format!("{key} · {state} · {} tareas", slot.agents.len());
                if let Some(time) = slot.refreshed {
                    text.push_str(&format!(
                        " · última consulta hace {} s",
                        time.elapsed().as_secs()
                    ));
                }
                if let Some(error) = &slot.error {
                    text.push_str(&format!(" · {error}"));
                }
                text
            })
            .collect()
    }
    pub fn ready_count(&self) -> usize {
        self.slots
            .values()
            .filter(|s| s.connection == Connection::Ready && s.error.is_none())
            .count()
    }
    pub fn snapshot(&self) -> (Vec<Agent>, BTreeMap<String, Metadata>) {
        (
            self.slots
                .values()
                .flat_map(|s| s.agents.iter().cloned())
                .collect(),
            self.slots
                .values()
                .flat_map(|s| s.metadata.iter().map(|(k, v)| (k.clone(), v.clone())))
                .collect(),
        )
    }
}

pub fn task_url(task: &str, host: Option<&str>) -> Result<String, String> {
    if task.is_empty() || task.len() > 256 || task.chars().any(char::is_control) {
        return Err("Invalid task identity".into());
    }
    fn encode(s: &str) -> String {
        s.as_bytes()
            .iter()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    (*b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }
    Ok(format!(
        "codex://threads/{}{}",
        encode(task),
        host.map(|h| format!("?hostId={}", encode(h)))
            .unwrap_or_default()
    ))
}
enum Pending {
    Initialize,
    Threads,
    Projects,
}
#[derive(Clone, Debug)]
struct CodexProject {
    name: String,
    position: i64,
    root: Option<std::path::PathBuf>,
}
struct Protocol {
    next: u64,
    pending: BTreeMap<u64, Pending>,
    threads: Vec<Value>,
    pages: Vec<Value>,
    cursors: std::collections::BTreeSet<String>,
    projects: BTreeMap<String, CodexProject>,
    project_pages: BTreeMap<String, CodexProject>,
    project_cursors: std::collections::BTreeSet<String>,
    status: StatusOverrides,
    coordination: workspace_core::coordination::Tracker,
    host: Option<String>,
    codex_host: Option<String>,
    generation: u64,
    refresh_failed: bool,
}
fn metadata_page(result: &Value) -> Option<&[Value]> {
    let data = result.get("data")?.as_array()?;
    data.iter().all(Value::is_object).then_some(data.as_slice())
}
impl Protocol {
    fn project_for_thread<'a>(
        &'a self,
        thread: &Value,
        visited: &mut std::collections::BTreeSet<String>,
    ) -> Option<(&'a str, &'a CodexProject)> {
        let id = thread["id"].as_str()?;
        if !visited.insert(id.to_owned()) || visited.len() > 64 {
            return None;
        }
        if let Some(project_id) = thread["projectId"].as_str()
            && let Some((key, project)) = self.projects.get_key_value(project_id)
        {
            return Some((key.as_str(), project));
        }
        if let Some(cwd) = thread["cwd"].as_str() {
            let path = std::path::Path::new(cwd);
            if path.is_absolute()
                && !path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                if let Some((key, project)) = self
                    .projects
                    .iter()
                    .filter(|(_, project)| {
                        project
                            .root
                            .as_ref()
                            .is_some_and(|root| path.starts_with(root))
                    })
                    .max_by(|(ka, a), (kb, b)| {
                        a.root
                            .as_ref()
                            .unwrap()
                            .components()
                            .count()
                            .cmp(&b.root.as_ref().unwrap().components().count())
                            .then_with(|| b.position.cmp(&a.position))
                            .then_with(|| kb.cmp(ka))
                    })
                {
                    return Some((key.as_str(), project));
                }
            }
        }
        let parent = codex::parent_id(thread)
            .or_else(|| self.coordination.creator(id).map(str::to_owned))?;
        let parent_thread = self
            .threads
            .iter()
            .find(|candidate| candidate["id"].as_str() == Some(parent.as_str()))?;
        self.project_for_thread(parent_thread, visited)
    }
    fn new(host: Option<String>) -> Self {
        let mut pending = BTreeMap::new();
        pending.insert(1, Pending::Initialize);
        Self {
            next: 2,
            pending,
            threads: Vec::new(),
            pages: Vec::new(),
            cursors: Default::default(),
            projects: BTreeMap::new(),
            project_pages: BTreeMap::new(),
            project_cursors: Default::default(),
            status: Default::default(),
            coordination: Default::default(),
            host,
            codex_host: None,
            generation: 0,
            refresh_failed: false,
        }
    }
    fn snapshot(&self) -> (Vec<Agent>, BTreeMap<String, Metadata>) {
        let mut details = BTreeMap::new();
        let agents = self
            .threads
            .iter()
            .filter_map(|t| {
                let mut a = codex::normalize_thread(
                    t,
                    self.status.get(t["id"].as_str()?),
                    self.host.as_deref(),
                )?;
                a.codex_host_id = self.codex_host.clone();
                let state = self.coordination.snapshot(t["id"].as_str()?);
                a.coordination_waiting = state.waiting;
                a.coordination_created = state.created;
                a.coordination_targets = state.targets;
                if a.parent_id.is_none()
                    && let Some(parent) = self.coordination.creator(t["id"].as_str()?)
                    && let Some(parent) = self.threads.iter().find(|thread| thread["id"] == parent)
                    && let Some(parent) =
                        codex::normalize_thread(parent, None, self.host.as_deref())
                {
                    a.parent_id = Some(parent.id);
                }

                a.label = t["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Sin título")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(512)
                    .collect();
                if let Some((id, project)) = self.project_for_thread(t, &mut Default::default()) {
                    a.codex_project_id = Some(id.into());
                    a.codex_project_name = Some(project.name.clone());
                    a.codex_project_position = Some(project.position);
                } else if let Some(id) = t["projectId"].as_str() {
                    a.codex_project_id = Some(id.into());
                }
                details.insert(
                    a.id.clone(),
                    Metadata {
                        cwd: t["cwd"].as_str().map(str::to_owned),
                        branch: t["gitInfo"]["branch"].as_str().map(str::to_owned),
                        project: a.codex_project_name.clone(),
                    },
                );
                Some(a)
            })
            .collect();
        (agents, details)
    }
    fn refresh(&mut self) -> Vec<Value> {
        if !self.pending.is_empty() {
            return Vec::new();
        }
        self.pages.clear();
        self.cursors.clear();
        self.project_pages.clear();
        self.project_cursors.clear();
        self.refresh_failed = false;
        let t = self.next;
        self.next += 1;
        self.pending.insert(t, Pending::Threads);
        let p = self.next;
        self.next += 1;
        self.pending.insert(p, Pending::Projects);
        vec![codex::thread_list(t), codex::project_list(p)]
    }
    fn receive(&mut self, m: Value) -> (Vec<Value>, bool, Option<String>) {
        // Server-initiated requests never grant permission in this metadata client.
        if m.get("method").is_some() && m.get("id").is_some() {
            return (
                vec![
                    json!({"id":m["id"],"error":{"code":-32601,"message":"Read-only overview client"}}),
                ],
                false,
                None,
            );
        }
        let coordination = self.coordination.observe(&m);
        let status = self.status.observe(&m);
        match coordination {
            Err(error) => return (Vec::new(), status, Some(error)),
            Ok(changed) if changed || status => return (Vec::new(), true, None),
            _ => {}
        }
        let Some(id) = m["id"].as_u64() else {
            return (Vec::new(), false, None);
        };
        let Some(kind) = self.pending.remove(&id) else {
            return (Vec::new(), false, None);
        };
        if m.get("error").is_some() {
            return (
                Vec::new(),
                false,
                Some(
                    "Codex rechazó una consulta de metadata; revisar compatibilidad del CLI".into(),
                ),
            );
        }
        let r = &m["result"];
        match kind {
            Pending::Initialize => {
                let mut out = vec![codex::initialized()];
                out.extend(self.refresh());
                (out, false, None)
            }
            Pending::Threads => {
                let data = metadata_page(r);
                if data.is_none()
                    || r.get("nextCursor")
                        .is_some_and(|v| !v.is_null() && !v.is_string())
                {
                    self.pages.clear();
                    self.cursors.clear();
                    return (
                        Vec::new(),
                        false,
                        Some("Página de tareas inválida; se conserva la metadata anterior".into()),
                    );
                }
                if let Some(data) = data {
                    for item in data
                        .iter()
                        .take(2048_usize.saturating_sub(self.pages.len()))
                    {
                        let mut clean = serde_json::Map::new();
                        for key in [
                            "id",
                            "name",
                            "status",
                            "source",
                            "threadSource",
                            "ephemeral",
                            "parentThreadId",
                            "updatedAt",
                            "recencyAt",
                            "projectId",
                            "gitInfo",
                            "cwd",
                        ] {
                            if let Some(value) = item.get(key) {
                                clean.insert(key.into(), value.clone());
                            }
                        }
                        self.pages.push(Value::Object(clean));
                    }
                }
                if let Some(cursor) = r["nextCursor"].as_str().filter(|_| self.pages.len() < 2048) {
                    if self.cursors.len() >= 100 || !self.cursors.insert(cursor.into()) {
                        self.pages.clear();
                        return (
                            Vec::new(),
                            false,
                            Some("Paginación Codex repetida o excesiva".into()),
                        );
                    }
                    let id = self.next;
                    self.next += 1;
                    self.pending.insert(id, Pending::Threads);
                    let mut request = codex::thread_list(id);
                    request["params"]["cursor"] = cursor.into();
                    (vec![request], false, None)
                } else {
                    self.threads = std::mem::take(&mut self.pages);
                    self.generation += 1;
                    self.coordination.retain(
                        &self
                            .threads
                            .iter()
                            .filter_map(|t| t["id"].as_str().map(str::to_owned))
                            .collect(),
                    );
                    (Vec::new(), true, None)
                }
            }
            Pending::Projects => {
                let Some(data) = metadata_page(r).filter(|_| {
                    r.get("nextCursor")
                        .is_none_or(|v| v.is_null() || v.is_string())
                }) else {
                    self.project_pages.clear();
                    self.project_cursors.clear();
                    return (
                        Vec::new(),
                        false,
                        Some(
                            "Página de proyectos inválida; se conserva la metadata anterior".into(),
                        ),
                    );
                };
                {
                    for p in data
                        .iter()
                        .take(1000_usize.saturating_sub(self.project_pages.len()))
                    {
                        if let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) {
                            let root = p["roots"]
                                .as_array()
                                .and_then(|roots| roots.first())
                                .and_then(|root| root["path"].as_str())
                                .map(std::path::PathBuf::from)
                                .filter(|path| {
                                    path.is_absolute()
                                        && !path
                                            .components()
                                            .any(|c| matches!(c, std::path::Component::ParentDir))
                                });
                            self.project_pages.insert(
                                id.into(),
                                CodexProject {
                                    name: name
                                        .chars()
                                        .filter(|c| !c.is_control())
                                        .take(512)
                                        .collect(),
                                    position: p["position"].as_i64().unwrap_or_default(),
                                    root,
                                },
                            );
                        }
                    }
                }
                if let Some(cursor) = r["nextCursor"]
                    .as_str()
                    .filter(|_| self.project_pages.len() < 1000)
                {
                    if self.project_cursors.len() >= 100
                        || !self.project_cursors.insert(cursor.into())
                    {
                        self.project_pages.clear();
                        self.project_cursors.clear();
                        return (
                            Vec::new(),
                            false,
                            Some("Paginación de proyectos repetida o excesiva".into()),
                        );
                    }
                    let id = self.next;
                    self.next += 1;
                    self.pending.insert(id, Pending::Projects);
                    let mut request = codex::project_list(id);
                    request["params"]["cursor"] = cursor.into();
                    return (vec![request], false, None);
                }
                self.projects = std::mem::take(&mut self.project_pages);
                self.project_cursors.clear();
                (Vec::new(), true, None)
            }
        }
    }
}
// Finder/LaunchServices may supply only system paths. The npm Codex launcher
// uses /usr/bin/env node, so an absolute Codex path alone is insufficient.
// Do not evaluate shell startup files or inherit a project-controlled PATH.
const LOCAL_CLI_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
fn local_cli_command(binary: &std::path::Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env("PATH", LOCAL_CLI_PATH)
        .env_remove("CODEX_THREAD_ID");
    command
}

impl Provider {
    pub fn fixture() -> Result<Self, String> {
        Self::fixture_at(None)
    }
    fn fixture_at(remote: Option<Remote>) -> Result<Self, String> {
        Self::fixture_notifying(remote, None)
    }
    fn fixture_notifying(
        remote: Option<Remote>,
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        let bundled = std::env::current_exe().ok().and_then(|p| {
            p.parent()?
                .parent()
                .map(|p| p.join("Resources/Helpers/fake_app_server.py"))
        });
        let bundled = bundled.filter(|p| p.is_file());
        let overview_fixture = bundled.is_some();
        let helper = bundled.unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../Resources/Helpers/fake_app_server.py")
        });
        let mut c = Command::new("/usr/bin/python3");
        c.arg(helper);
        if overview_fixture {
            c.arg("--overview");
        }
        Self::start_notifying(
            c,
            remote.as_ref().map(|r| r.ssh_host.clone()),
            remote.map(|r| r.codex_host_id),
            wake,
        )
    }
    pub fn local() -> Result<Self, String> {
        Self::local_notifying(None)
    }
    fn local_notifying(wake: Option<async_channel::Sender<()>>) -> Result<Self, String> {
        let binary = ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"]
            .iter()
            .find(|p| std::path::Path::new(p).is_file())
            .ok_or("Codex CLI no disponible")?;
        let mut c = local_cli_command(std::path::Path::new(binary));
        c.args(["app-server", "--stdio"]);
        Self::start_notifying(c, None, None, wake)
    }
    pub fn remote(remote: &Remote) -> Result<Self, String> {
        Self::remote_notifying(remote, None)
    }
    fn remote_notifying(
        remote: &Remote,
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        let mut c = Command::new("/usr/bin/ssh");
        c.args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "--",
            &remote.ssh_host,
            "codex app-server --stdio",
        ]);
        Self::start_notifying(
            c,
            Some(remote.ssh_host.clone()),
            Some(remote.codex_host_id.clone()),
            wake,
        )
    }
    fn start_notifying(
        mut command: Command,
        host: Option<String>,
        codex_host: Option<String>,
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let mut error = child.stderr.take().unwrap();
        let (events_tx, events) = crate::events::sync_channel(8, wake);
        let (tx, rx) = mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let refresh = Arc::new(AtomicBool::new(false));
        let requested = refresh.clone();
        std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            while let Ok(n) = output.read(&mut bytes) {
                if n == 0 || tx.send(bytes[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(n) = error.read(&mut bytes) {
                if n == 0 {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut protocol = Protocol::new(host);
            protocol.codex_host = codex_host;
            let mut decoder = JsonLines::default();
            let mut ready = false;
            let mut published_generation = 0;
            let mut last = Instant::now();
            let mut response_deadline = Instant::now() + Duration::from_secs(10);
            fn send(input: &mut impl Write, v: &Value) -> Result<(), String> {
                let mut bytes = serde_json::to_vec(v).map_err(|e| e.to_string())?;
                bytes.push(b'\n');
                input
                    .write_all(&bytes)
                    .and_then(|_| input.flush())
                    .map_err(|e| e.to_string())
            }
            if send(&mut input, &codex::initialize(1)).is_err() {
                let _ = events_tx.try_send(Event::Error(
                    "No se pudo iniciar la consulta de metadata Codex".into(),
                ));
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            while !cancelled.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_millis(25)) {
                    Ok(bytes) => match decoder.push(&bytes) {
                        Ok(messages) => {
                            for message in messages {
                                let init = message["id"] == 1 && message.get("result").is_some();
                                let rejected_init =
                                    message["id"] == 1 && message.get("error").is_some();
                                let (out, changed, error) = protocol.receive(message);
                                if init {
                                    ready = true;
                                    let _ = events_tx.try_send(Event::Connected);
                                }
                                if let Some(error) = error {
                                    protocol.refresh_failed = true;
                                    let _ = events_tx.try_send(Event::Error(error));
                                }
                                if rejected_init {
                                    cancelled.store(true, Ordering::Release);
                                    break;
                                }
                                for request in out {
                                    if send(&mut input, &request).is_err() {
                                        cancelled.store(true, Ordering::Release);
                                        break;
                                    }
                                }
                                if changed && protocol.generation > 0 {
                                    let (a, m) = protocol.snapshot();
                                    let _ = events_tx.try_send(Event::Snapshot(a, m));
                                }
                                if protocol.pending.is_empty()
                                    && !protocol.refresh_failed
                                    && protocol.generation > published_generation
                                {
                                    published_generation = protocol.generation;
                                    let _ = events_tx.try_send(Event::Refreshed);
                                }
                            }
                        }
                        Err(e) => {
                            let _ = events_tx.try_send(Event::Error(e));
                            break;
                        }
                    },
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = events_tx.try_send(Event::Error(
                            "Codex cerró el transporte de metadata. Comprobá la CLI y reintentá Conectar local.".into()
                        ));
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if !protocol.pending.is_empty() && Instant::now() > response_deadline {
                    let _ = events_tx.try_send(Event::Error("Timeout de metadata Codex".into()));
                    break;
                }
                if ready
                    && protocol.pending.is_empty()
                    && (requested.swap(false, Ordering::AcqRel)
                        || last.elapsed() >= Duration::from_secs(3))
                {
                    for request in protocol.refresh() {
                        let _ = send(&mut input, &request);
                    }
                    last = Instant::now();
                    response_deadline = last + Duration::from_secs(10);
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = events_tx.try_send(Event::Disconnected);
        });
        Ok(Self {
            events,
            stop,
            refresh,
        })
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finder_path_can_resolve_the_npm_launcher_runtime_without_reading_metadata() {
        // Exercise the actual shebang boundary with a private fixture, not a
        // Codex app-server or the user's task/configuration data.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tw-cli-path-{}", std::process::id()));
        crate::storage::private_directory(&dir).unwrap();
        let fixture = dir.join("launcher");
        std::fs::write(
            &fixture,
            "#!/usr/bin/env node\nprocess.stdout.write('runtime-ok');\n",
        )
        .unwrap();
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
        if !std::path::Path::new("/opt/homebrew/bin/node").is_file()
            && !std::path::Path::new("/usr/local/bin/node").is_file()
        {
            std::fs::remove_dir_all(dir).unwrap();
            return;
        }
        let missing = Command::new(&fixture)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .output()
            .unwrap();
        assert!(!missing.status.success());
        let result = local_cli_command(&fixture).output().unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"runtime-ok");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn delayed_recovery_cannot_override_a_current_connection_choice() {
        let mut untouched = LocalPreference::default();
        assert!(!untouched.restore(false));
        assert!(untouched.restore(true));
        assert!(untouched.remembered());
        let mut disconnected = LocalPreference::default();
        disconnected.forget();
        assert!(!disconnected.restore(true));
        assert!(!disconnected.remembered());
        let mut manual_connection = LocalPreference::default();
        manual_connection.choose_connection();
        assert!(!manual_connection.restore(true));
        assert!(!manual_connection.remembered());
        let mut toggle_before_load = LocalPreference::default();
        toggle_before_load.toggle();
        assert!(!toggle_before_load.restore(false));
        assert!(toggle_before_load.remembered());
        toggle_before_load.toggle();
        assert!(!toggle_before_load.restore(true));
        assert!(!toggle_before_load.remembered());
    }
    #[test]
    fn connection_state_requires_metadata_and_retains_errors_after_disconnect() {
        let mut registry = Registry::default();
        assert!(registry.status_lines()[0].contains("desconectado"));
        let (tx, rx) = mpsc::channel();
        let refresh = Arc::new(AtomicBool::new(false));
        registry
            .insert(
                "local".into(),
                Provider {
                    events: rx,
                    stop: Arc::new(AtomicBool::new(false)),
                    refresh: refresh.clone(),
                },
            )
            .unwrap();
        assert_eq!(registry.ready_count(), 0);
        assert!(registry.status_lines()[0].contains("conectando"));
        tx.send(Event::Connected).unwrap();
        registry.poll();
        assert_eq!(registry.ready_count(), 0);
        assert!(registry.status_lines()[0].contains("consultando metadata"));
        tx.send(Event::Snapshot(
            vec![Agent {
                id: "synthetic".into(),
                ..Default::default()
            }],
            BTreeMap::new(),
        ))
        .unwrap();
        tx.send(Event::Refreshed).unwrap();
        registry.poll();
        assert_eq!(registry.ready_count(), 1);
        registry.refresh();
        assert!(refresh.load(Ordering::Acquire));
        tx.send(Event::Error("fallo sintético".into())).unwrap();
        tx.send(Event::Disconnected).unwrap();
        registry.poll();
        assert_eq!(registry.ready_count(), 0);
        assert_eq!(registry.snapshot().0.len(), 1);
        assert!(registry.status_lines()[0].contains("fallo sintético"));
        assert!(registry.status_lines()[0].contains("desconectado"));
        registry.start_failed("local", "CLI no disponible");
        assert_eq!(registry.snapshot().0.len(), 1);
        assert!(registry.status_lines()[0].contains("CLI no disponible"));
        let (_tx, rx) = mpsc::channel();
        registry
            .insert(
                "local".into(),
                Provider {
                    events: rx,
                    stop: Arc::new(AtomicBool::new(false)),
                    refresh: Arc::new(AtomicBool::new(false)),
                },
            )
            .unwrap();
        assert_eq!(
            registry.snapshot().0.len(),
            1,
            "Retry cleared last metadata before a valid page"
        );
        assert_eq!(registry.ready_count(), 0);
        assert!(registry.status_lines()[0].contains("conectando"));
    }
    #[test]
    fn rejected_initialize_exits_instead_of_waiting_with_no_pending_request() {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-u", "-c", "import json,sys,time; m=json.loads(sys.stdin.readline()); print(json.dumps({'id':m['id'],'error':{'code':-1,'message':'synthetic rejection'}}),flush=True); time.sleep(2)"]);
        let provider = Provider::start_notifying(command, None, None, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut error = false;
        loop {
            match provider.events.recv_timeout(Duration::from_millis(50)) {
                Ok(Event::Error(_)) => error = true,
                Ok(Event::Connected | Event::Refreshed) => {
                    panic!("Rejected initialization looked connected")
                }
                Ok(Event::Disconnected) => {
                    assert!(error);
                    break;
                }
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "Rejected init left transport alive"
            );
        }
    }
    #[test]
    fn actual_stdio_transport_receives_synthetic_pages_and_shuts_down() {
        let provider = Provider::fixture().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match provider.events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Snapshot(agents, _))
                    if agents.len() == 2 && agents.iter().any(|a| a.coordination_waiting) =>
                {
                    assert!(agents.iter().any(|a| a.parent_id.is_some()));
                    break;
                }
                Ok(Event::Error(e)) => panic!("{e}"),
                _ => {}
            }
            assert!(Instant::now() < deadline, "fixture transport timed out");
        }
        drop(provider);
    }
    #[test]
    fn malformed_thread_refresh_does_not_erase_last_published_agents() {
        let mut p = Protocol::new(None);
        p.receive(json!({"id":1,"result":{}}));
        p.receive(json!({"id":2,"result":{"data":[{"id":"old","name":"Last good","status":{"type":"idle"}}],"nextCursor":null}}));
        p.receive(json!({"id":3,"result":{"data":[]}}));
        let requests = p.refresh();
        let id = requests
            .iter()
            .find(|r| r["method"] == "thread/list")
            .unwrap()["id"]
            .clone();
        let (_, changed, error) = p.receive(json!({"id":id,"result":{}}));
        assert_eq!(
            p.snapshot().0.len(),
            1,
            "malformed refresh erased last-good agents"
        );
        assert!(!changed);
        assert!(error.is_some());
    }
    fn populated_protocol() -> Protocol {
        let mut p = Protocol::new(None);
        p.receive(json!({"id":1,"result":{}}));
        p.receive(json!({"id":2,"result":{"data":[{"id":"old","name":"Last good","projectId":"project-old","status":{"type":"idle"}}],"nextCursor":null}}));
        p.receive(json!({"id":3,"result":{"data":[{"id":"project-old","name":"Last project","position":1}]}}));
        p
    }
    #[test]
    fn groups_tasks_by_explicit_project_then_longest_root_then_parent() {
        let mut p = Protocol::new(None);
        p.receive(json!({"id":1,"result":{}}));
        p.receive(json!({"id":2,"result":{"data":[
            {"id":"root","name":"Root task","cwd":"/synthetic/a/file","status":{"type":"idle"}},
            {"id":"nested","name":"Nested task","cwd":"/synthetic/a/sub/.worktrees/one","status":{"type":"idle"}},
            {"id":"explicit","name":"Explicit task","cwd":"/synthetic/a/sub","projectId":"outer","status":{"type":"idle"}},
            {"id":"child","name":"Child task","parentThreadId":"nested","status":{"type":"idle"}},
            {"id":"other","name":"Unassigned","cwd":"/synthetic/ab","status":{"type":"idle"}},
            {"id":"cycle-a","parentThreadId":"cycle-b","status":{"type":"idle"}},
            {"id":"cycle-b","parentThreadId":"cycle-a","status":{"type":"idle"}}
        ]}}));
        let (next, changed, error) = p.receive(json!({"id":3,"result":{"data":[{"id":"outer","name":"Outer","position":2,"roots":[{"path":"/synthetic/a"}]}],"nextCursor":"page-2"}}));
        assert!(!changed && error.is_none());
        assert!(
            p.snapshot()
                .0
                .iter()
                .all(|a| a.codex_project_name.is_none())
        );
        let id = request_id(&next, "project/list");
        let (_, changed, error) = p.receive(json!({"id":id,"result":{"data":[{"id":"inner","name":"Inner","position":1,"roots":[{"path":"/synthetic/a/sub"}]}]}}));
        assert!(changed && error.is_none());
        let agents = p.snapshot().0;
        let project = |id: &str| {
            agents
                .iter()
                .find(|a| a.task_id.as_deref() == Some(id))
                .unwrap()
                .codex_project_name
                .as_deref()
        };
        assert_eq!(project("root"), Some("Outer"));
        assert_eq!(project("nested"), Some("Inner"));
        assert_eq!(project("explicit"), Some("Outer"));
        assert_eq!(project("child"), Some("Inner"));
        assert_eq!(project("other"), None);
        assert_eq!(project("cycle-a"), None);
    }
    #[test]
    fn project_pagination_failure_keeps_last_complete_projects() {
        let mut p = populated_protocol();
        let requests = p.refresh();
        let (next, _, _) = p.receive(json!({"id":request_id(&requests,"project/list"),"result":{"data":[{"id":"partial","name":"Partial"}],"nextCursor":"again"}}));
        let (_, changed, error) = p.receive(json!({"id":request_id(&next,"project/list"),"result":{"data":[],"nextCursor":"again"}}));
        assert!(!changed && error.is_some());
        assert_eq!(
            p.snapshot().0[0].codex_project_name.as_deref(),
            Some("Last project")
        );
    }
    fn request_id(requests: &[Value], method: &str) -> Value {
        requests.iter().find(|r| r["method"] == method).unwrap()["id"].clone()
    }
    #[test]
    fn malformed_metadata_containers_and_cursor_types_preserve_published_state() {
        for result in [
            json!({}),
            json!({"data":null}),
            json!({"data":{}}),
            json!({"data":[1]}),
            json!({"data":[],"nextCursor":42}),
        ] {
            let mut p = populated_protocol();
            let requests = p.refresh();
            let (_, changed, error) =
                p.receive(json!({"id":request_id(&requests,"thread/list"),"result":result}));
            assert!(!changed);
            assert!(error.is_some());
            assert_eq!(p.snapshot().0[0].task_id.as_deref(), Some("old"));
            assert_eq!(
                p.snapshot().0[0].codex_project_name.as_deref(),
                Some("Last project")
            );
            assert!(p.pages.is_empty());
        }
        for result in [
            json!({}),
            json!({"data":null}),
            json!({"data":{}}),
            json!({"data":[false]}),
        ] {
            let mut p = populated_protocol();
            let requests = p.refresh();
            let (_, changed, error) =
                p.receive(json!({"id":request_id(&requests,"project/list"),"result":result}));
            assert!(!changed);
            assert!(error.is_some());
            assert_eq!(
                p.snapshot().0[0].codex_project_name.as_deref(),
                Some("Last project")
            );
        }
    }
    #[test]
    fn invalid_second_page_discards_staging_and_later_refresh_can_publish() {
        let mut p = populated_protocol();
        let requests = p.refresh();
        let (next, changed, error) = p.receive(json!({"id":request_id(&requests,"thread/list"),"result":{"data":[{"id":"partial","name":"Never publish"}],"nextCursor":"second"}}));
        assert!(!changed);
        assert!(error.is_none());
        assert_eq!(p.pages.len(), 1);
        let (_, changed, error) =
            p.receive(json!({"id":request_id(&next,"thread/list"),"result":{"data":null}}));
        assert!(!changed);
        assert!(error.is_some());
        assert!(p.pages.is_empty());
        assert_eq!(p.snapshot().0[0].task_id.as_deref(), Some("old"));
        p.receive(json!({"id":request_id(&requests,"project/list"),"result":{"data":[{"id":"project-old","name":"Last project"}]}}));
        let requests = p.refresh();
        assert_eq!(requests.len(), 2);
        let (_, changed, error) = p.receive(json!({"id":request_id(&requests,"thread/list"),"result":{"data":[{"id":"new","name":"Fresh"}],"nextCursor":null}}));
        assert!(changed);
        assert!(error.is_none());
        assert_eq!(p.snapshot().0[0].task_id.as_deref(), Some("new"));
        assert!(
            p.snapshot()
                .0
                .iter()
                .all(|a| a.task_id.as_deref() != Some("partial"))
        );
    }
    #[test]
    fn genuine_empty_pages_still_clear_metadata_and_repeated_cursors_preserve_agents() {
        let mut p = populated_protocol();
        let requests = p.refresh();
        let (next, _, _) = p.receive(json!({"id":request_id(&requests,"thread/list"),"result":{"data":[],"nextCursor":"repeated"}}));
        let (_, changed, error) = p.receive(json!({"id":request_id(&next,"thread/list"),"result":{"data":[],"nextCursor":"repeated"}}));
        assert!(!changed);
        assert!(error.is_some());
        assert_eq!(p.snapshot().0[0].task_id.as_deref(), Some("old"));
        let mut p = populated_protocol();
        let requests = p.refresh();
        let (_, changed, error) =
            p.receive(json!({"id":request_id(&requests,"project/list"),"result":{"data":[]}}));
        assert!(changed);
        assert!(error.is_none());
        assert!(p.snapshot().0[0].codex_project_name.is_none());
        let (_, changed, error) =
            p.receive(json!({"id":request_id(&requests,"thread/list"),"result":{"data":[]}}));
        assert!(changed);
        assert!(error.is_none());
        assert!(p.snapshot().0.is_empty());
    }
    #[test]
    fn protocol_is_paginated_metadata_only_and_denies_server_requests() {
        let mut p = Protocol::new(None);
        let (out, _, _) = p.receive(json!({"id":1,"result":{}}));
        assert_eq!(
            out.iter()
                .filter_map(|v| v["method"].as_str())
                .collect::<Vec<_>>(),
            vec!["initialized", "thread/list", "project/list"]
        );
        assert_eq!(out[1]["params"]["useStateDbOnly"], true);
        let(out,_,_)=p.receive(json!({"id":2,"result":{"data":[{"id":"a","name":"Título","preview":"must not retain","status":{"type":"idle"}}],"nextCursor":"next"}}));
        assert_eq!(out[0]["params"]["cursor"], "next");
        let next = out[0]["id"].as_u64().unwrap();
        p.receive(json!({"id":next,"result":{"data":[],"nextCursor":null}}));
        let (agents, _) = p.snapshot();
        assert_eq!(agents[0].label, "Título");
        assert!(p.threads[0].get("preview").is_none());
        let (out, _, _) =
            p.receive(json!({"id":900,"method":"item/commandExecution/requestApproval"}));
        assert_eq!(out[0]["error"]["code"], -32601);
        assert!(!out[0].to_string().contains("accept"));
    }
    #[test]
    fn collaboration_metadata_preserves_native_parent_host_and_never_requests_content() {
        let mut p = Protocol::new(Some("fixture".into()));
        p.receive(json!({"id":1,"result":{}}));
        p.receive(json!({"id":2,"result":{"data":[{"id":"parent","name":"Parent","status":{"type":"active"}},{"id":"child","name":"Child","parentThreadId":"native-origin","status":{"type":"idle"}}],"nextCursor":null}}));
        let item = |tool: &str, status: &str| json!({"method":"item/started","params":{"threadId":"parent","turnId":"turn","item":{"type":"collabAgentToolCall","id":tool,"tool":tool,"status":status,"receiverThreadIds":["child"],"prompt":"must not retain"}}});
        for message in [item("spawnAgent", "completed"), item("wait", "inProgress")] {
            let (out, changed, error) = p.receive(message);
            assert!(out.is_empty());
            assert!(changed);
            assert!(error.is_none());
        }
        let (agents, _) = p.snapshot();
        let parent = agents
            .iter()
            .find(|a| a.task_id.as_deref() == Some("parent"))
            .unwrap();
        assert!(parent.coordination_waiting);
        assert_eq!(parent.coordination_created, 1);
        assert_eq!(parent.coordination_targets, 1);
        let child = agents
            .iter()
            .find(|a| a.task_id.as_deref() == Some("child"))
            .unwrap();
        assert_eq!(
            child.parent_id.as_deref(),
            Some("codex-host:fixture:thread:native-origin")
        );
        assert!(
            !serde_json::to_string(&agents)
                .unwrap()
                .contains("must not retain")
        );
        p.threads[1]
            .as_object_mut()
            .unwrap()
            .remove("parentThreadId");
        assert_eq!(
            p.snapshot().0[1].parent_id.as_deref(),
            Some("codex-host:fixture:thread:parent")
        );
        p.receive(item("wait", "completed"));
        assert!(!p.snapshot().0[0].coordination_waiting);
        let mut request = item("wait", "inProgress");
        request["id"] = 900.into();
        let (out, changed, _) = p.receive(request);
        assert_eq!(out[0]["error"]["code"], -32601);
        assert!(!changed);
        assert!(!p.snapshot().0[0].coordination_waiting);
    }
    #[test]
    fn destination_is_encoded_without_shell_injection() {
        assert_eq!(
            task_url("a/b?", Some("local x")).unwrap(),
            "codex://threads/a%2Fb%3F?hostId=local%20x"
        );
        assert!(task_url("\n", None).is_err());
    }
    #[test]
    fn simultaneous_fixture_hosts_keep_identity_and_disconnect_independently() {
        use workspace_core::agents::FocusTarget;
        let mut registry = Registry::default();
        registry.connect_local(true).unwrap();
        registry
            .connect_remote(
                Remote::new("fixture", "remote-ssh-discovered:synthetic").unwrap(),
                true,
            )
            .unwrap();
        assert_eq!(registry.count(), 2);
        assert!(
            registry
                .connect_remote(
                    Remote::new("never-connect.invalid", "synthetic").unwrap(),
                    true
                )
                .is_err()
        );
        let until = Instant::now() + Duration::from_secs(8);
        let agents = loop {
            registry.poll();
            let (agents, _) = registry.snapshot();
            if agents.iter().any(|a| a.host_id.is_none())
                && agents
                    .iter()
                    .any(|a| a.host_id.as_deref() == Some("fixture"))
            {
                break agents;
            }
            assert!(
                Instant::now() < until,
                "fixture providers did not return both hosts"
            );
            std::thread::sleep(Duration::from_millis(15));
        };
        let remote = agents.iter().find(|a| a.host_id.is_some()).unwrap();
        let task_id = remote.task_id.clone().unwrap();
        assert_eq!(
            remote.focus_target(),
            Some(FocusTarget::CodexTask {
                task_id,
                host_id: Some("remote-ssh-discovered:synthetic".into())
            })
        );
        assert_eq!(
            agents
                .iter()
                .map(|a| &a.id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            agents.len()
        );
        let mut unmapped = remote.clone();
        unmapped.codex_host_id = None;
        assert_eq!(unmapped.focus_target(), None);
        registry.disconnect("fixture").unwrap();
        assert!(registry.connected("local"));
        assert!(!registry.connected("fixture"));
        let (after, _) = registry.snapshot();
        assert!(
            after
                .iter()
                .filter(|a| a.host_id.is_some())
                .all(|a| a.connection_state.as_deref() == Some("disconnected"))
        );
        assert!(
            after
                .iter()
                .filter(|a| a.host_id.is_none())
                .all(|a| a.connection_state.as_deref() != Some("disconnected"))
        );
        registry.disconnect("all").unwrap();
        assert_eq!(registry.count(), 0);
        assert_eq!(registry.snapshot().0.len(), agents.len());
        for bad in [
            "-oProxyCommand=x",
            "host;id",
            "host path",
            "local",
            "all",
            "host\n",
        ] {
            assert!(Remote::new(bad, "remote-id").is_err());
        }
        let (tx, rx) = mpsc::channel();
        drop(tx);
        registry.slots.get_mut("local").unwrap().provider = Some(Provider {
            events: rx,
            stop: Arc::new(AtomicBool::new(false)),
            refresh: Arc::new(AtomicBool::new(false)),
        });
        assert!(registry.poll().0);
        assert!(!registry.connected("local"));
        assert!(Remote::new("host", "local").is_err());
        assert!(Remote::new("host", "bad id").is_err());
    }
}
