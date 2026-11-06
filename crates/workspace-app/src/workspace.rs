//! UI state and background project indexing. Never discovers the user's home.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Instant,
};
use workspace_core::{
    agents::{Agent, Change, FocusTarget, Location, Overview, Side, TaskTree},
    input::{Binding, Context, KeyRouter, Route},
    picker::{PickerRequests, rank},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Terminal,
    Normal,
    Tree,
    Agents,
    Backlog,
    File,
    Picker,
    Details,
}
impl Surface {
    pub fn context(self) -> Context {
        match self {
            Self::Terminal => Context::TerminalInput,
            Self::Normal => Context::TerminalNormal,
            Self::Tree => Context::Tree,
            Self::Agents => Context::Agents,
            Self::Backlog => Context::Backlog,
            Self::Picker => Context::PickerInput,
            Self::Details => Context::Details,
            Self::File => Context::Workspace,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Project {
    pub label: String,
    pub path: PathBuf,
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub label: String,
    pub depth: usize,
    pub directory: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentRow {
    Group(String),
    Task(String),
    Backlog,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    Projects,
    Files,
    Buffers,
    Restore,
    Grep,
    Discover,
    Todo(bool),
    Diagnostics,
    References,
    Themes,
    Fonts,
    PullRequests,
    PrVideos,
}
impl PickerKind {
    pub fn multiple(self) -> bool {
        matches!(
            self,
            Self::Files
                | Self::Buffers
                | Self::Grep
                | Self::Todo(_)
                | Self::Diagnostics
                | Self::References
        )
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Projects => "Proyectos",
            Self::Files => "Archivos",
            Self::Buffers => "Buffers",
            Self::Restore => "Restaurar ocultos",
            Self::Grep => "Buscar en el proyecto",
            Self::Discover => "Subproyectos · profundidad 3",
            Self::Todo(false) => "TODOs del proyecto",
            Self::Todo(true) => "TODO / FIX / FIXME",
            Self::Diagnostics => "Diagnósticos LSP",
            Self::References => "Referencias LSP",
            Self::Themes => "Temas · perfil de la app",
            Self::Fonts => "Fuente y tamaño · terminal / editor",
            Self::PullRequests => "PRs asociados · Enter revisión · Ctrl+v videos",
            Self::PrVideos => "Videos del PR",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Choice {
    pub label: String,
    pub detail: String,
    pub value: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerFocus {
    Input,
    List,
}
pub struct Picker {
    pub kind: PickerKind,
    pub query: String,
    pub query_cursor: usize,
    pub insert: bool,
    pub focus: PickerFocus,
    top_prefix: Option<Instant>,
    pub choices: Arc<Vec<Choice>>,
    pub results: Vec<usize>,
    pub selected: usize,
    pub marked: Vec<Choice>,
    // Preserve how each choice was discovered even if the query later changes.
    marked_absolute: BTreeSet<String>,
    pub generation: u64,
    pub previous: Surface,
}
impl Picker {
    pub fn input_active(&self) -> bool {
        self.focus == PickerFocus::Input && self.insert
    }
    pub fn focus_input(&mut self) {
        self.focus = PickerFocus::Input;
        self.insert = true;
        self.query_cursor = self.query.len();
        self.top_prefix = None;
    }
    pub fn focus_list(&mut self) {
        self.focus = PickerFocus::List;
        self.insert = false;
        self.top_prefix = None;
    }
    fn step(&mut self, forward: bool) {
        let count = self.results.len();
        self.selected = if count == 0 {
            0
        } else if forward {
            (self.selected.min(count - 1) + 1) % count
        } else {
            (self.selected.min(count - 1) + count - 1) % count
        };
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    DeleteBuffer,
    AlternateBuffer,
    NumberedBuffer(usize),
    ToggleBufferPin,
    MoveBufferLine(bool),
    CycleBufferLine(bool),
    CloseWorkspace,
    CycleWorkspace(bool),
    ExplicitFile(PathBuf),
    MarkedFiles(Vec<PathBuf>, bool),
    NewFile,
    EditorWindow(&'static str),
    TerminalNormal(bool),
    TerminalMotion(String, usize),
    TerminalJump(bool, usize),
    TreeEdit(char),
    TreeRefresh,
    Navigate(PathBuf),
    Theme(String),
    Typography(String),
    SyntaxToggle,
    ToggleWrap,
    Diagnostics(bool),
    LocationUtf16(PathBuf, serde_json::Value),
    Git(String),
    Format,
    Csv,
    Preview(PathBuf, String),
    PrVideo(String),
    PrReview(String),
    PrAttachment(usize),
    RefreshAgents,
    AgentContext(String),
    AgentPr(String),
    Pty(Vec<u8>),
    Open(FocusTarget),
    File(PathBuf),
    TerminalBuffer(u64),
    FileAt(PathBuf, usize, usize),
    Project(usize),
    None,
}
struct PickerUpdate {
    generation: u64,
    results: Vec<usize>,
    choices: Option<Vec<Choice>>,
    error: Option<String>,
}
const PARTIAL_ABSOLUTE_SEARCH: &str = "Búsqueda parcial: límite o carpetas inaccesibles";

pub struct Workspace {
    pub surface: Surface,
    /// Changes to surface invalidate asynchronous operations from an older focus.
    pub focus_revision: u64,
    pub sidebar: bool,
    pub agents_visible: bool,
    pub right: bool,
    pub compact_context: bool,
    pub overview: Overview,
    pub selected: AgentRow,
    pub main_selection: AgentRow,
    pub backlog_selection: AgentRow,
    pub projects: Vec<Project>,
    pub project: usize,
    explorer_roots: BTreeMap<usize, PathBuf>,
    pub snacks_projects: Option<(crate::snacks_projects::Config, PathBuf)>,
    validation_root: Option<PathBuf>,
    pub entries: Vec<Entry>,
    pub expanded: BTreeSet<PathBuf>,
    pub tree_selected: usize,
    tree_scroll_requested: bool,
    agent_scroll_requested: bool,
    half_page_lines: [Option<usize>; 3],
    pub tree_marks: BTreeSet<PathBuf>,
    pub tree_anchor: Option<PathBuf>,
    pub picker: Option<Picker>,
    pub file: Option<(PathBuf, String)>,
    pub buffers: Vec<PathBuf>,
    pub pinned_buffers: BTreeSet<PathBuf>,
    pub unnamed: BTreeSet<PathBuf>,
    last_buffer: Option<PathBuf>,
    content_surface: Surface,
    last_window_surface: Surface,
    previous_window: Option<Surface>,
    window_prefix: bool,
    window_after_count: usize,
    previous_buffer: Option<PathBuf>,
    pub message: String,
    pub pending_since: Option<Instant>,
    pub count: usize,
    pub details: Option<String>,
    details_previous: Surface,
    pub notifications: bool,
    pub buffer_bar: bool,
    pub live_agents: bool,
    router: KeyRouter,
    picker_requests: PickerRequests,
    picker_tx: crate::events::Sender<PickerUpdate>,
    picker_rx: mpsc::Receiver<PickerUpdate>,
    picker_message: Option<String>,
    generation: u64,
}
impl Workspace {
    /// Normal launches never seed synthetic projects or agents. Validation stays
    /// inside its owned fixture directory and never starts a personal provider.
    pub fn from_profile(
        home: &Path,
        state: &Path,
        validation: bool,
        bindings: &[Binding],
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        if !state.is_absolute() || (!validation && !home.is_absolute()) {
            return Err("El perfil requiere rutas absolutas".into());
        }
        let projects = if validation {
            ["workspace", "notes-lab", "api-playground"]
                .into_iter()
                .map(|name| Project {
                    label: name.into(),
                    path: state.join("demo").join(name),
                })
                .collect()
        } else {
            vec![Project {
                label: "Home".into(),
                path: home.to_owned(),
            }]
        };
        let agents = if validation {
            serde_json::from_str(include_str!(
                "../../../Resources/Contracts/agents-demo.json"
            ))
            .map_err(|error| format!("Contrato de validación: {error}"))?
        } else {
            Vec::new()
        };
        let mut model = Self::new_notifying(projects, agents, bindings, wake)?;
        model.live_agents = !validation;
        if validation {
            model.validation_root = Some(state.join("demo"));
            model.snacks_projects = Some((
                crate::snacks_projects::Config::validation()?,
                state.join("demo"),
            ));
        } else {
            model.snacks_projects = Some((
                crate::snacks_projects::Config::configured()?,
                home.to_owned(),
            ));
            model.message = "Home · Conectar Agents para habilitar un proveedor".into();
            model.reconcile_agent_selection();
        }
        Ok(model)
    }
    pub fn new(
        projects: Vec<Project>,
        agents: Vec<Agent>,
        bindings: &[Binding],
    ) -> Result<Self, String> {
        Self::new_notifying(projects, agents, bindings, None)
    }
    pub fn new_notifying(
        projects: Vec<Project>,
        agents: Vec<Agent>,
        bindings: &[Binding],
        wake: Option<async_channel::Sender<()>>,
    ) -> Result<Self, String> {
        if projects.is_empty() {
            return Err("a workspace needs one explicit project".into());
        }
        let mut overview = Overview::default();
        overview.replace(agents, 1000)?;
        let selected = AgentRow::Group("demo-project".into());
        let (tx, rx) = crate::events::channel(wake);
        let mut router = KeyRouter::compile(bindings)?;
        router.set_context(Context::Tree);
        Ok(Self {
            surface: Surface::Tree,
            focus_revision: 0,
            sidebar: true,
            agents_visible: true,
            right: false,
            compact_context: false,
            overview,
            selected: selected.clone(),
            main_selection: selected,
            backlog_selection: AgentRow::Backlog,
            projects,
            project: 0,
            explorer_roots: BTreeMap::new(),
            snacks_projects: None,
            validation_root: None,
            entries: Vec::new(),
            expanded: BTreeSet::new(),
            tree_selected: 0,
            tree_scroll_requested: false,
            agent_scroll_requested: false,
            half_page_lines: [None; 3],
            tree_marks: BTreeSet::new(),
            tree_anchor: None,
            picker: None,
            file: None,
            buffers: Vec::new(),
            pinned_buffers: BTreeSet::new(),
            unnamed: BTreeSet::new(),
            last_buffer: None,
            content_surface: Surface::Terminal,
            last_window_surface: Surface::Tree,
            previous_window: None,
            window_prefix: false,
            window_after_count: 0,
            previous_buffer: None,
            message: "Flujo familiar · workspace de ejemplo".into(),
            pending_since: None,
            count: 0,
            details: None,
            details_previous: Surface::Agents,
            notifications: false,
            buffer_bar: false,
            live_agents: false,
            router,
            picker_requests: PickerRequests::default(),
            picker_tx: tx,
            picker_rx: rx,
            picker_message: None,
            generation: 0,
        })
    }
    pub fn focus(&mut self, surface: Surface) -> Effect {
        if self.surface != surface {
            self.focus_revision = self.focus_revision.wrapping_add(1);
        }
        // Picker/details are overlays, and terminal Insert/Normal are one window.
        // Neither changing mode nor a failed motion replaces window history.
        if Self::window_kind(surface).is_some() {
            if Self::window_kind(surface) != Self::window_kind(self.last_window_surface) {
                self.previous_window = Some(self.last_window_surface);
            }
            self.last_window_surface = surface;
        }
        let resume_terminal = surface == Surface::Terminal && self.surface != Surface::Terminal;
        if surface == Surface::Details && self.surface != Surface::Details {
            self.details_previous = self.surface;
        }
        let flushed = self.router.set_context(surface.context());
        if matches!(surface, Surface::Terminal | Surface::Normal | Surface::File) {
            self.content_surface = surface;
        }
        self.surface = surface;
        self.window_prefix = false;
        self.window_after_count = 0;
        if matches!(surface, Surface::Agents | Surface::Backlog) {
            self.reconcile_agent_selection();
        }
        self.count = 0;
        self.pending_since = None;
        if let Some(bytes) = flushed {
            Effect::Pty(bytes)
        } else if resume_terminal {
            Effect::TerminalNormal(false)
        } else {
            Effect::None
        }
    }
    fn window_kind(surface: Surface) -> Option<u8> {
        match surface {
            Surface::File | Surface::Normal | Surface::Terminal => Some(0),
            Surface::Tree => Some(1),
            Surface::Agents | Surface::Backlog => Some(2),
            _ => None,
        }
    }
    fn visible_windows(&self) -> Vec<Surface> {
        if !self.sidebar {
            return vec![self.content_surface];
        }
        let mut order = vec![Surface::Tree];
        if self.agents_visible {
            order.push(Surface::Agents);
        }
        if self.right {
            order.insert(0, self.content_surface);
        } else {
            order.push(self.content_surface);
        }
        order
    }
    fn focus_window(&mut self, target: Surface) -> Effect {
        if Self::window_kind(target) == Some(0) {
            self.focus_content()
        } else {
            self.focus(target)
        }
    }
    fn focus_content(&mut self) -> Effect {
        if self.content_surface == Surface::File {
            if self.file.is_none()
                && let Some(path) = self
                    .last_buffer
                    .clone()
                    .filter(|p| self.buffers.contains(p))
            {
                self.file = Some((path, String::new()));
            }
            if self.file.is_none() {
                self.content_surface = Surface::Terminal;
            }
        }
        let target = self.content_surface;
        let effect = self.focus(target);
        // A terminal in Normal keeps its copy cursor when another panel has focus.
        if target == Surface::Normal {
            Effect::TerminalNormal(true)
        } else {
            effect
        }
    }
    pub fn now(&self) -> i64 {
        if self.live_agents {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        } else {
            1000
        }
    }
    pub fn prefix_hints(&self) -> Vec<(String, Option<String>)> {
        self.router.hints()
    }
    pub fn key(&mut self, chord: &str, bytes: &[u8]) -> Effect {
        if chord == "<Esc>" && self.pending_since.is_some() && self.surface != Surface::Terminal {
            self.pending_since = None;
            self.router.expire_prefix();
            self.window_prefix = false;
            self.window_after_count = 0;
            self.count = 0;
            return Effect::None;
        }
        if self.window_prefix
            && chord.len() == 1
            && chord.as_bytes()[0].is_ascii_digit()
            && (chord != "0" || self.window_after_count > 0)
        {
            self.window_after_count =
                (self.window_after_count * 10 + (chord.as_bytes()[0] - b'0') as usize).min(9999);
            self.pending_since = Some(Instant::now());
            return Effect::None;
        }
        if self.surface == Surface::Picker {
            return self.picker_key(chord);
        }
        if self.surface == Surface::Details {
            if chord == "<Esc>" || chord == "q" {
                self.details = None;
                return self.focus(self.details_previous);
            }
            return Effect::None;
        }
        if matches!(
            self.surface,
            Surface::Agents | Surface::Backlog | Surface::Tree
        ) && chord.len() == 1
            && chord.as_bytes()[0].is_ascii_digit()
            && self.pending_since.is_none()
        {
            let digit = (chord.as_bytes()[0] - b'0') as usize;
            if digit > 0 || self.count > 0 {
                self.count = (self.count * 10 + digit).min(9999);
                return Effect::None;
            }
        }
        if matches!(self.surface, Surface::Agents | Surface::Backlog)
            && self.pending_since.is_none()
            && matches!(chord, "j" | "k" | "<Down>" | "<Up>" | "h" | "l" | "<Enter>")
        {
            self.agent_scroll_requested = true;
        }
        if self.surface == Surface::Tree && self.pending_since.is_none() {
            // A motion at the first/last row still reveals the cursor after
            // manual scrolling, even though its index does not change.
            if matches!(
                chord,
                "j" | "k" | "<Down>" | "<Up>" | "h" | "l" | "<Enter>" | "o" | "v" | "V"
            ) {
                self.tree_scroll_requested = true;
            }
            match chord {
                "<BS>" => {
                    return Effect::Navigate(
                        self.explorer_root()
                            .parent()
                            .unwrap_or(Path::new("/"))
                            .to_owned(),
                    );
                }
                "." => {
                    if let Some(entry) = self.tree_rows().get(self.tree_selected) {
                        return Effect::Navigate(if entry.directory {
                            entry.path.clone()
                        } else {
                            entry
                                .path
                                .parent()
                                .unwrap_or(self.explorer_root())
                                .to_owned()
                        });
                    }
                }
                "~" => {
                    return std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .map(Effect::Navigate)
                        .unwrap_or(Effect::None);
                }
                "v" | "V" => {
                    self.count = 0;
                    self.tree_anchor = if self.tree_anchor.is_some() {
                        None
                    } else {
                        self.tree_rows()
                            .get(self.tree_selected)
                            .map(|e| e.path.clone())
                    };
                    self.message = if self.tree_anchor.is_some() { "Rango visual del árbol · j/k extiende · Tab marca · y/x/T consume · Esc cancela" }
                        else { "Rango visual cancelado" }.into();
                    return Effect::None;
                }
                "<Esc>" if self.tree_anchor.take().is_some() => {
                    self.count = 0;
                    self.message = "Rango visual cancelado; marcas conservadas".into();
                    return Effect::None;
                }
                "<Tab>" => {
                    self.count = 0;
                    let mut marks = self.tree_marks.clone();
                    for path in self.tree_range() {
                        if !marks.remove(&path) {
                            marks.insert(path);
                        }
                    }
                    if marks.len() > 128 {
                        self.message =
                            "Límite de 128 marcas en el árbol; selección conservada".into();
                        return Effect::None;
                    }
                    self.tree_marks = marks;
                    self.tree_anchor = None;
                    self.message = format!(
                        "{} marcas · y copia / x mueve · Ctrl+; limpia",
                        self.tree_marks.len()
                    );
                    return Effect::None;
                }
                "<C-;>" => {
                    self.tree_marks.clear();
                    self.message = "Selección del árbol limpiada".into();
                    return Effect::None;
                }
                "<C-r>" => return Effect::TreeEdit('\u{12}'),
                "a" | "A" | "r" | "m" | "c" | "y" | "x" | "p" | "T" | "u" => {
                    self.count = 0;
                    return Effect::TreeEdit(chord.chars().next().unwrap());
                }
                "j" | "<Down>" => {
                    self.tree_step(1);
                    return Effect::None;
                }
                "k" | "<Up>" => {
                    self.tree_step(-1);
                    return Effect::None;
                }
                "h" => {
                    self.count = 0;
                    self.close_tree_node();
                    return Effect::None;
                }
                "l" | "<Enter>" | "o" => {
                    self.count = 0;
                    return self.open_tree();
                }
                _ => {}
            }
        }
        if self.surface == Surface::File && chord == "<Esc>" {
            return self.focus(Surface::Tree);
        }
        if self.surface == Surface::Normal && self.pending_since.is_none() {
            if matches!(chord, "<C-o>" | "<C-i>" | "<Tab>") {
                let count = std::mem::take(&mut self.count).max(1);
                return Effect::TerminalJump(chord == "<C-o>", count);
            }
            if matches!(chord, "<C-6>" | "<C-^>") {
                let count = std::mem::take(&mut self.count);
                if count != 0 {
                    return Effect::NumberedBuffer(count);
                }
                return Effect::AlternateBuffer;
            }
            if matches!(chord, "i" | "a" | "I" | "A") {
                self.focus(Surface::Terminal);
                return Effect::TerminalNormal(false);
            }
            if chord.len() == 1
                && chord.as_bytes()[0].is_ascii_digit()
                && (chord != "0" || self.count > 0)
            {
                self.count = (self.count * 10 + usize::from(chord.as_bytes()[0] - b'0')).min(9999);
                return Effect::None;
            }
            if matches!(
                chord,
                "h" | "j"
                    | "k"
                    | "l"
                    | "w"
                    | "b"
                    | "e"
                    | "W"
                    | "B"
                    | "E"
                    | "0"
                    | "^"
                    | "$"
                    | "G"
                    | "v"
                    | "V"
                    | "y"
                    | "<C-v>"
                    | "<Esc>"
                    | "<Left>"
                    | "<Right>"
                    | "<Up>"
                    | "<Down>"
                    | "<C-u>"
                    | "<C-d>"
                    | "<C-b>"
                    | "<C-f>"
                    | "n"
                    | "N"
            ) {
                let count = std::mem::take(&mut self.count).max(1);
                return Effect::TerminalMotion(chord.into(), count);
            }
        }
        match self.router.route(chord, bytes) {
            Route::Pty(bytes) => Effect::Pty(bytes.into_owned()),
            Route::Action(action) => {
                self.pending_since = None;
                let window_prefix = std::mem::take(&mut self.window_prefix);
                if window_prefix && self.window_after_count > 0 {
                    self.count = (self.count.max(1) * self.window_after_count).min(9999);
                }
                self.window_after_count = 0;
                let effect = self.action(&action);
                if window_prefix {
                    self.count = 0;
                }
                effect
            }
            Route::Pending => {
                if chord == "<C-w>" {
                    self.window_prefix = true;
                }
                self.pending_since = Some(Instant::now());
                Effect::None
            }
            Route::Unhandled => {
                self.pending_since = None;
                if std::mem::take(&mut self.window_prefix) {
                    self.count = 0;
                }
                self.window_after_count = 0;
                Effect::None
            }
        }
    }
    pub fn expire_prefix(&mut self) -> Effect {
        self.pending_since = None;
        if std::mem::take(&mut self.window_prefix) {
            self.count = 0;
        }
        self.window_after_count = 0;
        self.router
            .expire_prefix()
            .map(Effect::Pty)
            .unwrap_or(Effect::None)
    }
    pub fn selected_agent(&self) -> Option<&Agent> {
        if let AgentRow::Task(id) = &self.selected {
            self.overview.agents().iter().find(|a| a.id == *id)
        } else {
            None
        }
    }
    // Preserve existing Main keys; Later has its own persisted namespace, as in ui.lua.
    fn group_fold_key(group: &str, location: Location) -> String {
        if location == Location::Later {
            format!("later:{group}")
        } else {
            group.to_owned()
        }
    }
    pub fn group_collapsed(&self, group: &str, location: Location) -> bool {
        self.overview
            .view
            .collapsed
            .get(&Self::group_fold_key(group, location))
            == Some(&true)
    }
    fn agent_group(agent: &Agent) -> &str {
        agent
            .codex_project_id
            .as_deref()
            .unwrap_or(if agent.source == "codex_app_server" {
                "unassigned"
            } else {
                "terminals"
            })
    }
    pub fn take_agent_scroll_request(&mut self) -> bool {
        std::mem::take(&mut self.agent_scroll_requested)
    }
    pub fn rows(&self, location: Location) -> Vec<(AgentRow, usize)> {
        let agents = self.overview.visible(location);
        let mut rows = Vec::new();
        let mut groups: Vec<String> = Vec::new();
        for a in &agents {
            let group = Self::agent_group(a).to_owned();
            if !groups.contains(&group) {
                groups.push(group);
            }
        }
        groups.sort_by_key(|key| {
            let member = agents.iter().find(|a| Self::agent_group(a) == key);
            let position = member
                .and_then(|a| a.codex_project_position)
                .unwrap_or(i64::MAX);
            let category = match key.as_str() {
                "unassigned" => 1,
                "terminals" => 2,
                _ => 0,
            };
            (
                category,
                position,
                self.group_label(key).to_lowercase(),
                key.clone(),
            )
        });
        if location == Location::Later {
            rows.push((AgentRow::Backlog, 0));
            if self.overview.view.later_collapsed {
                return rows;
            }
        }
        for group in groups {
            rows.push((AgentRow::Group(group.clone()), 0));
            if self.group_collapsed(&group, location) {
                continue;
            }
            let members: Vec<_> = agents
                .iter()
                .copied()
                .filter(|a| Self::agent_group(a) == group)
                .collect();
            let tree = TaskTree::build(&members);
            fn append(
                id: &str,
                depth: usize,
                tree: &TaskTree,
                collapsed: &BTreeMap<String, bool>,
                rows: &mut Vec<(AgentRow, usize)>,
            ) {
                rows.push((AgentRow::Task(id.into()), depth + 1));
                if collapsed.get(&format!("task:{id}")) == Some(&true) {
                    return;
                }
                if let Some(children) = tree.children.get(id) {
                    for child in children {
                        append(child, depth + 1, tree, collapsed, rows);
                    }
                }
            }
            for id in &tree.roots {
                append(id, 0, &tree, &self.overview.view.collapsed, &mut rows);
            }
        }
        rows
    }
    /// Build once per pane, rather than traversing every task for each rendered row.
    pub fn task_folds(&self, location: Location) -> BTreeMap<String, bool> {
        let mut groups: BTreeMap<&str, Vec<&Agent>> = BTreeMap::new();
        for agent in self.overview.visible(location) {
            groups
                .entry(Self::agent_group(agent))
                .or_default()
                .push(agent);
        }
        let mut folds = BTreeMap::new();
        for agents in groups.values() {
            for (id, children) in TaskTree::build(agents).children {
                if !children.is_empty() {
                    let closed =
                        self.overview.view.collapsed.get(&format!("task:{id}")) == Some(&true);
                    folds.insert(id, closed);
                }
            }
        }
        folds
    }
    /// Provider refreshes can remove the selected row. Keep focus local to its
    /// pane and retain an existing selection whenever that row is still visible.
    pub fn reconcile_agent_selection(&mut self) {
        let main = self.rows(Location::Main);
        let later = self.rows(Location::Later);
        let normalize = |selection: &mut AgentRow, rows: &[(AgentRow, usize)]| {
            if !rows.iter().any(|(row, _)| row == selection)
                && let Some((first, _)) = rows.first()
            {
                *selection = first.clone();
            }
        };
        normalize(&mut self.main_selection, &main);
        normalize(&mut self.backlog_selection, &later);
        match self.surface {
            Surface::Agents => {
                normalize(&mut self.selected, &main);
                self.main_selection = self.selected.clone();
            }
            Surface::Backlog => {
                normalize(&mut self.selected, &later);
                self.backlog_selection = self.selected.clone();
            }
            _ => {}
        }
    }
    pub fn select_agent(&mut self, row: AgentRow, backlog: bool) -> Effect {
        self.selected = row.clone();
        if backlog {
            self.backlog_selection = row;
        } else {
            self.main_selection = row;
        }
        self.focus(if backlog {
            Surface::Backlog
        } else {
            Surface::Agents
        })
    }
    pub fn group_label(&self, key: &str) -> String {
        if key == "terminals" {
            "Terminales".into()
        } else if key == "unassigned" {
            "Sin proyecto".into()
        } else {
            self.overview
                .agents()
                .iter()
                .find(|a| a.codex_project_id.as_deref() == Some(key))
                .and_then(|a| a.codex_project_name.clone())
                .unwrap_or_else(|| key.into())
        }
    }
    fn step(&mut self, delta: isize) {
        let loc = if self.surface == Surface::Backlog {
            Location::Later
        } else {
            Location::Main
        };
        let rows = self.rows(loc);
        if rows.is_empty() {
            return;
        }
        let index = rows
            .iter()
            .position(|(r, _)| r == &self.selected)
            .unwrap_or(0);
        let count = std::mem::take(&mut self.count).max(1);
        let next =
            (index as isize + delta * count as isize).clamp(0, rows.len() as isize - 1) as usize;
        self.selected = rows[next].0.clone();
        if loc == Location::Main {
            self.main_selection = self.selected.clone()
        } else {
            self.backlog_selection = self.selected.clone()
        }
    }
    fn fold(&mut self, collapsed: bool) {
        match self.selected.clone() {
            AgentRow::Group(group) => {
                let location = if self.surface == Surface::Backlog {
                    Location::Later
                } else {
                    Location::Main
                };
                let _ = self.overview.view.apply(&[Change::Collapsed {
                    key: Self::group_fold_key(&group, location),
                    value: collapsed,
                }]);
            }
            AgentRow::Backlog => self.overview.view.later_collapsed = collapsed,
            AgentRow::Task(id) => {
                let location = if self.surface == Surface::Backlog {
                    Location::Later
                } else {
                    Location::Main
                };
                if self.task_folds(location).contains_key(&id) {
                    let _ = self.overview.view.apply(&[Change::Collapsed {
                        key: format!("task:{id}"),
                        value: collapsed,
                    }]);
                } else if self.surface == Surface::Backlog {
                    self.overview.view.later_collapsed = collapsed;
                    if collapsed {
                        self.selected = AgentRow::Backlog;
                        self.backlog_selection = AgentRow::Backlog;
                    }
                }
            }
        }
    }
    pub fn buffer_label(&self, path: &Path) -> String {
        if self.unnamed.contains(path) {
            "[No Name]".into()
        } else {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }
    }
    pub fn activate_file(&mut self, path: PathBuf, text: String) {
        if self.last_buffer.as_ref() != Some(&path) {
            self.previous_buffer = self.last_buffer.replace(path.clone());
        }
        if !self.buffers.contains(&path) {
            self.buffers.push(path.clone());
        }
        self.file = Some((path, text));
    }
    fn buffer_action(&self, action: &str) -> Effect {
        let current = self
            .file
            .as_ref()
            .map(|(p, _)| p)
            .or(self.last_buffer.as_ref());
        let destination = if action == "buffers.alternate" {
            self.previous_buffer
                .as_ref()
                .filter(|p| self.buffers.contains(p))
        } else if self.buffers.is_empty() {
            None
        } else {
            let index = current
                .and_then(|p| self.buffers.iter().position(|b| b == p))
                .unwrap_or(0);
            let index = if action == "buffers.next" {
                (index + 1) % self.buffers.len()
            } else {
                (index + self.buffers.len() - 1) % self.buffers.len()
            };
            self.buffers.get(index)
        };
        destination
            .filter(|p| Some(*p) != current)
            .map(|p| Effect::File(p.clone()))
            .unwrap_or(Effect::None)
    }
    pub fn action(&mut self, action: &str) -> Effect {
        if let Some(action) = action.strip_prefix("git.") {
            return Effect::Git(action.into());
        }
        match action {
            "buffers.delete" => return Effect::DeleteBuffer,
            "buffers.pin" => return Effect::ToggleBufferPin,
            "buffers.move.prev" => return Effect::MoveBufferLine(false),
            "buffers.move.next" => return Effect::MoveBufferLine(true),
            "buffers.line.prev" => return Effect::CycleBufferLine(false),
            "buffers.line.next" => return Effect::CycleBufferLine(true),
            "buffers.alternate" => return Effect::AlternateBuffer,
            "buffers.prev" | "buffers.next" => {
                return self.buffer_action(action);
            }
            "diagnostics.project" => return Effect::Diagnostics(false),
            "diagnostics.buffer" => return Effect::Diagnostics(true),
            "picker.todo" | "picker.todo_fix" => {
                self.open_picker(PickerKind::Todo(action == "picker.todo_fix"))
            }
            "preview.open_html" | "file.open_external" => {
                if let Some(entry) = self.tree_rows().get(self.tree_selected) {
                    return Effect::Preview(entry.path.clone(), "external".into());
                }
            }
            "preview.html" | "preview.tex" => {
                if let Some((path, _)) = &self.file {
                    return Effect::Preview(
                        path.clone(),
                        if action == "preview.tex" {
                            "tex"
                        } else {
                            "html"
                        }
                        .into(),
                    );
                }
            }
            "csv.toggle" => return Effect::Csv,
            "editor.format" => return Effect::Format,
            "editor.syntax_toggle" => return Effect::SyntaxToggle,
            "editor.wrap_toggle" => return Effect::ToggleWrap,
            "buffers.toggle_bar" => self.buffer_bar = !self.buffer_bar,
            "tree_agents.toggle" => {
                self.sidebar = !self.sidebar;
                self.agents_visible = true;
                return self.focus(if self.sidebar {
                    Surface::Tree
                } else {
                    Surface::Terminal
                });
            }
            "agents.toggle" => {
                self.agents_visible = !self.agents_visible;
                self.sidebar = true;
                return self.focus(if self.agents_visible {
                    Surface::Agents
                } else {
                    Surface::Tree
                });
            }
            "agents.close" => {
                self.agents_visible = false;
                return self.focus(Surface::Tree);
            }
            "file.new" => return Effect::NewFile,
            "editor.split.right" => return Effect::EditorWindow("v"),
            "editor.split.below" => return Effect::EditorWindow("s"),
            "editor.window.close" => return Effect::EditorWindow("q"),
            "workspace.close" => return Effect::CloseWorkspace,
            "workspace.next" => return Effect::CycleWorkspace(true),
            "workspace.prev" => return Effect::CycleWorkspace(false),
            "terminal.history_top" => {
                return Effect::TerminalMotion("gg".into(), std::mem::take(&mut self.count).max(1));
            }
            "terminal.normal" => {
                self.focus(Surface::Normal);
                return Effect::TerminalNormal(true);
            }
            "terminal.focus_root" | "terminal.toggle_root" | "terminal.toggle_cwd" => {
                return self.focus(Surface::Terminal);
            }
            "focus.left" | "focus.right" => {
                let sidebar = matches!(
                    self.surface,
                    Surface::Tree | Surface::Agents | Surface::Backlog
                );
                let toward_sidebar = (action == "focus.right") == self.right;
                if toward_sidebar && !sidebar {
                    self.sidebar = true;
                    return self.focus(Surface::Tree);
                }
                if !toward_sidebar && sidebar {
                    return self.focus_content();
                }
                return Effect::None;
            }
            "focus.last_accessed" => {
                if let Some(previous) = self.previous_window
                    && self
                        .visible_windows()
                        .iter()
                        .any(|surface| Self::window_kind(*surface) == Self::window_kind(previous))
                {
                    return self.focus_window(previous);
                }
                return Effect::None;
            }
            "focus.next" | "focus.previous" | "focus.first" | "focus.last" => {
                let order = self.visible_windows();
                let current = Self::window_kind(self.surface);
                let index = order
                    .iter()
                    .position(|surface| Self::window_kind(*surface) == current)
                    .unwrap_or(0);
                let target = match action {
                    "focus.next" | "focus.previous" if self.count > 0 => {
                        (self.count - 1).min(order.len() - 1)
                    }
                    "focus.first" => 0,
                    "focus.last" => order.len() - 1,
                    "focus.next" => (index + 1) % order.len(),
                    _ => (index + order.len() - 1) % order.len(),
                };
                return self.focus_window(order[target]);
            }
            "focus.down" => {
                if matches!(self.surface, Surface::Tree | Surface::Agents) {
                    self.agents_visible = true;
                    self.selected = self.main_selection.clone();
                    return self.focus(Surface::Agents);
                }
                return Effect::None;
            }
            "focus.up" => {
                if matches!(self.surface, Surface::Agents | Surface::Backlog) {
                    return self.focus(Surface::Tree);
                }
                return Effect::None;
            }
            "list.first" | "list.last" => {
                let count = std::mem::take(&mut self.count);
                if self.surface == Surface::Tree {
                    let length = self.tree_rows().len();
                    let index = if count > 0 {
                        count - 1
                    } else if action == "list.last" {
                        length.saturating_sub(1)
                    } else {
                        0
                    };
                    self.tree_selected = index.min(length.saturating_sub(1));
                    self.tree_scroll_requested = true;
                } else if matches!(self.surface, Surface::Agents | Surface::Backlog) {
                    let location = if self.surface == Surface::Backlog {
                        Location::Later
                    } else {
                        Location::Main
                    };
                    let rows = self.rows(location);
                    let index = if count > 0 {
                        count - 1
                    } else if action == "list.last" {
                        rows.len().saturating_sub(1)
                    } else {
                        0
                    };
                    if let Some((row, _)) = rows.get(index.min(rows.len().saturating_sub(1))) {
                        self.selected = row.clone();
                        if location == Location::Main {
                            self.main_selection = row.clone();
                        } else {
                            self.backlog_selection = row.clone();
                        }
                    }
                    self.agent_scroll_requested = true;
                }
                return Effect::None;
            }
            "selection.next" => self.step(1),
            "selection.previous" => self.step(-1),
            "group.collapse" => self.fold(true),
            "group.expand" => self.fold(false),
            "agent.open_or_fold" => {
                if matches!(self.selected, AgentRow::Task(_)) {
                    return self.action("agent.focus");
                }
                match &self.selected {
                    AgentRow::Backlog => {
                        self.overview.view.later_collapsed = !self.overview.view.later_collapsed
                    }
                    AgentRow::Group(k) => {
                        let location = if self.surface == Surface::Backlog {
                            Location::Later
                        } else {
                            Location::Main
                        };
                        let closed = !self.group_collapsed(k, location);
                        self.fold(closed);
                    }
                    _ => {}
                }
            }
            "agent.focus" => {
                if let Some(target) = self.selected_agent().and_then(Agent::focus_target) {
                    return Effect::Open(target);
                }
            }
            "backlog.focus" => {
                self.overview.view.later_collapsed = false;
                self.selected = self
                    .rows(Location::Later)
                    .get(1)
                    .map(|r| r.0.clone())
                    .unwrap_or(AgentRow::Backlog);
                return self.focus(Surface::Backlog);
            }
            "backlog.focus_preserve" => {
                self.overview.view.later_collapsed = false;
                self.selected = self.backlog_selection.clone();
                return self.focus(Surface::Backlog);
            }
            "agents.focus" => {
                let rows = self.rows(Location::Main);
                self.selected = if rows.iter().any(|(row, _)| row == &self.main_selection) {
                    self.main_selection.clone()
                } else {
                    rows.first()
                        .map(|(row, _)| row.clone())
                        .unwrap_or(AgentRow::Group("demo-project".into()))
                };
                self.main_selection = self.selected.clone();
                return self.focus(Surface::Agents);
            }
            "agent.toggle_backlog" => {
                if let AgentRow::Task(id) = self.selected.clone() {
                    match self.overview.toggle_backlog(&id) {
                        Ok(_) => {
                            let later = self.overview.view.task_locations.get(&id)
                                == Some(&Location::Later);
                            if later {
                                self.overview.view.later_collapsed = false;
                            }
                            self.message = "Tarea movida · el destino original se conserva".into();
                            return self.focus(if later {
                                Surface::Backlog
                            } else {
                                Surface::Agents
                            });
                        }
                        Err(e) => self.message = e,
                    }
                }
            }
            "agent.hide_ready" => {
                if self.surface != Surface::Agents {
                    return Effect::None;
                }
                if let AgentRow::Task(id) = self.selected.clone() {
                    match self.overview.dismiss(&id, self.now()) {
                        Ok(_) => {
                            self.message = "Chat oculto de la lista · X para restaurar".into();
                            self.selected = self
                                .rows(Location::Main)
                                .first()
                                .map(|r| r.0.clone())
                                .unwrap_or(AgentRow::Group("demo-project".into()));
                        }
                        Err(e) => self.message = e,
                    }
                }
            }
            "agent.restore_picker" => self.open_picker(PickerKind::Restore),
            "picker.projects" => self.open_picker(PickerKind::Projects),
            "picker.files" => self.open_picker(PickerKind::Files),
            "picker.buffers" => self.open_picker(PickerKind::Buffers),
            "picker.grep" => self.open_picker(PickerKind::Grep),
            "agent.context" => {
                if self.live_agents
                    && let AgentRow::Task(id) = &self.selected
                {
                    return Effect::AgentContext(id.clone());
                }
                if let Some(a) = self.selected_agent() {
                    self.details = Some(format!(
                        "{}\n\nActividad: {:?}\nFuente: {}\n\nDestino original:\n{:?}\n\nObjetivo / branch / PR: proveedor aún no conectado.\n\nDatos de ejemplo. No se leyó ninguna conversación.",
                        a.label,
                        a.status_kind(self.now()),
                        a.source,
                        a.focus_target()
                    ));
                    return self.focus(Surface::Details);
                }
            }
            "agent.toggle_context" => self.compact_context = !self.compact_context,
            "agent.open_pr" => {
                if self.live_agents
                    && let AgentRow::Task(id) = &self.selected
                {
                    return Effect::AgentPr(id.clone());
                }
                self.message = "Esta tarea de ejemplo no tiene un PR conectado".into()
            }
            "sidebar.toggle_side" => {
                self.right = !self.right;
                let _ = self.overview.view.apply(&[Change::SidebarSide {
                    value: if self.right {
                        Side::RightNormal
                    } else {
                        Side::Left
                    },
                }]);
            }
            "agents.refresh" => return Effect::RefreshAgents,
            "window.close" => {
                self.count = 0;
                return Effect::EditorWindow("q");
            }
            _ => self.message = format!("Acción pendiente: {action}"),
        }
        Effect::None
    }
    /// Page the focused native list using its measured viewport, never the PTY.
    pub fn half_page(
        &mut self,
        down: bool,
        top: usize,
        rows: usize,
        margin: usize,
    ) -> Option<usize> {
        use crate::list_paging::{self, Page};
        let rows = rows.max(1);
        if self.surface == Surface::Picker {
            let picker = self.picker.as_mut()?;
            let amount = (rows / 2).max(1) as isize;
            let page = list_paging::snacks(
                Page {
                    top,
                    cursor: picker.selected,
                },
                picker.results.len(),
                rows,
                margin,
                if down { amount } else { -amount },
            );
            picker.selected = page.cursor;
            picker.top_prefix = None;
            return Some(page.top);
        }
        let slot = match self.surface {
            Surface::Tree => 0,
            Surface::Agents => 1,
            Surface::Backlog => 2,
            _ => return None,
        };
        if self.count > 0 {
            self.half_page_lines[slot] = Some(std::mem::take(&mut self.count));
        }
        let amount = self.half_page_lines[slot].unwrap_or((rows / 2).max(1)) as isize;
        let delta = if down { amount } else { -amount };
        if self.surface == Surface::Tree {
            let page = list_paging::normal(
                Page {
                    top,
                    cursor: self.tree_selected,
                },
                self.tree_rows().len(),
                rows,
                delta,
            );
            self.tree_selected = page.cursor;
            self.tree_scroll_requested = true;
            Some(page.top)
        } else {
            let location = if slot == 2 {
                Location::Later
            } else {
                Location::Main
            };
            let rows_list: Vec<_> = self
                .rows(location)
                .into_iter()
                .filter(|(row, _)| !matches!(row, AgentRow::Backlog))
                .collect();
            let cursor = rows_list
                .iter()
                .position(|(row, _)| row == &self.selected)
                .unwrap_or(0);
            let page = list_paging::normal(Page { top, cursor }, rows_list.len(), rows, delta);
            if let Some((row, _)) = rows_list.get(page.cursor) {
                self.selected = row.clone();
                if slot == 2 {
                    self.backlog_selection = row.clone();
                } else {
                    self.main_selection = row.clone();
                }
            }
            self.agent_scroll_requested = true;
            Some(page.top)
        }
    }
    pub fn take_tree_scroll_request(&mut self) -> bool {
        std::mem::take(&mut self.tree_scroll_requested)
    }
    pub fn explorer_root(&self) -> &Path {
        self.explorer_roots
            .get(&self.project)
            .map(PathBuf::as_path)
            .unwrap_or(self.projects[self.project].path.as_path())
    }
    pub fn navigate_explorer(&mut self, root: PathBuf) {
        self.explorer_roots.insert(self.project, root);
        self.expanded.clear();
        self.entries.clear();
        self.tree_marks.clear();
        self.tree_anchor = None;
        self.tree_selected = 0;
    }
    pub fn tree_rows(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| {
                e.path
                    .ancestors()
                    .skip(1)
                    .take_while(|p| *p != self.explorer_root())
                    .all(|p| self.expanded.contains(p))
            })
            .collect()
    }
    /// Neo-tree commands consume tagged nodes plus the current node, uniquely.
    pub fn tree_selection(&self) -> Vec<PathBuf> {
        let root = self.explorer_root();
        let mut selected: Vec<_> = self
            .tree_marks
            .iter()
            .filter(|p| p.starts_with(root) && self.entries.iter().any(|e| &e.path == *p))
            .cloned()
            .collect();
        for path in self.tree_range() {
            if !selected.contains(&path) {
                selected.push(path);
            }
        }
        selected
    }
    /// Visible row range. Anchor identity survives index refreshes; missing
    /// anchors collapse to current row rather than selecting unrelated nodes.
    pub fn tree_range(&self) -> Vec<PathBuf> {
        let rows = self.tree_rows();
        let Some(current) = rows.get(self.tree_selected) else {
            return Vec::new();
        };
        let anchor = self
            .tree_anchor
            .as_ref()
            .and_then(|path| rows.iter().position(|e| &e.path == path))
            .unwrap_or(self.tree_selected);
        if self.tree_anchor.is_none() {
            return vec![current.path.clone()];
        }
        rows[anchor.min(self.tree_selected)..=anchor.max(self.tree_selected)]
            .iter()
            .map(|e| e.path.clone())
            .collect()
    }
    fn tree_step(&mut self, delta: isize) {
        let len = self.tree_rows().len();
        if len > 0 {
            let count = std::mem::take(&mut self.count).max(1);
            self.tree_selected = (self.tree_selected as isize + delta * count as isize)
                .clamp(0, len as isize - 1) as usize;
        }
    }
    fn close_tree_node(&mut self) {
        if let Some(entry) = self.tree_rows().get(self.tree_selected) {
            let has_children = entry.directory
                && self
                    .entries
                    .iter()
                    .any(|child| child.path.parent() == Some(&entry.path));
            let target = if has_children && self.expanded.contains(&entry.path) {
                Some(entry.path.clone())
            } else {
                entry.path.parent().map(Path::to_path_buf)
            };
            if let Some(target) = target {
                // Neo-tree close_node keeps the project root expanded.
                if target != self.explorer_root() {
                    self.expanded.remove(&target);
                    if let Some(index) = self.tree_rows().iter().position(|e| e.path == target) {
                        self.tree_selected = index;
                    }
                }
            }
        }
    }
    pub fn open_tree(&mut self) -> Effect {
        if let Some(e) = self.tree_rows().get(self.tree_selected) {
            let path = e.path.clone();
            if e.directory {
                if !self.expanded.remove(&path) {
                    self.expanded.insert(path);
                }
                Effect::TreeRefresh
            } else {
                Effect::File(path)
            }
        } else {
            Effect::None
        }
    }
    /// Reidentify an editor buffer without claiming that the original file or
    /// Explorer entries were moved on disk (e.g. first save or :saveas).
    pub fn rebind_buffer_path(&mut self, from: &Path, to: &Path) {
        let remap = |path: &PathBuf| {
            if path == from {
                to.to_path_buf()
            } else {
                path.clone()
            }
        };
        self.buffers = self.buffers.iter().map(remap).collect();
        self.pinned_buffers = self.pinned_buffers.iter().map(remap).collect();
        self.unnamed = self.unnamed.iter().map(remap).collect();
        self.last_buffer = self.last_buffer.as_ref().map(remap);
        self.previous_buffer = self.previous_buffer.as_ref().map(remap);
        if let Some((path, _)) = &mut self.file {
            *path = remap(path);
        }
    }
    pub fn rename_paths(&mut self, from: &Path, to: &Path) {
        let remap = |path: &PathBuf| {
            path.strip_prefix(from)
                .map(|tail| {
                    if tail.as_os_str().is_empty() {
                        to.to_path_buf()
                    } else {
                        to.join(tail)
                    }
                })
                .unwrap_or_else(|_| path.clone())
        };
        self.buffers = self.buffers.iter().map(remap).collect();
        self.pinned_buffers = self.pinned_buffers.iter().map(remap).collect();
        self.unnamed = self.unnamed.iter().map(remap).collect();
        self.last_buffer = self.last_buffer.as_ref().map(remap);
        self.previous_buffer = self.previous_buffer.as_ref().map(remap);
        if let Some((path, _)) = &mut self.file {
            *path = remap(path);
        }
        self.expanded = self.expanded.iter().map(remap).collect();
        self.tree_marks = self.tree_marks.iter().map(remap).collect();
        self.tree_anchor = self.tree_anchor.as_ref().map(remap);
        for entry in &mut self.entries {
            entry.path = remap(&entry.path);
            if entry.path == to {
                entry.label = to
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
            }
        }
    }
    pub fn open_picker(&mut self, kind: PickerKind) {
        self.clear_picker_message();
        let previous = self.surface;
        let choices = match kind {
            PickerKind::Grep
            | PickerKind::Discover
            | PickerKind::Todo(_)
            | PickerKind::Diagnostics
            | PickerKind::References
            | PickerKind::Themes
            | PickerKind::Fonts
            | PickerKind::PullRequests
            | PickerKind::PrVideos => Vec::new(),
            PickerKind::Projects => self
                .projects
                .iter()
                .enumerate()
                .map(|(i, p)| Choice {
                    label: p.label.clone(),
                    detail: p.path.display().to_string(),
                    value: i.to_string(),
                })
                .collect(),
            PickerKind::Files => self
                .entries
                .iter()
                .filter(|e| !e.directory)
                .map(|e| Choice {
                    label: e
                        .path
                        .strip_prefix(&self.projects[self.project].path)
                        .unwrap_or(&e.path)
                        .display()
                        .to_string(),
                    detail: "Archivo del proyecto".into(),
                    value: e.path.display().to_string(),
                })
                .collect(),
            // The global list spans workspaces and cannot establish access.
            // The view populates this picker from the current workspace's
            // authoritative registry after it opens.
            PickerKind::Buffers => Vec::new(),
            PickerKind::Restore => self
                .overview
                .agents()
                .iter()
                .filter(|a| self.overview.view.is_dismissed(&a.id))
                .map(|a| Choice {
                    label: a.label.clone(),
                    detail: "Volver a Main".into(),
                    value: a.id.clone(),
                })
                .collect(),
        };
        self.picker = Some(Picker {
            kind,
            query: String::new(),
            query_cursor: 0,
            insert: true,
            focus: PickerFocus::Input,
            top_prefix: None,
            choices: Arc::new(choices),
            results: Vec::new(),
            selected: 0,
            marked: Vec::new(),
            marked_absolute: BTreeSet::new(),
            generation: 0,
            previous,
        });
        let _ = self.focus(Surface::Picker);
        self.rank_picker();
    }
    pub fn picker_text(&mut self, text: &str) {
        if let Some(p) = &mut self.picker
            && p.input_active()
            && crate::picker_input::insert(&mut p.query, &mut p.query_cursor, text)
        {
            self.rank_picker();
        }
    }
    pub fn open_locations(&mut self, kind: PickerKind, choices: Vec<Choice>) {
        self.open_picker(kind);
        if let Some(picker) = &mut self.picker {
            picker.choices = Arc::new(choices.into_iter().take(500).collect());
        }
        self.rank_picker();
    }
    /// Replace the buffer picker entries with the current workspace's listed
    /// identities. Terminal values use NUL, which cannot occur in file paths.
    pub fn set_buffer_picker_choices(&mut self, choices: Vec<Choice>) {
        if let Some(picker) = &mut self.picker
            && picker.kind == PickerKind::Buffers
        {
            picker.choices = Arc::new(choices);
            self.rank_picker();
        }
    }
    pub fn cancel_picker(&mut self) -> Effect {
        if let Some(p) = self.picker.take() {
            self.picker_requests.begin();
            self.clear_picker_message();
            return self.focus(p.previous);
        }
        Effect::None
    }
    pub fn select_picker_row(&mut self, index: usize) -> bool {
        let Some(picker) = &mut self.picker else {
            return false;
        };
        if index >= picker.results.len() {
            return false;
        }
        picker.selected = index;
        picker.focus_list();
        true
    }
    fn picker_key(&mut self, key: &str) -> Effect {
        let insert = self.picker.as_ref().is_some_and(|p| p.insert);
        if let Some(p) = &mut self.picker {
            if key == "<M-w>" || (!insert && key == "/") {
                if p.focus == PickerFocus::Input {
                    p.focus_list();
                } else {
                    p.focus_input();
                }
                return Effect::None;
            }
            if p.focus == PickerFocus::List && key == "i" {
                p.focus_input();
                return Effect::None;
            }
            let top_prefix = p
                .top_prefix
                .take()
                .is_some_and(|since| since.elapsed() < std::time::Duration::from_millis(300));
            if !insert && key == "g" {
                if top_prefix {
                    p.selected = 0;
                } else {
                    p.top_prefix = Some(Instant::now());
                }
                return Effect::None;
            }
            if !insert && key == "G" {
                p.selected = p.results.len().saturating_sub(1);
                return Effect::None;
            }
        }
        if let Some(p) = &mut self.picker
            && p.focus == PickerFocus::Input
            && let Some(changed) =
                crate::picker_input::key(&mut p.query, &mut p.query_cursor, insert, key)
        {
            if changed {
                self.rank_picker();
            }
            return Effect::None;
        }
        match key {
            "<Esc>" if insert => {
                if let Some(p) = &mut self.picker {
                    p.insert = false;
                    p.query_cursor = crate::picker_input::previous(&p.query, p.query_cursor);
                }
            }
            "<Esc>" => return self.cancel_picker(),
            "<C-c>" if insert => return self.cancel_picker(),
            "q" if !insert => return self.cancel_picker(),
            "i" | "a" | "I" | "A"
                if !insert
                    && self
                        .picker
                        .as_ref()
                        .is_some_and(|p| p.focus == PickerFocus::Input) =>
            {
                if let Some(p) = &mut self.picker {
                    p.query_cursor = match key {
                        "a" => crate::picker_input::next(&p.query, p.query_cursor),
                        "I" => 0,
                        "A" => p.query.len(),
                        _ => p.query_cursor,
                    };
                    p.insert = true;
                }
            }
            "<Down>" | "<C-j>" | "<C-n>" => {
                if let Some(p) = &mut self.picker {
                    p.step(true);
                }
            }
            "j" if !insert => {
                if let Some(p) = &mut self.picker {
                    p.step(true);
                }
            }
            "<Up>" | "<C-k>" | "<C-p>" => {
                if let Some(p) = &mut self.picker {
                    p.step(false);
                }
            }
            "k" if !insert => {
                if let Some(p) = &mut self.picker {
                    p.step(false);
                }
            }
            "<Tab>" | "<S-Tab>" => {
                if let Some(p) = &mut self.picker
                    && p.kind.multiple()
                {
                    if let Some(choice) = p.results.get(p.selected).and_then(|i| p.choices.get(*i))
                    {
                        if p.marked.iter().any(|c| c.value == choice.value) {
                            p.marked.retain(|c| c.value != choice.value);
                            p.marked_absolute.remove(&choice.value);
                        } else if p.marked.len() < 128 {
                            if p.kind == PickerKind::Files && p.query.starts_with('/') {
                                p.marked_absolute.insert(choice.value.clone());
                            }
                            p.marked.push(choice.clone());
                        } else {
                            self.message = "Selección limitada a 128 elementos".into();
                            return Effect::None;
                        }
                    }
                    p.step(key != "<S-Tab>");
                }
            }
            "<C-a>" => {
                if let Some(p) = &mut self.picker
                    && p.kind.multiple()
                {
                    if p.marked.len() == p.results.len() {
                        p.marked.clear();
                        p.marked_absolute.clear();
                    } else if p.results.len() <= 128 {
                        p.marked = p
                            .results
                            .iter()
                            .filter_map(|i| p.choices.get(*i).cloned())
                            .collect();
                        p.marked_absolute =
                            if p.kind == PickerKind::Files && p.query.starts_with('/') {
                                p.marked.iter().map(|choice| choice.value.clone()).collect()
                            } else {
                                BTreeSet::new()
                            };
                    } else {
                        self.message = "Filtrá la lista: selección limitada a 128 elementos".into();
                    }
                }
            }
            "<Enter>" => return self.choose_picker(),
            "<C-v>"
                if self
                    .picker
                    .as_ref()
                    .is_some_and(|p| p.kind == PickerKind::PullRequests) =>
            {
                return match self.choose_picker() {
                    Effect::PrReview(url) => Effect::PrVideo(url),
                    effect => effect,
                };
            }
            _ => {}
        }
        Effect::None
    }
    pub fn choose_picker(&mut self) -> Effect {
        let Some(p) = &self.picker else {
            return Effect::None;
        };
        let Some(choice) = p.marked.first().cloned().or_else(|| {
            p.results
                .get(p.selected)
                .and_then(|i| p.choices.get(*i))
                .cloned()
        }) else {
            return Effect::None;
        };
        let absolute_file_query = p.kind == PickerKind::Files
            && if p.marked.is_empty() {
                p.query.starts_with('/')
            } else {
                p.marked_absolute.contains(&choice.value)
            };
        let mut paths = Vec::new();
        for choice in &p.marked {
            if p.kind == PickerKind::Buffers && choice.value.starts_with('\0') {
                if choice
                    .value
                    .strip_prefix("\0terminal:")
                    .and_then(|s| s.parse::<u64>().ok())
                    .is_none()
                {
                    self.message = "Terminal seleccionada inválida".into();
                    return Effect::None;
                }
                continue;
            }
            let path: Option<PathBuf> = match p.kind {
                PickerKind::Files | PickerKind::Buffers => Some(choice.value.clone().into()),
                PickerKind::Grep | PickerKind::Todo(_) | PickerKind::Diagnostics => {
                    serde_json::from_str::<(PathBuf, usize, usize)>(&choice.value)
                        .ok()
                        .map(|v| v.0)
                }
                PickerKind::References => {
                    serde_json::from_str::<(PathBuf, serde_json::Value)>(&choice.value)
                        .ok()
                        .map(|v| v.0)
                }
                _ => None,
            };
            let Some(path) = path.filter(|candidate| {
                candidate.is_absolute()
                    && !candidate
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    && (p.kind == PickerKind::Files
                        && p.marked_absolute.contains(&choice.value)
                        && self
                            .validation_root
                            .as_ref()
                            .is_none_or(|root| candidate.starts_with(root))
                        || self
                            .projects
                            .iter()
                            .any(|project| candidate.starts_with(&project.path)))
            }) else {
                self.message = "Selección inválida o fuera de los proyectos explícitos".into();
                return Effect::None;
            };
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        if p.kind == PickerKind::Files
            && paths.len() > 1
            && paths.iter().any(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        matches!(
                            ext.to_ascii_lowercase().as_str(),
                            "mp4" | "mov" | "webm" | "png" | "jpg" | "jpeg" | "gif" | "webp"
                        )
                    })
            })
        {
            self.message = "Abrí imágenes y videos de a uno; multiselección pendiente".into();
            return Effect::None;
        }
        // Snacks lists every marked buffer, then displays the first selected item.
        // Reads remain lazy and still pass through the ordinary confined reader.
        for path in &paths {
            if !self.buffers.contains(&path) {
                self.buffers.push(path.clone());
            }
        }
        let kind = p.kind;
        let previous = p.previous;
        self.picker = None;
        self.picker_requests.begin();
        self.clear_picker_message();
        let _ = self.focus(Surface::Tree);
        match kind {
            PickerKind::PullRequests => {
                let _ = self.focus(previous);
                Effect::PrReview(choice.value)
            }
            PickerKind::PrVideos => {
                let _ = self.focus(previous);
                choice
                    .value
                    .parse()
                    .map(Effect::PrAttachment)
                    .unwrap_or(Effect::None)
            }
            PickerKind::Fonts => {
                let _ = self.focus(previous);
                Effect::Typography(choice.value)
            }
            PickerKind::Themes => {
                let _ = self.focus(previous);
                Effect::Theme(choice.value)
            }
            PickerKind::Grep | PickerKind::Todo(_) | PickerKind::Diagnostics => {
                // Results replace the window that opened the picker, just as
                // ordinary file picks do. A synthetic Tree focus here makes a
                // guarded async read look like it came from another pane.
                let _ = self.focus(previous);
                serde_json::from_str::<(PathBuf, usize, usize)>(&choice.value)
                    .map(|(p, l, c)| Effect::FileAt(p, l, c))
                    .unwrap_or(Effect::None)
            }
            PickerKind::References => {
                let _ = self.focus(previous);
                serde_json::from_str::<(PathBuf, serde_json::Value)>(&choice.value)
                    .map(|(path, position)| Effect::LocationUtf16(path, position))
                    .unwrap_or(Effect::None)
            }
            PickerKind::Discover => {
                let path: PathBuf = choice.value.into();
                let index = self
                    .projects
                    .iter()
                    .position(|p| p.path == path)
                    .unwrap_or_else(|| {
                        self.projects.push(Project {
                            label: choice.label,
                            path,
                        });
                        self.projects.len() - 1
                    });
                Effect::Project(index)
            }
            PickerKind::Projects => {
                if let Ok(index) = choice.value.parse() {
                    Effect::Project(index)
                } else {
                    let path = PathBuf::from(choice.value);
                    if !path.is_absolute() {
                        return Effect::None;
                    }
                    let index = self
                        .projects
                        .iter()
                        .position(|p| p.path == path)
                        .unwrap_or_else(|| {
                            self.projects.push(Project {
                                label: choice.label,
                                path,
                            });
                            self.projects.len() - 1
                        });
                    Effect::Project(index)
                }
            }
            PickerKind::Buffers if choice.value.starts_with('\0') => {
                let _ = self.focus(previous);
                choice
                    .value
                    .strip_prefix("\0terminal:")
                    .and_then(|value| value.parse().ok())
                    .map(Effect::TerminalBuffer)
                    .unwrap_or(Effect::None)
            }
            PickerKind::Files | PickerKind::Buffers => {
                // A media preview may overlay the current buffer instead of
                // opening a new one. Preserve the source focus for its close
                // action, and while ordinary files are loading asynchronously.
                let _ = self.focus(previous);
                if kind == PickerKind::Files && paths.len() > 1 {
                    return Effect::MarkedFiles(paths, absolute_file_query);
                }
                if absolute_file_query {
                    Effect::ExplicitFile(choice.value.into())
                } else {
                    Effect::File(choice.value.into())
                }
            }
            PickerKind::Restore => {
                if let Err(e) = self.overview.restore(&choice.value) {
                    self.message = e;
                }
                self.selected = AgentRow::Task(choice.value);
                let _ = self.focus(Surface::Agents);
                Effect::None
            }
        }
    }
    fn rank_picker(&mut self) {
        let Some(p) = &mut self.picker else {
            return;
        };
        self.generation += 1;
        p.generation = self.generation;
        p.selected = 0;
        // Do not allow Enter to open a stale result while the new query is
        // still being resolved by the background worker.
        p.results.clear();
        let generation = p.generation;
        let choices = p.choices.clone();
        let query = p.query.clone();
        let request = self.picker_requests.begin();
        let tx = self.picker_tx.clone();
        if matches!(
            p.kind,
            PickerKind::Files
                | PickerKind::Grep
                | PickerKind::Discover
                | PickerKind::Todo(_)
                | PickerKind::Projects
        ) {
            let root = self.projects[self.project].path.clone();
            let validation_root = self.validation_root.clone();
            let kind = p.kind;
            let configured = self.snacks_projects.clone();
            let remembered = self
                .projects
                .iter()
                .map(|p| p.path.clone())
                .collect::<Vec<_>>();
            let labels = self
                .projects
                .iter()
                .map(|p| (p.path.clone(), p.label.clone()))
                .collect::<BTreeMap<_, _>>();
            std::thread::spawn(move || {
                let mut warning = None;
                let choices = if kind == PickerKind::Projects {
                    let paths = match configured {
                        Some((config, home)) => config.discover(&home, &remembered, &request),
                        None => Ok(remembered),
                    };
                    paths.map(|paths| {
                        paths
                            .into_iter()
                            .map(|path| Choice {
                                label: labels.get(&path).cloned().unwrap_or_else(|| {
                                    path.file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned()
                                }),
                                detail: path.display().to_string(),
                                value: path.display().to_string(),
                            })
                            .collect()
                    })
                } else if kind == PickerKind::Files && query.starts_with('/') {
                    absolute_file_choices(&query, validation_root.as_deref(), &|| {
                        request.cancelled()
                    })
                    .map(|(choices, partial)| {
                        if partial {
                            warning = Some(PARTIAL_ABSOLUTE_SEARCH.into());
                        }
                        choices
                    })
                } else if kind == PickerKind::Files {
                    scan_project_while(&root, &|| request.cancelled()).map(|entries| {
                        entries
                            .into_iter()
                            .filter(|e| !e.directory)
                            .map(|e| Choice {
                                label: e
                                    .path
                                    .strip_prefix(&root)
                                    .unwrap_or(&e.path)
                                    .display()
                                    .to_string(),
                                detail: "Archivo de la carpeta actual".into(),
                                value: e.path.display().to_string(),
                            })
                            .collect()
                    })
                } else if matches!(kind, PickerKind::Grep | PickerKind::Todo(_)) {
                    let pattern = match kind {
                        PickerKind::Todo(true) => r"\b(TODO|FIX|FIXME):",
                        PickerKind::Todo(false) => {
                            r"\b(TODO|FIX|FIXME|BUG|ISSUE|HACK|WARN|WARNING|PERF|OPTIM|OPTIMIZE|PERFORMANCE|NOTE|INFO|TEST|TESTING|PASSED|FAILED):"
                        }
                        _ => &query,
                    };
                    scan_project_while(&root, &|| request.cancelled())
                        .and_then(|entries| {
                            let paths = entries
                                .into_iter()
                                .filter(|e| !e.directory)
                                .map(|e| e.path)
                                .collect::<Vec<_>>();
                            crate::search::grep(&root, &paths, pattern, &request)
                        })
                        .map(|hits| {
                            hits.into_iter()
                                .filter(|h| {
                                    !matches!(kind, PickerKind::Todo(_))
                                        || query.is_empty()
                                        || format!("{} {}", h.path.display(), h.preview)
                                            .to_lowercase()
                                            .contains(&query.to_lowercase())
                                })
                                .map(|h| Choice {
                                    label: format!(
                                        "{}:{}:{}",
                                        h.path.strip_prefix(&root).unwrap_or(&h.path).display(),
                                        h.line,
                                        h.column
                                    ),
                                    detail: h.preview,
                                    value: serde_json::to_string(&(h.path, h.line, h.column))
                                        .unwrap_or_default(),
                                })
                                .collect::<Vec<_>>()
                        })
                } else {
                    crate::search::projects(&root, &request).map(|paths| {
                        paths
                            .into_iter()
                            .filter(|p| {
                                query.is_empty()
                                    || p.to_string_lossy()
                                        .to_lowercase()
                                        .contains(&query.to_lowercase())
                            })
                            .map(|p| Choice {
                                label: p
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned(),
                                detail: p.display().to_string(),
                                value: p.display().to_string(),
                            })
                            .collect::<Vec<_>>()
                    })
                };
                if !request.cancelled() {
                    let update = match choices {
                        Ok(choices) => {
                            let results =
                                if matches!(kind, PickerKind::Files | PickerKind::Projects) {
                                    let candidates = choices
                                        .iter()
                                        .map(|c| {
                                            if kind == PickerKind::Projects {
                                                c.detail.clone()
                                            } else {
                                                c.label.clone()
                                            }
                                        })
                                        .collect::<Vec<_>>();
                                    rank(&request, &query, &candidates, 100)
                                        .unwrap_or_default()
                                        .into_iter()
                                        .map(|m| m.index)
                                        .collect()
                                } else {
                                    (0..choices.len()).collect()
                                };
                            PickerUpdate {
                                generation,
                                results,
                                choices: Some(choices),
                                error: warning,
                            }
                        }
                        Err(e) => PickerUpdate {
                            generation,
                            results: Vec::new(),
                            choices: Some(Vec::new()),
                            error: Some(e),
                        },
                    };
                    let _ = tx.send(update);
                }
            });
            return;
        }
        std::thread::spawn(move || {
            let candidates = choices.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
            if let Ok(matches) = rank(&request, &query, &candidates, 100) {
                let _ = tx.send(PickerUpdate {
                    generation,
                    results: matches.iter().map(|m| m.index).collect(),
                    choices: None,
                    error: None,
                });
            }
        });
    }
    pub fn poll_picker(&mut self) -> bool {
        let mut changed = false;
        while let Ok(update) = self.picker_rx.try_recv() {
            if let Some(p) = &mut self.picker
                && p.generation == update.generation
            {
                p.results = update.results;
                if let Some(choices) = update.choices {
                    p.choices = Arc::new(choices);
                }
                self.clear_picker_message();
                if let Some(error) = update.error {
                    self.message = error.clone();
                    self.picker_message = Some(error);
                }
                changed = true;
            }
        }
        changed
    }
    fn clear_picker_message(&mut self) {
        if let Some(previous) = self.picker_message.take()
            && self.message == previous
        {
            self.message.clear();
        }
    }
}

/// Filesystem browser: only enumerate the root and branches the user expanded.
/// Never recurse into unopened directories or follow symlinks implicitly.
pub fn scan_expanded(root: &Path, expanded: &BTreeSet<PathBuf>) -> Result<Vec<Entry>, String> {
    fn walk(
        path: &Path,
        depth: usize,
        expanded: &BTreeSet<PathBuf>,
        out: &mut Vec<Entry>,
    ) -> Result<(), String> {
        if depth > 32 || out.len() >= 10000 {
            return Ok(());
        }
        let mut children = std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .take(10000usize.saturating_sub(out.len()))
            .collect::<Vec<_>>();
        children.sort_by_key(|e| (!e.file_type().is_ok_and(|t| t.is_dir()), e.file_name()));
        for child in children {
            if out.len() >= 10000 {
                break;
            }
            let Ok(ty) = child.file_type() else { continue };
            let p = child.path();
            // Symlink folders can be entered explicitly with '.', never traversed by expansion.
            let directory = ty.is_dir() || (ty.is_symlink() && p.is_dir());
            out.push(Entry {
                path: p.clone(),
                label: child.file_name().to_string_lossy().into_owned(),
                depth,
                directory,
            });
            if ty.is_dir() && expanded.contains(&p) {
                let _ = walk(&p, depth + 1, expanded, out);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, 0, expanded, &mut out)?;
    Ok(out)
}

/// Search below the explicitly typed absolute parent, with bounded work.
/// The breadth-first traversal does not recurse through discovered symlink directories.
fn absolute_file_choices(
    query: &str,
    validation_root: Option<&Path>,
    cancelled: &impl Fn() -> bool,
) -> Result<(Vec<Choice>, bool), String> {
    let path = Path::new(query);
    let parent = if query.ends_with('/') {
        path
    } else {
        path.parent().unwrap_or(Path::new("/"))
    };
    if let Some(root) = validation_root {
        let relative = parent
            .strip_prefix(root)
            .map_err(|_| "Validación: ruta fuera de fixtures")?;
        let mut current = root.to_path_buf();
        for part in relative.components() {
            let std::path::Component::Normal(name) = part else {
                return Err("Validación: ruta fuera de fixtures".into());
            };
            current.push(name);
            let metadata = std::fs::symlink_metadata(&current)
                .map_err(|_| "Validación: ruta no disponible")?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("Validación: enlaces no permitidos".into());
            }
        }
    }
    const MAX_ENTRIES: usize = 10_000;
    const MAX_DEPTH: usize = 12;
    let mut pending = std::collections::VecDeque::from([(parent.to_path_buf(), 0usize)]);
    let mut choices = Vec::new();
    let mut visited = 0usize;
    let mut partial = false;
    while let Some((directory, depth)) = pending.pop_front() {
        if cancelled() {
            return Err("cancelled".into());
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            partial = true;
            continue;
        };
        let mut batch = Vec::new();
        for entry in entries.take(MAX_ENTRIES.saturating_sub(visited)) {
            if cancelled() {
                return Err("cancelled".into());
            }
            visited += 1;
            match entry {
                Ok(entry) => batch.push(entry),
                Err(_) => partial = true,
            }
        }
        batch.sort_by_key(|entry| entry.file_name());
        for entry in batch {
            if cancelled() {
                return Err("cancelled".into());
            }
            let Ok(kind) = entry.file_type() else {
                partial = true;
                continue;
            };
            let path = entry.path();
            if kind.is_dir() && depth < MAX_DEPTH {
                pending.push_back((path, depth + 1));
            } else if kind.is_dir() {
                partial = true;
            } else if kind.is_file()
                || (validation_root.is_none() && kind.is_symlink() && path.is_file())
            {
                let value = path.to_string_lossy().into_owned();
                choices.push(Choice {
                    label: value.clone(),
                    detail: "Ruta absoluta".into(),
                    value,
                });
            }
        }
        if visited >= MAX_ENTRIES {
            partial = true;
            break;
        }
    }
    Ok((choices, partial))
}

pub fn scan_project(root: &Path) -> Result<Vec<Entry>, String> {
    scan_project_while(root, &|| false)
}
fn scan_project_while(root: &Path, cancelled: &dyn Fn() -> bool) -> Result<Vec<Entry>, String> {
    let mut result = Vec::new();
    fn walk(
        root: &Path,
        path: &Path,
        depth: usize,
        out: &mut Vec<Entry>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        if cancelled() {
            return Err("cancelled".into());
        }
        if depth > 12 || out.len() >= 10000 {
            return Ok(());
        }
        let mut entries = std::fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .take(10000_usize.saturating_sub(out.len()))
            .collect::<Vec<_>>();
        entries.sort_by_key(|e| {
            let dir = e.file_type().is_ok_and(|t| t.is_dir());
            (!dir, e.file_name())
        });
        for e in entries {
            if cancelled() {
                return Err("cancelled".into());
            }
            if out.len() >= 10000 {
                break;
            }
            let ty = e.file_type().map_err(|e| e.to_string())?;
            if ty.is_symlink() {
                continue;
            }
            let path = e.path();
            if !path.starts_with(root) {
                continue;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" || name == ".worktrees" {
                continue;
            }
            out.push(Entry {
                path: path.clone(),
                label: name,
                depth,
                directory: ty.is_dir(),
            });
            if ty.is_dir() {
                // Inaccessible child folders must not hide accessible siblings.
                let _ = walk(root, &path, depth + 1, out, cancelled);
            }
        }
        Ok(())
    }
    walk(root, root, 0, &mut result, cancelled)?;
    if cancelled() {
        return Err("cancelled".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn leader_buffer_and_pane_actions_route_from_normal_editor() {
        for (keys, expected) in [
            (["<Space>", "u", "w"], Effect::ToggleWrap),
            (["<Space>", "b", "d"], Effect::DeleteBuffer),
            (["<Space>", "b", "p"], Effect::ToggleBufferPin),
            (["<Space>", "w", "q"], Effect::EditorWindow("q")),
            (["<Space>", "w", "d"], Effect::EditorWindow("q")),
        ] {
            let mut w = model();
            w.focus(Surface::File);
            for key in &keys[..2] {
                assert_eq!(w.key(key, key.as_bytes()), Effect::None);
            }
            assert_eq!(w.key(keys[2], keys[2].as_bytes()), expected);
        }
    }
    mod paging_tests {
        include!("workspace_paging_tests.rs");
    }
    mod picker_focus_tests {
        include!("workspace_picker_focus_tests.rs");
    }
    #[test]
    fn terminal_normal_leader_splits_reach_window_actions() {
        for (key, expected) in [("|", "v"), ("-", "s")] {
            let mut w = model();
            w.focus(Surface::Normal);
            assert_eq!(w.key("<Space>", b" "), Effect::None);
            assert_eq!(w.key(key, key.as_bytes()), Effect::EditorWindow(expected));
        }
    }
    #[test]
    fn workspace_cycle_shortcuts_do_not_consume_terminal_input() {
        for surface in [Surface::Tree, Surface::Normal, Surface::File] {
            let mut w = model();
            w.focus(surface);
            assert_eq!(w.key("<Space>", b" "), Effect::None);
            assert_eq!(w.key("w", b"w"), Effect::None);
            assert_eq!(w.key("n", b"n"), Effect::CycleWorkspace(true));
            assert_eq!(w.key("<Space>", b" "), Effect::None);
            assert_eq!(w.key("w", b"w"), Effect::None);
            assert_eq!(w.key("p", b"p"), Effect::CycleWorkspace(false));
        }
        let mut w = model();
        w.focus(Surface::Terminal);
        assert_eq!(w.key("w", b"w"), Effect::Pty(b"w".to_vec()));
    }
    #[test]
    fn focus_crosses_only_visible_neighbors_and_restores_cached_buffer() {
        for right in [false, true] {
            let mut w = model();
            w.right = right;
            let path = w.projects[0].path.join("synthetic.txt");
            w.activate_file(path.clone(), "cached".into());
            w.focus(Surface::File);
            let to_sidebar = if right { "focus.right" } else { "focus.left" };
            let to_content = if right { "focus.left" } else { "focus.right" };
            w.action(to_content);
            assert_eq!(w.surface, Surface::File);
            w.action(to_sidebar);
            assert_eq!(w.surface, Surface::Tree);
            w.action("focus.down");
            assert_eq!(w.surface, Surface::Agents);
            w.action("focus.up");
            assert_eq!(w.surface, Surface::Tree);
            w.action(to_content);
            assert_eq!(w.surface, Surface::File);
            assert_eq!(w.file.as_ref().unwrap().0, path);
            w.file = None; // Terminal display hides the cached editor.
            w.focus(Surface::Tree);
            w.action(to_content);
            assert_eq!(w.surface, Surface::File);
            assert_eq!(w.file.as_ref().unwrap().0, path);
            w.action("focus.up");
            w.action("focus.down");
            assert_eq!(w.surface, Surface::File);
        }
    }
    #[test]
    fn control_w_directions_and_cycle_preserve_the_actual_content_mode() {
        for right in [false, true] {
            for content in [Surface::Normal, Surface::File] {
                let mut w = model();
                w.right = right;
                if content == Surface::File {
                    w.activate_file(PathBuf::from("/demo/src/main.rs"), "retained".into());
                    w.focus(Surface::File);
                } else {
                    w.focus(content);
                }
                let to_side = if right { "l" } else { "h" };
                let to_main = if right { "h" } else { "l" };
                assert_eq!(w.key("<C-w>", b"\x17"), Effect::None);
                w.key(to_side, to_side.as_bytes());
                assert_eq!(w.surface, Surface::Tree);
                w.key("<C-w>", b"\x17");
                w.key("j", b"j");
                assert_eq!(w.surface, Surface::Agents);
                w.key("<C-w>", b"\x17");
                w.key("<C-k>", b"\x0b");
                assert_eq!(w.surface, Surface::Tree);
                w.key("<C-w>", b"\x17");
                w.key(to_main, to_main.as_bytes());
                assert_eq!(w.surface, content);
                for _ in 0..3 {
                    w.key("<C-w>", b"\x17");
                    w.key("w", b"w");
                }
                assert_eq!(w.surface, content);
                w.key("<C-w>", b"\x17");
                w.key("W", b"W");
                w.key("<C-w>", b"\x17");
                w.key("<C-w>", b"\x17");
                assert_eq!(w.surface, content);
            }
        }
        let mut w = model();
        w.activate_file(PathBuf::from("/demo/src/main.rs"), "cached".into());
        w.focus(Surface::Terminal); // Explicit terminal choice must beat cached file.
        w.action("focus.left");
        w.action("focus.right");
        assert_eq!(w.surface, Surface::Terminal);
        w.focus(Surface::Normal);
        w.sidebar = false;
        w.key("<C-w>", b"\x17");
        w.key("w", b"w");
        assert_eq!(w.surface, Surface::Normal);
        assert!(!w.sidebar);
    }
    #[test]
    fn standard_terminal_normal_sequence_and_failed_prefix_preserve_bytes() {
        let mut w = model();
        w.focus(Surface::Terminal);
        assert_eq!(w.key("<C-\\>", b"\x1c"), Effect::None);
        assert_eq!(w.key("<C-n>", b"\x0e"), Effect::TerminalNormal(true));
        assert_eq!(w.surface, Surface::Normal);
        w.key("i", b"i");
        assert_eq!(w.key("<C-w>", b"\x17"), Effect::Pty(vec![23]));
        w.key("<C-\\>", b"\x1c");
        assert_eq!(w.expire_prefix(), Effect::Pty(vec![28]));
        w.key("<C-\\>", b"\x1c");
        assert_eq!(w.key("x", b"x"), Effect::Pty(vec![28, b'x']));
        w.focus(Surface::Tree);
        w.key("<C-w>", b"\x17");
        w.key("<Esc>", b"\x1b");
        assert_eq!(w.surface, Surface::Tree);
        assert!(w.pending_since.is_none());
    }
    #[test]
    fn window_focus_replays_clean_neovim_reference() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-window-focus.json"
        ))
        .unwrap();
        for case in reference["cases"].as_array().unwrap() {
            let mut w = model();
            w.right = true;
            w.focus(Surface::Normal);
            for (command, expected) in case["commands"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["observations"].as_array().unwrap())
            {
                for key in command.as_array().unwrap() {
                    w.key(key.as_str().unwrap(), b"");
                }
                let actual = match w.surface {
                    Surface::Normal => 1,
                    Surface::Tree => 2,
                    Surface::Agents => 3,
                    _ => panic!("Unexpected surface"),
                };
                assert_eq!(
                    actual,
                    expected["window"].as_u64().unwrap(),
                    "{} / {command}",
                    case["label"]
                );
            }
        }
    }
    #[test]
    fn counted_window_commands_replay_neovim_and_consume_counts() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-window-count.json"
        ))
        .unwrap();
        for case in reference["cases"].as_array().unwrap() {
            let mut w = model();
            w.right = true;
            w.focus(Surface::Normal);
            for (command, expected) in case["commands"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["observations"].as_array().unwrap())
            {
                for key in command.as_array().unwrap() {
                    w.key(key.as_str().unwrap(), b"");
                }
                let actual = match w.surface {
                    Surface::Normal => 1,
                    Surface::Tree => 2,
                    Surface::Agents => 3,
                    _ => panic!("Unexpected surface"),
                };
                assert_eq!(
                    actual,
                    expected["window"].as_u64().unwrap(),
                    "{} / {command}",
                    case["label"]
                );
                assert_eq!(w.count, 0);
                assert!(!w.window_prefix);
            }
        }
    }
    #[test]
    fn tree_extremes_and_numbered_rows_replay_neovim_reference() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-list-extremes.json"
        ))
        .unwrap();
        for case in reference["cases"].as_array().unwrap() {
            let mut w = model();
            let root = w.projects[0].path.clone();
            w.entries = (1..=8)
                .map(|n| Entry {
                    path: root.join(format!("file-{n}")),
                    label: format!("file-{n}"),
                    depth: 0,
                    directory: false,
                })
                .collect();
            w.tree_selected = 3;
            for (command, expected) in case["commands"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["observations"].as_array().unwrap())
            {
                for key in command.as_array().unwrap() {
                    w.key(key.as_str().unwrap(), b"");
                }
                assert_eq!(
                    w.tree_selected + 1,
                    expected["line"].as_u64().unwrap() as usize,
                    "{} / {command}",
                    case["label"]
                );
                assert_eq!(w.count, 0);
                assert!(
                    w.take_tree_scroll_request(),
                    "boundary motion must reveal after wheel scrolling"
                );
            }
        }
    }
    #[test]
    fn list_extremes_use_visible_rows_preserve_marks_and_keep_agent_selection_namespaces() {
        let mut w = model();
        let root = w.projects[0].path.clone();
        w.entries = [("a", true, 0), ("a/child", false, 1), ("z", false, 0)]
            .into_iter()
            .map(|(path, directory, depth)| Entry {
                path: root.join(path),
                label: path.into(),
                directory,
                depth,
            })
            .collect();
        w.tree_marks.insert(root.join("z"));
        w.key("G", b"G");
        assert_eq!(w.tree_selected, 1, "collapsed children do not count");
        w.key("v", b"v");
        w.key("g", b"g");
        w.key("g", b"g");
        assert_eq!(w.tree_range().len(), 2);
        assert!(w.tree_marks.contains(&root.join("z")));
        w.entries.clear();
        w.key("9", b"9");
        w.key("G", b"G");
        assert_eq!(w.tree_selected, 0);
        assert_eq!(w.count, 0);
        for surface in [Surface::Agents, Surface::Backlog] {
            let mut w = model();
            w.focus(Surface::Agents);
            if surface == Surface::Backlog {
                w.selected = AgentRow::Task("codex-thread:demo-ready".into());
                w.key("m", b"m");
            }
            assert_eq!(w.surface, surface);
            let location = if surface == Surface::Backlog {
                Location::Later
            } else {
                Location::Main
            };
            let rows = w.rows(location);
            w.key("G", b"G");
            assert_eq!(w.selected, rows.last().unwrap().0);
            assert!(w.take_agent_scroll_request());
            w.key("g", b"g");
            w.key("g", b"g");
            assert_eq!(w.selected, rows.first().unwrap().0);
            w.key("9", b"9");
            w.key("9", b"9");
            w.key("G", b"G");
            assert_eq!(w.selected, rows.last().unwrap().0);
            assert_eq!(w.count, 0);
            assert_eq!(
                w.selected,
                if surface == Surface::Backlog {
                    w.backlog_selection
                } else {
                    w.main_selection
                }
            );
        }
    }
    #[test]
    fn window_prefix_counts_cancel_on_escape_invalid_key_timeout_and_focus_change() {
        for cancel in ["escape", "invalid", "expire", "focus"] {
            let mut w = model();
            w.focus(Surface::Normal);
            for key in ["2", "<C-w>", "3"] {
                w.key(key, b"");
            }
            assert!(w.window_prefix);
            match cancel {
                "escape" => {
                    w.key("<Esc>", b"\x1b");
                }
                "invalid" => {
                    w.key("!", b"!");
                }
                "expire" => {
                    w.expire_prefix();
                }
                _ => {
                    w.focus(Surface::Normal);
                }
            }
            assert_eq!(w.count, 0);
            assert_eq!(w.window_after_count, 0);
            assert!(!w.window_prefix);
            assert_eq!(w.key("j", b"j"), Effect::TerminalMotion("j".into(), 1));
        }
        let mut w = model();
        w.focus(Surface::Terminal);
        assert_eq!(w.key("<C-w>", b"\x17"), Effect::Pty(vec![23]));
        assert_eq!(w.key("2", b"2"), Effect::Pty(b"2".to_vec()));
        assert!(!w.window_prefix);
    }
    #[test]
    fn window_history_survives_overlays_modes_and_failed_motion_without_reopening_hidden_panels() {
        let mut w = model();
        w.right = true;
        w.focus(Surface::File);
        w.focus(Surface::Agents);
        w.open_picker(PickerKind::Files);
        w.cancel_picker();
        assert_eq!(w.surface, Surface::Agents);
        w.action("focus.last_accessed");
        // No actual file is active, so the content fallback resumes the terminal.
        assert_eq!(w.surface, Surface::Terminal);
        w.focus(Surface::Normal); // Mode change must not replace previous window.
        w.action("focus.left"); // No window on this side.
        w.action("focus.last_accessed");
        assert_eq!(w.surface, Surface::Agents);
        w.focus(Surface::Normal);
        w.agents_visible = false;
        w.action("focus.last_accessed");
        assert_eq!(w.surface, Surface::Normal);
        assert!(!w.agents_visible);
        w.sidebar = false;
        for action in ["focus.first", "focus.last", "focus.next", "focus.previous"] {
            w.action(action);
            assert_eq!(w.surface, Surface::Normal);
            assert!(!w.sidebar);
        }
    }
    #[test]
    fn window_edges_follow_sidebar_geometry_and_previous_restores_backlog_view() {
        for right in [false, true] {
            let mut w = model();
            w.right = right;
            w.focus(Surface::Normal);
            w.action("focus.first");
            assert_eq!(
                w.surface,
                if right {
                    Surface::Normal
                } else {
                    Surface::Tree
                }
            );
            w.action("focus.last");
            assert_eq!(
                w.surface,
                if right {
                    Surface::Agents
                } else {
                    Surface::Normal
                }
            );
            w.focus(Surface::Backlog);
            w.focus(Surface::Normal);
            w.action("focus.last_accessed");
            assert_eq!(w.surface, Surface::Backlog);
        }
    }
    #[test]
    fn terminal_normal_releases_input_and_routes_motion_without_shell_bytes() {
        let mut w = model();
        w.focus(Surface::Terminal);
        assert_eq!(w.key("<Esc>", b"\x1b"), Effect::None);
        assert_eq!(w.key("<Esc>", b"\x1b"), Effect::TerminalNormal(true));
        assert_eq!(w.surface, Surface::Normal);
        w.key("2", b"2");
        assert_eq!(w.key("k", b"k"), Effect::TerminalMotion("k".into(), 2));
        assert_eq!(w.key("v", b"v"), Effect::TerminalMotion("v".into(), 1));
        assert_eq!(w.key("g", b"g"), Effect::None);
        assert_eq!(w.key("g", b"g"), Effect::TerminalMotion("gg".into(), 1));
        assert_eq!(w.key("i", b"i"), Effect::TerminalNormal(false));
        assert_eq!(w.surface, Surface::Terminal);
        assert_eq!(w.key("w", b"w"), Effect::Pty(b"w".to_vec()));
        assert_eq!(w.key("q", b"q"), Effect::Pty(b"q".to_vec()));
        w.focus(Surface::Normal);
        assert_eq!(w.key("2", b"2"), Effect::None);
        assert_eq!(w.key("q", b"q"), Effect::EditorWindow("q"));
        assert_eq!(w.count, 0);
        assert_eq!(w.surface, Surface::Normal);
        w.focus(Surface::Tree);
        assert_eq!(
            w.action("terminal.focus_root"),
            Effect::TerminalNormal(false)
        );
        assert_eq!(w.surface, Surface::Terminal);
    }
    #[test]
    fn terminal_normal_routes_counted_jump_keys_without_sending_them_to_shell() {
        let mut w = model();
        w.focus(Surface::Normal);
        assert_eq!(w.key("<C-o>", b"\x0f"), Effect::TerminalJump(true, 1));
        assert_eq!(w.key("3", b"3"), Effect::None);
        assert_eq!(w.key("<C-i>", b"\t"), Effect::TerminalJump(false, 3));
        assert_eq!(w.key("<Tab>", b"\t"), Effect::TerminalJump(false, 1));
        w.focus(Surface::Terminal);
        assert_eq!(w.key("<C-i>", b"\t"), Effect::Pty(b"\t".to_vec()));
    }
    #[test]
    fn buffer_picker_returns_a_typed_terminal_without_treating_it_as_a_file() {
        let mut w = model();
        w.focus(Surface::File);
        w.open_picker(PickerKind::Buffers);
        let terminal = Choice {
            label: "Terminal".into(),
            detail: "fixture".into(),
            value: "\0terminal:42".into(),
        };
        w.set_buffer_picker_choices(vec![terminal.clone()]);
        let picker = w.picker.as_mut().unwrap();
        picker.results = vec![0];
        picker.marked.push(terminal);
        assert_eq!(w.choose_picker(), Effect::TerminalBuffer(42));
        assert!(w.buffers.is_empty());
        assert_eq!(w.surface, Surface::File);
    }
    #[test]
    fn wrap_leader_is_normal_only_and_does_not_change_shell_input() {
        for origin in [
            Surface::Tree,
            Surface::Normal,
            Surface::Agents,
            Surface::File,
        ] {
            let mut w = model();
            w.focus(origin);
            assert_eq!(w.key("<Space>", b" "), Effect::None);
            assert_eq!(w.key("u", b"u"), Effect::None);
            assert_eq!(w.key("w", b"w"), Effect::ToggleWrap);
        }
        let mut w = model();
        w.focus(Surface::Terminal);
        for (key, bytes) in [("<Space>", b" ".as_slice()), ("u", b"u"), ("w", b"w")] {
            assert_eq!(w.key(key, bytes), Effect::Pty(bytes.to_vec()));
        }
    }
    #[test]
    fn new_file_leader_is_normal_only_and_does_not_change_shell_input() {
        for origin in [
            Surface::Tree,
            Surface::Normal,
            Surface::Agents,
            Surface::File,
        ] {
            let mut w = model();
            w.focus(origin);
            w.key("<Space>", b" ");
            w.key("f", b"f");
            assert_eq!(w.key("n", b"n"), Effect::NewFile);
            assert!(w.buffers.is_empty());
        }
        let mut w = model();
        w.focus(Surface::Terminal);
        for (key, bytes) in [("<Space>", b" ".as_slice()), ("f", b"f"), ("n", b"n")] {
            assert_eq!(w.key(key, bytes), Effect::Pty(bytes.to_vec()));
        }
        let root = w.projects[0].path.clone();
        let path = crate::unnamed::id(&root);
        w.unnamed.insert(path.clone());
        w.activate_file(path.clone(), String::new());
        assert_eq!(w.buffer_label(&path), "[No Name]");
        w.open_picker(PickerKind::Buffers);
        assert!(w.picker.as_ref().unwrap().choices.is_empty());
        w.set_buffer_picker_choices(vec![Choice {
            label: w.buffer_label(&path),
            detail: "Buffer sin nombre · :w ruta".into(),
            value: path.display().to_string(),
        }]);
        assert_eq!(w.picker.as_ref().unwrap().choices[0].label, "[No Name]");
    }
    #[test]
    fn buffer_picker_starts_empty_until_workspace_registry_supplies_choices() {
        let mut w = model();
        w.buffers = vec!["/example/demo/other-workspace.txt".into()];
        w.open_picker(PickerKind::Buffers);
        assert!(w.picker.as_ref().unwrap().choices.is_empty());
        assert!(w.picker.as_ref().unwrap().results.is_empty());
        assert_eq!(w.choose_picker(), Effect::None);
        let authorized = Choice {
            label: "local.txt".into(),
            detail: "local".into(),
            value: "/example/demo/local.txt".into(),
        };
        w.set_buffer_picker_choices(vec![authorized.clone()]);
        let choices = &w.picker.as_ref().unwrap().choices;
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].value, authorized.value);
    }
    #[test]
    fn normal_profile_starts_at_home_without_synthetic_agents_or_discovery() {
        // Deliberately nonexistent paths: startup must not inspect personal data.
        let mut normal = Workspace::from_profile(
            Path::new("/synthetic-home-not-created"),
            Path::new("/synthetic-state-not-created"),
            false,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(normal.projects.len(), 1);
        assert_eq!(normal.projects[normal.project].label, "Home");
        assert_eq!(
            normal.projects[normal.project].path,
            Path::new("/synthetic-home-not-created")
        );
        assert!(normal.overview.agents().is_empty());
        assert!(normal.entries.is_empty());
        assert!(normal.live_agents);
        assert!(normal.snacks_projects.is_some());
        normal.focus(Surface::Agents);
        normal.key("j", b"j");
        normal.key("k", b"k");
        assert!(normal.overview.agents().is_empty());
        assert!(
            Workspace::from_profile(Path::new("relative"), Path::new("/state"), false, &[], None)
                .is_err()
        );
    }
    #[test]
    fn validation_profile_keeps_examples_inside_its_private_state() {
        let validation = Workspace::from_profile(
            Path::new(""),
            Path::new("/synthetic-state-not-created"),
            true,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(validation.projects.len(), 3);
        assert!(
            validation
                .projects
                .iter()
                .all(|p| p.path.starts_with("/synthetic-state-not-created/demo"))
        );
        assert!(!validation.overview.agents().is_empty());
        assert!(!validation.live_agents);
        let (config, virtual_home) = validation.snacks_projects.as_ref().unwrap();
        assert_eq!(virtual_home, Path::new("/synthetic-state-not-created/demo"));
        assert_eq!(config.dev, ["~/snacks-projects"]);
        assert_eq!(config.projects, ["~/workspace"]);
        assert!(!config.recent);
    }
    #[test]
    fn validation_project_picker_discovers_packaged_marker_inside_demo_only() {
        let state =
            std::env::temp_dir().join(format!("tw-snacks-validation-{}", std::process::id()));
        let project = state.join("demo/snacks-projects/discovered-demo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("compose.yaml"),
            include_bytes!(
                "../../../Resources/Demo/projects/snacks-projects/discovered-demo/compose.yaml"
            ),
        )
        .unwrap();
        // The supplied HOME is deliberately unusable. Only state/demo is expanded.
        let mut model = Workspace::from_profile(
            Path::new("/unreadable-personal-home"),
            &state,
            true,
            &[],
            None,
        )
        .unwrap();
        model.open_picker(PickerKind::Projects);
        model.picker.as_mut().unwrap().query = "discovered-demo".into();
        model.rank_picker();
        for _ in 0..100 {
            model.poll_picker();
            if model.picker.as_ref().is_some_and(|p| {
                p.results.len() == 1
                    && p.choices[p.results[0]].value == project.display().to_string()
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let picker = model.picker.as_ref().unwrap();
        assert_eq!(picker.results.len(), 1);
        assert_eq!(
            picker.choices[picker.results[0]].value,
            project.display().to_string()
        );
        assert!(
            picker
                .choices
                .iter()
                .all(|c| Path::new(&c.value).starts_with(state.join("demo")))
        );
        assert_eq!(model.projects.len(), 3);
        assert_eq!(model.choose_picker(), Effect::Project(3));
        assert_eq!(model.projects[3].path, project);
        std::fs::remove_dir_all(state).unwrap();
    }
    #[test]
    fn browser_lists_only_expanded_directories_and_explicit_root_navigation() {
        let root = std::env::temp_dir().join(format!("tw-browser-{}", std::process::id()));
        std::fs::create_dir_all(root.join("child/deep")).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("child/deep/hidden.txt"), "test").unwrap();
        let mut expanded = BTreeSet::new();
        assert_eq!(scan_expanded(&root, &expanded).unwrap().len(), 1);
        expanded.insert(root.join("child"));
        let entries = scan_expanded(&root, &expanded).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(!entries.iter().any(|e| e.label == "hidden.txt"));
        let mut w = model();
        w.projects[0].path = root.clone();
        w.entries = entries;
        w.surface = Surface::Tree;
        assert_eq!(w.key(".", b"."), Effect::Navigate(root.join("child")));
        assert_eq!(
            w.key("<BS>", b""),
            Effect::Navigate(root.parent().unwrap().to_owned())
        );
        assert_eq!(w.open_tree(), Effect::TreeRefresh);
        // Search must still find files below closed tree branches.
        w.entries = scan_expanded(&root, &BTreeSet::new()).unwrap();
        w.open_picker(PickerKind::Grep);
        w.picker_text("test");
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while !w.picker.as_ref().is_some_and(|p| !p.results.is_empty()) {
            w.poll_picker();
            assert!(Instant::now() < deadline, "grep below closed branch");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            w.choose_picker(),
            Effect::FileAt(root.join("child/deep/hidden.txt"), 1, 1)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn model() -> Workspace {
        let bindings: Vec<Binding> =
            serde_json::from_str(include_str!("../../../Resources/Contracts/keymaps.json"))
                .unwrap();
        let agents: Vec<Agent> = serde_json::from_str(include_str!(
            "../../../Resources/Contracts/agents-demo.json"
        ))
        .unwrap();
        Workspace::new(
            vec![Project {
                label: "demo".into(),
                path: "/example/demo".into(),
            }],
            agents,
            &bindings,
        )
        .unwrap()
    }
    #[test]
    fn resume_timeout_preserves_real_router_control_prefix_without_replay() {
        use crate::terminal_resume::{Context, Fence, ResumeTimeout};
        for (chord, bytes, expected) in [
            ("<Esc>", &b"\x1b"[..], "1b"),
            ("<C-\\>", &b"\x1c"[..], "1c"),
        ] {
            let mut w = model();
            w.focus(Surface::Terminal);
            assert_eq!(w.key(chord, bytes), Effect::None);
            let now = Instant::now();
            let context = Context {
                project: 0,
                attachment: 42,
            };
            let mut fence = Fence::default();
            let token = fence.begin(context, now);
            let timeout = |w: &mut Workspace, fence: &mut Fence, at| {
                fence.mark_expired_with_prefix(at, || match w.focus(Surface::Normal) {
                    Effect::Pty(prefix) => prefix,
                    _ => Vec::new(),
                })
            };
            assert_eq!(timeout(&mut w, &mut fence, now), ResumeTimeout::NotExpired);
            assert_eq!(w.surface, Surface::Terminal);
            assert_eq!(
                timeout(&mut w, &mut fence, now + std::time::Duration::from_secs(16)),
                ResumeTimeout::Retained
            );
            let old = fence.retained_guard().unwrap();
            assert_eq!(fence.prefix_hex_if_guarded(&old).as_deref(), Some(expected));
            assert!(fence.ack(context, token, true, true, now).is_none());
            assert!(fence.active());
            assert!(!fence.cancel_guarded(&old));
            let current = fence.retained_guard().unwrap();
            assert_eq!(
                fence.prefix_hex_if_guarded(&current).as_deref(),
                Some(expected)
            );
            assert!(fence.cancel_guarded(&current));
        }
    }
    #[test]
    fn failed_resume_ack_preserves_router_prefix_and_distinct_input_batches() {
        use crate::terminal_resume::{Context, Fence};
        for (chord, bytes, expected) in [
            ("<Esc>", &b"\x1b"[..], "1b"),
            ("<C-\\>", &b"\x1c"[..], "1c"),
        ] {
            let mut w = model();
            w.focus(Surface::Terminal);
            let now = Instant::now();
            let context = Context {
                project: 0,
                attachment: 42,
            };
            let mut fence = Fence::default();
            let token = fence.begin(context, now);
            fence.push(context, b"possibly sent").unwrap();
            assert_eq!(
                fence.ack(context, token, true, true, now),
                Some(b"possibly sent".to_vec())
            );
            fence.push(context, b"not sent").unwrap();
            assert_eq!(w.key(chord, bytes), Effect::None);
            let mut take = || match w.focus(Surface::Normal) {
                Effect::Pty(prefix) => prefix,
                _ => Vec::new(),
            };
            assert!(
                fence
                    .ack_with_failure_prefix(context, token + 1, false, false, now, &mut take)
                    .is_none()
            );
            assert!(
                fence
                    .ack_with_failure_prefix(context, token, false, false, now, &mut take)
                    .is_none()
            );
            assert_eq!(w.surface, Surface::Normal);
            let guard = fence.retained_guard().unwrap();
            assert_eq!(
                fence.prefix_hex_if_guarded(&guard).as_deref(),
                Some(expected)
            );
            assert_eq!(fence.text_if_guarded(&guard), Some("not sent"));
            assert_eq!(
                fence.uncertain_text_if_guarded(&guard),
                Some("possibly sent")
            );
            assert!(fence.recovery_blocked());
            assert!(fence.cancel_guarded(&guard));
        }
    }
    #[test]
    fn failed_ready_ack_retains_prefix_even_without_buffered_text() {
        use crate::terminal_resume::{Context, Fence};
        let mut w = model();
        w.focus(Surface::Terminal);
        let now = Instant::now();
        let context = Context {
            project: 0,
            attachment: 42,
        };
        let mut fence = Fence::default();
        let token = fence.begin(context, now);
        assert_eq!(w.key("<Esc>", b"\x1b"), Effect::None);
        assert!(
            fence
                .ack_with_failure_prefix(context, token, true, false, now, || {
                    match w.focus(Surface::Normal) {
                        Effect::Pty(prefix) => prefix,
                        _ => Vec::new(),
                    }
                })
                .is_none()
        );
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.prefix_hex_if_guarded(&guard).as_deref(), Some("1b"));
        assert_eq!(fence.text_if_guarded(&guard), None);
        assert_eq!(fence.uncertain_text_if_guarded(&guard), None);
        assert!(fence.recovery_blocked());
    }
    #[test]
    fn pending_terminal_prefix_defers_focus_until_resume_batches_are_acknowledged() {
        use crate::terminal_resume::{Context, Fence, router_prefix_blocks_detach};
        let mut w = model();
        w.focus(Surface::Terminal);
        let now = Instant::now();
        let context = Context {
            project: 0,
            attachment: 42,
        };
        let mut fence = Fence::default();
        let token = fence.begin(context, now);
        fence.push(context, b"earlier").unwrap();
        assert_eq!(w.key("<Esc>", b"\x1b"), Effect::None);
        let blocked = |f: &Fence, w: &Workspace| {
            router_prefix_blocks_detach(
                f,
                w.surface == Surface::Terminal,
                w.pending_since.is_some(),
            )
        };
        assert!(blocked(&fence, &w));
        assert_eq!(
            fence.ack(context, token, true, true, now),
            Some(b"earlier".to_vec())
        );
        assert!(blocked(&fence, &w));
        assert!(fence.ack(context, token, false, true, now).is_none());
        assert!(!blocked(&fence, &w));
        assert_eq!(w.expire_prefix(), Effect::Pty(b"\x1b".to_vec()));
    }
    fn file_choices(count: usize) -> Vec<Choice> {
        (0..count)
            .map(|i| Choice {
                label: format!("file{i}"),
                detail: String::new(),
                value: format!("/example/demo/file{i}.txt"),
            })
            .collect()
    }
    #[test]
    fn long_tree_scroll_requests_follow_counted_visual_motion_and_boundaries() {
        let mut w = model();
        let root = w.projects[0].path.clone();
        w.entries = (0..240)
            .map(|row| Entry {
                path: root.join(format!("file-{row:03}")),
                label: format!("file-{row:03}"),
                depth: 0,
                directory: false,
            })
            .collect();
        assert!(!w.take_tree_scroll_request());
        for key in ["1", "2", "0"] {
            w.key(key, b"");
            assert!(!w.take_tree_scroll_request());
        }
        w.key("j", b"");
        assert_eq!(w.tree_selected, 120);
        assert!(w.take_tree_scroll_request());
        assert!(!w.take_tree_scroll_request());
        for key in ["V", "9", "9", "9", "<Down>"] {
            w.key(key, b"");
        }
        assert_eq!(w.tree_selected, 239);
        assert_eq!(w.tree_range().len(), 120);
        assert!(w.take_tree_scroll_request());
        w.key("j", b"");
        assert_eq!(w.tree_selected, 239);
        assert!(w.take_tree_scroll_request());
        for key in ["9", "9", "9", "<Up>"] {
            w.key(key, b"");
        }
        assert_eq!(w.tree_selected, 0);
        assert_eq!(w.tree_range().len(), 121);
        assert!(w.take_tree_scroll_request());
        w.key("k", b"");
        assert!(w.take_tree_scroll_request());
        w.focus(Surface::Terminal);
        w.key("j", b"j");
        assert!(!w.take_tree_scroll_request());
    }
    #[test]
    fn visual_tree_range_toggles_all_rows_exits_and_preserves_tags_on_escape() {
        let mut w = model();
        let root = w.projects[0].path.clone();
        w.entries = ["a", "b", "c"]
            .into_iter()
            .map(|name| Entry {
                path: root.join(name),
                label: name.into(),
                depth: 0,
                directory: false,
            })
            .collect();
        for key in ["<Tab>", "j", "v", "j"] {
            w.key(key, b"");
        }
        assert_eq!(w.tree_range(), vec![root.join("b"), root.join("c")]);
        assert_eq!(
            w.tree_selection(),
            vec![root.join("a"), root.join("b"), root.join("c")]
        );
        w.key("<Esc>", b"");
        assert!(w.tree_anchor.is_none());
        assert_eq!(w.tree_marks.len(), 1);
        for key in ["V", "k", "<Tab>"] {
            w.key(key, b"");
        }
        assert!(w.tree_anchor.is_none());
        assert_eq!(w.tree_marks.len(), 3);
        for key in ["v", "j", "<Tab>"] {
            w.key(key, b"");
        }
        assert_eq!(w.tree_marks, BTreeSet::from([root.join("a")]));
    }
    #[test]
    fn visual_tree_range_rejects_over_limit_without_partial_marks_and_handles_missing_anchor() {
        let mut w = model();
        let root = w.projects[0].path.clone();
        w.entries = (0..129)
            .map(|i| Entry {
                path: root.join(format!("{i:03}")),
                label: i.to_string(),
                depth: 0,
                directory: false,
            })
            .collect();
        w.key("v", b"");
        w.tree_selected = 128;
        w.key("<Tab>", b"");
        assert!(w.tree_marks.is_empty() && w.tree_anchor.is_some());
        w.tree_anchor = Some(root.join("gone"));
        assert_eq!(w.tree_range(), vec![root.join("128")]);
        w.key("<Tab>", b"");
        assert!(w.tree_marks.contains(&root.join("128")) && w.tree_anchor.is_none());
    }
    #[test]
    fn tree_tags_do_not_advance_and_commands_include_current_unique() {
        let mut w = model();
        let root = w.projects[0].path.clone();
        w.entries = ["a", "b", "c"]
            .into_iter()
            .map(|name| Entry {
                path: root.join(name),
                label: name.into(),
                depth: 0,
                directory: false,
            })
            .collect();
        w.key("<Tab>", b"");
        assert_eq!(w.tree_selected, 0);
        assert_eq!(w.tree_selection(), vec![root.join("a")]);
        w.key("j", b"j");
        assert_eq!(w.tree_selection(), vec![root.join("a"), root.join("b")]);
        w.key("<Tab>", b"");
        w.key("<Tab>", b"");
        assert_eq!(w.tree_marks.len(), 1);
        w.rename_paths(&root.join("a"), &root.join("renamed"));
        assert!(w.tree_marks.contains(&root.join("renamed")));
        w.key("<C-;>", b"");
        assert!(w.tree_marks.is_empty());
        assert_eq!(w.tree_selection(), vec![root.join("b")]);
    }
    #[test]
    fn picker_tabs_toggle_and_confirm_selection_order_with_lazy_buffers() {
        let mut s = model();
        s.open_locations(PickerKind::Files, file_choices(4));
        let p = s.picker.as_mut().unwrap();
        p.results = vec![0, 1, 2, 3];
        p.selected = 2;
        s.key("<Tab>", b"");
        assert_eq!(s.picker.as_ref().unwrap().selected, 3);
        s.key("<S-Tab>", b"");
        assert_eq!(s.picker.as_ref().unwrap().selected, 2);
        s.key("<Tab>", b""); // Unmark file2, leaving file3 first.
        assert_eq!(s.picker.as_ref().unwrap().marked.len(), 1);
        assert_eq!(
            s.choose_picker(),
            Effect::File("/example/demo/file3.txt".into())
        );
        assert_eq!(s.buffers, vec![PathBuf::from("/example/demo/file3.txt")]);
        assert!(s.file.is_none()); // Only the normal async reader can load content.
    }
    #[test]
    fn picker_marks_survive_filtering_and_escape_discards_them() {
        let mut s = model();
        s.open_locations(PickerKind::Files, file_choices(3));
        s.picker.as_mut().unwrap().results = vec![2, 0, 1];
        s.key("<Tab>", b"");
        s.key("<Tab>", b"");
        s.picker.as_mut().unwrap().results.clear(); // Both marks filtered out.
        assert_eq!(
            s.choose_picker(),
            Effect::File("/example/demo/file2.txt".into())
        );
        assert_eq!(
            s.buffers,
            vec![
                PathBuf::from("/example/demo/file2.txt"),
                PathBuf::from("/example/demo/file0.txt")
            ]
        );
        s.open_locations(PickerKind::Files, file_choices(3));
        s.picker.as_mut().unwrap().results = vec![1];
        s.key("<Tab>", b"");
        s.key("<Esc>", b"");
        assert!(!s.picker.as_ref().unwrap().insert);
        s.key("<Esc>", b"");
        assert!(s.picker.is_none());
        assert!(
            !s.buffers
                .contains(&PathBuf::from("/example/demo/file1.txt"))
        );
        s.open_picker(PickerKind::Buffers);
        assert!(s.picker.as_ref().unwrap().marked.is_empty());
    }
    #[test]
    fn picker_select_all_replaces_filtered_selection_and_budget_is_atomic() {
        let mut s = model();
        s.open_locations(PickerKind::Files, file_choices(129));
        s.picker.as_mut().unwrap().results = (0..129).collect();
        s.key("<C-a>", b"");
        assert!(s.picker.as_ref().unwrap().marked.is_empty());
        s.picker.as_mut().unwrap().results = vec![3, 1];
        s.key("<C-a>", b"");
        assert_eq!(s.picker.as_ref().unwrap().marked.len(), 2);
        assert!(
            s.picker.as_ref().unwrap().marked[0]
                .value
                .ends_with("file3.txt")
        );
        s.key("<C-a>", b"");
        assert!(s.picker.as_ref().unwrap().marked.is_empty());
        s.picker.as_mut().unwrap().results = vec![3, 3];
        s.key("<C-a>", b"");
        s.key("<Tab>", b"");
        assert!(s.picker.as_ref().unwrap().marked.is_empty());
    }
    #[test]
    fn picker_locations_list_all_buffers_but_navigate_only_to_first_position() {
        let mut s = model();
        let choices = [0, 1].map(|i| Choice {
            label: format!("hit{i}"),
            detail: String::new(),
            value: serde_json::to_string(&(
                PathBuf::from(format!("/example/demo/file{i}.txt")),
                i + 2,
                i + 4,
            ))
            .unwrap(),
        });
        s.open_locations(PickerKind::Diagnostics, choices.into());
        s.picker.as_mut().unwrap().results = vec![1, 0];
        s.key("<C-a>", b"");
        assert_eq!(
            s.choose_picker(),
            Effect::FileAt("/example/demo/file1.txt".into(), 3, 5)
        );
        assert_eq!(s.buffers.len(), 2);
    }
    #[test]
    fn picker_rejects_marked_escape_without_partial_buffer_registration() {
        let mut s = model();
        let mut choices = file_choices(1);
        choices.push(Choice {
            label: "escape".into(),
            detail: String::new(),
            value: "/example/demo/../outside.txt".into(),
        });
        s.open_locations(PickerKind::Files, choices);
        s.picker.as_mut().unwrap().results = vec![0, 1];
        s.key("<C-a>", b"");
        assert_eq!(s.choose_picker(), Effect::None);
        assert!(s.buffers.is_empty());
        assert!(s.picker.is_some());
        s.open_picker(PickerKind::Projects);
        s.picker.as_mut().unwrap().results = vec![0];
        s.key("<Tab>", b"");
        s.key("<C-a>", b"");
        assert!(s.picker.as_ref().unwrap().marked.is_empty());
    }
    #[test]
    fn configured_project_picker_worker_filters_imports_and_cancels() {
        let root = std::env::temp_dir().join(format!("tw-snacks-picker-{}", std::process::id()));
        std::fs::create_dir_all(root.join("dev/alpha/.git")).unwrap();
        let mut s = model();
        s.snacks_projects = Some((
            crate::snacks_projects::Config {
                patterns: vec![".git".into()],
                dev: vec!["~/dev".into()],
                projects: vec!["~/explicit".into()],
                max_depth: 3,
                recent: false,
            },
            root.clone(),
        ));
        s.open_picker(PickerKind::Projects);
        s.picker.as_mut().unwrap().query = "alpha".into();
        s.rank_picker();
        for _ in 0..100 {
            s.poll_picker();
            if s.picker.as_ref().is_some_and(|p| {
                p.results.len() == 1 && p.choices[p.results[0]].value.ends_with("/dev/alpha")
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let picker = s.picker.as_ref().unwrap();
        assert_eq!(picker.results.len(), 1);
        assert_eq!(
            picker.choices[picker.results[0]].value,
            root.join("dev/alpha").display().to_string()
        );
        assert_eq!(s.projects.len(), 1);
        assert_eq!(s.choose_picker(), Effect::Project(1));
        assert_eq!(s.projects[1].path, root.join("dev/alpha"));
        s.open_picker(PickerKind::Projects);
        s.key("<Esc>", b"\x1b");
        s.key("<Esc>", b"\x1b");
        s.poll_picker();
        assert!(s.picker.is_none());
        // Cancellation worker touches directory names only, so preserve the fixture until it stops.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn project_picker_imports_paths_without_overwriting_remembered_roots() {
        let mut s = model();
        let original = s.projects[0].path.clone();
        let path = PathBuf::from("/synthetic/imported/nvim");
        s.open_locations(
            PickerKind::Projects,
            vec![Choice {
                label: "nvim".into(),
                detail: path.display().to_string(),
                value: path.display().to_string(),
            }],
        );
        s.picker.as_mut().unwrap().results = vec![0];
        let count = s.projects.len();
        assert_eq!(s.choose_picker(), Effect::Project(count));
        assert_eq!(s.projects[0].path, original);
        assert_eq!(s.projects[count].path, path);
        s.open_locations(
            PickerKind::Projects,
            vec![Choice {
                label: "nvim".into(),
                detail: String::new(),
                value: path.display().to_string(),
            }],
        );
        s.picker.as_mut().unwrap().results = vec![0];
        assert_eq!(s.choose_picker(), Effect::Project(count));
        assert_eq!(s.projects.len(), count + 1);
    }
    #[test]
    fn explorer_navigation_keeps_workspace_and_restores_each_root() {
        let mut w = model();
        w.projects.push(Project {
            label: "other".into(),
            path: "/synthetic/other".into(),
        });
        let original = w.projects[0].path.clone();
        let count = w.projects.len();
        w.navigate_explorer(PathBuf::from("/synthetic/explorer/child"));
        assert_eq!(w.project, 0);
        assert_eq!(w.projects.len(), count);
        assert_eq!(w.projects[0].path, original);
        w.focus(Surface::Tree);
        assert_eq!(
            w.key("<BS>", b""),
            Effect::Navigate(PathBuf::from("/synthetic/explorer"))
        );
        w.project = 1;
        let second = w.projects[1].path.clone();
        assert_eq!(w.explorer_root(), second);
        w.project = 0;
        assert_eq!(w.explorer_root(), Path::new("/synthetic/explorer/child"));
        assert_eq!(w.key("<Space>", b" "), Effect::None);
        assert_eq!(w.key("w", b"w"), Effect::None);
        assert_eq!(w.key("x", b"x"), Effect::CloseWorkspace);
    }
    #[test]
    fn absolute_picker_finds_explicit_parent_without_using_project_root() {
        let root = std::env::temp_dir().join(format!("tw-absolute-picker-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("example file.txt");
        std::fs::write(&file, "synthetic").unwrap();
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let deep_file = nested.join("example deep.txt");
        std::fs::write(&deep_file, "synthetic").unwrap();
        let link = root.join("alias");
        std::os::unix::fs::symlink(&nested, &link).unwrap();
        let file_link = root.join("alias-file.txt");
        std::os::unix::fs::symlink(&file, &file_link).unwrap();
        let (choices, partial) =
            absolute_file_choices(file.to_str().unwrap(), None, &|| false).unwrap();
        assert!(!partial);
        assert!(
            choices
                .iter()
                .any(|choice| Path::new(&choice.value) == file)
        );
        assert!(
            choices
                .iter()
                .any(|choice| Path::new(&choice.value) == deep_file)
        );
        let query = root.join("example").to_string_lossy().into_owned();
        let labels = choices
            .iter()
            .map(|choice| choice.label.clone())
            .collect::<Vec<_>>();
        let request = PickerRequests::default().begin();
        let matches = workspace_core::picker::rank(&request, &query, &labels, 100).unwrap();
        assert!(
            matches
                .iter()
                .any(|item| Path::new(&choices[item.index].value) == deep_file)
        );
        assert!(
            choices
                .iter()
                .any(|choice| Path::new(&choice.value) == file_link)
        );
        assert!(
            !choices
                .iter()
                .any(|choice| Path::new(&choice.value).starts_with(&link))
        );
        assert!(absolute_file_choices(file.to_str().unwrap(), None, &|| true).is_err());
        let checks = std::cell::Cell::new(0usize);
        assert!(
            absolute_file_choices(file.to_str().unwrap(), None, &|| {
                checks.set(checks.get() + 1);
                checks.get() > 3
            })
            .is_err()
        );
        assert!(absolute_file_choices(file.to_str().unwrap(), Some(&nested), &|| false).is_err());
        assert!(
            absolute_file_choices(&format!("{}/example", link.display()), Some(&root), &|| {
                false
            })
            .is_err()
        );
        let (validated, _) =
            absolute_file_choices(file.to_str().unwrap(), Some(&root), &|| false).unwrap();
        assert!(
            validated
                .iter()
                .any(|choice| Path::new(&choice.value) == deep_file)
        );
        assert!(
            !validated
                .iter()
                .any(|choice| Path::new(&choice.value) == file_link)
        );
        let mut deep_dirs = Vec::new();
        let mut path = root.clone();
        for _ in 0..14 {
            path.push("child");
            std::fs::create_dir(&path).unwrap();
            deep_dirs.push(path.clone());
        }
        let beyond = path.join("beyond.txt");
        std::fs::write(&beyond, "synthetic").unwrap();
        let (bounded, partial) =
            absolute_file_choices(file.to_str().unwrap(), None, &|| false).unwrap();
        assert!(partial);
        assert!(
            !bounded
                .iter()
                .any(|choice| Path::new(&choice.value) == beyond)
        );
        std::fs::remove_file(beyond).unwrap();
        for directory in deep_dirs.iter().rev() {
            std::fs::remove_dir(directory).unwrap();
        }
        std::fs::remove_file(file_link).unwrap();
        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(deep_file).unwrap();
        std::fs::remove_dir(nested).unwrap();
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn picker_does_not_open_old_results_while_absolute_query_is_loading() {
        let mut w = model();
        w.open_locations(
            PickerKind::Files,
            vec![Choice {
                label: "old.txt".into(),
                detail: String::new(),
                value: "/example/demo/old.txt".into(),
            }],
        );
        w.picker.as_mut().unwrap().results = vec![0];
        w.picker_text("/synthetic/new");
        assert!(w.picker.as_ref().unwrap().results.is_empty());
        assert_eq!(w.choose_picker(), Effect::None);
    }
    #[test]
    fn file_picker_preserves_origin_and_buffer_for_a_media_overlay() {
        for origin in [Surface::File, Surface::Tree, Surface::Normal] {
            let mut s = model();
            let original = PathBuf::from("/example/demo/notes.txt");
            s.file = Some((original.clone(), "unchanged".into()));
            s.buffers.push(original.clone());
            s.focus(origin);
            s.open_locations(
                PickerKind::Files,
                vec![Choice {
                    label: "clip.mov".into(),
                    detail: String::new(),
                    value: "/example/demo/clip.mov".into(),
                }],
            );
            // Ranking is a separate worker; pin this single known result.
            s.picker.as_mut().unwrap().results = vec![0];
            assert_eq!(
                s.choose_picker(),
                Effect::File("/example/demo/clip.mov".into())
            );
            assert_eq!(s.surface, origin);
            assert_eq!(s.file, Some((original.clone(), "unchanged".into())));
            assert_eq!(s.buffers, vec![original]);
        }
    }
    #[test]
    fn location_pickers_restore_the_origin_before_opening_results() {
        let path = PathBuf::from("/example/demo/target.txt");
        for origin in [Surface::File, Surface::Tree, Surface::Normal] {
            for kind in [
                PickerKind::Grep,
                PickerKind::Todo(false),
                PickerKind::Todo(true),
                PickerKind::Diagnostics,
                PickerKind::References,
            ] {
                let mut workspace = model();
                let _ = workspace.focus(origin);
                let value = if kind == PickerKind::References {
                    serde_json::to_string(&(
                        path.clone(),
                        serde_json::json!({"line": 2, "character": 3}),
                    ))
                    .unwrap()
                } else {
                    serde_json::to_string(&(path.clone(), 3, 4)).unwrap()
                };
                workspace.open_locations(
                    kind,
                    vec![Choice {
                        label: "target".into(),
                        detail: String::new(),
                        value,
                    }],
                );
                workspace.picker.as_mut().unwrap().results = vec![0];
                let effect = workspace.choose_picker();
                if kind == PickerKind::References {
                    assert_eq!(
                        effect,
                        Effect::LocationUtf16(
                            path.clone(),
                            serde_json::json!({"line": 2, "character": 3})
                        )
                    );
                } else {
                    assert_eq!(effect, Effect::FileAt(path.clone(), 3, 4));
                }
                assert_eq!(
                    workspace.surface, origin,
                    "origin {origin:?}, picker {kind:?}"
                );
            }
        }
    }
    #[test]
    fn pending_open_detects_picker_round_trip_and_ignores_redundant_focus() {
        use crate::editor_panes::take_changed_focus;
        for origin in [Surface::File, Surface::Tree, Surface::Normal] {
            let mut workspace = model();
            let _ = workspace.focus(origin);
            let generation = 7;
            let mut pending = Some((generation, workspace.focus_revision));
            let _ = workspace.focus(origin);
            assert!(!take_changed_focus(
                &mut pending,
                generation,
                workspace.focus_revision
            ));
            pending = Some((generation, workspace.focus_revision));
            workspace.open_locations(PickerKind::Files, Vec::new());
            let _ = workspace.cancel_picker();
            assert_eq!(workspace.surface, origin);
            assert!(take_changed_focus(
                &mut pending,
                generation,
                workspace.focus_revision
            ));
            assert!(pending.is_none());

            // A delayed response from an older generation cannot consume the
            // new request's focus guard.
            pending = Some((generation + 1, workspace.focus_revision));
            assert!(!take_changed_focus(
                &mut pending,
                generation,
                workspace.focus_revision
            ));
            assert_eq!(pending, Some((generation + 1, workspace.focus_revision)));
        }
    }
    #[test]
    fn pr_picker_ctrl_v_is_contextual_and_attachment_selection_restores_the_editor() {
        let mut s = model();
        s.focus(Surface::File);
        let url = "https://github.com/terminal-workspace/synthetic/pull/1";
        s.open_locations(
            PickerKind::PullRequests,
            vec![Choice {
                label: "PR1".into(),
                detail: String::new(),
                value: url.into(),
            }],
        );
        s.picker.as_mut().unwrap().results = vec![0];
        assert_eq!(s.key("<C-v>", b"\x16"), Effect::PrVideo(url.into()));
        assert_eq!(s.surface, Surface::File);
        s.open_locations(
            PickerKind::PrVideos,
            vec![Choice {
                label: "synthetic-first".into(),
                detail: String::new(),
                value: "0".into(),
            }],
        );
        s.picker.as_mut().unwrap().results = vec![0];
        assert_eq!(s.key("<C-v>", b"\x16"), Effect::None);
        assert!(s.picker.is_some());
        assert_eq!(s.key("<Enter>", b"\r"), Effect::PrAttachment(0));
        assert_eq!(s.surface, Surface::File);
    }
    #[test]
    fn theme_picker_returns_to_origin_and_escape_keeps_choice_unapplied() {
        let mut s = model();
        s.focus(Surface::File);
        let choices = || {
            vec![Choice {
                label: "night-owl".into(),
                detail: String::new(),
                value: "night-owl".into(),
            }]
        };
        s.open_locations(PickerKind::Themes, choices());
        s.key("<Esc>", b"\x1b");
        s.key("<Esc>", b"\x1b");
        assert_eq!(s.surface, Surface::File);
        assert!(s.picker.is_none());
        s.open_locations(PickerKind::Themes, choices());
        for _ in 0..100 {
            s.poll_picker();
            if s.picker.as_ref().is_some_and(|p| !p.results.is_empty()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(s.choose_picker(), Effect::Theme("night-owl".into()));
        assert_eq!(s.surface, Surface::File);
    }
    #[test]
    fn task_folds_hide_only_descendants_keep_enter_destination_and_match_leaf_context() {
        let mut s = model();
        let make = |id: &str, parent: Option<&str>| Agent {
            id: id.into(),
            task_id: Some(id.into()),
            source: "codex_app_server".into(),
            status: "idle".into(),
            updated_at: Some(1),
            codex_project_id: Some("group".into()),
            parent_id: parent.map(str::to_owned),
            ..Default::default()
        };
        s.overview
            .replace(
                vec![
                    make("parent", None),
                    make("child", Some("parent")),
                    make("peer", None),
                ],
                1000,
            )
            .unwrap();
        s.focus(Surface::Agents);
        s.selected = AgentRow::Task("parent".into());
        s.key("h", b"h");
        assert_eq!(s.selected, AgentRow::Task("parent".into()));
        assert!(
            s.rows(Location::Main)
                .iter()
                .all(|(row, _)| *row != AgentRow::Task("child".into()))
        );
        assert!(
            s.rows(Location::Main)
                .iter()
                .any(|(row, _)| *row == AgentRow::Task("peer".into()))
        );
        assert_eq!(s.overview.view.collapsed.get("task:parent"), Some(&true));
        assert_ne!(s.overview.view.collapsed.get("group"), Some(&true));
        assert!(
            matches!(s.key("<Enter>",b"\r"),Effect::Open(FocusTarget::CodexTask {task_id,..}) if task_id=="parent")
        );
        s.focus(Surface::Agents);
        s.key("l", b"l");
        assert!(
            s.rows(Location::Main)
                .iter()
                .any(|(row, _)| *row == AgentRow::Task("child".into()))
        );
        s.selected = AgentRow::Task("peer".into());
        s.key("h", b"h");
        assert_ne!(s.overview.view.collapsed.get("group"), Some(&true));
        for id in ["parent", "child"] {
            s.overview.toggle_backlog(id).unwrap();
        }
        s.overview.view.later_collapsed = false;
        s.focus(Surface::Backlog);
        s.selected = AgentRow::Task("parent".into());
        s.key("h", b"h");
        assert!(!s.overview.view.later_collapsed);
        assert!(
            s.rows(Location::Later)
                .iter()
                .all(|(row, _)| *row != AgentRow::Task("child".into()))
        );
        s.key("l", b"l");
        s.selected = AgentRow::Task("child".into());
        s.key("h", b"h");
        assert!(s.overview.view.later_collapsed);
        assert_eq!(s.selected, AgentRow::Backlog);
        s.key("l", b"l");
        assert!(!s.overview.view.later_collapsed);
    }
    #[test]
    fn provider_refresh_reconciles_selection_without_stealing_editor_focus() {
        let mut s = model();
        let make = |id: &str| Agent {
            id: id.into(),
            task_id: Some(id.into()),
            source: "codex_app_server".into(),
            status: "idle".into(),
            updated_at: Some(1),
            codex_project_id: Some("new-project".into()),
            ..Default::default()
        };
        s.focus(Surface::Agents);
        s.overview
            .replace(vec![make("a"), make("b")], 1000)
            .unwrap();
        s.reconcile_agent_selection();
        assert_eq!(s.selected, AgentRow::Group("new-project".into()));
        s.key("j", b"j");
        assert_eq!(s.selected, AgentRow::Task("a".into()));
        s.reconcile_agent_selection();
        assert_eq!(s.selected, AgentRow::Task("a".into()));
        s.overview.replace(vec![make("b")], 1000).unwrap();
        s.reconcile_agent_selection();
        assert_eq!(s.selected, AgentRow::Group("new-project".into()));
        s.focus(Surface::File);
        s.overview.replace(vec![make("c")], 1000).unwrap();
        s.reconcile_agent_selection();
        assert_eq!(s.surface, Surface::File);
        s.focus(Surface::Backlog);
        assert_eq!(s.selected, AgentRow::Backlog);
    }
    #[test]
    fn codex_project_order_is_canonical_and_unassigned_is_not_a_terminal() {
        let mut w = model();
        let make = |id: &str, project: Option<&str>, position: i64| Agent {
            id: id.into(),
            task_id: Some(id.into()),
            source: "codex_app_server".into(),
            status: "idle".into(),
            updated_at: Some(1),
            codex_project_id: project.map(str::to_owned),
            codex_project_name: project.map(str::to_owned),
            codex_project_position: Some(position),
            ..Default::default()
        };
        let agents = vec![
            make("b", Some("second"), 20),
            make("none", None, 0),
            make("a", Some("first"), 10),
        ];
        w.overview.replace(agents.clone(), 1000).unwrap();
        let groups = |w: &Workspace| {
            w.rows(Location::Main)
                .into_iter()
                .filter_map(|(row, _)| {
                    if let AgentRow::Group(key) = row {
                        Some(key)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(groups(&w), ["first", "second", "unassigned"]);
        w.overview
            .replace(agents.into_iter().rev().collect(), 1000)
            .unwrap();
        assert_eq!(groups(&w), ["first", "second", "unassigned"]);
        assert_eq!(w.group_label("unassigned"), "Sin proyecto");
        w.focus(Surface::Agents);
        w.key("k", b"k");
        assert!(w.take_agent_scroll_request());
        assert!(!w.take_agent_scroll_request());
        assert_eq!(w.key("r", b"r"), Effect::RefreshAgents);
        assert_eq!(w.key("R", b"R"), Effect::RefreshAgents);
    }
    #[test]
    fn project_folds_are_independent_in_main_and_backlog() {
        let mut s = model();
        let make = |id: &str| Agent {
            id: id.into(),
            task_id: Some(id.into()),
            source: "codex_app_server".into(),
            status: "idle".into(),
            updated_at: Some(1),
            codex_project_id: Some("same-project".into()),
            ..Default::default()
        };
        s.overview
            .replace(vec![make("main-task"), make("later-task")], 1000)
            .unwrap();
        s.overview.toggle_backlog("later-task").unwrap();
        s.overview.view.later_collapsed = false;
        let visible = |s: &Workspace, location, id: &str| {
            s.rows(location)
                .iter()
                .any(|(row, _)| *row == AgentRow::Task(id.into()))
        };
        s.focus(Surface::Agents);
        s.selected = AgentRow::Group("same-project".into());
        s.key("h", b"h");
        assert!(s.group_collapsed("same-project", Location::Main));
        assert!(!s.group_collapsed("same-project", Location::Later));
        assert!(!visible(&s, Location::Main, "main-task"));
        assert!(visible(&s, Location::Later, "later-task"));
        s.focus(Surface::Backlog);
        s.selected = AgentRow::Group("same-project".into());
        s.key("<Enter>", b"\r");
        assert!(s.group_collapsed("same-project", Location::Later));
        s.focus(Surface::Agents);
        s.selected = AgentRow::Group("same-project".into());
        s.key("l", b"l");
        assert!(visible(&s, Location::Main, "main-task"));
        assert!(!visible(&s, Location::Later, "later-task"));
        s.focus(Surface::Backlog);
        s.selected = AgentRow::Group("same-project".into());
        s.key("<Enter>", b"\r");
        assert!(visible(&s, Location::Later, "later-task"));
        assert!(!s.overview.view.later_collapsed);
    }
    #[test]
    fn buffer_cycles_and_alternate_follow_successful_activation_only() {
        let mut workspace = Workspace::new(
            vec![Project {
                label: "test".into(),
                path: PathBuf::from("/synthetic"),
            }],
            Vec::new(),
            &[],
        )
        .unwrap();
        let a = PathBuf::from("/synthetic/a.rs");
        let b = PathBuf::from("/synthetic/b.rs");
        let c = PathBuf::from("/synthetic/c.rs");
        workspace.activate_file(a.clone(), String::new());
        assert!(matches!(workspace.action("buffers.next"), Effect::None));
        workspace.activate_file(b.clone(), String::new());
        workspace.activate_file(c.clone(), String::new());
        assert!(matches!(workspace.action("buffers.next"),Effect::File(p) if p==a));
        assert!(matches!(workspace.action("buffers.prev"),Effect::File(p) if p==b));
        // Merely requesting an open does not update history (disk may fail/cancel).
        assert!(matches!(workspace.buffer_action("buffers.alternate"),Effect::File(p) if p==b));
        workspace.activate_file(a.clone(), String::new());
        assert!(matches!(workspace.buffer_action("buffers.alternate"),Effect::File(p) if p==c));
        workspace.activate_file(c.clone(), String::new());
        assert!(matches!(workspace.buffer_action("buffers.alternate"),Effect::File(p) if p==a));
        workspace.file = None;
        assert!(matches!(workspace.action("buffers.next"),Effect::File(p) if p==a));
        workspace.buffers.retain(|p| p != &a);
        assert!(matches!(
            workspace.buffer_action("buffers.alternate"),
            Effect::None
        ));
    }
    #[test]
    fn alternate_buffer_shortcuts_defer_to_the_active_panel_and_do_not_steal_shell_input() {
        let mut normal = model();
        normal.focus(Surface::Normal);
        assert_eq!(normal.key("<C-6>", b"\x1e"), Effect::AlternateBuffer);
        assert_eq!(normal.key("<C-^>", b"\x1e"), Effect::AlternateBuffer);
        normal.key("2", b"2");
        assert_eq!(normal.key("<C-6>", b"\x1e"), Effect::NumberedBuffer(2));
        for (origin, sequence) in [
            (Surface::File, vec!["<Space>", "b", "b"]),
            (Surface::Normal, vec!["<Space>", "b", "b"]),
            (Surface::File, vec!["<Space>", "`"]),
        ] {
            let mut w = model();
            w.focus(origin);
            for key in &sequence[..sequence.len() - 1] {
                assert_eq!(w.key(key, b""), Effect::None);
            }
            assert_eq!(
                w.key(sequence.last().unwrap(), b""),
                Effect::AlternateBuffer
            );
        }
        let mut w = model();
        w.focus(Surface::Terminal);
        for (key, bytes) in [("<Space>", b" ".as_slice()), ("b", b"b"), ("b", b"b")] {
            assert_eq!(w.key(key, bytes), Effect::Pty(bytes.to_vec()));
        }
    }
    #[test]
    fn tree_close_selects_parent_and_open_matches_lazyvim_mapping() {
        let mut w = model();
        let root = w.projects[0].path.clone();
        w.entries = [
            ("docs", true, 0),
            ("docs/nested", true, 1),
            ("docs/nested/ñ.txt", false, 2),
            ("docs/empty", true, 1),
            ("top.txt", false, 0),
        ]
        .into_iter()
        .map(|(name, directory, depth)| Entry {
            path: root.join(name),
            label: name.into(),
            depth,
            directory,
        })
        .collect();
        w.expanded
            .extend([root.join("docs"), root.join("docs/nested")]);
        w.tree_selected = 2;
        w.key("h", b"h");
        assert_eq!(
            w.tree_rows()[w.tree_selected].path,
            root.join("docs/nested")
        );
        assert!(!w.expanded.contains(&root.join("docs/nested")));
        w.key("l", b"l");
        w.key("j", b"j");
        assert_eq!(
            w.key("l", b"l"),
            Effect::File(root.join("docs/nested/ñ.txt"))
        );
        w.key("h", b"h");
        w.key("h", b"h");
        assert_eq!(w.tree_rows()[w.tree_selected].path, root.join("docs"));
        assert!(!w.expanded.contains(&root.join("docs")));
        w.key("l", b"l");
        w.tree_selected = 2; // Empty folder closes its parent, as close_node does.
        w.key("h", b"h");
        assert_eq!(w.tree_rows()[w.tree_selected].path, root.join("docs"));
        w.tree_selected = 1; // Root-level file must not collapse the project root.
        w.key("h", b"h");
        assert_eq!(w.tree_rows()[w.tree_selected].path, root.join("top.txt"));
        w.key("9", b"9");
        w.key("h", b"h");
        assert_eq!(w.count, 0);
        w.focus(Surface::Terminal);
        assert_eq!(w.key("h", b"h"), Effect::Pty(vec![b'h']));
    }
    #[test]
    fn tree_edit_keys_and_rename_keep_buffers_alternate_and_descendants() {
        let mut w = model();
        for action in ['a', 'A', 'r', 'm', 'c', 'y', 'x', 'p', 'T', 'u'] {
            w.focus(Surface::Tree);
            assert_eq!(
                w.key(&action.to_string(), action.to_string().as_bytes()),
                Effect::TreeEdit(action)
            );
        }
        let from = PathBuf::from("/example/demo/src/a.rs");
        let to = PathBuf::from("/example/demo/src/renamed.rs");
        let other = PathBuf::from("/example/demo/src/b.rs");
        w.activate_file(from.clone(), "unsaved-state-marker".into());
        w.activate_file(other.clone(), String::new());
        w.rename_paths(&from, &to);
        assert!(
            matches!(w.buffer_action("buffers.alternate"),Effect::File(p) if p.as_os_str()==to.as_os_str())
        );
        assert!(!w.buffers.contains(&from));
        w.activate_file(to.clone(), "unsaved-state-marker".into());
        w.rename_paths(
            Path::new("/example/demo/src"),
            Path::new("/example/demo/code"),
        );
        let (path, text) = w.file.as_ref().unwrap();
        assert_eq!(
            path.as_os_str(),
            Path::new("/example/demo/code/renamed.rs").as_os_str()
        );
        assert_eq!(text, "unsaved-state-marker");
        w.focus(Surface::Terminal);
        assert_eq!(w.key("a", b"a"), Effect::Pty(vec![b'a']));
    }
    #[test]
    fn rebind_buffer_keeps_real_explorer_entries_and_tree_marks() {
        let mut w = model();
        let old = w.projects[0].path.join("old.txt");
        let new = w.projects[0].path.join("new.txt");
        w.entries = vec![Entry {
            path: old.clone(),
            label: "old.txt".into(),
            depth: 0,
            directory: false,
        }];
        w.tree_marks.insert(old.clone());
        w.activate_file(old.clone(), "synthetic".into());
        w.rebind_buffer_path(&old, &new);
        assert_eq!(w.file.as_ref().unwrap().0, new);
        assert!(w.buffers.contains(&new));
        assert_eq!(w.entries[0].path, old);
        assert!(w.tree_marks.contains(&old));
    }
    #[test]
    fn terminal_owns_text_and_prefix_flushes_to_old_target() {
        let mut s = model();
        s.focus(Surface::Terminal);
        assert_eq!(s.key("<Space>", b" "), Effect::Pty(vec![32]));
        assert_eq!(s.key("<Esc>", b"\x1b"), Effect::None);
        assert_eq!(s.focus(Surface::Agents), Effect::Pty(vec![27]));
    }
    #[test]
    fn main_rows_counts_hierarchy_and_ready_hide_are_real_actions() {
        let mut s = model();
        s.focus(Surface::Agents);
        s.key("2", b"2");
        s.key("j", b"j");
        assert_eq!(s.selected, AgentRow::Task("codex-thread:demo-child".into()));
        s.key("x", b"x");
        assert!(!s.overview.view.is_dismissed("codex-thread:demo-child"));
        s.selected = AgentRow::Task("codex-thread:demo-ready".into());
        s.key("x", b"x");
        assert!(s.overview.view.is_dismissed("codex-thread:demo-ready"));
    }
    #[test]
    fn backlog_move_and_return_keep_original_destination() {
        let mut s = model();
        s.focus(Surface::Agents);
        s.selected = AgentRow::Task("codex-thread:demo-ready".into());
        let before = s.selected_agent().unwrap().focus_target();
        s.key("m", b"m");
        assert_eq!(s.surface, Surface::Backlog);
        assert_eq!(s.selected_agent().unwrap().focus_target(), before);
        assert_eq!(s.overview.visible(Location::Later).len(), 1);
        s.key("x", b"x");
        assert!(!s.overview.view.is_dismissed("codex-thread:demo-ready"));
        s.key("<C-k>", b"");
        assert_eq!(s.surface, Surface::Agents);
        assert!(
            s.rows(Location::Main)
                .iter()
                .any(|(row, _)| row == &s.selected)
        );
    }
    #[test]
    fn picker_escape_changes_mode_without_discarding_query_or_selection() {
        let mut s = model();
        s.focus(Surface::File);
        s.open_locations(PickerKind::Files, file_choices(3));
        s.picker.as_mut().unwrap().results = vec![0, 1, 2];
        s.key("<Tab>", b"");
        s.picker_text("file");
        let generation = s.picker.as_ref().unwrap().generation;
        s.key("<Esc>", b"\x1b");
        let p = s.picker.as_ref().unwrap();
        assert_eq!(s.surface, Surface::Picker);
        assert!(!p.insert);
        assert_eq!(p.query, "file");
        assert_eq!(p.marked.len(), 1);
        assert_eq!(
            p.generation, generation,
            "mode change must not cancel ranking"
        );
        s.key("<Esc>", b"\x1b");
        assert_eq!(s.surface, Surface::File);
        assert!(s.picker.is_none());
        assert!(s.buffers.is_empty(), "cancel must not open marked files");
    }
    #[test]
    fn picker_normal_commands_do_not_become_filter_text_and_insert_q_is_literal() {
        let mut s = model();
        s.open_locations(PickerKind::Files, file_choices(3));
        s.picker.as_mut().unwrap().results = vec![0, 1, 2];
        s.key("<Esc>", b"\x1b");
        s.key("j", b"j");
        assert_eq!(s.picker.as_ref().unwrap().selected, 1);
        s.key("k", b"k");
        s.picker_text("accidental committed text");
        assert_eq!(s.picker.as_ref().unwrap().selected, 0);
        assert!(s.picker.as_ref().unwrap().query.is_empty());
        s.key("i", b"i");
        assert!(s.picker.as_ref().unwrap().insert);
        s.picker_text("q");
        assert_eq!(s.picker.as_ref().unwrap().query, "q");
        s.key("<C-c>", b"\x03");
        assert_eq!(s.surface, Surface::Tree);
        s.open_locations(PickerKind::Files, file_choices(1));
        s.key("<Esc>", b"\x1b");
        s.key("q", b"q");
        assert!(s.picker.is_none());
        assert_eq!(s.surface, Surface::Tree);
    }
    #[test]
    fn picker_cursor_editing_reranks_only_mutations_and_keeps_modal_ownership() {
        let mut s = model();
        s.focus(Surface::File);
        s.open_locations(PickerKind::Files, file_choices(3));
        s.picker_text("abcd");
        let generation = s.picker.as_ref().unwrap().generation;
        for key in ["<Home>", "<Right>"] {
            s.key(key, b"");
        }
        assert_eq!(s.picker.as_ref().unwrap().generation, generation);
        s.picker_text("X");
        assert_eq!(s.picker.as_ref().unwrap().query, "aXbcd");
        assert!(s.picker.as_ref().unwrap().generation > generation);
        s.key("<Esc>", b"\x1b");
        assert_eq!(s.picker.as_ref().unwrap().query_cursor, 1);
        s.key("x", b"x");
        assert_eq!(s.picker.as_ref().unwrap().query, "abcd");
        s.picker_text("unowned");
        assert_eq!(s.picker.as_ref().unwrap().query, "abcd");
        s.key("A", b"A");
        s.picker_text("q");
        assert_eq!(s.picker.as_ref().unwrap().query, "abcdq");
        s.cancel_picker();
        assert_eq!(s.surface, Surface::File);
    }
    #[test]
    fn project_picker_is_contextual_and_cancellable() {
        let mut s = model();
        for key in ["<Space>", "f", "p"] {
            s.key(key, b"");
        }
        assert_eq!(s.surface, Surface::Picker);
        s.picker_text("absent");
        s.key("<Esc>", b"\x1b");
        s.key("<Esc>", b"\x1b");
        assert_eq!(s.surface, Surface::Tree);
        assert!(s.picker.is_none());
    }
    #[test]
    fn todo_and_reference_pickers_open_typed_locations_and_cancel_workers() {
        let root = std::env::temp_dir().join(format!("tw-todo-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("unicode.txt");
        std::fs::write(&path, "á TODO: café\nFIXME: reparar\nNOTE: concepto\n").unwrap();
        let mut s = model();
        s.projects[0].path = root.clone();
        s.entries = scan_project(&root).unwrap();
        for key in ["<Space>", "s", "T"] {
            s.key(key, b"");
        }
        for _ in 0..100 {
            s.poll_picker();
            if s.picker.as_ref().is_some_and(|p| p.results.len() == 2) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(s.picker.as_ref().unwrap().results.len(), 2);
        assert_eq!(s.choose_picker(), Effect::FileAt(path.clone(), 1, 3));
        s.open_locations(
            PickerKind::References,
            vec![Choice {
                label: "unicode.txt:1".into(),
                detail: "Referencia".into(),
                value: serde_json::to_string(&(
                    path.clone(),
                    serde_json::json!({"line":0,"character":3}),
                ))
                .unwrap(),
            }],
        );
        for _ in 0..100 {
            s.poll_picker();
            if s.picker.as_ref().is_some_and(|p| !p.results.is_empty()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            s.choose_picker(),
            Effect::LocationUtf16(path, serde_json::json!({"line":0,"character":3}))
        );
        s.open_picker(PickerKind::Todo(false));
        s.key("<Esc>", b"\x1b");
        s.key("<Esc>", b"\x1b");
        s.poll_picker();
        assert!(s.picker.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
