//! Behavior port from the configured agent_overview UI. No discovery or I/O here.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Agent {
    pub id: String,
    pub source: String,
    pub label: String,
    pub status: String,
    pub status_detail: Option<String>,
    pub connection_state: Option<String>,
    pub cli_state: Option<String>,
    pub rollout_status: Option<String>,
    pub activity_signature: Option<String>,
    pub updated_at: Option<i64>,
    pub host_id: Option<String>,
    pub codex_host_id: Option<String>,
    pub task_id: Option<String>,
    pub parent_id: Option<String>,
    pub codex_project_id: Option<String>,
    pub codex_project_name: Option<String>,
    pub codex_project_position: Option<i64>,
    pub tmux_session: Option<String>,
    pub tmux_window: Option<String>,
    pub tmux_pane: Option<String>,
    pub terminal_id: Option<String>,
    pub app_bundle_id: Option<String>,
    pub coordination_waiting: bool,
    pub coordination_created: usize,
    pub coordination_targets: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Offline,
    Session,
    Attention,
    Coordinating,
    Running,
    Ready,
}

impl Agent {
    pub fn fresh_activity(&self, now: i64) -> bool {
        self.host_id.is_none()
            && self.source == "codex_app_server"
            && self.rollout_status.as_deref() != Some("completed")
            && self
                .updated_at
                .is_some_and(|updated| updated >= now.saturating_sub(300))
    }
    pub fn status_kind(&self, now: i64) -> StatusKind {
        if self.connection_state.as_deref() == Some("disconnected") {
            return StatusKind::Offline;
        }
        if self.source == "remote_host" {
            return StatusKind::Session;
        }
        if self.source == "process" {
            return match self.cli_state.as_deref() {
                Some("working") => StatusKind::Running,
                Some("waiting") => StatusKind::Attention,
                _ => StatusKind::Session,
            };
        }
        if self.source == "application" {
            return StatusKind::Session;
        }
        let detail = self.status_detail.as_deref().unwrap_or("").to_lowercase();
        if self.status == "error"
            || self.status == "blocked"
            || ["blocked", "waiting", "approval", "input"]
                .iter()
                .any(|s| detail.contains(s))
        {
            return StatusKind::Attention;
        }
        if self.coordination_waiting && self.rollout_status.as_deref() != Some("completed") {
            return StatusKind::Coordinating;
        }
        if self.status == "running" || self.fresh_activity(now) {
            return StatusKind::Running;
        }
        StatusKind::Ready
    }
    pub fn signature(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.activity_signature.as_deref().unwrap_or(""),
            self.status,
            self.status_detail.as_deref().unwrap_or(""),
            self.rollout_status.as_deref().unwrap_or(""),
            self.connection_state.as_deref().unwrap_or(""),
            self.updated_at.map(|t| t.to_string()).unwrap_or_default(),
            self.coordination_waiting
        )
    }
    /// Typed navigation only. The application must validate live targets before acting.
    pub fn focus_target(&self) -> Option<FocusTarget> {
        if let Some(id) = self.terminal_id.as_ref().filter(|id| valid_id(id)) {
            return Some(FocusTarget::OwnedTerminal { id: id.clone() });
        }
        if let Some(pane) = self.tmux_pane.as_ref().filter(|id| tmux_id(id, '%'))
            && let (Some(session), Some(window)) = (&self.tmux_session, &self.tmux_window)
            && valid_id(session)
            && valid_id(window)
        {
            return Some(FocusTarget::Tmux {
                session: session.clone(),
                window: window.clone(),
                pane: pane.clone(),
            });
        }
        if self.source == "codex_app_server"
            && let Some(task) = self.task_id.as_ref().filter(|id| valid_id(id))
        {
            // A remote task without an explicit app-host mapping must never open locally.
            if self.host_id.is_some() && self.codex_host_id.as_ref().is_none_or(|id| !valid_id(id))
            {
                return None;
            }
            return Some(FocusTarget::CodexTask {
                task_id: task.clone(),
                host_id: self.codex_host_id.clone().filter(|id| valid_id(id)),
            });
        }
        self.app_bundle_id
            .as_ref()
            .filter(|id| valid_id(id))
            .map(|id| FocusTarget::Application {
                bundle_id: id.clone(),
            })
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control)
}
fn tmux_id(id: &str, prefix: char) -> bool {
    id.starts_with(prefix) && id.len() > 1 && id[1..].bytes().all(|b| b.is_ascii_digit())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FocusTarget {
    OwnedTerminal {
        id: String,
    },
    Tmux {
        session: String,
        window: String,
        pane: String,
    },
    CodexTask {
        task_id: String,
        host_id: Option<String>,
    },
    Application {
        bundle_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Dismissal {
    Legacy(bool),
    Snapshot {
        signature: String,
        updated_at: Option<i64>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Location {
    Main,
    Later,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Left,
    Right,
    RightNormal,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ViewState {
    pub collapsed: BTreeMap<String, bool>,
    pub task_locations: BTreeMap<String, Location>,
    pub dismissed_tasks: BTreeMap<String, Dismissal>,
    pub later_collapsed: bool,
    pub sidebar_side: Option<Side>,
    pub sidebar_right_layout: Option<Side>,
}
impl Default for ViewState {
    fn default() -> Self {
        Self {
            collapsed: BTreeMap::new(),
            task_locations: BTreeMap::new(),
            dismissed_tasks: BTreeMap::new(),
            later_collapsed: true,
            sidebar_side: None,
            sidebar_right_layout: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "field", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Collapsed { key: String, value: bool },
    TaskLocations { key: String, value: Location },
    DismissedTasks { key: String, value: Dismissal },
    LaterCollapsed { value: bool },
    SidebarSide { value: Side },
    SidebarRightLayout { value: Side },
}

#[derive(Deserialize, Serialize)]
struct JournalEvent {
    version: u8,
    changes: Vec<Change>,
}

impl ViewState {
    pub fn is_dismissed(&self, id: &str) -> bool {
        matches!(
            self.dismissed_tasks.get(id),
            Some(Dismissal::Legacy(true) | Dismissal::Snapshot { .. })
        )
    }
    pub fn apply(&mut self, changes: &[Change]) -> Result<(), String> {
        if changes.len() > 4096 {
            return Err("too many view changes".into());
        }
        for change in changes {
            match change {
                Change::Collapsed { key, .. }
                | Change::TaskLocations { key, .. }
                | Change::DismissedTasks { key, .. }
                    if !valid_id(key) =>
                {
                    return Err("invalid view key".into());
                }
                Change::SidebarRightLayout { value: Side::Left } => {
                    return Err("right layout must be right or right-normal".into());
                }
                Change::DismissedTasks {
                    value: Dismissal::Snapshot { signature, .. },
                    ..
                } if signature.len() > 8192 => return Err("dismissal signature too large".into()),
                _ => {}
            }
        }
        let mut next = self.clone();
        for change in changes {
            match change {
                Change::Collapsed { key, value: true } => {
                    next.collapsed.insert(key.clone(), true);
                }
                Change::Collapsed { key, value: false } => {
                    next.collapsed.remove(key);
                }
                Change::TaskLocations { key, value } => {
                    next.task_locations.insert(key.clone(), *value);
                }
                Change::DismissedTasks {
                    key,
                    value: Dismissal::Legacy(false),
                } => {
                    next.dismissed_tasks.remove(key);
                }
                Change::DismissedTasks { key, value } => {
                    next.dismissed_tasks.insert(key.clone(), value.clone());
                }
                Change::LaterCollapsed { value } => next.later_collapsed = *value,
                Change::SidebarSide { value } => next.sidebar_side = Some(*value),
                Change::SidebarRightLayout { value } => next.sidebar_right_layout = Some(*value),
            }
        }
        if next.collapsed.len() > 4096
            || next.task_locations.len() > 4096
            || next.dismissed_tasks.len() > 4096
        {
            return Err("view state exceeds entry budget".into());
        }
        *self = next;
        Ok(())
    }
    pub fn journal_record(changes: Vec<Change>) -> Result<String, String> {
        Self::default().apply(&changes)?;
        serde_json::to_string(&JournalEvent {
            version: 1,
            changes,
        })
        .map(|s| format!("\n{s}\n"))
        .map_err(|e| e.to_string())
    }
    /// Same complete-record contract as view.jsonl. A damaged tail is not applied.
    pub fn replay_journal(&mut self, journal: &str) -> Result<usize, String> {
        if journal.len() > 8 * 1024 * 1024 {
            return Err("journal exceeds replay budget".into());
        }
        let mut applied = 0;
        for line in journal.split_inclusive('\n') {
            if !line.ends_with('\n') || line.len() > 65536 {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<JournalEvent>(line)
                && event.version == 1
                && self.apply(&event.changes).is_ok()
            {
                applied += 1;
            }
        }
        Ok(applied)
    }
}

#[derive(Default)]
pub struct Overview {
    pub view: ViewState,
    agents: Vec<Agent>,
    observed: BTreeMap<String, String>,
}
impl Overview {
    pub fn replace(&mut self, agents: Vec<Agent>, now: i64) -> Result<(), String> {
        if agents.len() > 2048 {
            return Err("agent snapshot exceeds budget".into());
        }
        let mut ids = BTreeSet::new();
        for agent in &agents {
            if !valid_id(&agent.id) || !ids.insert(agent.id.clone()) || agent.label.len() > 4096 {
                return Err("invalid or duplicate agent identity".into());
            }
        }
        for agent in &agents {
            if agent.source != "codex_app_server" {
                continue;
            }
            if let Some(dismissal) = self
                .view
                .dismissed_tasks
                .get(&agent.id)
                .filter(|value| !matches!(value, Dismissal::Legacy(false)))
            {
                let changed = self.observed.get(&agent.id) != Some(&agent.signature());
                if changed && has_resumed(agent, dismissal, now) {
                    self.view.dismissed_tasks.remove(&agent.id);
                }
            }
        }
        self.observed = agents
            .iter()
            .map(|agent| (agent.id.clone(), agent.signature()))
            .collect();
        self.agents = agents;
        Ok(())
    }
    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }
    pub fn visible(&self, location: Location) -> Vec<&Agent> {
        self.agents
            .iter()
            .filter(|agent| {
                let codex = agent.source == "codex_app_server";
                let actual = if codex {
                    self.view
                        .task_locations
                        .get(&agent.id)
                        .copied()
                        .unwrap_or(Location::Main)
                } else {
                    Location::Main
                };
                actual == location && (!codex || !self.view.is_dismissed(&agent.id))
            })
            .collect()
    }
    pub fn dismiss(&mut self, id: &str, now: i64) -> Result<Vec<Change>, String> {
        let agent = self
            .agents
            .iter()
            .find(|a| a.id == id)
            .ok_or("unknown task")?;
        if agent.source != "codex_app_server" || agent.status_kind(now) != StatusKind::Ready {
            return Err("only ready Codex chats can be dismissed".into());
        }
        let changes = vec![Change::DismissedTasks {
            key: id.into(),
            value: Dismissal::Snapshot {
                signature: agent.signature(),
                updated_at: agent.updated_at,
            },
        }];
        self.view.apply(&changes)?;
        Ok(changes)
    }
    pub fn toggle_backlog(&mut self, id: &str) -> Result<Vec<Change>, String> {
        let agent = self
            .agents
            .iter()
            .find(|a| a.id == id)
            .ok_or("unknown task")?;
        if agent.source != "codex_app_server" {
            return Err("only Codex tasks can move to Backlog".into());
        }
        let target = if self.view.task_locations.get(id) == Some(&Location::Later) {
            Location::Main
        } else {
            Location::Later
        };
        let changes = vec![
            Change::TaskLocations {
                key: id.into(),
                value: target,
            },
            Change::DismissedTasks {
                key: id.into(),
                value: Dismissal::Legacy(false),
            },
        ];
        self.view.apply(&changes)?;
        Ok(changes)
    }
    pub fn restore(&mut self, id: &str) -> Result<Vec<Change>, String> {
        if !self
            .agents
            .iter()
            .any(|a| a.id == id && a.source == "codex_app_server")
        {
            return Err("unknown Codex task".into());
        }
        let changes = vec![
            Change::TaskLocations {
                key: id.into(),
                value: Location::Main,
            },
            Change::DismissedTasks {
                key: id.into(),
                value: Dismissal::Legacy(false),
            },
        ];
        self.view.apply(&changes)?;
        Ok(changes)
    }
}

fn has_resumed(agent: &Agent, dismissed: &Dismissal, now: i64) -> bool {
    if agent.connection_state.as_deref() == Some("disconnected") {
        return false;
    }
    if let Dismissal::Snapshot {
        signature,
        updated_at,
    } = dismissed
    {
        if *signature == agent.signature() {
            return false;
        }
        if let (Some(current), Some(previous)) = (agent.updated_at, updated_at) {
            if current < *previous {
                return false;
            }
            if current > *previous {
                return true;
            }
        }
    }
    agent.coordination_waiting
        || agent.status == "running"
        || agent.fresh_activity(now)
        || matches!(
            agent.rollout_status.as_deref(),
            Some("inProgress" | "waitingOnApproval" | "waitingOnUserInput")
        )
        || agent
            .status_detail
            .as_deref()
            .is_some_and(|s| s.contains("waitingOnApproval") || s.contains("waitingOnUserInput"))
}

#[derive(Debug, Serialize)]
pub struct TaskTree {
    pub roots: Vec<String>,
    pub children: BTreeMap<String, Vec<String>>,
    pub parents: BTreeMap<String, String>,
}
impl TaskTree {
    pub fn build(agents: &[&Agent]) -> Self {
        let ids: BTreeSet<_> = agents.iter().map(|a| a.id.as_str()).collect();
        let mut parents: BTreeMap<String, String> = agents
            .iter()
            .filter_map(|a| {
                a.parent_id
                    .as_ref()
                    .filter(|parent| ids.contains(parent.as_str()) && **parent != a.id)
                    .map(|parent| (a.id.clone(), parent.clone()))
            })
            .collect();
        for agent in agents {
            let mut current = agent.id.clone();
            let mut seen = BTreeSet::new();
            while let Some(parent) = parents.get(&current).cloned() {
                seen.insert(current.clone());
                if seen.contains(&parent) {
                    parents.remove(&current);
                    break;
                }
                current = parent;
            }
        }
        let mut tree = Self {
            roots: Vec::new(),
            children: BTreeMap::new(),
            parents,
        };
        for agent in agents {
            if let Some(parent) = tree.parents.get(&agent.id) {
                tree.children
                    .entry(parent.clone())
                    .or_default()
                    .push(agent.id.clone());
            } else {
                tree.roots.push(agent.id.clone());
            }
        }
        tree
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(id: &str, status: &str, at: i64) -> Agent {
        Agent {
            id: id.into(),
            source: "codex_app_server".into(),
            status: status.into(),
            updated_at: Some(at),
            ..Default::default()
        }
    }
    #[test]
    fn idle_is_ready_not_done_and_process_presence_is_only_a_session() {
        assert_eq!(task("a", "idle", 1).status_kind(1000), StatusKind::Ready);
        assert_eq!(
            Agent {
                source: "process".into(),
                status: "running".into(),
                ..Default::default()
            }
            .status_kind(1000),
            StatusKind::Session
        );
        let mut waiting = task("a", "running", 999);
        waiting.status_detail = Some("waitingOnApproval".into());
        assert_eq!(waiting.status_kind(1000), StatusKind::Attention);
    }
    #[test]
    fn main_backlog_and_hide_follow_overview_contract() {
        let mut model = Overview::default();
        model
            .replace(
                vec![task("idle", "idle", 1), task("busy", "running", 1)],
                1000,
            )
            .unwrap();
        assert!(model.dismiss("busy", 1000).is_err());
        model.toggle_backlog("idle").unwrap();
        assert_eq!(model.visible(Location::Later).len(), 1);
        model.dismiss("idle", 1000).unwrap();
        assert!(model.visible(Location::Later).is_empty());
        model.restore("idle").unwrap();
        assert_eq!(model.visible(Location::Main).len(), 2);
    }
    #[test]
    fn stale_or_unchanged_snapshots_do_not_revive_hidden_chats() {
        let mut model = Overview::default();
        model.replace(vec![task("a", "idle", 500)], 1000).unwrap();
        model.dismiss("a", 1000).unwrap();
        model
            .replace(vec![task("a", "running", 499)], 1000)
            .unwrap();
        assert!(model.visible(Location::Main).is_empty());
        model.replace(vec![task("a", "idle", 501)], 1000).unwrap();
        assert_eq!(model.visible(Location::Main).len(), 1);
    }
    #[test]
    fn explicit_wait_revives_hidden_task_but_origin_count_and_disconnect_do_not() {
        let mut model = Overview::default();
        let mut agent = task("a", "idle", 1);
        model.replace(vec![agent.clone()], 1000).unwrap();
        model.dismiss("a", 1000).unwrap();
        agent.coordination_created = 1;
        model.replace(vec![agent.clone()], 1000).unwrap();
        assert!(model.visible(Location::Main).is_empty());
        agent.connection_state = Some("disconnected".into());
        agent.coordination_waiting = true;
        model.replace(vec![agent.clone()], 1000).unwrap();
        assert!(model.visible(Location::Main).is_empty());
        agent.connection_state = None;
        model.replace(vec![agent], 1000).unwrap();
        assert_eq!(model.visible(Location::Main).len(), 1);
    }
    #[test]
    fn view_journal_ignores_partial_tail_and_applies_transactions_atomically() {
        let mut view = ViewState::default();
        let complete =
            ViewState::journal_record(vec![Change::LaterCollapsed { value: false }]).unwrap();
        let journal = format!(
            "{complete}{{\"version\":1,\"changes\":[{{\"field\":\"later_collapsed\",\"value\":true}}]}}"
        );
        assert_eq!(view.replay_journal(&journal).unwrap(), 1);
        assert!(!view.later_collapsed);
        assert!(
            view.apply(&[
                Change::LaterCollapsed { value: true },
                Change::SidebarRightLayout { value: Side::Left }
            ])
            .is_err()
        );
        assert!(!view.later_collapsed);
    }
    #[test]
    fn malformed_parent_cycles_leave_every_task_reachable_once() {
        let mut a = task("a", "idle", 1);
        a.parent_id = Some("b".into());
        let mut b = task("b", "idle", 1);
        b.parent_id = Some("a".into());
        let tree = TaskTree::build(&[&a, &b]);
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.parents.len(), 1);
    }
    #[test]
    fn agent_navigation_retains_original_destinations_without_sending_messages() {
        let mut agent = task("a", "idle", 1);
        agent.task_id = Some("task-demo".into());
        assert!(matches!(
            agent.focus_target(),
            Some(FocusTarget::CodexTask { .. })
        ));
        agent.tmux_session = Some("demo".into());
        agent.tmux_window = Some("1".into());
        agent.tmux_pane = Some("%3".into());
        assert!(matches!(
            agent.focus_target(),
            Some(FocusTarget::Tmux { .. })
        ));
    }
    #[test]
    fn legacy_false_dismissal_does_not_hide_a_conversation() {
        let mut model = Overview {
            view: serde_json::from_value(serde_json::json!({"dismissed_tasks":{"a":false}}))
                .unwrap(),
            ..Default::default()
        };
        model.replace(vec![task("a", "idle", 1)], 1000).unwrap();
        assert!(!model.view.is_dismissed("a"));
        assert_eq!(model.visible(Location::Main).len(), 1);
    }
}
