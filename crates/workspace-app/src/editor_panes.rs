//! GUI adapter identities and independent viewports. Text/undo stay in the editor bank.
use crate::editor::{Editor, Mode, WrapCursor, WrappedTop};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
pub use workspace_core::editor_panes::{Axis, Layout, Node, PaneId, ViewState};

/// The shared editor holds the active cursor; other windows keep their own.
pub fn status_position(active: bool, state: &ViewState, editor: Option<&Editor>) -> (usize, usize) {
    if !active {
        return (state.row, state.column);
    }
    editor.map_or((state.row, state.column), |editor| {
        if editor.mode == Mode::VisualBlock {
            editor.display_position()
        } else {
            editor.position()
        }
    })
}

/// Whether a forced close may release its buffer after the active pane closes.
/// Other workspace owners count even if their view is currently hidden.
pub fn force_close_releases_buffer(
    force: bool,
    other_views: usize,
    other_workspace_view: bool,
    other_workspace_owner: bool,
) -> bool {
    force && other_views == 0 && !other_workspace_view && !other_workspace_owner
}

/// Window-local 'scroll'. A new window and each height change use half its height.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneScroll {
    rows: Option<usize>,
    value: Option<usize>,
}
impl PaneScroll {
    pub fn prepare(&mut self, rows: usize) -> Option<usize> {
        if rows != 0 && self.rows != Some(rows) {
            self.rows = Some(rows);
            self.value = None;
        }
        self.value
    }
    pub fn save(&mut self, value: Option<usize>) {
        self.value = value;
    }
}

/// Window-local wrap option and viewport state; text remains in the buffer bank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneWrap {
    pub enabled: bool,
    pub path: Option<PathBuf>,
    pub top: WrappedTop,
    pub cursor: Option<WrapCursor>,
    pub desired_column: Option<usize>,
    pub wheel_goal: Option<usize>,
    pub wheel_coladd: Option<usize>,
    pub version: Option<u64>,
    pub width: Option<usize>,
    pub height: Option<usize>,
    pub mode: Option<Mode>,
    pub scroll: PaneScroll,
}
impl Default for PaneWrap {
    fn default() -> Self {
        Self {
            enabled: false,
            path: None,
            top: WrappedTop {
                source_line: 0,
                visual_row: 0,
            },
            cursor: None,
            desired_column: None,
            wheel_goal: None,
            wheel_coladd: None,
            version: None,
            width: None,
            height: None,
            mode: None,
            scroll: PaneScroll::default(),
        }
    }
}
impl PaneWrap {
    /// Changing buffers preserves the window option but clears view-specific
    /// cursor and anchor data from the previous buffer.
    pub fn activate(&mut self, path: &Path, source_line: usize) {
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_owned());
            self.top = WrappedTop {
                source_line,
                visual_row: 0,
            };
            self.cursor = None;
            self.desired_column = None;
            self.wheel_goal = None;
            self.wheel_coladd = None;
            self.version = None;
            self.width = None;
            self.height = None;
            self.mode = None;
        }
    }
}
/// Locate the active wrapped cursor in the same bounded viewport used by paint.
/// Return None while composition or stale/unsupported geometry prevents a
/// reliable measurement, so callers can use their ordinary fallback.
pub fn wrapped_navigation_cell(
    editor: &Editor,
    state: &PaneWrap,
    rows: usize,
    cols: usize,
) -> Option<(usize, usize)> {
    if !state.enabled || editor.input_display().is_some() {
        return None;
    }
    let block = editor.mode == Mode::VisualBlock;
    let virtual_cursor = if block {
        editor.wrapped_block_cursor(cols).ok().flatten()
    } else {
        None
    };
    let cursor = if block {
        virtual_cursor?.cursor
    } else {
        let offset = editor.display_cursor();
        let line = editor.source_line_at(offset);
        let start = editor.visible_lines(line, 1).first()?.start;
        state
            .cursor
            .filter(|cursor| cursor.line == line && cursor.scalar == offset.saturating_sub(start))
            .or_else(|| {
                editor
                    .wrapped_display_cursor_at(
                        offset,
                        cols,
                        matches!(editor.mode, Mode::Insert | Mode::Visual | Mode::VisualLine),
                    )
                    .ok()
                    .flatten()
            })?
    };
    let viewport = if matches!(
        editor.mode,
        Mode::Insert | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
    ) {
        editor.wrapped_viewport_for_cursor(state.top, rows, cols, cursor)
    } else {
        editor.wrapped_viewport(state.top, rows, cols)
    }
    .ok()?;
    let cell = if let Some(cursor) = virtual_cursor {
        editor.wrapped_virtual_cursor_cell(&viewport, cursor, cols)
    } else {
        editor.wrapped_cursor_cell(&viewport, cursor)
    }?;
    Some((cell.row, cell.columns.start))
}
/// Paint rectangles can share a rasterized border pixel. Navigation uses their
/// interiors so a shared border does not masquerade as overlapping panes.
pub fn navigation_interior(
    mut bounds: workspace_core::editor_panes::Bounds,
) -> workspace_core::editor_panes::Bounds {
    if bounds.width > 2 {
        bounds.x += 1;
        bounds.width -= 2;
    }
    if bounds.height > 2 {
        bounds.y += 1;
        bounds.height -= 2;
    }
    bounds
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    File(PathBuf),
    /// Stable identity owned by the terminal backend, not a PTY handle.
    Terminal(u64),
    /// These identities refer to view state in their respective controllers.
    Explorer(u64),
    Agents(u64),
}
/// Materialize the currently focused terminal view before a file replaces it,
/// so the buffer registry can retain it as this pane's alternate.
pub fn prepare_terminal_for_file(
    panes: &mut Option<Panes>,
    terminal: Option<u64>,
    terminal_focused: bool,
) {
    if panes.is_none()
        && terminal_focused
        && let Some(terminal) = terminal
    {
        *panes = Some(Panes::terminal(terminal));
    }
}
/// Only file panes publish an outer measurement. Other content may reuse a
/// pane ID whose old file measurement remains in the cache.
pub fn resize_rect<'a, T>(content: Option<&Content>, inner: &'a T, outer: Option<&'a T>) -> &'a T {
    if matches!(content, Some(Content::File(_))) {
        outer.unwrap_or(inner)
    } else {
        inner
    }
}
/// One count unit is a character cell horizontally or a line vertically.
pub fn resize_delta(key: &str, count: usize, cell: i64, line: i64) -> Option<(Axis, i64)> {
    let count = count.clamp(1, 9999) as i64;
    match key {
        ">" => Some((Axis::Row, cell.max(1).saturating_mul(count))),
        "<" => Some((Axis::Row, -cell.max(1).saturating_mul(count))),
        "+" => Some((Axis::Column, line.max(1).saturating_mul(count))),
        "-" => Some((Axis::Column, -line.max(1).saturating_mul(count))),
        _ => None,
    }
}
/// A zero count requests the largest available size. Counted targets describe
/// text cells; the measured outer-minus-content area is retained as chrome.
pub fn absolute_resize_delta(count: usize, cell: i64, outer: i64, content: i64) -> i64 {
    let outer = outer.max(1);
    if count == 0 {
        return i64::MAX.saturating_sub(outer);
    }
    let chrome = outer.saturating_sub(content.clamp(0, outer));
    let target = (count.min(9999) as i64)
        .saturating_mul(cell.max(1))
        .saturating_add(chrome);
    target.saturating_sub(outer)
}
/// Reserve a text line in addition to the measured non-text editor chrome.
pub fn file_minimum_height(outer: i64, text: i64, line: i64) -> i64 {
    if outer <= 0 || text <= 0 || text > outer {
        return 40;
    }
    outer
        .saturating_sub(text)
        .saturating_add(line.max(1))
        .max(40)
}
/// Guard an asynchronous editor-originated open against a later context change.
#[derive(Clone, Debug)]
pub struct OpenGuard {
    pub generation: u64,
    pub project: usize,
    pub pane: Option<PaneId>,
    pub path: PathBuf,
    pub version: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum SavedCloseAction {
    ClosePane,
    Quit,
    KeepChanged,
    KeepPending,
    KeepOtherBuffers,
}
pub fn saved_close_action(
    valid: bool,
    pending: bool,
    multiple_panes: bool,
    other_busy: bool,
) -> SavedCloseAction {
    if pending {
        SavedCloseAction::KeepPending
    } else if !valid {
        SavedCloseAction::KeepChanged
    } else if multiple_panes {
        SavedCloseAction::ClosePane
    } else if other_busy {
        SavedCloseAction::KeepOtherBuffers
    } else {
        SavedCloseAction::Quit
    }
}
/// Prevent a terminal-originated asynchronous open from replacing a later pane.
#[derive(Clone, Debug)]
pub struct TerminalOpenGuard {
    pub generation: u64,
    pub project: usize,
    pub pane: Option<PaneId>,
    pub attachment: Option<u64>,
}
/// A tmux target change may keep the same native pane and PTY attachment.
/// Invalidate the generation so an already-running read cannot complete in
/// the newly selected tmux window or pane.
pub fn cancel_terminal_open_for_target_change(
    pending: &mut Option<TerminalOpenGuard>,
    generation: &mut u64,
    target_changed: bool,
) -> bool {
    if target_changed && pending.take().is_some() {
        *generation = generation.wrapping_add(1);
        true
    } else {
        false
    }
}
impl TerminalOpenGuard {
    pub fn valid(
        &self,
        project: usize,
        pane: Option<PaneId>,
        attachment: Option<u64>,
        normal: bool,
    ) -> bool {
        self.project == project && self.pane == pane && self.attachment == attachment && normal
    }
}
impl OpenGuard {
    pub fn after_format(mut self, expected_version: u64, formatted_version: u64) -> Option<Self> {
        if self.version != expected_version {
            return None;
        }
        self.version = formatted_version;
        Some(self)
    }
    pub fn after_naming(mut self, old: &Path, new: &Path) -> Option<Self> {
        if self.path != old {
            return None;
        }
        self.path = new.to_owned();
        Some(self)
    }
    pub fn valid(
        &self,
        project: usize,
        pane: Option<PaneId>,
        path: Option<&Path>,
        version: Option<u64>,
        normal: bool,
    ) -> bool {
        self.project == project
            && self.pane == pane
            && path == Some(self.path.as_path())
            && version == Some(self.version)
            && normal
    }
}
/// Split commands issued from a sidebar during a file read belong to the
/// resulting editor, provided the originating pane and project are unchanged.
#[derive(Default)]
pub struct PendingFileSplits {
    context: Option<(u64, usize, PaneId)>,
    origin: Option<Content>,
    commands: Vec<(String, usize)>,
    recovery: Vec<(usize, PaneId, bool, Vec<(String, usize)>)>,
    recovery_revision: u64,
}
impl PendingFileSplits {
    pub fn begin(&mut self, generation: u64, project: usize, pane: PaneId, origin: Content) {
        self.retain_active();
        self.context = Some((generation, project, pane));
        self.origin = Some(origin);
    }
    pub fn retain_active(&mut self) {
        if let Some((_, project, pane)) = self.context {
            let commands = std::mem::take(&mut self.commands);
            self.retain(project, pane, false, commands);
        }
    }
    /// Retain commands only for explicit review; never dispatch them in a
    /// later pane merely because it reused the original numeric identity.
    pub fn retain(
        &mut self,
        project: usize,
        pane: PaneId,
        uncertain: bool,
        commands: Vec<(String, usize)>,
    ) {
        if !commands.is_empty() {
            self.recovery.push((project, pane, uncertain, commands));
            self.recovery_revision = self.recovery_revision.wrapping_add(1);
        }
    }
    pub fn recovery_revision(&self) -> Option<u64> {
        (!self.recovery.is_empty()).then_some(self.recovery_revision)
    }
    pub fn recovery_text(&self) -> String {
        let mut text = String::from(
            "Acciones de panel retenidas; revisá el contexto antes de volver a ejecutarlas.\n",
        );
        for (project, pane, uncertain, commands) in &self.recovery {
            text.push_str(&format!(
                "Workspace {project}, panel {}{}:\n",
                pane.0,
                if *uncertain {
                    " (la primera acción pudo ejecutarse)"
                } else {
                    ""
                }
            ));
            for (key, count) in commands {
                let (operation, path) = key
                    .split_once(':')
                    .map_or((key.as_str(), None), |(operation, path)| {
                        (operation, Some(path))
                    });
                let command = match operation {
                    "s" => "split horizontal",
                    "v" => "split vertical",
                    "n" => "nuevo panel horizontal",
                    "N" => "nuevo panel vertical",
                    _ => "acción desconocida",
                };
                if let Some(path) = path {
                    text.push_str(&format!("  {command}, cuenta {count}, ruta {path:?}\n"));
                } else {
                    text.push_str(&format!("  {command}, cuenta {count}\n"));
                }
            }
        }
        text
    }
    pub fn discard_recovery(&mut self, revision: u64) -> bool {
        if self.recovery_revision() != Some(revision) {
            return false;
        }
        self.recovery.clear();
        self.recovery_revision = self.recovery_revision.wrapping_add(1);
        true
    }
    /// None means this read was not started under the sidebar guard.
    pub fn is_current(
        &self,
        generation: u64,
        project: usize,
        pane: Option<PaneId>,
        content: Option<&Content>,
    ) -> Option<bool> {
        let (g, original_project, original_pane) = self.context?;
        (g == generation).then_some(
            self.origin.as_ref() == content
                && content.is_some()
                && original_project == project
                && Some(original_pane) == pane,
        )
    }
    pub fn queue(
        &mut self,
        generation: u64,
        project: usize,
        pane: PaneId,
        key: &str,
        count: usize,
    ) -> Option<bool> {
        if self.context != Some((generation, project, pane))
            || !matches!(key, "s" | "v" | "n" | "N")
        {
            return None;
        }
        if self.commands.len() >= 32 || self.recovery.len() >= 32 {
            return Some(false);
        }
        self.commands.push((key.to_owned(), count));
        Some(true)
    }
    pub fn finish(
        &mut self,
        generation: u64,
        project: usize,
        pane: Option<PaneId>,
        success: bool,
    ) -> Vec<(String, usize)> {
        if self.context.is_none_or(|(g, _, _)| g != generation) {
            return Vec::new();
        }
        let matches = self.context == pane.map(|id| (generation, project, id));
        let (_, original_project, original_pane) = self.context.expect("generation checked");
        self.context = None;
        self.origin = None;
        let commands = std::mem::take(&mut self.commands);
        if success && matches {
            commands
        } else {
            self.retain(original_project, original_pane, false, commands);
            Vec::new()
        }
    }

    fn cancel(&mut self, generation: u64) {
        if self.context.is_some_and(|(g, _, _)| g == generation) {
            self.retain_active();
            self.context = None;
            self.origin = None;
        }
    }
}

/// A later activation must invalidate a pending open at the moment context
/// changes. Checking only when the read completes misses A -> B -> A.
pub fn take_changed_focus(
    pending: &mut Option<(u64, u64)>,
    generation: u64,
    revision: u64,
) -> bool {
    pending
        .take_if(|(recorded_generation, _)| *recorded_generation == generation)
        .is_some_and(|(_, recorded_revision)| recorded_revision != revision)
}

pub struct PendingOpens<'a> {
    pub generation: &'a mut u64,
    pub read: &'a mut Option<OpenGuard>,
    pub edit: &'a mut Option<OpenGuard>,
    pub terminal: &'a mut Option<TerminalOpenGuard>,
    pub sidebar: &'a mut PendingFileSplits,
}
impl PendingOpens<'_> {
    pub fn activate(&mut self, project: usize, pane: PaneId, content: &Content) -> bool {
        let generation = *self.generation;
        let editor_changed = |guard: &OpenGuard| {
            guard.generation == generation
                && (guard.project != project
                    || guard.pane != Some(pane)
                    || !matches!(content, Content::File(path) if path == &guard.path))
        };
        let editor_stale = self.read.as_ref().is_some_and(editor_changed)
            || self.edit.as_ref().is_some_and(editor_changed);
        let terminal_stale = self.terminal.as_ref().is_some_and(|guard| {
            guard.generation == generation
                && (guard.project != project
                    || guard.pane != Some(pane)
                    || !matches!(content, Content::Terminal(id) if Some(*id) == guard.attachment))
        });
        let sidebar_stale = self
            .sidebar
            .is_current(generation, project, Some(pane), Some(content))
            == Some(false);
        if !(editor_stale || terminal_stale || sidebar_stale) {
            return false;
        }
        self.read.take_if(|guard| guard.generation == generation);
        self.edit.take_if(|guard| guard.generation == generation);
        self.terminal
            .take_if(|guard| guard.generation == generation);
        self.sidebar.cancel(generation);
        *self.generation = generation.wrapping_add(1);
        true
    }
}
/// Counts before and after Ctrl-w combine, while zero is not a first digit.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowPrefix {
    before: usize,
    after: usize,
}
impl WindowPrefix {
    pub fn new(before: usize) -> Self {
        Self { before, after: 0 }
    }
    pub fn digit(&mut self, key: &str) -> bool {
        let [digit] = key.as_bytes() else {
            return false;
        };
        if !digit.is_ascii_digit() || (*digit == b'0' && self.after == 0) {
            return false;
        }
        self.after = (self.after.saturating_mul(10) + usize::from(*digit - b'0')).min(9999);
        true
    }
    pub fn count(self) -> usize {
        match (self.before, self.after) {
            (0, n) | (n, 0) => n,
            (a, b) => a.saturating_mul(b).min(9999),
        }
    }
}
#[derive(Clone)]
pub struct Panes {
    pub layout: Layout,
    contents: BTreeMap<u64, Content>,
    content_identities: BTreeMap<PaneId, Arc<()>>,
    pub sizes: BTreeMap<PaneId, (usize, usize)>,
    weights: BTreeMap<(u8, Vec<PaneId>), f32>,
    sidebar_dock: BTreeSet<PaneId>,
    equalized: bool,
    wrap: BTreeMap<PaneId, PaneWrap>,
}

/// The structural and sizing inputs used by the pane renderer. View/cursor
/// updates do not invalidate geometry, but a split, dock or resize does.
#[derive(Clone, PartialEq)]
pub struct GeometryKey {
    root: Node,
    weights: BTreeMap<(u8, Vec<PaneId>), f32>,
    sidebar_dock: BTreeSet<PaneId>,
    equalized: bool,
}

/// Stage an already validated listed-buffer switch and its window-local jump.
/// The caller must save the active view and check target availability first.
/// Terminal origins need a terminal-specific mark model and are not recorded.
pub fn stage_listed_buffer_switch(
    panes: &Panes,
    numbers: &crate::buffer_numbers::BufferTracking,
    workspace: usize,
    target: crate::buffer_registry::Buffer,
) -> Result<Panes, workspace_core::editor_panes::Error> {
    use crate::buffer_registry::Buffer;
    use workspace_core::editor_jumplist::{Location, View};
    let pane = panes.layout.active();
    let current = match panes.content(pane) {
        Some(Content::File(path)) => Some(Buffer::File(path.clone())),
        Some(Content::Terminal(id)) => Some(Buffer::Terminal(*id)),
        _ => None,
    };
    let mut next = panes.clone();
    if let Some(Buffer::File(path)) = current.as_ref()
        && current.as_ref() != Some(&target)
        && let (Ok(source_id), Ok(_)) = (
            numbers.number_for(workspace, &Buffer::File(path.clone())),
            numbers.number_for(workspace, &target),
        )
        && let Some(view) = panes.layout.panes().get(&pane)
    {
        let wrapped = panes.wrap_state(pane).is_some_and(|state| state.enabled);
        let origin = Location {
            buffer: source_id,
            row: view.row,
            column: view.column,
            view: View {
                top: view.top,
                left: view.left,
                skip: 0,
                valid: !wrapped,
            },
        };
        let guard = panes.layout.focus_guard();
        let recorded = next.layout.record_jump_guarded(&guard, origin);
        debug_assert!(recorded, "staged clone must retain its focus guard");
    }
    next.set_content(match target {
        Buffer::File(path) => Content::File(path),
        Buffer::Terminal(id) => Content::Terminal(id),
        Buffer::Explorer(id) => Content::Explorer(id),
        Buffer::Agents(id) => Content::Agents(id),
    })?;
    Ok(next)
}

/// Stage a terminal-origin jump to a loaded file without moving a PTY or
/// changing the caller's history. The anchor belongs to the captured terminal
/// position and is retained when the current position enters the jump list.
pub struct StagedTerminalOriginJump {
    pub panes: Panes,
    pub registry: crate::buffer_registry::BufferRegistry,
    pub destination: Option<(PathBuf, workspace_core::editor_jumplist::Location)>,
}

/// A terminal-origin jump to an unloaded file must not commit its history or
/// switch the pane until a matching read has completed and been revalidated.
pub struct PendingTerminalOriginLoad {
    workspace: usize,
    generation: u64,
    attachment: u64,
    focus: workspace_core::editor_panes::FocusGuard,
    history: workspace_core::editor_jumplist::JumpList,
    origin: workspace_core::editor_jumplist::Location,
    anchor: workspace_core::editor_jumplist::AnchorId,
    older: bool,
    count: usize,
    destination: (PathBuf, workspace_core::editor_jumplist::Location),
}

impl PendingTerminalOriginLoad {
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        panes: &Panes,
        numbers: &crate::buffer_numbers::BufferTracking,
        registry: &crate::buffer_registry::BufferRegistry,
        workspace: usize,
        generation: u64,
        attachment: u64,
        focus: &workspace_core::editor_panes::FocusGuard,
        origin: workspace_core::editor_jumplist::Location,
        anchor: workspace_core::editor_jumplist::AnchorId,
        older: bool,
        count: usize,
        suspended: impl Fn(u64) -> bool,
    ) -> Result<Option<Self>, String> {
        use crate::buffer_registry::Buffer;
        let staged = stage_terminal_origin_file_jump(
            panes,
            numbers,
            registry,
            workspace,
            attachment,
            focus,
            origin,
            anchor,
            older,
            count,
            |path| {
                registry.is_unloaded(path)
                    && numbers
                        .number_for(workspace, &Buffer::File(path.to_owned()))
                        .is_ok_and(&suspended)
            },
        )?;
        let Some(destination) = staged.and_then(|staged| staged.destination) else {
            return Ok(None);
        };
        let history = panes
            .layout
            .jump_list(panes.layout.active())
            .map_err(|_| "Historial no disponible")?
            .clone();
        Ok(Some(Self {
            workspace,
            generation,
            attachment,
            focus: focus.clone(),
            history,
            origin,
            anchor,
            older,
            count,
            destination,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.destination.0
    }

    /// Replan against current state; no caller state is mutated on failure.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &self,
        panes: &Panes,
        numbers: &crate::buffer_numbers::BufferTracking,
        registry: &crate::buffer_registry::BufferRegistry,
        workspace: usize,
        generation: u64,
        attachment: u64,
        available: impl Fn(&Path) -> bool,
    ) -> Result<StagedTerminalOriginJump, String> {
        if workspace != self.workspace
            || generation != self.generation
            || attachment != self.attachment
            || !panes.layout.focus_guard_is_current(&self.focus)
            || panes.layout.jump_list(panes.layout.active()).ok() != Some(&self.history)
            || !registry.is_unloaded(&self.destination.0)
        {
            return Err("Cambió el panel o el historial del salto".into());
        }
        let staged = stage_terminal_origin_file_jump(
            panes,
            numbers,
            registry,
            workspace,
            attachment,
            &self.focus,
            self.origin,
            self.anchor,
            self.older,
            self.count,
            available,
        )?
        .ok_or("Salto ya no disponible")?;
        if staged.destination.as_ref() != Some(&self.destination) {
            return Err("Cambió el destino del salto".into());
        }
        Ok(staged)
    }
}

/// A terminal-to-terminal jump must restore the target's backend mark before
/// the pane and its history can be committed. This plan retains the source
/// guard so a delayed backend acknowledgement cannot retarget another pane.
pub struct PendingTerminalOriginRestore {
    workspace: usize,
    generation: u64,
    attachment: u64,
    focus: workspace_core::editor_panes::FocusGuard,
    history: workspace_core::editor_jumplist::JumpList,
    origin: workspace_core::editor_jumplist::Location,
    origin_anchor: workspace_core::editor_jumplist::AnchorId,
    older: bool,
    count: usize,
    destination: (
        u64,
        workspace_core::editor_jumplist::Location,
        workspace_core::editor_jumplist::AnchorId,
    ),
}

impl PendingTerminalOriginRestore {
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        panes: &Panes,
        numbers: &crate::buffer_numbers::BufferTracking,
        registry: &crate::buffer_registry::BufferRegistry,
        workspace: usize,
        generation: u64,
        attachment: u64,
        focus: &workspace_core::editor_panes::FocusGuard,
        origin: workspace_core::editor_jumplist::Location,
        origin_anchor: workspace_core::editor_jumplist::AnchorId,
        older: bool,
        count: usize,
        available: impl Fn(
            u64,
            workspace_core::editor_jumplist::Location,
            workspace_core::editor_jumplist::AnchorId,
        ) -> bool,
    ) -> Result<Option<Self>, String> {
        use crate::buffer_registry::Buffer;
        let pane = panes.layout.active();
        let source = Buffer::Terminal(attachment);
        if !panes.layout.focus_guard_is_current(focus)
            || panes.content(pane) != Some(&Content::Terminal(attachment))
            || !registry.knows(&source)
            || numbers.number_for(workspace, &source).ok() != Some(origin.buffer)
        {
            return Err("Cambió el panel o la identidad de terminal".into());
        }
        let Some(plan) =
            panes
                .layout
                .prepare_jump_with_anchor(origin, Some(origin_anchor), older, count)
        else {
            return Ok(None);
        };
        let Some(location) = plan.target() else {
            return Ok(None);
        };
        let Buffer::Terminal(target) = numbers
            .resolve(workspace, location.buffer)
            .map_err(|_| "Buffer no disponible")?
        else {
            return Ok(None);
        };
        let target_anchor = plan
            .target_anchor()
            .ok_or("La marca de terminal no está disponible")?;
        if !registry.knows(&Buffer::Terminal(target)) || !available(target, location, target_anchor)
        {
            return Err("La terminal o su marca ya no están disponibles".into());
        }
        let history = panes
            .layout
            .jump_list(pane)
            .map_err(|_| "Historial no disponible")?
            .clone();
        Ok(Some(Self {
            workspace,
            generation,
            attachment,
            focus: focus.clone(),
            history,
            origin,
            origin_anchor,
            older,
            count,
            destination: (target, location, target_anchor),
        }))
    }

    pub fn destination(
        &self,
    ) -> (
        u64,
        workspace_core::editor_jumplist::Location,
        workspace_core::editor_jumplist::AnchorId,
    ) {
        self.destination
    }

    /// Call only after the exact backend restore is acknowledged. If this
    /// returns an error after a restore, the caller must report that the PTY
    /// may have moved; it must not commit or retry it as though it had not.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &self,
        panes: &Panes,
        numbers: &crate::buffer_numbers::BufferTracking,
        registry: &crate::buffer_registry::BufferRegistry,
        workspace: usize,
        generation: u64,
        attachment: u64,
        available: impl Fn(
            u64,
            workspace_core::editor_jumplist::Location,
            workspace_core::editor_jumplist::AnchorId,
        ) -> bool,
    ) -> Result<(Panes, crate::buffer_registry::BufferRegistry), String> {
        use crate::buffer_registry::Buffer;
        if workspace != self.workspace
            || generation != self.generation
            || attachment != self.attachment
            || !panes.layout.focus_guard_is_current(&self.focus)
            || panes.layout.jump_list(panes.layout.active()).ok() != Some(&self.history)
        {
            return Err("Cambió el panel o el historial del salto".into());
        }
        let current = Self::prepare(
            panes,
            numbers,
            registry,
            workspace,
            generation,
            attachment,
            &self.focus,
            self.origin,
            self.origin_anchor,
            self.older,
            self.count,
            available,
        )?
        .ok_or("Salto ya no disponible")?;
        if current.destination != self.destination {
            return Err("Cambió el destino del salto".into());
        }
        let plan = panes
            .layout
            .prepare_jump_with_anchor(
                self.origin,
                Some(self.origin_anchor),
                self.older,
                self.count,
            )
            .ok_or("Salto ya no disponible")?;
        let mut next = panes.clone();
        let mut next_registry = registry.clone();
        let pane = next.layout.active();
        if !next.layout.commit_jump(plan, self.origin) {
            return Err("Cambió el historial del panel".into());
        }
        let target = Buffer::Terminal(self.destination.0);
        next.set_content(Content::Terminal(self.destination.0))
            .map_err(|_| "No se pudo seleccionar la terminal")?;
        next_registry.replace(pane, Some(&Buffer::Terminal(attachment)), target.clone());
        next_registry.relist(&target);
        Ok((next, next_registry))
    }
}

/// A terminal capture may finish after the newly opened file receives input.
/// Retain the origin only while the same file, pane and jump history remain.
#[derive(Clone)]
pub struct OpenedTerminalFileGuard {
    workspace: usize,
    generation: u64,
    path: PathBuf,
    focus: workspace_core::editor_panes::FocusGuard,
    history: workspace_core::editor_jumplist::JumpList,
}

impl OpenedTerminalFileGuard {
    pub fn capture(workspace: usize, generation: u64, path: &Path, panes: &Panes) -> Option<Self> {
        let pane = panes.layout.active();
        if !matches!(panes.content(pane), Some(Content::File(active)) if active == path) {
            return None;
        }
        Some(Self {
            workspace,
            generation,
            path: path.to_owned(),
            focus: panes.layout.focus_guard(),
            history: panes.layout.jump_list(pane).ok()?.clone(),
        })
    }

    pub fn is_current(
        &self,
        workspace: usize,
        generation: u64,
        path: &Path,
        panes: &Panes,
    ) -> bool {
        let pane = panes.layout.active();
        self.workspace == workspace
            && self.generation == generation
            && self.path == path
            && panes.layout.focus_guard_is_current(&self.focus)
            && matches!(panes.content(pane), Some(Content::File(active)) if active == path)
            && panes
                .layout
                .jump_list(pane)
                .is_ok_and(|history| *history == self.history)
    }
}

/// Record only after the requested file really replaced the captured pane.
#[allow(clippy::too_many_arguments)]
pub fn record_terminal_origin_after_open(
    panes: &mut Panes,
    numbers: &crate::buffer_numbers::BufferTracking,
    registry: &crate::buffer_registry::BufferRegistry,
    workspace: usize,
    attachment: u64,
    guard: &workspace_core::editor_panes::FocusGuard,
    path: &Path,
    origin: workspace_core::editor_jumplist::Location,
    anchor: workspace_core::editor_jumplist::AnchorId,
) -> bool {
    use crate::buffer_registry::Buffer;
    let terminal = Buffer::Terminal(attachment);
    let file = Buffer::File(path.to_owned());
    panes.layout.focus_guard_is_current(guard)
        && matches!(panes.content(panes.layout.active()), Some(Content::File(active)) if active == path)
        && registry.knows(&terminal)
        && registry.knows(&file)
        && numbers.number_for(workspace, &terminal).ok() == Some(origin.buffer)
        && panes
            .layout
            .record_jump_guarded_with_anchor(guard, origin, Some(anchor))
}

#[allow(clippy::too_many_arguments)]
pub fn stage_terminal_origin_file_jump(
    panes: &Panes,
    numbers: &crate::buffer_numbers::BufferTracking,
    registry: &crate::buffer_registry::BufferRegistry,
    workspace: usize,
    attachment: u64,
    guard: &workspace_core::editor_panes::FocusGuard,
    origin: workspace_core::editor_jumplist::Location,
    anchor: workspace_core::editor_jumplist::AnchorId,
    older: bool,
    count: usize,
    available: impl Fn(&Path) -> bool,
) -> Result<Option<StagedTerminalOriginJump>, String> {
    use crate::buffer_registry::Buffer;
    let pane = panes.layout.active();
    let source = Buffer::Terminal(attachment);
    if !panes.layout.focus_guard_is_current(guard)
        || panes.content(pane) != Some(&Content::Terminal(attachment))
        || !registry.knows(&source)
        || numbers.number_for(workspace, &source).ok() != Some(origin.buffer)
    {
        return Err("Cambió el panel o la identidad de terminal".into());
    }
    let Some(plan) = panes
        .layout
        .prepare_jump_with_anchor(origin, Some(anchor), older, count)
    else {
        return Ok(None);
    };
    let destination = match plan.target() {
        None => None,
        Some(location) => {
            let buffer = numbers
                .resolve(workspace, location.buffer)
                .map_err(|_| "Buffer no disponible")?;
            let Buffer::File(path) = &buffer else {
                return Err("El destino de terminal requiere otra restauración".into());
            };
            if plan.target_anchor().is_some() || !registry.knows(&buffer) || !available(path) {
                return Err("El archivo del salto no está cargado o cambió".into());
            }
            Some((path.clone(), location))
        }
    };
    let mut next = panes.clone();
    let mut next_registry = registry.clone();
    if !next.layout.commit_jump(plan, origin) {
        return Err("Cambió el historial del panel".into());
    }
    if let Some((path, _)) = &destination {
        next.set_content(Content::File(path.clone()))
            .map_err(|_| "No se pudo seleccionar el archivo")?;
        let target = Buffer::File(path.clone());
        next_registry.replace(pane, Some(&source), target.clone());
        next_registry.relist(&target);
    }
    Ok(Some(StagedTerminalOriginJump {
        panes: next,
        registry: next_registry,
        destination,
    }))
}

/// An asynchronous file operation belongs to the initiating window and
/// workspace, even when another window happens to show the same path.
#[derive(Clone, Debug)]
pub struct EditorContextGuard {
    workspace: usize,
    file_generation: u64,
    path: PathBuf,
    focus: Option<workspace_core::editor_panes::FocusGuard>,
}
impl EditorContextGuard {
    pub fn capture(
        workspace: usize,
        file_generation: u64,
        path: &Path,
        panes: Option<&Panes>,
    ) -> Option<Self> {
        if let Some(panes) = panes
            && !matches!(panes.content(panes.layout.active()), Some(Content::File(active)) if active == path)
        {
            return None;
        }
        Some(Self {
            workspace,
            file_generation,
            path: path.to_owned(),
            focus: panes.map(|panes| panes.layout.focus_guard()),
        })
    }
    pub fn is_current(
        &self,
        workspace: usize,
        file_generation: u64,
        path: &Path,
        panes: Option<&Panes>,
    ) -> bool {
        self.workspace == workspace
            && self.file_generation == file_generation
            && self.path == path
            && match (&self.focus, panes) {
                (Some(focus), Some(panes)) => {
                    panes.layout.focus_guard_is_current(focus)
                        && matches!(panes.content(panes.layout.active()), Some(Content::File(active)) if active == path)
                }
                (None, None) => true,
                _ => false,
            }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct VimBufferDeletePlan {
    pub close: Vec<PaneId>,
    /// When every window shows the buffer, the last view must be replaced;
    /// the caller must resolve and preflight a safe replacement first.
    pub replace: Option<PaneId>,
}

/// Plan Vim's :bdelete window topology without changing any pane or buffer.
/// Snacks' Space-b-d intentionally uses a different, pane-preserving path.
pub fn plan_vim_buffer_delete(panes: &Panes, path: &Path) -> Option<VimBufferDeletePlan> {
    plan_vim_buffer_delete_target(
        panes,
        &crate::buffer_registry::Buffer::File(path.to_owned()),
    )
}

pub fn plan_vim_buffer_delete_target(
    panes: &Panes,
    target: &crate::buffer_registry::Buffer,
) -> Option<VimBufferDeletePlan> {
    use crate::buffer_registry::Buffer;
    let order = panes.layout.order();
    let affected: Vec<_> = order
        .iter()
        .copied()
        .filter(|pane| match (panes.content(*pane), target) {
            (Some(Content::File(path)), Buffer::File(target)) => path == target,
            (Some(Content::Terminal(id)), Buffer::Terminal(target)) => id == target,
            _ => false,
        })
        .collect();
    if affected.is_empty() {
        return None;
    }
    let replace = (affected.len() == order.len()).then(|| *affected.last().expect("nonempty"));
    let close = affected
        .into_iter()
        .filter(|pane| Some(*pane) != replace)
        .collect();
    Some(VimBufferDeletePlan { close, replace })
}

/// Apply a preflighted :bdelete to private copies. No caller state changes if
/// the buffer, a pane or the final replacement no longer matches the plan.
pub fn stage_vim_buffer_delete(
    panes: &Panes,
    registry: &crate::buffer_registry::BufferRegistry,
    path: &Path,
    replacement: Option<crate::buffer_registry::Buffer>,
) -> Result<Option<(Panes, crate::buffer_registry::BufferRegistry, Vec<PaneId>)>, String> {
    use crate::buffer_registry::Buffer;
    stage_vim_buffer_delete_target(panes, registry, &Buffer::File(path.to_owned()), replacement)
}

/// This stages terminal layout and registry changes only. PTY input and
/// tmux-session lifetime remain the caller's separate responsibility.
pub fn stage_vim_buffer_delete_target(
    panes: &Panes,
    registry: &crate::buffer_registry::BufferRegistry,
    target: &crate::buffer_registry::Buffer,
    replacement: Option<crate::buffer_registry::Buffer>,
) -> Result<Option<(Panes, crate::buffer_registry::BufferRegistry, Vec<PaneId>)>, String> {
    use crate::buffer_registry::Buffer;
    let Some(plan) = plan_vim_buffer_delete_target(panes, target) else {
        return Ok(None);
    };
    if !registry.knows(target) {
        return Err("El registro no conoce el buffer".into());
    }
    if plan.replace.is_some()
        && replacement
            .as_ref()
            .is_none_or(|replacement| replacement == target || !registry.knows(replacement))
    {
        return Err("No hay reemplazo verificado para la última ventana".into());
    }
    let mut next_panes = panes.clone();
    let mut next_registry = registry.clone();
    let original_active = panes.layout.active();
    for id in &plan.close {
        next_panes.layout.focus(*id).map_err(|e| format!("{e:?}"))?;
        let previous = next_panes.layout.clone();
        next_panes.layout.close().map_err(|e| format!("{e:?}"))?;
        next_panes.sizes.remove(id);
        next_registry.close_pane(*id);
        next_panes.topology_changed_from(&previous);
    }
    if let Some(pane) = plan.replace {
        let replacement = replacement.expect("replacement preflighted");
        next_panes
            .layout
            .focus(pane)
            .map_err(|e| format!("{e:?}"))?;
        next_panes
            .set_content(match &replacement {
                Buffer::File(path) => Content::File(path.clone()),
                Buffer::Terminal(id) => Content::Terminal(*id),
                Buffer::Explorer(id) => Content::Explorer(*id),
                Buffer::Agents(id) => Content::Agents(*id),
            })
            .map_err(|e| format!("{e:?}"))?;
        next_registry.replace(pane, Some(target), replacement);
    } else if next_panes.layout.panes().contains_key(&original_active) {
        // Deleting a different buffer must not steal focus from a window
        // that survived the deletion.
        next_panes
            .layout
            .focus(original_active)
            .map_err(|e| format!("{e:?}"))?;
    }
    match target {
        Buffer::File(path) if !next_registry.unlist_file(path) => {
            return Err("No se pudo deslistar el buffer".into());
        }
        Buffer::File(_) => {}
        Buffer::Terminal(_) | Buffer::Explorer(_) | Buffer::Agents(_) => {
            next_registry.remove(target)
        }
    }
    Ok(Some((next_panes, next_registry, plan.close)))
}

pub struct VimDeleteWorkspace<'a> {
    pub project: usize,
    pub panes: &'a Panes,
    pub registry: crate::buffer_registry::BufferRegistry,
    pub replacement: Option<crate::buffer_registry::Buffer>,
}

pub struct StagedVimDelete {
    pub project: usize,
    pub panes: Panes,
    pub registry: crate::buffer_registry::BufferRegistry,
    pub closed: Vec<PaneId>,
}

/// Stage every workspace before returning any edits to the caller. If even one
/// pane or replacement fails, the caller receives no partially staged plan.
pub fn stage_vim_delete_workspaces(
    workspaces: &[VimDeleteWorkspace<'_>],
    path: &Path,
) -> Result<Vec<StagedVimDelete>, String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut staged = Vec::new();
    for source in workspaces {
        if !seen.insert(source.project) {
            return Err("Workspace duplicado en el plan".into());
        }
        if let Some((panes, registry, closed)) = stage_vim_buffer_delete(
            source.panes,
            &source.registry,
            path,
            source.replacement.clone(),
        )? {
            staged.push(StagedVimDelete {
                project: source.project,
                panes,
                registry,
                closed,
            });
        }
    }
    Ok(staged)
}
impl Panes {
    pub fn geometry_key(&self) -> GeometryKey {
        GeometryKey {
            root: self.layout.root().clone(),
            weights: self.weights.clone(),
            sidebar_dock: self.sidebar_dock.clone(),
            equalized: self.equalized,
        }
    }

    pub fn new(path: PathBuf, mut view: ViewState) -> Self {
        view.buffer = 1;
        Self {
            layout: Layout::new(view),
            contents: BTreeMap::from([(1, Content::File(path))]),
            content_identities: BTreeMap::new(),
            sizes: BTreeMap::new(),
            weights: BTreeMap::new(),
            sidebar_dock: BTreeSet::new(),
            equalized: false,
            wrap: BTreeMap::new(),
        }
    }

    pub fn wrap_state(&self, pane: PaneId) -> Option<&PaneWrap> {
        self.wrap.get(&pane)
    }

    pub fn wrap_state_mut(&mut self, pane: PaneId) -> Option<&mut PaneWrap> {
        self.layout
            .panes()
            .contains_key(&pane)
            .then(|| self.wrap.entry(pane).or_default())
    }

    /// Change one window's wrap option; splits inherit the current value.
    /// Return the effective value, whether it changed, and the previous top.
    pub fn configure_wrap(
        &mut self,
        pane: PaneId,
        requested: Option<bool>,
        path: Option<&Path>,
        source_top: usize,
    ) -> Option<(bool, bool, usize)> {
        let state = self.wrap_state_mut(pane)?;
        if let Some(path) = path {
            state.activate(path, source_top);
        }
        let was_enabled = state.enabled;
        let previous_top = state.top.source_line;
        let enabled = requested.unwrap_or(!was_enabled);
        if enabled != was_enabled {
            state.enabled = enabled;
            if enabled {
                state.top = WrappedTop {
                    source_line: source_top,
                    visual_row: 0,
                };
            }
            state.cursor = None;
            state.desired_column = None;
            state.wheel_goal = None;
            state.wheel_coladd = None;
            state.version = None;
            state.width = None;
            state.height = None;
            state.mode = None;
        }
        Some((enabled, enabled != was_enabled, previous_top))
    }

    pub fn duplicate_wrap(&mut self, source: PaneId, target: PaneId) {
        if !self.layout.panes().contains_key(&source) || !self.layout.panes().contains_key(&target)
        {
            return;
        }
        if let Some(mut state) = self.wrap.get(&source).cloned() {
            // Neovim initializes 'scroll' for the new window independently.
            state.scroll = PaneScroll::default();
            self.wrap.insert(target, state);
        } else {
            self.wrap.remove(&target);
        }
    }

    pub fn forget_wrap(&mut self, pane: PaneId) {
        self.wrap.remove(&pane);
    }
    pub fn terminal(id: u64) -> Self {
        let mut panes = Self::new(
            PathBuf::new(),
            ViewState {
                buffer: 1,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        panes.contents.insert(1, Content::Terminal(id));
        panes
    }
    /// Build the initial workspace as one tree. This does not start or stop
    /// any terminal or provider; identities are supplied by their owners.
    pub fn with_sidebar(
        terminal: u64,
        explorer: u64,
        agents: Option<u64>,
        right: bool,
    ) -> Result<Self, workspace_core::editor_panes::Error> {
        let mut panes = Self::terminal(terminal);
        let original = panes.layout.active();
        panes.layout.split(Axis::Row, right)?;
        panes.sidebar_dock.insert(panes.layout.active());
        panes.set_content(Content::Explorer(explorer))?;
        if let Some(agents) = agents {
            panes.layout.split(Axis::Column, true)?;
            panes.sidebar_dock.insert(panes.layout.active());
            panes.set_content(Content::Agents(agents))?;
        }
        panes.layout.focus(original)?;
        Ok(panes)
    }
    /// Add sidebar leaves next to the active content without changing the
    /// active view. Failure restores the original topology.
    pub fn insert_sidebar(
        &mut self,
        explorer: u64,
        agents: Option<u64>,
        right: bool,
    ) -> Result<(), workspace_core::editor_panes::Error> {
        let original = self.clone();
        let active = self.layout.active();
        let mut inserted = BTreeSet::new();
        let result = (|| {
            self.layout.split(Axis::Row, right)?;
            inserted.insert(self.layout.active());
            self.set_content(Content::Explorer(explorer))?;
            if let Some(agents) = agents {
                self.layout.split(Axis::Column, true)?;
                inserted.insert(self.layout.active());
                self.set_content(Content::Agents(agents))?;
            }
            self.layout.focus(active)
        })();
        if result.is_err() {
            *self = original;
        } else {
            // Adding both Explorer and Agents changes more than one leaf.
            // Keep only complete unchanged groups, so the normal sidebar
            // dock sizing remains intact for the newly created group.
            self.update_topology_weights(&original.layout, false, None);
            // Only dock a new sidebar when it occupies a complete root
            // sibling; a nested insertion may share a branch with files.
            if let Node::Split(Axis::Row, children) = self.layout.root()
                && children.iter().any(|child| {
                    let mut leaves = Vec::new();
                    Self::leaves(child, &mut leaves);
                    leaves.into_iter().collect::<BTreeSet<_>>() == inserted
                })
            {
                self.sidebar_dock.extend(inserted);
            }
        }
        result
    }
    /// Remove sidebar views only; never stop a process or close a file.
    pub fn remove_sidebar(&mut self) -> Result<Vec<PaneId>, workspace_core::editor_panes::Error> {
        let original = self.clone();
        let previous = self.layout.active();
        let mut removed = Vec::new();
        let result = (|| {
            let ids = self.layout.order();
            for id in ids {
                if matches!(
                    self.content(id),
                    Some(Content::Explorer(_) | Content::Agents(_))
                ) {
                    self.layout.focus(id)?;
                    let previous = self.layout.clone();
                    removed.push(self.layout.close()?);
                    self.sizes.remove(&id);
                    self.topology_changed_from(&previous);
                }
            }
            if self.layout.panes().contains_key(&previous) {
                self.layout.focus(previous)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            *self = original;
            return Err(error);
        }
        Ok(removed)
    }
    pub fn set_agents_visible(
        &mut self,
        agents: u64,
        visible: bool,
    ) -> Result<Vec<PaneId>, workspace_core::editor_panes::Error> {
        let original = self.clone();
        let active = self.layout.active();
        let mut removed = Vec::new();
        let mut added_to_dock = None;
        let result = (|| {
            let order = self.layout.order();
            if visible {
                if order
                    .iter()
                    .any(|id| matches!(self.content(*id), Some(Content::Agents(_))))
                {
                    return Ok(());
                }
                let explorer = order
                    .iter()
                    .copied()
                    .find(|id| matches!(self.content(*id), Some(Content::Explorer(_))))
                    .ok_or(workspace_core::editor_panes::Error::UnknownPane)?;
                self.layout.focus(explorer)?;
                let previous = self.layout.clone();
                self.layout.split(Axis::Column, true)?;
                if self.sidebar_dock.contains(&explorer) {
                    added_to_dock = Some(self.layout.active());
                }
                self.set_content(Content::Agents(agents))?;
                self.topology_changed_from(&previous);
            } else {
                for id in order {
                    if matches!(self.content(id), Some(Content::Agents(_))) {
                        self.layout.focus(id)?;
                        let previous = self.layout.clone();
                        removed.push(self.layout.close()?);
                        self.sizes.remove(&id);
                        self.topology_changed_from(&previous);
                    }
                }
            }
            if self.layout.panes().contains_key(&active) {
                self.layout.focus(active)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            *self = original;
            return Err(error);
        }
        if let Some(id) = added_to_dock {
            self.sidebar_dock.insert(id);
        }
        Ok(removed)
    }
    pub fn content(&self, id: PaneId) -> Option<&Content> {
        self.contents.get(&self.layout.panes().get(&id)?.buffer)
    }
    /// A token changes whenever this pane replaces its content, including a
    /// round trip back to the same buffer while an async operation is pending.
    pub fn track_content_identity(&mut self, id: PaneId) -> Option<Arc<()>> {
        self.content(id)?;
        Some(self.content_identities.entry(id).or_default().clone())
    }
    pub fn content_identity_is_current(&self, id: PaneId, identity: &Arc<()>) -> bool {
        self.content(id).is_some()
            && self
                .content_identities
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, identity))
    }
    fn content_changed(&mut self, id: PaneId) {
        if let Some(identity) = self.content_identities.get_mut(&id) {
            *identity = Arc::new(());
        }
    }
    fn leaves(node: &Node, ids: &mut Vec<PaneId>) {
        match node {
            Node::Pane(id) => ids.push(*id),
            Node::Split(_, children) => {
                for child in children {
                    Self::leaves(child, ids);
                }
            }
        }
    }
    /// Dock sizing follows window identities even when a buffer replaces a
    /// sidebar. If windows are rearranged across root siblings, the renderer
    /// falls back to ordinary sizing rather than docking multiple groups.
    pub fn contains_sidebar_dock(&self, node: &Node) -> bool {
        let mut ids = Vec::new();
        Self::leaves(node, &mut ids);
        ids.iter().any(|id| self.sidebar_dock.contains(id))
    }
    fn resize_group(node: &Node, active: PaneId, axis: Axis) -> Option<(&[Node], usize)> {
        let Node::Split(current, children) = node else {
            return None;
        };
        for (index, child) in children.iter().enumerate() {
            let mut ids = Vec::new();
            Self::leaves(child, &mut ids);
            if ids.contains(&active) {
                return Self::resize_group(child, active, axis).or_else(|| {
                    (*current == axis && children.len() > 1).then_some((children, index))
                });
            }
        }
        None
    }
    fn weight_key(axis: Axis, node: &Node) -> Option<(u8, Vec<PaneId>)> {
        let mut ids = Vec::new();
        Self::leaves(node, &mut ids);
        // A group's identity does not change when its windows are exchanged
        // or rotated. Preserve its outer weight across those operations.
        ids.sort_unstable();
        (!ids.is_empty()).then_some((if axis == Axis::Row { 0 } else { 1 }, ids))
    }
    pub fn weight(&self, axis: Axis, node: &Node) -> Option<f32> {
        self.weights.get(&Self::weight_key(axis, node)?).copied()
    }
    pub fn equalize(&mut self) {
        self.weights.clear();
        self.equalized = true;
    }
    /// Resizing weights describe the previous sibling groups. Reusing them
    /// after a split or close can give a new sibling almost zero space.
    pub fn topology_changed(&mut self) {
        self.weights.clear();
        self.content_identities
            .retain(|id, _| self.layout.panes().contains_key(id));
        self.sidebar_dock
            .retain(|id| self.layout.panes().contains_key(id));
        self.wrap
            .retain(|id, _| self.layout.panes().contains_key(id));
    }
    /// Retain a resized sibling group only when every old child maps to one
    /// distinct new child. A split inside one child can change its leaf set
    /// without changing that group's outer geometry. Ambiguous structural
    /// changes deliberately fall back to fresh sizing.
    pub fn topology_changed_from(&mut self, previous: &Layout) {
        self.update_topology_weights(previous, true, None);
    }
    /// Closing an inactive window can expand a neighbor other than the
    /// focused view. Preserve weights against that actual neighbor.
    pub fn topology_after_close(&mut self, previous: &Layout, neighbor: PaneId) {
        self.update_topology_weights(previous, true, Some(neighbor));
    }
    fn update_topology_weights(
        &mut self,
        previous: &Layout,
        remap_changed: bool,
        close_neighbor: Option<PaneId>,
    ) {
        self.content_identities
            .retain(|id, _| self.layout.panes().contains_key(id));
        // Closing a parent can promote a whole nested split to the root.
        // Match the complete set of direct sibling groups, not just individual
        // pane IDs, so unrelated groups never inherit one another's weights.
        fn has_group(old: &Node, axis: Axis, keys: &[(u8, Vec<PaneId>)]) -> bool {
            let Node::Split(old_axis, children) = old else {
                return false;
            };
            let mut old_keys: Vec<_> = children
                .iter()
                .filter_map(|child| Panes::weight_key(*old_axis, child))
                .collect();
            old_keys.sort_unstable();
            (*old_axis == axis && old_keys.len() == children.len() && old_keys == keys)
                || children.iter().any(|child| has_group(child, axis, keys))
        }
        fn preserve_groups(
            old: &Node,
            new: &Node,
            old_weights: &BTreeMap<(u8, Vec<PaneId>), f32>,
            kept: &mut BTreeMap<(u8, Vec<PaneId>), f32>,
        ) {
            let Node::Split(axis, children) = new else {
                return;
            };
            let mut keys: Vec<_> = children
                .iter()
                .filter_map(|child| Panes::weight_key(*axis, child))
                .collect();
            keys.sort_unstable();
            if keys.len() == children.len()
                && has_group(old, *axis, &keys)
                && keys.iter().all(|key| old_weights.contains_key(key))
            {
                for key in keys {
                    kept.insert(key.clone(), old_weights[&key]);
                }
            }
            for child in children {
                preserve_groups(old, child, old_weights, kept);
            }
        }
        fn remap(
            old: &Node,
            new: &Node,
            old_weights: &BTreeMap<(u8, Vec<PaneId>), f32>,
            kept: &mut BTreeMap<(u8, Vec<PaneId>), f32>,
            previous_active: PaneId,
            current_active: PaneId,
            added: Option<PaneId>,
            removed: Option<PaneId>,
        ) {
            let (Node::Split(axis, old_children), Node::Split(new_axis, new_children)) = (old, new)
            else {
                return;
            };
            if axis != new_axis {
                return;
            }
            let split = added.filter(|id| {
                new_children.len() == old_children.len() + 1
                    && new_children.contains(&Node::Pane(*id))
                    && old_children.contains(&Node::Pane(previous_active))
            });
            let close = removed.filter(|id| {
                old_children.len() == new_children.len() + 1
                    && old_children.contains(&Node::Pane(*id))
            });
            if old_children.len() != new_children.len() && split.is_none() && close.is_none() {
                return;
            }
            let mut pairs = Vec::new();
            for old_child in old_children {
                if close.is_some_and(|id| *old_child == Node::Pane(id)) {
                    continue;
                }
                let mut old_ids = Vec::new();
                Panes::leaves(old_child, &mut old_ids);
                let matches: Vec<_> = new_children
                    .iter()
                    .enumerate()
                    .filter(|(_, child)| {
                        let mut new_ids = Vec::new();
                        Panes::leaves(child, &mut new_ids);
                        new_ids.iter().any(|id| old_ids.contains(id))
                    })
                    .collect();
                if matches.len() != 1 || pairs.iter().any(|(index, _, _)| *index == matches[0].0) {
                    return;
                }
                pairs.push((matches[0].0, old_child, matches[0].1));
            }
            let mut values: Option<Vec<_>> = pairs
                .iter()
                .map(|(_, child, next)| {
                    Some((
                        Panes::weight_key(*axis, next)?,
                        *old_weights.get(&Panes::weight_key(*axis, child)?)?,
                    ))
                })
                .collect();
            if let Some(values) = &mut values {
                if let Some(id) = split {
                    let source = pairs
                        .iter()
                        .position(|(_, child, _)| **child == Node::Pane(previous_active));
                    if let Some(source) = source {
                        let part = values[source].1 / 2.;
                        values[source].1 = part;
                        values.push((
                            Panes::weight_key(*axis, &Node::Pane(id)).expect("pane has a key"),
                            part,
                        ));
                    } else {
                        return;
                    }
                }
                if let Some(id) = close {
                    let destination = pairs.iter().position(|(_, _, child)| {
                        let mut ids = Vec::new();
                        Panes::leaves(child, &mut ids);
                        ids.contains(&current_active)
                    });
                    let removed_weight = old_weights
                        .get(&Panes::weight_key(*axis, &Node::Pane(id)).expect("pane has a key"));
                    if let (Some(destination), Some(weight)) = (destination, removed_weight) {
                        values[destination].1 += *weight;
                    } else {
                        return;
                    }
                }
                kept.extend(values.iter().cloned());
            }
            for (_, child, next) in pairs {
                remap(
                    child,
                    next,
                    old_weights,
                    kept,
                    previous_active,
                    current_active,
                    added,
                    removed,
                );
            }
        }
        let added = self
            .layout
            .panes()
            .keys()
            .find(|id| !previous.panes().contains_key(id))
            .copied();
        let removed = previous
            .panes()
            .keys()
            .find(|id| !self.layout.panes().contains_key(id))
            .copied();
        let mut kept = BTreeMap::new();
        preserve_groups(
            previous.root(),
            self.layout.root(),
            &self.weights,
            &mut kept,
        );
        if remap_changed {
            remap(
                previous.root(),
                self.layout.root(),
                &self.weights,
                &mut kept,
                previous.active(),
                close_neighbor.unwrap_or_else(|| self.layout.active()),
                added,
                removed,
            );
        }
        self.topology_changed();
        self.weights = kept;
    }
    pub fn is_equalized(&self) -> bool {
        self.equalized
    }
    pub fn exchange(&mut self, count: usize) -> Result<(), workspace_core::editor_panes::Error> {
        // Neovim's window sizes follow the exchanged window identities.
        self.layout.exchange(count)?;
        Ok(())
    }
    fn subtree_minimum(node: &Node, axis: Axis, minimums: &BTreeMap<PaneId, i64>) -> i64 {
        match node {
            Node::Pane(id) => minimums.get(id).copied().unwrap_or(40).max(1),
            Node::Split(split_axis, children) if *split_axis == axis => {
                children.iter().fold(0_i64, |total, child| {
                    total.saturating_add(Self::subtree_minimum(child, axis, minimums))
                })
            }
            Node::Split(_, children) => children
                .iter()
                .map(|child| Self::subtree_minimum(child, axis, minimums))
                .max()
                .unwrap_or(40),
        }
    }
    /// Stage a split with a requested new-view size using the last measured
    /// geometry. Keep the original state if any sibling lacks a measurement.
    pub fn split_sized(
        &mut self,
        axis: Axis,
        after: bool,
        requested: i64,
        minimum: i64,
        bounds: &[workspace_core::editor_panes::Bounds],
    ) -> Result<PaneId, workspace_core::editor_panes::Error> {
        use workspace_core::editor_panes::Error;
        if !self.layout.geometry_valid(bounds) {
            return Err(Error::InvalidGeometry);
        }
        let source = self.layout.active();
        let source_bounds = bounds
            .iter()
            .find(|rect| rect.pane == source)
            .ok_or(Error::InvalidGeometry)?;
        let total = if axis == Axis::Row {
            source_bounds.width
        } else {
            source_bounds.height
        };
        let minimum = minimum.max(1);
        if total < minimum.saturating_mul(2) {
            return Err(Error::InvalidGeometry);
        }
        let target_size = requested.clamp(minimum, total - minimum);
        let mut staged = self.clone();
        let previous = staged.layout.clone();
        let created = staged.layout.split(axis, after)?;
        staged.topology_changed_from(&previous);
        let (children, _) = Self::resize_group(staged.layout.root(), created, axis)
            .ok_or(Error::InvalidGeometry)?;
        if !children.contains(&Node::Pane(source)) || !children.contains(&Node::Pane(created)) {
            return Err(Error::InvalidGeometry);
        }
        let mut sizes = Vec::new();
        for child in children {
            let key = Self::weight_key(axis, child).ok_or(Error::InvalidGeometry)?;
            let size = if *child == Node::Pane(source) {
                total - target_size
            } else if *child == Node::Pane(created) {
                target_size
            } else {
                let mut ids = Vec::new();
                Self::leaves(child, &mut ids);
                let mut start = i64::MAX;
                let mut end = i64::MIN;
                for id in ids {
                    let rect = bounds
                        .iter()
                        .find(|rect| rect.pane == id)
                        .ok_or(Error::InvalidGeometry)?;
                    let (origin, extent) = if axis == Axis::Row {
                        (rect.x, rect.width)
                    } else {
                        (rect.y, rect.height)
                    };
                    if extent <= 0 {
                        return Err(Error::InvalidGeometry);
                    }
                    start = start.min(origin);
                    end = end.max(origin.saturating_add(extent));
                }
                end.saturating_sub(start)
            };
            if size <= 0 {
                return Err(Error::InvalidGeometry);
            }
            sizes.push((key, size as f32));
        }
        for (key, size) in sizes {
            staged.weights.insert(key, size);
        }
        *self = staged;
        Ok(created)
    }
    /// Adjust the nearest matching split using measured screen geometry.
    /// Returns false without mutation when geometry or minimum space is missing.
    pub fn resize(
        &mut self,
        axis: Axis,
        delta: i64,
        bounds: &[workspace_core::editor_panes::Bounds],
        minimums: &BTreeMap<PaneId, i64>,
    ) -> bool {
        let Some((children, active)) =
            Self::resize_group(self.layout.root(), self.layout.active(), axis)
        else {
            return true;
        };
        let mut sizes = Vec::new();
        for child in children {
            let mut ids = Vec::new();
            Self::leaves(child, &mut ids);
            let mut start = i64::MAX;
            let mut end = i64::MIN;
            for id in ids {
                let Some(rect) = bounds.iter().find(|rect| rect.pane == id) else {
                    return false;
                };
                let (origin, extent) = if axis == Axis::Row {
                    (rect.x, rect.width)
                } else {
                    (rect.y, rect.height)
                };
                if extent <= 0 {
                    return false;
                }
                start = start.min(origin);
                end = end.max(origin.saturating_add(extent));
            }
            let Some(key) = Self::weight_key(axis, child) else {
                return false;
            };
            sizes.push((key, end.saturating_sub(start)));
        }
        // Neovim moves as far as possible when a count exceeds available space.
        // Measurements can round a pane just below its minimum, or a small
        // window can already be below it. Let it grow but never shrink more.
        let active_min =
            sizes[active]
                .1
                .min(Self::subtree_minimum(&children[active], axis, minimums));
        let neighbors = (active + 1..children.len()).chain((0..active).rev());
        let mut moved = 0_i64;
        if delta > 0 {
            let mut left = delta;
            for neighbor in neighbors {
                let minimum = sizes[neighbor].1.min(Self::subtree_minimum(
                    &children[neighbor],
                    axis,
                    minimums,
                ));
                let amount = left.min(sizes[neighbor].1 - minimum);
                let Some(enlarged) = sizes[active].1.checked_add(amount) else {
                    return false;
                };
                sizes[active].1 = enlarged;
                sizes[neighbor].1 -= amount;
                left -= amount;
                moved += amount;
                if left == 0 {
                    break;
                }
            }
        } else if delta < 0 {
            let amount = delta.saturating_neg().min(sizes[active].1 - active_min);
            if let Some(neighbor) = neighbors.into_iter().next() {
                let Some(enlarged) = sizes[neighbor].1.checked_add(amount) else {
                    return false;
                };
                sizes[neighbor].1 = enlarged;
                sizes[active].1 -= amount;
                moved = amount;
            }
        }
        if moved == 0 {
            return true;
        }
        for (key, size) in sizes {
            self.weights.insert(key, size as f32);
        }
        true
    }
    pub fn path(&self, id: PaneId) -> Option<&PathBuf> {
        match self.content(id)? {
            Content::File(path) => Some(path),
            Content::Terminal(_) | Content::Explorer(_) | Content::Agents(_) => None,
        }
    }
    fn identity(&mut self, content: Content) -> u64 {
        if let Some((id, _)) = self.contents.iter().find(|(_, c)| **c == content) {
            return *id;
        }
        let id = self.contents.keys().next_back().copied().unwrap_or(0) + 1;
        self.contents.insert(id, content);
        id
    }
    /// Changes the active view only; creating or destroying the PTY remains
    /// the backend's responsibility, including after a failed split.
    pub fn set_terminal(&mut self, id: u64) -> Result<(), workspace_core::editor_panes::Error> {
        self.set_content(Content::Terminal(id))
    }
    /// Reveal an existing terminal view, or make room for it without hiding
    /// the last visible view of the current file.
    pub fn reveal_terminal(
        &mut self,
        attachment: u64,
    ) -> Result<PaneId, workspace_core::editor_panes::Error> {
        if let Some(id) = self
            .layout
            .order()
            .into_iter()
            .find(|id| self.content(*id) == Some(&Content::Terminal(attachment)))
        {
            self.layout.focus(id)?;
            return Ok(id);
        }
        let active = self.layout.active();
        if matches!(
            self.content(active),
            Some(Content::Explorer(_) | Content::Agents(_))
        ) {
            let previous = self.layout.clone();
            let created = self.layout.split(Axis::Row, true)?;
            self.duplicate_wrap(active, created);
            self.topology_changed_from(&previous);
        }
        if let Some(Content::File(path)) = self.content(active) {
            let views = self
                .layout
                .order()
                .into_iter()
                .filter(|id| self.path(*id) == Some(path))
                .count();
            if views == 1 {
                let previous = self.layout.clone();
                let created = self.layout.split(Axis::Row, true)?;
                self.duplicate_wrap(active, created);
                self.topology_changed_from(&previous);
            }
        }
        self.set_terminal(attachment)?;
        Ok(self.layout.active())
    }
    /// Keep each window's alternate in step with the pane actually changed.
    /// Reusing an existing terminal only changes focus, so its alternate stays.
    pub fn reveal_terminal_with_registry(
        &mut self,
        registry: &mut crate::buffer_registry::BufferRegistry,
        attachment: u64,
    ) -> Result<PaneId, workspace_core::editor_panes::Error> {
        use crate::buffer_registry::Buffer;
        let terminal = Buffer::Terminal(attachment);
        let existing = self
            .layout
            .order()
            .into_iter()
            .find(|id| self.content(*id) == Some(&Content::Terminal(attachment)));
        let previous = match self.content(self.layout.active()) {
            Some(Content::File(path)) => Some(Buffer::File(path.clone())),
            Some(Content::Terminal(id)) => Some(Buffer::Terminal(*id)),
            _ => None,
        };
        let mut panes = self.clone();
        let mut next_registry = registry.clone();
        let pane = panes.reveal_terminal(attachment)?;
        if existing.is_some() {
            next_registry.register(terminal.clone());
            next_registry.touch(&terminal);
        } else {
            if let Some(old) = &previous {
                next_registry.register(old.clone());
            }
            next_registry.replace(pane, previous.as_ref(), terminal);
        }
        *self = panes;
        *registry = next_registry;
        Ok(pane)
    }
    /// Rebind every view of one disconnected attachment after reconnecting the
    /// same tmux session. Other terminal and file identities are untouched.
    pub fn rebind_terminal(&mut self, old: u64, new: u64) {
        if old == new {
            return;
        }
        let changed: Vec<_> = self
            .layout
            .order()
            .into_iter()
            .filter(|id| self.content(*id) == Some(&Content::Terminal(old)))
            .collect();
        for id in changed {
            self.content_changed(id);
        }
        for content in self.contents.values_mut() {
            if *content == Content::Terminal(old) {
                *content = Content::Terminal(new);
            }
        }
    }
    /// Replace every view of a deleted buffer, including inactive views.
    pub fn replace_file(&mut self, old: &Path, new: &Path) {
        if old == new {
            return;
        }
        let changed: Vec<_> = self
            .layout
            .order()
            .into_iter()
            .filter(|id| self.path(*id).is_some_and(|path| path == old))
            .collect();
        for id in changed {
            self.content_changed(id);
        }
        for content in self.contents.values_mut() {
            if matches!(content, Content::File(path) if path == old) {
                *content = Content::File(new.to_owned());
            }
        }
    }
    /// A buffer that receives a name keeps its window-local views and wrap
    /// state; all windows displaying it must follow its new identity.
    pub fn rebind_file(&mut self, old: &Path, new: &Path) {
        self.replace_file(old, new);
        for state in self.wrap.values_mut() {
            if state.path.as_deref() == Some(old) {
                state.path = Some(new.to_owned());
            }
        }
    }
    /// Replace every view of a deleted file with a terminal identity. The
    /// terminal process is owned separately and is never started or stopped.
    pub fn replace_file_with_terminal(&mut self, old: &Path, terminal: u64) {
        let changed: Vec<_> = self
            .layout
            .order()
            .into_iter()
            .filter(|id| self.path(*id).is_some_and(|path| path == old))
            .collect();
        for id in changed {
            self.content_changed(id);
        }
        for content in self.contents.values_mut() {
            if matches!(content, Content::File(path) if path == old) {
                *content = Content::Terminal(terminal);
            }
        }
    }
    /// Replace each visible occurrence independently, without changing panes
    /// that display another buffer. A complete plan is required before edits.
    pub fn replace_file_views(
        &mut self,
        old: &Path,
        replacements: &BTreeMap<PaneId, Content>,
    ) -> bool {
        let affected: Vec<_> = self
            .layout
            .order()
            .into_iter()
            .filter(|pane| self.path(*pane).is_some_and(|path| path == old))
            .collect();
        if affected.is_empty() || affected.iter().any(|pane| !replacements.contains_key(pane)) {
            return false;
        }
        for pane in affected {
            if self.content(pane) != Some(&replacements[&pane]) {
                self.content_changed(pane);
            }
            let buffer = self.identity(replacements[&pane].clone());
            if let Ok(view) = self.layout.view_mut(pane) {
                view.buffer = buffer;
            }
        }
        true
    }
    /// Replace every view of one terminal without changing the window layout.
    pub fn replace_terminal_views(
        &mut self,
        terminal: u64,
        replacements: &BTreeMap<PaneId, Content>,
    ) -> bool {
        let affected: Vec<_> = self
            .layout
            .order()
            .into_iter()
            .filter(|pane| self.content(*pane) == Some(&Content::Terminal(terminal)))
            .collect();
        if affected.is_empty() || affected.iter().any(|pane| !replacements.contains_key(pane)) {
            return false;
        }
        for pane in affected {
            if self.content(pane) != Some(&replacements[&pane]) {
                self.content_changed(pane);
            }
            let buffer = self.identity(replacements[&pane].clone());
            if let Ok(view) = self.layout.view_mut(pane) {
                view.buffer = buffer;
            }
        }
        true
    }
    /// Replace the buffer in this view without changing another view or
    /// stopping any backend process. A split copies the same identity first.
    pub fn set_content(
        &mut self,
        content: Content,
    ) -> Result<(), workspace_core::editor_panes::Error> {
        let pane = self.layout.active();
        if self.content(pane) != Some(&content) {
            self.content_changed(pane);
        }
        let buffer = self.identity(content);
        self.layout.view_mut(pane)?.buffer = buffer;
        Ok(())
    }
    pub fn save(&mut self, path: &Path, mut view: ViewState) {
        let pane = self.layout.active();
        if self.path(pane).is_none_or(|current| current != path) {
            self.content_changed(pane);
        }
        view.buffer = self.identity(Content::File(path.to_owned()));
        if let Ok(active) = self.layout.view_mut(pane) {
            *active = view;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_to_terminal_restore_is_staged_and_revalidated() {
        use crate::{
            buffer_numbers::BufferTracking,
            buffer_registry::{Buffer, BufferRegistry},
        };
        use workspace_core::editor_jumplist::{AnchorId, Location, View};
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-two-term.json"
        ))
        .unwrap();
        let first = Buffer::Terminal(77);
        let second = Buffer::Terminal(88);
        let mut numbers = BufferTracking::default();
        numbers.register(1, first.clone());
        numbers.register(1, second.clone());
        let mut registry = BufferRegistry::default();
        registry.register(first.clone());
        registry.register(second.clone());
        let location = |buffer: &Buffer, case: &str| Location {
            buffer: numbers.number_for(1, buffer).unwrap(),
            row: reference["cases"][case]["cursor"][0].as_u64().unwrap() as usize - 1,
            column: reference["cases"][case]["cursor"][1].as_u64().unwrap() as usize,
            view: View::default(),
        };
        let first_location = location(&first, "before");
        let second_location = location(&second, "second");
        let first_anchor = AnchorId::new(1).unwrap();
        let second_anchor = AnchorId::new(2).unwrap();
        let mut panes = Panes::terminal(88);
        let pane = panes.layout.active();
        panes
            .layout
            .jump_list_mut(pane)
            .unwrap()
            .record_with_anchor(first_location, Some(first_anchor));
        let focus = panes.layout.focus_guard();
        let available = |id, loc, anchor| (id, loc, anchor) == (77, first_location, first_anchor);
        let pending = PendingTerminalOriginRestore::prepare(
            &panes,
            &numbers,
            &registry,
            1,
            7,
            88,
            &focus,
            second_location,
            second_anchor,
            true,
            1,
            available,
        )
        .unwrap()
        .unwrap();
        assert_eq!(pending.destination(), (77, first_location, first_anchor));
        assert_eq!(panes.content(pane), Some(&Content::Terminal(88)));
        assert!(
            pending
                .finish(&panes, &numbers, &registry, 1, 8, 88, available)
                .is_err()
        );
        assert!(
            pending
                .finish(&panes, &numbers, &registry, 1, 7, 88, |_, _, _| false)
                .is_err()
        );
        let mut changed = panes.clone();
        changed
            .layout
            .jump_list_mut(pane)
            .unwrap()
            .record(second_location);
        assert!(
            pending
                .finish(&changed, &numbers, &registry, 1, 7, 88, available)
                .is_err()
        );
        let mut refocused = panes.clone();
        let other = refocused.layout.split(Axis::Row, true).unwrap();
        refocused.layout.focus(other).unwrap();
        refocused.layout.focus(pane).unwrap();
        assert!(
            pending
                .finish(&refocused, &numbers, &registry, 1, 7, 88, available)
                .is_err()
        );
        let mut removed = registry.clone();
        removed.remove(&first);
        assert!(
            pending
                .finish(&panes, &numbers, &removed, 1, 7, 88, available)
                .is_err()
        );
        let (back, back_registry) = pending
            .finish(&panes, &numbers, &registry, 1, 7, 88, available)
            .unwrap();
        assert_eq!(back.content(pane), Some(&Content::Terminal(77)));
        assert_eq!(reference["cases"]["back"]["buffer"], "first");
        assert_eq!(panes.content(pane), Some(&Content::Terminal(88)));
        let next_focus = back.layout.focus_guard();
        let forward = PendingTerminalOriginRestore::prepare(
            &back,
            &numbers,
            &back_registry,
            1,
            8,
            77,
            &next_focus,
            first_location,
            first_anchor,
            false,
            1,
            |id, loc, anchor| (id, loc, anchor) == (88, second_location, second_anchor),
        )
        .unwrap()
        .unwrap();
        assert_eq!(forward.destination(), (88, second_location, second_anchor));
        let (restored, _) = forward
            .finish(
                &back,
                &numbers,
                &back_registry,
                1,
                8,
                77,
                |id, loc, anchor| (id, loc, anchor) == (88, second_location, second_anchor),
            )
            .unwrap();
        assert_eq!(restored.content(pane), Some(&Content::Terminal(88)));
        assert_eq!(reference["cases"]["forward"]["buffer"], "second");
    }
    #[test]
    fn terminal_origin_forward_jump_is_staged_and_preserves_window_anchor() {
        use crate::{
            buffer_numbers::BufferTracking,
            buffer_registry::{Buffer, BufferRegistry},
        };
        use workspace_core::editor_jumplist::{AnchorId, Location, View};
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-terminal.json"
        ))
        .unwrap();
        let file_path = PathBuf::from("/synthetic/file.txt");
        let file = Buffer::File(file_path.clone());
        let terminal = Buffer::Terminal(77);
        let mut numbers = BufferTracking::default();
        numbers.register(1, file.clone());
        numbers.register(1, terminal.clone());
        let mut registry = BufferRegistry::default();
        registry.register(file.clone());
        registry.register(terminal.clone());
        let mut panes = Panes::new(
            file_path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let terminal_location = Location {
            buffer: numbers.number_for(1, &terminal).unwrap(),
            row: 1,
            column: 1,
            view: View::default(),
        };
        let file_location = Location {
            buffer: numbers.number_for(1, &file).unwrap(),
            row: 0,
            column: 0,
            view: View::default(),
        };
        let saved = AnchorId::new(11).unwrap();
        let active = panes.layout.active();
        panes
            .layout
            .jump_list_mut(active)
            .unwrap()
            .record_with_anchor(terminal_location, Some(saved));
        let backwards = panes.layout.prepare_jump(file_location, true, 1).unwrap();
        assert_eq!(backwards.target_anchor(), Some(saved));
        assert!(panes.layout.commit_jump(backwards, file_location));
        panes.set_content(Content::Terminal(77)).unwrap();
        let guard = panes.layout.focus_guard();
        let new_anchor = AnchorId::new(12).unwrap();
        let rejected = stage_terminal_origin_file_jump(
            &panes,
            &numbers,
            &registry,
            1,
            77,
            &guard,
            terminal_location,
            new_anchor,
            false,
            1,
            |_| false,
        );
        assert!(rejected.is_err());
        assert_eq!(panes.content(active), Some(&Content::Terminal(77)));
        let staged = stage_terminal_origin_file_jump(
            &panes,
            &numbers,
            &registry,
            1,
            77,
            &guard,
            terminal_location,
            new_anchor,
            false,
            1,
            |path| path == file_path,
        )
        .unwrap()
        .unwrap();
        let (path, location) = staged.destination.unwrap();
        assert_eq!(path, file_path);
        assert_eq!(
            (location.row + 1, location.column),
            (
                reference["after_forward"]["cursor"][0].as_u64().unwrap() as usize,
                reference["after_forward"]["cursor"][1].as_u64().unwrap() as usize
            )
        );
        assert_eq!(staged.panes.content(active), Some(&Content::File(path)));
        let back = staged
            .panes
            .layout
            .prepare_jump(file_location, true, 1)
            .unwrap();
        assert_eq!(back.target_anchor(), Some(saved));
        assert_eq!(panes.content(active), Some(&Content::Terminal(77)));

        assert!(registry.mark_unloaded(&file_path));
        let file_number = numbers.number_for(1, &file).unwrap();
        let pending = PendingTerminalOriginLoad::prepare(
            &panes,
            &numbers,
            &registry,
            1,
            9,
            77,
            &guard,
            terminal_location,
            new_anchor,
            false,
            1,
            |number| number == file_number,
        )
        .unwrap()
        .unwrap();
        assert_eq!(pending.path(), file_path);
        assert_eq!(panes.content(active), Some(&Content::Terminal(77)));
        assert!(
            pending
                .finish(&panes, &numbers, &registry, 1, 10, 77, |_| true)
                .is_err()
        );
        assert!(
            pending
                .finish(&panes, &numbers, &registry, 1, 9, 78, |_| true)
                .is_err()
        );
        assert!(
            pending
                .finish(&panes, &numbers, &registry, 1, 9, 77, |_| false)
                .is_err()
        );
        let mut moved = panes.clone();
        moved.layout.split(Axis::Row, true).unwrap();
        moved.layout.focus(active).unwrap();
        assert!(
            pending
                .finish(&moved, &numbers, &registry, 1, 9, 77, |_| true)
                .is_err()
        );
        let mut changed = panes.clone();
        changed
            .layout
            .jump_list_mut(active)
            .unwrap()
            .record(Location {
                row: 9,
                ..file_location
            });
        assert!(
            pending
                .finish(&changed, &numbers, &registry, 1, 9, 77, |_| true)
                .is_err()
        );
        let loaded = pending
            .finish(&panes, &numbers, &registry, 1, 9, 77, |path| {
                path == file_path
            })
            .unwrap();
        assert_eq!(loaded.destination, Some((file_path.clone(), file_location)));
        assert_eq!(
            loaded.panes.content(active),
            Some(&Content::File(file_path.clone()))
        );
        assert_eq!(panes.content(active), Some(&Content::Terminal(77)));
    }
    #[test]
    fn staged_buffer_switch_records_the_origin_like_neovim() {
        use crate::{buffer_numbers::BufferTracking, buffer_registry::Buffer};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-buffers.json"
        ))
        .unwrap();
        let a = Buffer::File("/synthetic/a.txt".into());
        let b = Buffer::File("/synthetic/b.txt".into());
        let mut numbers = BufferTracking::default();
        numbers.register(1, a.clone());
        numbers.register(1, b.clone());
        let a_id = numbers.number_for(1, &a).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let mut panes = Panes::new(
                "/synthetic/a.txt".into(),
                ViewState {
                    buffer: 0,
                    row: 1,
                    column: 1,
                    top: 0,
                    left: 0,
                },
            );
            let old = panes.layout.active();
            if name == "split" {
                panes.layout.split(Axis::Column, true).unwrap();
            }
            let active = panes.layout.active();
            let target = if name == "same" { a.clone() } else { b.clone() };
            let after = stage_listed_buffer_switch(&panes, &numbers, 1, target).unwrap();
            let jumps = after.layout.jump_list(active).unwrap().entries();
            let expected = case["after"]["jumps"].as_array().unwrap();
            assert_eq!(jumps.len(), expected.len(), "{name}");
            for (actual, expected) in jumps.iter().zip(expected) {
                assert_eq!(
                    (actual.buffer, actual.row + 1, actual.column),
                    (
                        a_id,
                        expected["row"].as_u64().unwrap() as usize,
                        expected["col"].as_u64().unwrap() as usize
                    ),
                    "{name}"
                );
            }
            assert!(
                panes.layout.jump_list(active).unwrap().entries().is_empty(),
                "staging must not mutate source"
            );
            if name == "split" {
                assert!(after.layout.jump_list(old).unwrap().entries().is_empty());
            }
        }
        let panes = Panes::new(
            "/synthetic/a.txt".into(),
            ViewState {
                buffer: 0,
                row: 1,
                column: 1,
                top: 0,
                left: 0,
            },
        );
        let denied = stage_listed_buffer_switch(&panes, &numbers, 2, b).unwrap();
        assert!(
            denied
                .layout
                .jump_list(denied.layout.active())
                .unwrap()
                .entries()
                .is_empty()
        );
    }
    #[test]
    fn inactive_window_status_uses_its_own_saved_cursor() {
        let mut editor = Editor::new("one\ntwo\nthree\nfour".into());
        editor.goto(3, 2);
        let saved = ViewState {
            buffer: 1,
            row: 1,
            column: 1,
            top: 0,
            left: 0,
        };
        assert_eq!(status_position(false, &saved, Some(&editor)), (1, 1));
        assert_eq!(status_position(true, &saved, Some(&editor)), (3, 2));
        assert_eq!(status_position(true, &saved, None), (1, 1));
    }
    #[test]
    fn async_editor_context_rejects_focus_reuse_workspace_and_generation_changes() {
        let path = PathBuf::from("/synthetic/search.txt");
        let mut panes = Panes::new(
            path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let original = panes.layout.active();
        let guard = EditorContextGuard::capture(7, 4, &path, Some(&panes)).unwrap();
        assert!(guard.is_current(7, 4, &path, Some(&panes)));
        assert!(!guard.is_current(8, 4, &path, Some(&panes)));
        assert!(!guard.is_current(7, 5, &path, Some(&panes)));
        assert!(!guard.is_current(7, 4, Path::new("/synthetic/other"), Some(&panes)));
        let other = panes.layout.split(Axis::Row, true).unwrap();
        assert!(!guard.is_current(7, 4, &path, Some(&panes)));
        panes.layout.focus(original).unwrap();
        assert!(
            !guard.is_current(7, 4, &path, Some(&panes)),
            "focus away and back must invalidate"
        );
        let new_guard = EditorContextGuard::capture(7, 4, &path, Some(&panes)).unwrap();
        panes.set_content(Content::Terminal(2)).unwrap();
        assert!(!new_guard.is_current(7, 4, &path, Some(&panes)));
        assert!(EditorContextGuard::capture(7, 4, &path, Some(&panes)).is_none());
        panes.layout.focus(other).unwrap();
        let unrelated = Panes::new(
            path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        assert!(!guard.is_current(7, 4, &path, Some(&unrelated)));
        let no_panes = EditorContextGuard::capture(7, 4, &path, None).unwrap();
        assert!(no_panes.is_current(7, 4, &path, None));
        assert!(!no_panes.is_current(7, 4, &path, Some(&unrelated)));
    }

    #[test]
    fn late_search_from_another_window_cannot_move_shared_editor() {
        let path = PathBuf::from("/synthetic/search.txt");
        let mut panes = Panes::new(
            path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let guard = EditorContextGuard::capture(1, 9, &path, Some(&panes)).unwrap();
        let mut editor = Editor::new("start\nneedle\nend".into());
        editor.key("/");
        editor.insert("needle");
        let job = editor.take_search_job().unwrap();
        editor.key("<Enter>");
        panes.layout.split(Axis::Row, true).unwrap();
        assert!(
            matches!(panes.content(panes.layout.active()), Some(Content::File(p)) if p == &path)
        );
        let output = job.run();
        assert!(!guard.is_current(1, 9, &path, Some(&panes)));
        assert!(editor.cancel_search_result(&output));
        assert!(!editor.apply_search(output));
        assert_eq!(editor.position(), (0, 0));
        assert!(!editor.search_busy());
    }
    #[test]
    fn vim_buffer_delete_plan_matches_clean_neovim_window_topology() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-bdelete-layout-reference.json"
        ))
        .unwrap();
        let a = PathBuf::from("/synthetic/a");
        let b = PathBuf::from("/synthetic/b");
        for case in reference.as_array().unwrap() {
            let mut panes = Panes::new(
                a.clone(),
                ViewState {
                    buffer: 0,
                    row: 0,
                    column: 0,
                    top: 0,
                    left: 0,
                },
            );
            match case["case"].as_str().unwrap() {
                "all" => {
                    panes.set_content(Content::File(b.clone())).unwrap();
                    panes.layout.split(Axis::Row, false).unwrap();
                    panes.layout.split(Axis::Column, false).unwrap();
                }
                "mixed" => {
                    panes.layout.split(Axis::Row, false).unwrap();
                    panes.set_content(Content::File(b.clone())).unwrap();
                    panes.layout.split(Axis::Column, false).unwrap();
                }
                "one" => {
                    panes.layout.split(Axis::Row, false).unwrap();
                    panes.set_content(Content::File(b.clone())).unwrap();
                }
                _ => unreachable!(),
            }
            let before = panes.layout.order();
            let names: Vec<_> = before
                .iter()
                .map(|id| {
                    if panes.path(*id) == Some(&b) {
                        "b"
                    } else {
                        "a"
                    }
                })
                .collect();
            let expected: Vec<_> = case["before"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(names, expected);
            let mut registry = crate::buffer_registry::BufferRegistry::default();
            let a_buffer = crate::buffer_registry::Buffer::File(a.clone());
            let b_buffer = crate::buffer_registry::Buffer::File(b.clone());
            registry.register(a_buffer.clone());
            registry.register(b_buffer.clone());
            let (staged, staged_registry, _) =
                stage_vim_buffer_delete(&panes, &registry, &b, Some(a_buffer))
                    .unwrap()
                    .unwrap();
            // Staging leaves live state untouched, including its listing.
            assert_eq!(panes.layout.order(), before);
            assert!(registry.listed().contains(&b_buffer));
            assert!(staged_registry.is_unlisted(&b_buffer));
            assert!(staged_registry.is_unloaded(&b));
            panes = staged;
            let remaining: Vec<_> = panes
                .layout
                .order()
                .iter()
                .map(|id| before.iter().position(|original| original == id).unwrap())
                .collect();
            let expected: Vec<_> = case["remaining"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as usize)
                .collect();
            assert_eq!(remaining, expected);
            assert_eq!(
                before
                    .iter()
                    .position(|id| *id == panes.layout.active())
                    .unwrap() as u64,
                case["active"].as_u64().unwrap()
            );
            assert_eq!(panes.path(panes.layout.active()), Some(&a));
        }
    }
    #[test]
    fn vim_buffer_delete_staging_fails_without_mutating_on_missing_replacement() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        let path = PathBuf::from("/synthetic/dirty");
        let panes = Panes::new(
            path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let before = panes.layout.order();
        let mut registry = BufferRegistry::default();
        registry.register(Buffer::File(path.clone()));
        assert!(stage_vim_buffer_delete(&panes, &registry, &path, None).is_err());
        assert_eq!(panes.layout.order(), before);
        assert_eq!(panes.path(panes.layout.active()), Some(&path));
        assert!(registry.listed().contains(&Buffer::File(path)));
    }
    #[test]
    fn targeted_vim_delete_preserves_the_surviving_active_window() {
        use crate::buffer_registry::{
            Buffer, BufferRegistry, VimDeletePermission, vim_delete_permission,
        };
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-bdelete-target-reference.json"
        ))
        .unwrap();
        let a = PathBuf::from("/synthetic/a");
        let b = PathBuf::from("/synthetic/b");
        let mut panes = Panes::new(
            a.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let original = panes.layout.active();
        panes.layout.split(Axis::Row, false).unwrap();
        panes.set_content(Content::File(b.clone())).unwrap();
        panes.layout.focus(original).unwrap();
        let mut registry = BufferRegistry::default();
        let a_buffer = Buffer::File(a.clone());
        let b_buffer = Buffer::File(b.clone());
        registry.register(a_buffer.clone());
        registry.register(b_buffer.clone());
        registry.replace(original, Some(&b_buffer), a_buffer);
        let (next, next_registry, _) = stage_vim_buffer_delete(&panes, &registry, &b, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            next.layout.order().len() as u64,
            reference["visible"]["after_count"].as_u64().unwrap()
        );
        assert_eq!(
            next.layout.active() == original,
            reference["visible"]["same_window"].as_bool().unwrap()
        );
        assert_eq!(next.path(next.layout.active()), Some(&a));
        assert!(next_registry.is_unlisted(&b_buffer));
        assert_eq!(
            vim_delete_permission(true, false, false),
            VimDeletePermission::Modified
        );
        assert!(reference["rejected"]["e89"].as_bool().unwrap());
        assert_eq!(
            registry.alternate(original, &Buffer::File(a)),
            Some(b_buffer)
        );
    }
    #[test]
    fn terminal_delete_stages_layout_and_wipes_identity_without_touching_a_pty() {
        use crate::buffer_registry::{
            Buffer, BufferRegistry, VimDeletePermission, vim_delete_permission,
        };
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-bdelete-terminal-reference.json"
        ))
        .unwrap();
        let terminal = Buffer::Terminal(42);
        let file = Buffer::File("/synthetic/a".into());
        let mut panes = Panes::terminal(42);
        panes.layout.split(Axis::Row, false).unwrap();
        assert_eq!(
            panes.layout.order().len() as u64,
            reference["before_count"].as_u64().unwrap()
        );
        let mut registry = BufferRegistry::default();
        registry.register(file.clone());
        registry.register(terminal.clone());
        assert_eq!(
            vim_delete_permission(true, false, false),
            VimDeletePermission::Modified
        );
        assert!(reference["plain"]["e89"].as_bool().unwrap());
        let (next, next_registry, _) =
            stage_vim_buffer_delete_target(&panes, &registry, &terminal, Some(file.clone()))
                .unwrap()
                .unwrap();
        assert_eq!(
            next.layout.order().len() as u64,
            reference["windows_after"].as_u64().unwrap()
        );
        assert_eq!(
            next.content(next.layout.active()),
            Some(&Content::File("/synthetic/a".into()))
        );
        assert_eq!(
            u64::from(next_registry.knows(&terminal)),
            reference["exists"].as_u64().unwrap()
        );
        assert!(registry.knows(&terminal));
        assert_eq!(panes.layout.order().len(), 2);
        // The reference terminated /bin/cat. Staging is deliberately pure;
        // persistent tmux session handling is not implemented by this test.
        assert!(!reference["alive_after"].as_bool().unwrap());
    }
    #[test]
    fn multi_workspace_delete_preflight_is_atomic_and_scoped() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        let path = PathBuf::from("/synthetic/shared");
        let first = Panes::new(
            path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let second = Panes::new(
            path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let old = Buffer::File(path.clone());
        let first_replacement = Buffer::File("/synthetic/first".into());
        let second_replacement = Buffer::Terminal(42);
        let mut first_registry = BufferRegistry::default();
        first_registry.register(old.clone());
        first_registry.register(first_replacement.clone());
        let mut second_registry = BufferRegistry::default();
        second_registry.register(old.clone());
        second_registry.register(second_replacement.clone());
        let mut editor = crate::editor::Editor::new("original".into());
        editor.key("i");
        editor.insert("unsaved-");
        let text_before = editor.contents();
        let sources = [
            VimDeleteWorkspace {
                project: 1,
                panes: &first,
                registry: first_registry.clone(),
                replacement: Some(first_replacement.clone()),
            },
            VimDeleteWorkspace {
                project: 2,
                panes: &second,
                registry: second_registry.clone(),
                replacement: None,
            },
        ];
        assert!(stage_vim_delete_workspaces(&sources, &path).is_err());
        assert_eq!(first.path(first.layout.active()), Some(&path));
        assert_eq!(second.path(second.layout.active()), Some(&path));
        assert!(first_registry.listed().contains(&old));
        assert!(second_registry.listed().contains(&old));
        assert!(editor.dirty());
        assert_eq!(editor.contents(), text_before);

        let sources = [
            VimDeleteWorkspace {
                project: 1,
                panes: &first,
                registry: first_registry.clone(),
                replacement: Some(first_replacement.clone()),
            },
            VimDeleteWorkspace {
                project: 2,
                panes: &second,
                registry: second_registry.clone(),
                replacement: Some(second_replacement.clone()),
            },
        ];
        let staged = stage_vim_delete_workspaces(&sources, &path).unwrap();
        assert_eq!(staged.len(), 2);
        assert_eq!(
            staged[0].panes.content(staged[0].panes.layout.active()),
            Some(&Content::File("/synthetic/first".into()))
        );
        assert_eq!(
            staged[1].panes.content(staged[1].panes.layout.active()),
            Some(&Content::Terminal(42))
        );
        for workspace in &staged {
            assert!(workspace.registry.is_unlisted(&old));
        }
        assert_eq!(first.path(first.layout.active()), Some(&path));
        assert_eq!(second.path(second.layout.active()), Some(&path));
        assert_eq!(editor.contents(), text_before);

        let duplicate = [
            VimDeleteWorkspace {
                project: 1,
                panes: &first,
                registry: first_registry,
                replacement: Some(first_replacement),
            },
            VimDeleteWorkspace {
                project: 1,
                panes: &second,
                registry: second_registry,
                replacement: Some(second_replacement),
            },
        ];
        assert!(stage_vim_delete_workspaces(&duplicate, &path).is_err());
    }
    #[test]
    fn forced_close_releases_only_an_unshared_buffer() {
        assert!(force_close_releases_buffer(true, 0, false, false));
        assert!(!force_close_releases_buffer(false, 0, false, false));
        assert!(!force_close_releases_buffer(true, 1, false, false));
        assert!(!force_close_releases_buffer(true, 0, true, false));
        assert!(!force_close_releases_buffer(true, 0, false, true));
    }

    #[test]
    fn saved_close_requires_same_view_and_respects_other_work() {
        assert_eq!(
            saved_close_action(true, false, true, true),
            SavedCloseAction::ClosePane
        );
        assert_eq!(
            saved_close_action(true, false, false, false),
            SavedCloseAction::Quit
        );
        assert_eq!(
            saved_close_action(false, false, true, false),
            SavedCloseAction::KeepChanged
        );
        assert_eq!(
            saved_close_action(true, true, true, false),
            SavedCloseAction::KeepPending
        );
        assert_eq!(
            saved_close_action(true, false, false, true),
            SavedCloseAction::KeepOtherBuffers
        );
    }
    #[test]
    fn named_save_close_guard_tracks_only_the_original_buffer() {
        let old = PathBuf::from("/synthetic/unnamed");
        let new = PathBuf::from("/synthetic/named.txt");
        let guard = OpenGuard {
            generation: 1,
            project: 2,
            pane: Some(PaneId(3)),
            path: old.clone(),
            version: 4,
        };
        assert!(
            guard
                .clone()
                .after_naming(Path::new("/synthetic/other"), &new)
                .is_none()
        );
        let guard = guard.after_naming(&old, &new).unwrap();
        assert!(guard.valid(2, Some(PaneId(3)), Some(&new), Some(4), true));
        assert!(!guard.valid(2, Some(PaneId(4)), Some(&new), Some(4), true));
        assert!(!guard.valid(2, Some(PaneId(3)), Some(&new), Some(5), true));
    }
    #[test]
    fn format_rebases_only_a_matching_close_guard() {
        let guard = OpenGuard {
            generation: 1,
            project: 2,
            pane: Some(PaneId(3)),
            path: "/synthetic/a.xml".into(),
            version: 4,
        };
        assert!(guard.clone().after_format(5, 6).is_none());
        let guard = guard.after_format(4, 5).unwrap();
        assert!(guard.valid(
            2,
            Some(PaneId(3)),
            Some(Path::new("/synthetic/a.xml")),
            Some(5),
            true
        ));
        assert!(!guard.valid(
            2,
            Some(PaneId(3)),
            Some(Path::new("/synthetic/a.xml")),
            Some(6),
            true
        ));
    }
    #[test]
    fn opened_terminal_file_guard_rejects_changed_history_context_and_focus() {
        use workspace_core::editor_jumplist::{Location, View};
        let path = PathBuf::from("/synthetic/unnamed");
        let mut panes = Panes::new(
            path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let pane = panes.layout.active();
        let guard = OpenedTerminalFileGuard::capture(2, 7, &path, &panes).unwrap();
        assert!(guard.is_current(2, 7, &path, &panes));
        // Ordinary insert input may arrive before the queued tmux mark.
        let mut editor = Editor::new(String::new());
        editor.key("i");
        editor.insert("texto rápido");
        let mut tracking = crate::editor::LiveJumpTracking::default();
        let origin = crate::editor::JumpOrigin {
            layout_index: 0,
            focus: panes.layout.focus_guard(),
        };
        tracking.sync(1, &mut editor, Some(&origin), &mut [&mut panes.layout]);
        assert!(guard.is_current(2, 7, &path, &panes));
        assert_eq!(editor.contents(), "texto rápido");
        assert!(!guard.is_current(3, 7, &path, &panes));
        assert!(!guard.is_current(2, 8, &path, &panes));
        assert!(!guard.is_current(2, 7, Path::new("/synthetic/other"), &panes));
        let mut changed = panes.clone();
        changed
            .layout
            .jump_list_mut(pane)
            .unwrap()
            .record(Location {
                buffer: 1,
                row: 0,
                column: 0,
                view: View::default(),
            });
        assert!(!guard.is_current(2, 7, &path, &changed));
        panes.layout.split(Axis::Row, true).unwrap();
        panes.layout.focus(pane).unwrap();
        assert!(!guard.is_current(2, 7, &path, &panes));
    }
    #[test]
    fn terminal_open_guard_rejects_focus_project_and_attachment_changes() {
        let guard = TerminalOpenGuard {
            generation: 7,
            project: 2,
            pane: Some(PaneId(1)),
            attachment: Some(9),
        };
        assert!(guard.valid(2, Some(PaneId(1)), Some(9), true));
        assert!(!guard.valid(3, Some(PaneId(1)), Some(9), true));
        assert!(!guard.valid(2, Some(PaneId(2)), Some(9), true));
        assert!(!guard.valid(2, Some(PaneId(1)), Some(10), true));
        assert!(!guard.valid(2, Some(PaneId(1)), Some(9), false));
    }
    #[test]
    fn tmux_target_change_invalidates_pending_terminal_open_generation() {
        use crate::tmux::Action;
        let mut generation = 7;
        let mut pending = Some(TerminalOpenGuard {
            generation,
            project: 2,
            pane: Some(PaneId(1)),
            attachment: Some(9),
        });
        assert!(!cancel_terminal_open_for_target_change(
            &mut pending,
            &mut generation,
            Action::Resize('L', 2).changes_input_target()
        ));
        assert_eq!(generation, 7);
        let stale = pending.as_ref().unwrap().generation;
        assert!(cancel_terminal_open_for_target_change(
            &mut pending,
            &mut generation,
            Action::Direction('L').changes_input_target()
        ));
        assert!(pending.is_none());
        assert_ne!(stale, generation);
    }
    #[test]
    fn configure_wrap_is_idempotent_window_local_and_resets_stale_view() {
        let mut panes = Panes::new(
            "a.txt".into(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let first = panes.layout.active();
        assert_eq!(
            panes.configure_wrap(first, Some(false), Some(Path::new("a.txt")), 3),
            Some((false, false, 3))
        );
        assert_eq!(
            panes.configure_wrap(first, Some(true), Some(Path::new("a.txt")), 3),
            Some((true, true, 3))
        );
        let state = panes.wrap_state_mut(first).unwrap();
        state.top = WrappedTop {
            source_line: 9,
            visual_row: 2,
        };
        state.cursor = Some(WrapCursor {
            line: 9,
            scalar: 4,
            visual_row: 2,
        });
        assert_eq!(
            panes.configure_wrap(first, Some(true), Some(Path::new("a.txt")), 3),
            Some((true, false, 9))
        );
        assert!(panes.wrap_state(first).unwrap().cursor.is_some());
        let second = panes.layout.split(Axis::Row, true).unwrap();
        panes.topology_changed();
        panes.duplicate_wrap(first, second);
        assert_eq!(
            panes.configure_wrap(second, None, Some(Path::new("a.txt")), 3),
            Some((false, true, 9))
        );
        assert!(panes.wrap_state(first).unwrap().enabled);
        assert_eq!(
            panes.configure_wrap(second, None, Some(Path::new("a.txt")), 5),
            Some((true, true, 9))
        );
        let second_state = panes.wrap_state(second).unwrap();
        assert_eq!(
            second_state.top,
            WrappedTop {
                source_line: 5,
                visual_row: 0
            }
        );
        assert!(second_state.cursor.is_none());
        assert_eq!(
            panes.configure_wrap(second, Some(false), Some(Path::new("b.txt")), 7),
            Some((false, true, 7))
        );
        assert_eq!(
            panes.wrap_state(second).unwrap().path.as_deref(),
            Some(Path::new("b.txt"))
        );
    }

    #[test]
    fn half_page_option_belongs_to_window_and_resets_when_height_changes() {
        use crate::editor::Editor;
        let mut panes = Panes::new(
            "a.txt".into(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let first = panes.layout.active();
        let mut editor = Editor::new((0..100).map(|n| format!("{n}\n")).collect());
        let mut apply = |panes: &mut Panes, pane, rows, key: &str| {
            let state = panes.wrap_state_mut(pane).unwrap();
            editor.set_window_scroll(state.scroll.prepare(rows));
            editor.jump_viewport(0, 0, rows, 80);
            editor.key(key);
            state.scroll.save(editor.window_scroll());
        };
        apply(&mut panes, first, 10, "3");
        apply(&mut panes, first, 10, "<C-d>");
        assert_eq!(panes.wrap_state(first).unwrap().scroll.value, Some(3));
        let second = panes.layout.split(Axis::Row, true).unwrap();
        panes.duplicate_wrap(first, second);
        assert_eq!(panes.wrap_state(second).unwrap().scroll.value, None);
        apply(&mut panes, second, 10, "6");
        apply(&mut panes, second, 10, "<C-d>");
        assert_eq!(panes.wrap_state(first).unwrap().scroll.value, Some(3));
        assert_eq!(panes.wrap_state(second).unwrap().scroll.value, Some(6));
        apply(&mut panes, first, 10, "<C-d>");
        assert_eq!(panes.wrap_state(first).unwrap().scroll.value, Some(3));
        let second_state = panes.wrap_state_mut(second).unwrap();
        second_state.activate(Path::new("b.txt"), 0);
        assert_eq!(second_state.scroll.prepare(0), Some(6));
        assert_eq!(second_state.scroll.prepare(10), Some(6));
        assert_eq!(panes.wrap_state_mut(first).unwrap().scroll.prepare(8), None);
        assert_eq!(
            panes.wrap_state_mut(second).unwrap().scroll.prepare(8),
            None
        );
        assert_eq!(panes.wrap_state_mut(first).unwrap().scroll.prepare(8), None);
    }

    #[test]
    fn wrap_state_is_window_local_cloned_on_split_and_forgotten_on_close() {
        let mut panes = Panes::new(
            "a.txt".into(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let first = panes.layout.active();
        let state = panes.wrap_state_mut(first).unwrap();
        state.enabled = true;
        state.activate(Path::new("a.txt"), 3);
        state.cursor = Some(WrapCursor {
            line: 3,
            scalar: 2,
            visual_row: 1,
        });
        state.desired_column = Some(7);
        state.wheel_goal = Some(88);
        state.wheel_coladd = Some(4);
        state.mode = Some(Mode::Normal);
        state.height = Some(20);
        let second = panes.layout.split(Axis::Row, true).unwrap();
        panes.topology_changed();
        panes.duplicate_wrap(first, second);
        assert_eq!(panes.wrap_state(first), panes.wrap_state(second));
        panes.wrap_state_mut(first).unwrap().wheel_goal = Some(91);
        assert_eq!(panes.wrap_state(second).unwrap().wheel_goal, Some(88));
        let state = panes.wrap_state_mut(second).unwrap();
        state.enabled = false;
        state.activate(Path::new("b.txt"), 9);
        assert!(!state.enabled);
        assert_eq!(
            state.top,
            WrappedTop {
                source_line: 9,
                visual_row: 0
            }
        );
        assert_eq!(state.cursor, None);
        assert_eq!(state.desired_column, None);
        assert_eq!(state.wheel_goal, None);
        assert_eq!(state.wheel_coladd, None);
        assert_eq!(state.mode, None);
        assert_eq!(state.height, None);
        assert!(panes.wrap_state(first).unwrap().enabled);
        panes.layout.close().unwrap();
        panes.topology_changed();
        assert!(panes.wrap_state(second).is_none());
        assert!(panes.wrap_state(first).is_some());
        assert!(panes.wrap_state_mut(second).is_none());
    }

    #[test]
    fn revealing_terminal_by_split_preserves_window_wrap_option() {
        let mut panes = Panes::new(
            "a.txt".into(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let file = panes.layout.active();
        panes.wrap_state_mut(file).unwrap().enabled = true;
        let terminal = panes.reveal_terminal(42).unwrap();
        assert_ne!(file, terminal);
        assert!(panes.wrap_state(file).unwrap().enabled);
        assert!(panes.wrap_state(terminal).unwrap().enabled);
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
    }
    #[test]
    fn deleting_a_file_replaces_its_views_without_replacing_other_content() {
        let file = PathBuf::from("fixture.rs");
        let mut panes = Panes::new(
            file.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Row, true).unwrap();
        let sidebar = panes.layout.split(Axis::Column, true).unwrap();
        panes.set_content(Content::Explorer(17)).unwrap();
        panes.replace_file_with_terminal(&file, 42);
        assert_eq!(panes.content(first), Some(&Content::Terminal(42)));
        assert_eq!(panes.content(second), Some(&Content::Terminal(42)));
        assert_eq!(panes.content(sidebar), Some(&Content::Explorer(17)));
    }
    #[test]
    fn delete_plan_can_restore_a_distinct_alternate_in_each_view() {
        let file = PathBuf::from("fixture.rs");
        let mut panes = Panes::new(
            file.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let left = panes.layout.active();
        let right = panes.layout.split(Axis::Row, true).unwrap();
        let partial = BTreeMap::from([(left, Content::Terminal(7))]);
        assert!(!panes.replace_file_views(&file, &partial));
        assert_eq!(panes.path(left), Some(&file));
        assert_eq!(panes.path(right), Some(&file));
        let plan = BTreeMap::from([
            (left, Content::Terminal(7)),
            (right, Content::File("other.rs".into())),
        ]);
        assert!(panes.replace_file_views(&file, &plan));
        assert_eq!(panes.content(left), Some(&Content::Terminal(7)));
        assert_eq!(
            panes.content(right),
            Some(&Content::File("other.rs".into()))
        );
    }
    #[test]
    fn shared_file_plans_can_use_a_different_terminal_in_each_workspace() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        let old = PathBuf::from("shared.rs");
        let view = ViewState {
            buffer: 0,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        };
        let mut a = Panes::new(old.clone(), view.clone());
        let mut b = Panes::new(old.clone(), view);
        let mut ra = BufferRegistry::default();
        let mut rb = BufferRegistry::default();
        let file = Buffer::File(old.clone());
        let ta = Buffer::Terminal(7);
        let tb = Buffer::Terminal(8);
        ra.register(ta.clone());
        rb.register(tb.clone());
        ra.replace(a.layout.active(), Some(&ta), file.clone());
        rb.replace(b.layout.active(), Some(&tb), file.clone());
        let pa = ra
            .plan_replacements(&a.layout.order(), &file, |_| true)
            .unwrap();
        let pb = rb
            .plan_replacements(&b.layout.order(), &file, |_| true)
            .unwrap();
        let map = |plan: BTreeMap<PaneId, Buffer>| {
            plan.into_iter()
                .map(|(id, buffer)| {
                    let Buffer::Terminal(id2) = buffer else {
                        panic!("expected terminal")
                    };
                    (id, Content::Terminal(id2))
                })
                .collect()
        };
        assert!(a.replace_file_views(&old, &map(pa)));
        assert!(b.replace_file_views(&old, &map(pb)));
        assert_eq!(a.content(a.layout.active()), Some(&Content::Terminal(7)));
        assert_eq!(b.content(b.layout.active()), Some(&Content::Terminal(8)));
    }
    #[test]
    fn window_prefix_counts_combine_and_are_local_to_each_command() {
        let mut prefix = WindowPrefix::new(2);
        assert!(prefix.digit("3"));
        assert_eq!(prefix.count(), 6);
        let mut next = WindowPrefix::new(0);
        assert!(!next.digit("0"));
        assert!(next.digit("2"));
        assert!(next.digit("0"));
        assert_eq!(next.count(), 20);
        assert_eq!(WindowPrefix::new(0).count(), 0);
    }
    #[test]
    fn resize_uses_current_terminal_measurement_not_previous_file_outer_bounds() {
        let inner = 460;
        let stale_file_outer = 911;
        assert_eq!(
            *resize_rect(
                Some(&Content::Terminal(42)),
                &inner,
                Some(&stale_file_outer)
            ),
            460
        );
        assert_eq!(
            *resize_rect(Some(&Content::Explorer(1)), &inner, Some(&stale_file_outer)),
            460
        );
        assert_eq!(
            *resize_rect(
                Some(&Content::File(PathBuf::from("fixture"))),
                &inner,
                Some(&stale_file_outer)
            ),
            911
        );
        assert_eq!(
            *resize_rect(Some(&Content::File(PathBuf::from("fixture"))), &inner, None),
            460
        );
    }
    #[test]
    fn navigation_tolerates_shared_border_pixels_but_not_real_overlap() {
        use workspace_core::editor_panes::{Bounds, Direction, Error, Point};
        let mut panes = Panes::terminal(42);
        let terminal = panes.layout.active();
        let explorer = panes.layout.split(Axis::Row, true).unwrap();
        let agents = panes.layout.split(Axis::Column, true).unwrap();
        panes.layout.focus(terminal).unwrap();
        let measured = [
            Bounds {
                pane: terminal,
                x: 6,
                y: 68,
                width: 1622,
                height: 911,
            },
            Bounds {
                pane: explorer,
                x: 1634,
                y: 64,
                width: 270,
                height: 460,
            },
            Bounds {
                pane: agents,
                x: 1634,
                y: 523,
                width: 270,
                height: 459,
            },
        ];
        let cursor = Point { x: 10, y: 100 };
        assert_eq!(
            panes
                .layout
                .focus_direction(Direction::Right, &measured, cursor),
            Err(Error::InvalidGeometry)
        );
        let interiors = measured.map(navigation_interior);
        assert_eq!(
            panes
                .layout
                .focus_direction(Direction::Right, &interiors, cursor),
            Ok(true)
        );
        assert_eq!(panes.layout.active(), explorer);
        panes.layout.focus(terminal).unwrap();
        let mut bad = measured;
        bad[2].y -= 10;
        assert_eq!(
            panes
                .layout
                .focus_direction(Direction::Right, &bad.map(navigation_interior), cursor),
            Err(Error::InvalidGeometry)
        );
    }
    #[test]
    fn reveal_terminal_from_sidebar_keeps_sidebar_visible() {
        for sidebar in [Content::Explorer(1), Content::Agents(1)] {
            let mut panes = Panes::terminal(42);
            panes.set_content(sidebar.clone()).unwrap();
            let original = panes.layout.active();
            let terminal = panes.reveal_terminal(42).unwrap();
            assert_ne!(terminal, original);
            assert_eq!(panes.content(original), Some(&sidebar));
            assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
        }
    }
    #[test]
    fn reveal_terminal_keeps_file_visible_and_reuses_existing_view() {
        let mut panes = Panes::new(
            "/synthetic/a.rs".into(),
            ViewState {
                buffer: 0,
                row: 3,
                column: 2,
                top: 1,
                left: 0,
            },
        );
        let file = panes.layout.active();
        let terminal = panes.reveal_terminal(42).unwrap();
        assert_ne!(terminal, file);
        assert_eq!(panes.path(file), Some(&PathBuf::from("/synthetic/a.rs")));
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
        panes.layout.focus(file).unwrap();
        assert_eq!(panes.reveal_terminal(42).unwrap(), terminal);
        assert_eq!(panes.layout.panes().len(), 2);
        assert_eq!(panes.layout.panes()[&file].row, 3);
    }
    #[test]
    fn revealing_terminal_updates_buffer_history_for_the_changed_window() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        let file = Buffer::File("/synthetic/a.rs".into());
        let terminal = Buffer::Terminal(42);
        let mut registry = BufferRegistry::default();
        registry.register(terminal.clone());
        registry.register(file.clone());
        let mut panes = Panes::new(
            "/synthetic/a.rs".into(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let file_pane = panes.layout.active();
        let terminal_pane = panes
            .reveal_terminal_with_registry(&mut registry, 42)
            .unwrap();
        assert_ne!(terminal_pane, file_pane);
        assert_eq!(
            panes.content(file_pane),
            Some(&Content::File("/synthetic/a.rs".into()))
        );
        assert_eq!(
            registry.alternate(terminal_pane, &terminal),
            Some(file.clone())
        );
        assert_eq!(registry.cycle(&terminal, true), Some(file));
    }
    #[test]
    fn revealing_existing_terminal_keeps_its_window_alternate() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        let file_a = Buffer::File("/synthetic/a.rs".into());
        let file_b = Buffer::File("/synthetic/b.rs".into());
        let terminal = Buffer::Terminal(42);
        let mut registry = BufferRegistry::default();
        registry.register(file_a.clone());
        registry.register(file_b.clone());
        let mut panes = Panes::new(
            "/synthetic/a.rs".into(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let file_pane = panes.layout.active();
        let terminal_pane = panes.layout.split(Axis::Row, true).unwrap();
        panes.set_terminal(42).unwrap();
        registry.replace(terminal_pane, Some(&file_b), terminal.clone());
        panes.layout.focus(file_pane).unwrap();
        assert_eq!(
            panes
                .reveal_terminal_with_registry(&mut registry, 42)
                .unwrap(),
            terminal_pane
        );
        assert_eq!(panes.layout.panes().len(), 2);
        assert_eq!(registry.alternate(terminal_pane, &terminal), Some(file_b));
        assert_eq!(registry.cycle(&terminal, true), Some(file_a));
    }
    #[test]
    fn reveal_terminal_replaces_only_one_duplicate_file_view() {
        let mut panes = Panes::new(
            "/synthetic/a.rs".into(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Row, true).unwrap();
        assert_eq!(panes.reveal_terminal(42).unwrap(), second);
        assert_eq!(panes.path(first), Some(&PathBuf::from("/synthetic/a.rs")));
        assert_eq!(panes.content(second), Some(&Content::Terminal(42)));
        assert_eq!(panes.layout.panes().len(), 2);
    }
    #[test]
    fn replacing_a_deleted_buffer_preserves_split_views() {
        let mut panes = Panes::new(
            PathBuf::from("/a"),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Column, true).unwrap();
        panes.replace_file(Path::new("/a"), Path::new("/b"));
        assert_eq!(panes.path(first), Some(&PathBuf::from("/b")));
        assert_eq!(panes.path(second), Some(&PathBuf::from("/b")));
    }

    #[test]
    fn naming_a_buffer_rebinds_all_views_and_preserves_wrap_state() {
        let old = PathBuf::from("/synthetic/unnamed");
        let new = PathBuf::from("/synthetic/named.txt");
        let mut panes = Panes::new(
            old.clone(),
            ViewState {
                buffer: 0,
                row: 4,
                column: 3,
                top: 2,
                left: 0,
            },
        );
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Row, true).unwrap();
        let wrap = panes.wrap_state_mut(first).unwrap();
        wrap.enabled = true;
        wrap.path = Some(old.clone());
        wrap.top = WrappedTop {
            source_line: 2,
            visual_row: 1,
        };
        panes.rebind_file(&old, &new);
        assert_eq!(panes.path(first), Some(&new));
        assert_eq!(panes.path(second), Some(&new));
        let wrap = panes.wrap_state(first).unwrap();
        assert!(wrap.enabled);
        assert_eq!(wrap.path.as_ref(), Some(&new));
        assert_eq!(
            wrap.top,
            WrappedTop {
                source_line: 2,
                visual_row: 1
            }
        );
    }
    #[test]
    fn initial_workspace_places_both_side_views_and_preserves_terminal_focus() {
        for (right, expected) in [(true, "row(1,col(2,3))"), (false, "row(col(2,3),1)")] {
            let panes = Panes::with_sidebar(42, 7, Some(9), right).unwrap();
            assert_eq!(panes.layout.root().to_string(), expected);
            assert_eq!(
                panes.content(panes.layout.active()),
                Some(&Content::Terminal(42))
            );
            let contents: Vec<_> = panes
                .layout
                .order()
                .iter()
                .map(|id| panes.content(*id).unwrap().clone())
                .collect();
            assert!(contents.contains(&Content::Explorer(7)));
            assert!(contents.contains(&Content::Agents(9)));
        }
        let panes = Panes::with_sidebar(42, 7, None, true).unwrap();
        assert_eq!(panes.layout.root().to_string(), "row(1,2)");
    }
    #[test]
    fn sidebar_toggle_preserves_editor_and_terminal_views() {
        let mut panes = Panes::terminal(42);
        let terminal = panes.layout.active();
        let file = panes.layout.split(Axis::Column, true).unwrap();
        panes
            .set_content(Content::File("/synthetic/a.rs".into()))
            .unwrap();
        let before = panes.layout.root().to_string();
        panes.insert_sidebar(7, Some(9), true).unwrap();
        assert_eq!(panes.layout.active(), file);
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
        assert_eq!(
            panes.content(file),
            Some(&Content::File("/synthetic/a.rs".into()))
        );
        assert_eq!(panes.remove_sidebar().unwrap().len(), 2);
        assert_eq!(panes.layout.root().to_string(), before);
        assert_eq!(panes.layout.active(), file);
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
    }
    #[test]
    fn agents_visibility_does_not_replace_explorer_or_content() {
        let mut panes = Panes::with_sidebar(42, 7, Some(9), true).unwrap();
        let terminal = panes.layout.active();
        assert_eq!(panes.set_agents_visible(9, false).unwrap().len(), 1);
        assert_eq!(panes.layout.root().to_string(), "row(1,2)");
        assert_eq!(panes.layout.active(), terminal);
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
        assert!(panes.set_agents_visible(9, true).unwrap().is_empty());
        assert_eq!(panes.layout.root().to_string(), "row(1,col(2,4))");
        assert_eq!(panes.layout.active(), terminal);
    }
    #[test]
    fn resize_counts_use_cells_and_lines_in_each_direction() {
        assert_eq!(resize_delta("+", 3, 10, 20), Some((Axis::Column, 60)));
        assert_eq!(resize_delta("-", 3, 10, 20), Some((Axis::Column, -60)));
        assert_eq!(resize_delta(">", 3, 10, 20), Some((Axis::Row, 30)));
        assert_eq!(resize_delta("<", 0, 10, 20), Some((Axis::Row, -10)));
        assert_eq!(resize_delta("x", 3, 10, 20), None);
        assert_eq!(absolute_resize_delta(8, 10, 400, 360), -280);
        assert!(absolute_resize_delta(0, 10, 400, 360) > 1000);
    }
    #[test]
    fn pending_file_splits_replay_only_for_the_original_open_context() {
        let pane = Panes::terminal(42).layout.active();
        let mut pending = PendingFileSplits::default();
        pending.begin(1, 3, pane, Content::Explorer(3));
        assert_eq!(pending.queue(1, 3, pane, "s", 0), Some(true));
        assert_eq!(pending.queue(1, 3, pane, "v", 2), Some(true));
        assert_eq!(pending.queue(2, 3, pane, "s", 0), None);
        assert!(pending.finish(2, 3, Some(pane), true).is_empty());
        assert_eq!(
            pending.finish(1, 3, Some(pane), true),
            vec![("s".into(), 0), ("v".into(), 2)]
        );
        pending.begin(3, 3, pane, Content::Explorer(3));
        assert_eq!(pending.queue(3, 3, pane, "s", 0), Some(true));
        assert_eq!(
            pending.is_current(3, 3, Some(pane), Some(&Content::Explorer(3))),
            Some(true)
        );
        assert_eq!(pending.is_current(3, 3, Some(pane), None), Some(false));
        assert_eq!(
            pending.is_current(3, 3, Some(pane), Some(&Content::Agents(3))),
            Some(false)
        );
        assert_eq!(
            pending.is_current(3, 4, Some(pane), Some(&Content::Explorer(3))),
            Some(false)
        );
        assert_eq!(
            pending.is_current(2, 3, Some(pane), Some(&Content::Explorer(3))),
            None
        );
        assert!(pending.finish(3, 4, Some(pane), true).is_empty());
        pending.begin(4, 3, pane, Content::Explorer(3));
        assert_eq!(pending.queue(4, 3, pane, "v", 0), Some(true));
        assert!(pending.finish(4, 3, Some(pane), false).is_empty());
    }
    #[test]
    fn asynchronous_open_guard_rejects_edited_or_replaced_context() {
        let pane = Panes::terminal(42).layout.active();
        let guard = OpenGuard {
            generation: 7,
            project: 1,
            pane: Some(pane),
            path: "old.txt".into(),
            version: 4,
        };
        assert!(guard.valid(1, Some(pane), Some(Path::new("old.txt")), Some(4), true));
        assert!(!guard.valid(1, Some(pane), Some(Path::new("old.txt")), Some(5), true));
        assert!(!guard.valid(1, Some(pane), Some(Path::new("old.txt")), Some(4), false));
        assert!(!guard.valid(2, Some(pane), Some(Path::new("old.txt")), Some(4), true));
        assert!(!guard.valid(1, None, Some(Path::new("old.txt")), Some(4), true));
        assert!(!guard.valid(1, Some(pane), Some(Path::new("other.txt")), Some(4), true));
    }
    #[test]
    fn navigation_invalidates_delayed_open_even_when_returning_to_original_buffer() {
        let pane = PaneId(1);
        let original = Content::File("a.txt".into());
        let other = Content::File("b.txt".into());
        let guard = OpenGuard {
            generation: 7,
            project: 1,
            pane: Some(pane),
            path: "a.txt".into(),
            version: 4,
        };
        // The old completion-only check accepts a return to exactly A.
        assert!(guard.valid(1, Some(pane), Some(Path::new("a.txt")), Some(4), true));
        let mut generation = 7;
        let mut read = Some(guard);
        let mut edit = None;
        let mut terminal = None;
        let mut sidebar = PendingFileSplits::default();
        {
            let mut pending = PendingOpens {
                generation: &mut generation,
                read: &mut read,
                edit: &mut edit,
                terminal: &mut terminal,
                sidebar: &mut sidebar,
            };
            assert!(!pending.activate(1, pane, &original));
            assert!(pending.activate(1, pane, &other));
            assert!(!pending.activate(1, pane, &original));
        }
        assert_eq!(generation, 8);
        assert!(read.is_none());
    }
    #[test]
    fn pending_open_guards_track_panes_terminals_and_sidebar_queues() {
        let mut generation = 9;
        let mut read = None;
        let mut edit = Some(OpenGuard {
            generation: 9,
            project: 2,
            pane: Some(PaneId(1)),
            path: "a".into(),
            version: 1,
        });
        let mut terminal = None;
        let mut sidebar = PendingFileSplits::default();
        {
            let mut pending = PendingOpens {
                generation: &mut generation,
                read: &mut read,
                edit: &mut edit,
                terminal: &mut terminal,
                sidebar: &mut sidebar,
            };
            assert!(pending.activate(2, PaneId(2), &Content::File("a".into())));
        }
        assert_eq!(generation, 10);
        assert!(edit.is_none());
        terminal = Some(TerminalOpenGuard {
            generation,
            project: 2,
            pane: Some(PaneId(1)),
            attachment: Some(20),
        });
        {
            let mut pending = PendingOpens {
                generation: &mut generation,
                read: &mut read,
                edit: &mut edit,
                terminal: &mut terminal,
                sidebar: &mut sidebar,
            };
            assert!(!pending.activate(2, PaneId(1), &Content::Terminal(20)));
            assert!(pending.activate(2, PaneId(1), &Content::Terminal(21)));
        }
        assert_eq!(generation, 11);
        assert!(terminal.is_none());
        sidebar.begin(generation, 2, PaneId(3), Content::Explorer(2));
        assert_eq!(sidebar.queue(generation, 2, PaneId(3), "v", 1), Some(true));
        {
            let mut pending = PendingOpens {
                generation: &mut generation,
                read: &mut read,
                edit: &mut edit,
                terminal: &mut terminal,
                sidebar: &mut sidebar,
            };
            assert!(!pending.activate(2, PaneId(3), &Content::Explorer(2)));
            assert!(pending.activate(2, PaneId(4), &Content::Explorer(2)));
        }
        assert_eq!(generation, 12);
        assert!(sidebar.finish(11, 2, Some(PaneId(3)), true).is_empty());
        sidebar.begin(generation, 2, PaneId(3), Content::Explorer(2));
        {
            let mut pending = PendingOpens {
                generation: &mut generation,
                read: &mut read,
                edit: &mut edit,
                terminal: &mut terminal,
                sidebar: &mut sidebar,
            };
            assert!(pending.activate(2, PaneId(3), &Content::Agents(2)));
        }
        assert_eq!(generation, 13);
        // An old guard from an earlier generation must not cancel a new one.
        read = Some(OpenGuard {
            generation: 3,
            project: 2,
            pane: Some(PaneId(1)),
            path: "old".into(),
            version: 1,
        });
        let mut pending = PendingOpens {
            generation: &mut generation,
            read: &mut read,
            edit: &mut edit,
            terminal: &mut terminal,
            sidebar: &mut sidebar,
        };
        assert!(!pending.activate(2, PaneId(8), &Content::File("new".into())));
        assert_eq!(generation, 13);
    }
    #[test]
    fn resize_keeps_a_text_line_and_respects_nested_sibling_minima() {
        use workspace_core::editor_panes::Bounds;
        let mut panes = Panes::new(
            PathBuf::from("test.rs"),
            ViewState {
                buffer: 1,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Column, true).unwrap();
        let third = panes.layout.split(Axis::Column, true).unwrap();
        let bounds = [
            Bounds {
                pane: first,
                x: 0,
                y: 0,
                width: 400,
                height: 300,
            },
            Bounds {
                pane: second,
                x: 0,
                y: 300,
                width: 400,
                height: 300,
            },
            Bounds {
                pane: third,
                x: 0,
                y: 600,
                width: 400,
                height: 300,
            },
        ];
        let minimum = file_minimum_height(300, 180, 20);
        assert_eq!(minimum, 140);
        let minima = BTreeMap::from([(first, minimum), (second, minimum), (third, minimum)]);
        assert!(panes.resize(Axis::Column, 1000, &bounds, &minima));
        let Node::Split(_, children) = panes.layout.root() else {
            panic!("expected split")
        };
        assert_eq!(panes.weight(Axis::Column, &children[0]), Some(140.));
        assert_eq!(panes.weight(Axis::Column, &children[1]), Some(140.));
        assert_eq!(panes.weight(Axis::Column, &children[2]), Some(620.));
        assert_eq!(panes.layout.active(), third);
    }
    #[test]
    fn resize_uses_multiple_neighbors_in_neovim_order() {
        use workspace_core::editor_panes::Bounds;
        let mut panes = Panes::terminal(42);
        let a = panes.layout.active();
        let b = panes.layout.split(Axis::Row, true).unwrap();
        let c = panes.layout.split(Axis::Row, true).unwrap();
        panes.layout.focus(b).unwrap();
        let bounds = [
            Bounds {
                pane: a,
                x: 0,
                y: 0,
                width: 100,
                height: 300,
            },
            Bounds {
                pane: b,
                x: 100,
                y: 0,
                width: 100,
                height: 300,
            },
            Bounds {
                pane: c,
                x: 200,
                y: 0,
                width: 100,
                height: 300,
            },
        ];
        assert!(panes.resize(Axis::Row, 90, &bounds, &BTreeMap::new()));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(a)), Some(70.));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(b)), Some(190.));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(c)), Some(40.));
        assert!(panes.resize(Axis::Row, -90, &bounds, &BTreeMap::new()));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(a)), Some(100.));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(b)), Some(40.));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(c)), Some(160.));
        assert_eq!(panes.layout.active(), b);
    }
    #[test]
    fn absolute_resize_sets_requested_width_then_maximizes() {
        use workspace_core::editor_panes::Bounds;
        let mut panes = Panes::terminal(42);
        let a = panes.layout.active();
        let b = panes.layout.split(Axis::Row, true).unwrap();
        let c = panes.layout.split(Axis::Row, true).unwrap();
        panes.layout.focus(b).unwrap();
        let bounds = [
            Bounds {
                pane: a,
                x: 0,
                y: 0,
                width: 100,
                height: 300,
            },
            Bounds {
                pane: b,
                x: 100,
                y: 0,
                width: 100,
                height: 300,
            },
            Bounds {
                pane: c,
                x: 200,
                y: 0,
                width: 100,
                height: 300,
            },
        ];
        let delta = absolute_resize_delta(8, 10, 100, 100);
        assert!(panes.resize(Axis::Row, delta, &bounds, &BTreeMap::new()));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(b)), Some(80.));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(c)), Some(120.));
        let after = [
            bounds[0],
            Bounds {
                width: 80,
                ..bounds[1]
            },
            Bounds {
                x: 180,
                width: 120,
                ..bounds[2]
            },
        ];
        let delta = absolute_resize_delta(0, 10, 80, 80);
        assert!(panes.resize(Axis::Row, delta, &after, &BTreeMap::new()));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(a)), Some(40.));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(b)), Some(220.));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(c)), Some(40.));
    }
    #[test]
    fn resize_uses_nearest_split_and_preserves_focus_on_invalid_geometry() {
        use workspace_core::editor_panes::Bounds;
        let mut panes = Panes::terminal(42);
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Row, true).unwrap();
        let bounds = [
            Bounds {
                pane: first,
                x: 0,
                y: 0,
                width: 400,
                height: 300,
            },
            Bounds {
                pane: second,
                x: 400,
                y: 0,
                width: 400,
                height: 300,
            },
        ];
        assert!(panes.resize(Axis::Row, 20, &bounds, &BTreeMap::new()));
        let Node::Split(_, children) = panes.layout.root() else {
            panic!("expected split")
        };
        assert_eq!(panes.weight(Axis::Row, &children[0]), Some(380.));
        assert_eq!(panes.weight(Axis::Row, &children[1]), Some(420.));
        assert!(panes.resize(Axis::Row, 1000, &bounds, &BTreeMap::new()));
        let Node::Split(_, children) = panes.layout.root() else {
            panic!("expected split")
        };
        assert_eq!(panes.weight(Axis::Row, &children[0]), Some(40.));
        assert_eq!(panes.weight(Axis::Row, &children[1]), Some(760.));
        let constrained = [
            Bounds {
                width: 39,
                ..bounds[0]
            },
            Bounds {
                x: 39,
                width: 761,
                ..bounds[1]
            },
        ];
        assert!(panes.resize(Axis::Row, 20, &constrained, &BTreeMap::new()));
        assert!(panes.resize(Axis::Row, -20, &constrained, &BTreeMap::new()));
        let Node::Split(_, children) = panes.layout.root() else {
            panic!("expected split")
        };
        assert_eq!(panes.weight(Axis::Row, &children[0]), Some(59.));
        assert_eq!(panes.weight(Axis::Row, &children[1]), Some(741.));
        assert!(panes.resize(Axis::Column, 20, &bounds, &BTreeMap::new()));
        assert!(!panes.resize(Axis::Row, 20, &bounds[..1], &BTreeMap::new()));
        assert_eq!(panes.layout.active(), second);
        panes.equalize();
        assert!(panes.is_equalized());
        let Node::Split(_, children) = panes.layout.root() else {
            panic!("expected split")
        };
        assert_eq!(panes.weight(Axis::Row, &children[0]), None);
        assert_eq!(panes.weight(Axis::Row, &children[1]), None);
    }
    #[test]
    fn split_discards_stale_resize_weights() {
        use workspace_core::editor_panes::Bounds;
        let mut panes = Panes::with_sidebar(42, 7, None, true).unwrap();
        let terminal = panes.layout.active();
        let tree = panes.layout.order()[1];
        let bounds = [
            Bounds {
                pane: terminal,
                x: 0,
                y: 0,
                width: 700,
                height: 500,
            },
            Bounds {
                pane: tree,
                x: 700,
                y: 0,
                width: 300,
                height: 500,
            },
        ];
        assert!(panes.resize(Axis::Row, 20, &bounds, &BTreeMap::new()));
        panes.layout.split(Axis::Row, true).unwrap();
        panes.topology_changed();
        let Node::Split(_, children) = panes.layout.root() else {
            panic!("expected split")
        };
        assert!(
            children
                .iter()
                .all(|node| panes.weight(Axis::Row, node).is_none())
        );
    }
    #[test]
    fn exchanging_resized_windows_keeps_their_own_sizes() {
        use workspace_core::editor_panes::Bounds;
        let mut panes = Panes::terminal(42);
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Row, true).unwrap();
        let bounds = [
            Bounds {
                pane: first,
                x: 0,
                y: 0,
                width: 400,
                height: 300,
            },
            Bounds {
                pane: second,
                x: 400,
                y: 0,
                width: 400,
                height: 300,
            },
        ];
        assert!(panes.resize(Axis::Row, 100, &bounds, &BTreeMap::new()));
        panes.exchange(0).unwrap();
        assert_eq!(panes.layout.order(), vec![second, first]);
        assert_eq!(panes.layout.active(), first);
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(second)), Some(500.));
        assert_eq!(panes.weight(Axis::Row, &Node::Pane(first)), Some(300.));
    }
    #[test]
    fn only_removes_terminal_view_without_discarding_attachment_identity() {
        let mut panes = Panes::terminal(42);
        panes.layout.split(Axis::Row, true).unwrap();
        panes
            .set_content(Content::File("synthetic.txt".into()))
            .unwrap();
        let file = panes.layout.active();
        assert_eq!(panes.layout.only().len(), 1);
        assert_eq!(panes.path(file), Some(&PathBuf::from("synthetic.txt")));
        let terminal = panes.reveal_terminal(42).unwrap();
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
        assert!(panes.layout.order().contains(&file));
    }
    #[test]
    fn closing_one_shared_terminal_view_keeps_attachment_listed() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        let mut panes = Panes::terminal(42);
        let mut registry = BufferRegistry::default();
        let first = panes.layout.active();
        registry.register(Buffer::Terminal(42));
        let second = panes.layout.split(Axis::Row, true).unwrap();
        registry.duplicate_pane(first, second);
        assert_eq!(panes.content(first), Some(&Content::Terminal(42)));
        assert_eq!(panes.content(second), Some(&Content::Terminal(42)));
        let closed = panes.layout.close().unwrap();
        registry.close_pane(closed);
        assert_eq!(closed, second);
        assert_eq!(panes.layout.active(), first);
        assert_eq!(panes.content(first), Some(&Content::Terminal(42)));
        assert_eq!(registry.listed(), &[Buffer::Terminal(42)]);
    }
    #[test]
    fn closing_terminal_pane_returns_to_file_and_keeps_terminal_listed() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        use workspace_core::editor_panes::Error;
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-terminal-q-reference.json"
        ))
        .unwrap();
        let path = PathBuf::from("/synthetic/a");
        let mut panes = Panes::new(
            path.clone(),
            ViewState {
                buffer: 0,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            },
        );
        let file_pane = panes.layout.active();
        let terminal_pane = panes.layout.split(Axis::Row, true).unwrap();
        assert_eq!(
            panes.layout.panes().len() as u64,
            reference["split"]["before"].as_u64().unwrap()
        );
        panes.set_terminal(42).unwrap();
        let mut registry = BufferRegistry::default();
        registry.register(Buffer::File(path.clone()));
        registry.register(Buffer::Terminal(42));
        assert_eq!(panes.layout.close(), Ok(terminal_pane));
        registry.close_pane(terminal_pane);
        assert_eq!(panes.layout.active(), file_pane);
        assert_eq!(
            panes.layout.panes().len() as u64,
            reference["split"]["after"].as_u64().unwrap()
        );
        assert_eq!(panes.path(file_pane), Some(&path));
        assert!(registry.listed().contains(&Buffer::Terminal(42)));
        assert!(reference["split"]["terminal_listed"].as_bool().unwrap());
        assert_eq!(panes.layout.close(), Err(Error::LastPane));
        assert!(reference["last"]["e444"].as_bool().unwrap());
        assert_eq!(panes.path(file_pane), Some(&path));
    }
    #[test]
    fn file_open_from_sole_terminal_retains_terminal_as_alternate() {
        use crate::buffer_registry::{Buffer, BufferRegistry};
        let mut panes = None;
        prepare_terminal_for_file(&mut panes, Some(42), true);
        let panes = panes.as_mut().unwrap();
        let pane = panes.layout.active();
        let old = Buffer::Terminal(42);
        let new = Buffer::File("/synthetic/new.txt".into());
        let mut registry = BufferRegistry::default();
        registry.register(old.clone());
        registry.replace(pane, Some(&old), new.clone());
        panes
            .set_content(Content::File("/synthetic/new.txt".into()))
            .unwrap();
        assert_eq!(registry.alternate(pane, &new), Some(old));
        assert_eq!(registry.cycle(&new, false), Some(Buffer::Terminal(42)));
        assert_eq!(panes.layout.panes().len(), 1);
        let mut no_terminal = None;
        prepare_terminal_for_file(&mut no_terminal, Some(42), false);
        assert!(no_terminal.is_none());
        let mut existing = Some(Panes::terminal(7));
        prepare_terminal_for_file(&mut existing, Some(42), true);
        let existing = existing.unwrap();
        assert_eq!(
            existing.content(existing.layout.active()),
            Some(&Content::Terminal(7))
        );
    }
    #[test]
    fn directional_focus_uses_visible_wrapped_cursor_row() {
        use workspace_core::editor_panes::{Bounds, Direction, Point};
        let mut editor = Editor::new("abcdefghijklmnopqrstuvwxyz1234567890".into());
        editor.goto(0, 25);
        let mut state = PaneWrap {
            enabled: true,
            ..PaneWrap::default()
        };
        let (row, col) = wrapped_navigation_cell(&editor, &state, 4, 10).unwrap();
        assert_eq!((row, col), (2, 5));

        let mut panes = Panes::terminal(1);
        let left = panes.layout.active();
        let top = panes.layout.split(Axis::Row, true).unwrap();
        let bottom = panes.layout.split(Axis::Column, true).unwrap();
        panes.layout.focus(left).unwrap();
        let bounds = [
            Bounds {
                pane: left,
                x: 0,
                y: 0,
                width: 100,
                height: 40,
            },
            Bounds {
                pane: top,
                x: 100,
                y: 0,
                width: 100,
                height: 20,
            },
            Bounds {
                pane: bottom,
                x: 100,
                y: 20,
                width: 100,
                height: 20,
            },
        ];
        assert!(
            panes
                .layout
                .focus_direction(
                    Direction::Right,
                    &bounds,
                    Point {
                        x: (col * 10 + 5) as i64,
                        y: (row * 10 + 5) as i64,
                    }
                )
                .unwrap()
        );
        assert_eq!(panes.layout.active(), bottom);
        state.top.visual_row = 1;
        assert_eq!(
            wrapped_navigation_cell(&editor, &state, 4, 10),
            Some((1, 5))
        );
    }
    #[test]
    fn directional_focus_matches_clean_neovim_wrapped_three_pane_reference() {
        use workspace_core::editor_panes::{Bounds, Direction, Point};
        // Neovim 0.12.1 --clean, 80x24, splitright/splitbelow, wrap,
        // scrolloff=0: a 900-character first line at byte 20 selected the
        // upper-right window; byte 490 selected the lower-right window.
        let mut editor = Editor::new("x".repeat(900));
        let state = PaneWrap {
            enabled: true,
            ..PaneWrap::default()
        };
        let mut panes = Panes::terminal(1);
        let left = panes.layout.active();
        let top = panes.layout.split(Axis::Row, true).unwrap();
        let bottom = panes.layout.split(Axis::Column, true).unwrap();
        let bounds = [
            Bounds {
                pane: left,
                x: 0,
                y: 0,
                width: 39,
                height: 22,
            },
            Bounds {
                pane: top,
                x: 40,
                y: 0,
                width: 40,
                height: 10,
            },
            Bounds {
                pane: bottom,
                x: 40,
                y: 11,
                width: 40,
                height: 11,
            },
        ];
        for (column, expected_row, expected_col, target) in
            [(20, 0, 20, top), (490, 12, 22, bottom)]
        {
            editor.goto(0, column);
            let (row, col) = wrapped_navigation_cell(&editor, &state, 22, 39).unwrap();
            assert_eq!((row, col), (expected_row, expected_col));
            panes.layout.focus(left).unwrap();
            assert!(
                panes
                    .layout
                    .focus_direction(
                        Direction::Right,
                        &bounds,
                        Point {
                            x: col as i64,
                            y: row as i64,
                        }
                    )
                    .unwrap()
            );
            assert_eq!(panes.layout.active(), target);
        }
    }
    #[test]
    fn wrapped_navigation_cell_respects_block_virtual_column_and_clipping() {
        let mut editor = Editor::new("abcdefghijklmnopqrstuvwxyz1234567890".into());
        editor.goto(0, 25);
        editor.key("<C-v>");
        editor.key("l");
        let mut state = PaneWrap {
            enabled: true,
            ..PaneWrap::default()
        };
        assert_eq!(
            wrapped_navigation_cell(&editor, &state, 4, 10),
            Some((2, 6))
        );
        state.top.visual_row = 1;
        assert_eq!(
            wrapped_navigation_cell(&editor, &state, 4, 10),
            Some((1, 6))
        );
        assert_eq!(editor.mode, Mode::VisualBlock);
    }
    #[test]
    fn wrapped_navigation_rejects_unrendered_search_end_cursor() {
        let mut editor = Editor::new("abcdef".into());
        editor.cursor = 6;
        editor.mode = Mode::Search;
        let state = PaneWrap {
            enabled: true,
            ..PaneWrap::default()
        };
        assert_eq!(wrapped_navigation_cell(&editor, &state, 4, 10), None);
        editor.mode = Mode::Insert;
        assert_eq!(
            wrapped_navigation_cell(&editor, &state, 4, 10),
            Some((0, 6))
        );
    }
    #[test]
    fn nested_groups_have_distinct_resize_weight_keys() {
        let pane = Node::Pane(PaneId(2));
        let group = Node::Split(Axis::Column, vec![pane.clone(), Node::Pane(PaneId(3))]);
        assert_ne!(
            Panes::weight_key(Axis::Column, &pane),
            Panes::weight_key(Axis::Column, &group)
        );
    }
    #[test]
    fn mixed_panels_keep_terminal_and_file_identities_distinct() {
        let mut panes = Panes::terminal(42);
        let terminal = panes.layout.active();
        let editor = panes.layout.split(Axis::Row, true).unwrap();
        panes.save(
            Path::new("/synthetic/a.rs"),
            ViewState {
                buffer: 0,
                row: 7,
                column: 2,
                top: 3,
                left: 0,
            },
        );
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
        assert_eq!(panes.path(editor), Some(&PathBuf::from("/synthetic/a.rs")));
        let other = panes.layout.split(Axis::Column, true).unwrap();
        panes.set_terminal(99).unwrap();
        assert_eq!(panes.content(other), Some(&Content::Terminal(99)));
        panes.layout.focus(terminal).unwrap();
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
        panes.layout.focus(other).unwrap();
        panes.layout.close().unwrap();
        assert_eq!(panes.content(terminal), Some(&Content::Terminal(42)));
        assert_eq!(panes.layout.panes()[&editor].row, 7);
    }
    #[test]
    fn reconnect_rebinds_all_shared_views_without_changing_other_content() {
        let mut panes = Panes::terminal(42);
        let first = panes.layout.active();
        let second = panes.layout.split(Axis::Row, true).unwrap();
        let third = panes.layout.split(Axis::Column, true).unwrap();
        panes.set_terminal(99).unwrap();
        panes.rebind_terminal(42, 77);
        assert_eq!(panes.content(first), Some(&Content::Terminal(77)));
        assert_eq!(panes.content(second), Some(&Content::Terminal(77)));
        assert_eq!(panes.content(third), Some(&Content::Terminal(99)));
        assert_eq!(panes.layout.active(), third);
    }
    #[test]
    fn explorer_agents_and_terminal_share_a_single_split_layout() {
        let mut p = Panes::terminal(42);
        let first = p.layout.active();
        let second = p.layout.split(Axis::Row, true).unwrap();
        assert_eq!(p.content(first), Some(&Content::Terminal(42)));
        assert_eq!(p.content(second), Some(&Content::Terminal(42)));
        p.set_content(Content::Explorer(7)).unwrap();
        let third = p.layout.split(Axis::Column, true).unwrap();
        p.set_content(Content::Agents(9)).unwrap();
        assert_eq!(p.content(first), Some(&Content::Terminal(42)));
        assert_eq!(p.content(second), Some(&Content::Explorer(7)));
        assert_eq!(p.content(third), Some(&Content::Agents(9)));
        p.layout.focus(first).unwrap();
        let duplicate = p.layout.split(Axis::Column, true).unwrap();
        assert_eq!(p.content(duplicate), Some(&Content::Terminal(42)));
        p.layout.close().unwrap();
        assert_eq!(p.content(first), Some(&Content::Terminal(42)));
        assert_eq!(p.content(second), Some(&Content::Explorer(7)));
        assert_eq!(p.content(third), Some(&Content::Agents(9)));
    }
    #[test]
    fn nested_splits_share_buffer_identity_but_restore_independent_views() {
        let state = ViewState {
            buffer: 0,
            row: 12,
            column: 7,
            top: 5,
            left: 2,
        };
        let mut p = Panes::new("/synthetic/a.rs".into(), state.clone());
        let first = p.layout.active();
        let second = p.layout.split(Axis::Row, true).unwrap();
        assert_eq!(p.path(first), p.path(second));
        let mut changed = state.clone();
        changed.row = 80;
        changed.top = 70;
        p.save(Path::new("/synthetic/a.rs"), changed.clone());
        assert_eq!(p.layout.panes()[&first].row, 12);
        assert_eq!(p.layout.panes()[&second].row, 80);
        p.layout.split(Axis::Column, true).unwrap();
        p.save(Path::new("/synthetic/b.txt"), state);
        assert_eq!(p.layout.panes()[&second].buffer, 1);
        assert_eq!(
            p.path(p.layout.active()).unwrap(),
            Path::new("/synthetic/b.txt")
        );
        p.layout.close().unwrap();
        assert_eq!(p.layout.active(), second);
        p.layout.focus(first).unwrap();
        assert_eq!(p.layout.panes()[&first].left, 2);
    }
}
