//! Guarded state for a floating zoom view of an existing pane.
//!
//! The GUI supplies a separately measured view and coordinates input,
//! focus changes and content rebinding around this state.
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::Arc};

use crate::buffer_registry::{Buffer, BufferRegistry};
use crate::editor::{Editor, EditorIdentity};
use crate::editor_panes::{Content, PaneId, Panes, ViewState};
use workspace_core::{
    editor_jumplist::Location as JumpLocation,
    editor_panes::{FocusGuard, Layout, PaneJump},
};

#[derive(Clone)]
pub struct ZoomSession {
    window_identity: Arc<()>,
    identity: Arc<()>,
    indicator_visible: bool,
    insets: (usize, usize),
    resize_capacity: Option<(usize, usize)>,
    workspace: usize,
    parent: PaneId,
    focus: FocusGuard,
    detached: bool,
    content_identity: Arc<()>,
    content: Content,
    alternate: Option<Buffer>,
    saved_file_views: BTreeMap<PathBuf, ZoomFileView>,
    saved_views_revision: u64,
    jump_layout: Layout,
    editor_identity: Option<EditorIdentity>,
    terminal_identity: Option<Arc<()>>,
    view: ViewState,
}

#[derive(Clone)]
struct ZoomFileView {
    editor: EditorIdentity,
    version: u64,
    row: usize,
    column: usize,
    left: usize,
}

/// Identifies one render of one floating view, independently of its parent.
/// A callback holding this value must still validate its editor or terminal
/// instance before applying the measured dimensions or routing input.
#[derive(Clone)]
pub struct ZoomRenderGuard {
    identity: Arc<()>,
    render: u64,
    insets: (usize, usize),
}

/// Identifies the exact floating file view that requested an async close.
#[derive(Clone)]
pub struct ZoomCloseGuard {
    incarnation: Arc<()>,
    editor: EditorIdentity,
}

/// Proof that this exact attached zoom was current immediately before close.
/// The caller must not yield or run another layout mutation before completing it.
pub struct ZoomOriginCloseGuard {
    session: Arc<()>,
    origin: PaneId,
    focus: FocusGuard,
}

/// A staged transition cannot be unpacked until its source is revalidated.
/// Callers must also fence asynchronous input, saves and focus state.
pub struct StagedZoomOriginClose {
    panes: Panes,
    zoom: ZoomSession,
    registry: BufferRegistry,
    closed: PaneId,
    neighbor: PaneId,
    source_panes: Panes,
    source_zoom: ZoomSession,
    source_registry: BufferRegistry,
    source_owner: ZoomOwnerSnapshot,
}

pub struct ValidatedZoomOriginClose {
    pub panes: Panes,
    pub zoom: ZoomSession,
    pub registry: BufferRegistry,
    pub closed: PaneId,
    pub neighbor: PaneId,
}

enum ZoomOwnerSnapshot {
    File { identity: EditorIdentity, version: u64, cursor: usize, mode: crate::editor::Mode },
    Terminal(Arc<()>),
    List,
}

/// The caller resolves the new owner from the focused pane's actual content.
pub enum RebindOwner<'a> {
    File(&'a Editor),
    Terminal(Arc<()>),
    List,
}

impl StagedZoomOriginClose {
    /// Revalidate the live source immediately before a synchronous publish.
    /// External asynchronous operations and other model state need separate fences.
    pub fn into_parts_if_current(
        self,
        workspace: usize,
        live_zoom: &ZoomSession,
        live_panes: &Panes,
        live_registry: &BufferRegistry,
        owner: RebindOwner<'_>,
    ) -> Option<ValidatedZoomOriginClose> {
        let before = &self.source_zoom;
        let same_zoom = live_zoom.is_current(workspace, live_panes)
            && Arc::ptr_eq(&before.window_identity, &live_zoom.window_identity)
            && Arc::ptr_eq(&before.identity, &live_zoom.identity)
            && Arc::ptr_eq(&before.content_identity, &live_zoom.content_identity)
            && before.workspace == live_zoom.workspace
            && before.parent == live_zoom.parent
            && before.detached == live_zoom.detached
            && before.content == live_zoom.content
            && before.alternate == live_zoom.alternate
            && before.saved_views_revision == live_zoom.saved_views_revision
            && before.jump_layout.matches_snapshot(&live_zoom.jump_layout)
            && before.view == live_zoom.view
            && before.indicator_visible == live_zoom.indicator_visible
            && before.insets == live_zoom.insets
            && before.resize_capacity == live_zoom.resize_capacity;
        let same_owner = match (&self.source_owner, owner) {
            (ZoomOwnerSnapshot::File { identity, version, cursor, mode }, RebindOwner::File(editor)) => {
                identity.matches(editor) && !editor.saving && editor.version == *version
                    && editor.cursor == *cursor && editor.mode == *mode
                    && live_zoom.owns_editor(workspace, live_panes, editor)
            }
            (ZoomOwnerSnapshot::Terminal(saved), RebindOwner::Terminal(instance)) => {
                Arc::ptr_eq(saved, &instance) && live_zoom.owns_terminal(workspace, live_panes, &instance)
            }
            (ZoomOwnerSnapshot::List, RebindOwner::List) => matches!(live_zoom.content(), Content::Explorer(_) | Content::Agents(_)),
            _ => false,
        };
        if !same_zoom || !same_owner || !live_panes.matches_snapshot(&self.source_panes)
            || live_registry != &self.source_registry
        {
            return None;
        }
        Some(ValidatedZoomOriginClose {
            panes: self.panes, zoom: self.zoom, registry: self.registry,
            closed: self.closed, neighbor: self.neighbor,
        })
    }
}

impl ZoomSession {
    /// Capture an existing focused window and its initial viewport. The
    /// caller must first synchronize any live editor cursor into the pane.
    fn capture(workspace: usize, panes: &mut Panes, alternate: Option<Buffer>) -> Option<Self> {
        let parent = panes.layout.active();
        let view = panes.layout.panes().get(&parent)?.clone();
        let content = panes.content(parent)?.clone();
        let focus = panes.layout.focus_guard();
        let jump_layout = panes.layout.clone_active_window().ok()?;
        let content_identity = panes.track_content_identity(parent)?;
        Some(Self {
            window_identity: Arc::new(()),
            identity: Arc::new(()),
            indicator_visible: true,
            insets: (0, 0),
            resize_capacity: None,
            workspace,
            parent,
            focus,
            detached: false,
            content_identity,
            content,
            alternate,
            saved_file_views: BTreeMap::new(),
            saved_views_revision: 0,
            jump_layout,
            editor_identity: None,
            terminal_identity: None,
            view,
        })
    }

    /// Capture the live editor associated with the focused file. The caller
    /// must resolve that editor from the focused path before calling this.
    pub fn capture_file(workspace: usize, panes: &mut Panes, editor: &Editor, alternate: Option<Buffer>) -> Option<Self> {
        if !matches!(panes.content(panes.layout.active()), Some(Content::File(_))) {
            return None;
        }
        let mut session = Self::capture(workspace, panes, alternate)?;
        session.editor_identity = Some(editor.identity());
        Some(session)
    }

    /// Bind the view to the exact terminal instance attached to the pane.
    /// The caller must resolve that instance from the focused attachment.
    pub fn capture_terminal(
        workspace: usize,
        panes: &mut Panes,
        instance: Arc<()>,
        alternate: Option<Buffer>,
    ) -> Option<Self> {
        if !matches!(
            panes.content(panes.layout.active()),
            Some(Content::Terminal(_))
        ) {
            return None;
        }
        let mut session = Self::capture(workspace, panes, alternate)?;
        session.terminal_identity = Some(instance);
        Some(session)
    }

    /// Explorer and Agents use list state instead of an editor or PTY.
    pub fn capture_list(workspace: usize, panes: &mut Panes, alternate: Option<Buffer>) -> Option<Self> {
        if !matches!(
            panes.content(panes.layout.active()),
            Some(Content::Explorer(_) | Content::Agents(_))
        ) {
            return None;
        }
        Self::capture(workspace, panes, alternate)
    }

    /// Reject focus round trips, reused pane numbers, replaced buffers and
    /// workspace changes; a stale floating view must not mutate its parent.
    pub fn is_current(&self, workspace: usize, panes: &Panes) -> bool {
        self.workspace == workspace
            && panes.layout.focus_guard_is_current(&self.focus)
            && if self.detached {
                panes.floating_content_identity_is_current(
                    self.parent, &self.content_identity, &self.window_identity,
                )
            } else {
                panes.content_identity_is_current(self.parent, &self.content_identity)
                    && panes.content(self.parent) == Some(&self.content)
                    && panes.layout.panes().get(&self.parent)
                        .is_some_and(|view| view.buffer == self.view.buffer)
            }
    }

    pub fn parent(&self) -> PaneId {
        self.parent
    }

    pub fn is_detached(&self) -> bool {
        self.detached
    }

    pub fn indicator_visible(&self) -> bool {
        self.indicator_visible
    }

    pub fn close_indicator(&mut self) {
        self.indicator_visible = false;
    }

    pub fn insets(&self) -> (usize, usize) {
        self.insets
    }

    /// Observe the painted content size only for the exact inset generation.
    pub fn observe_geometry(&mut self, guard: &ZoomRenderGuard, columns: usize, rows: usize) {
        if self.same_session(guard) && guard.insets == self.insets {
            self.resize_capacity = Some((
                columns.saturating_add(guard.insets.0),
                rows.saturating_add(guard.insets.1),
            ));
        }
    }

    /// Change only the float's cell-sized margins, leaving its parent layout.
    pub fn resize_relative(&mut self, key: &str, count: usize) -> bool {
        let Some((columns, rows)) = self.resize_capacity else {
            return false;
        };
        let amount = count.clamp(1, 9999);
        match key {
            "<" => {
                self.insets.0 = self.insets.0.saturating_add(amount).min(columns.saturating_sub(1));
            }
            ">" => self.insets.0 = self.insets.0.saturating_sub(amount),
            "-" => {
                self.insets.1 = self.insets.1.saturating_add(amount).min(rows.saturating_sub(1));
            }
            "+" => self.insets.1 = self.insets.1.saturating_sub(amount),
            _ => return false,
        }
        true
    }

    /// A missing count expands the selected dimension to its available maximum.
    pub fn resize_absolute(&mut self, key: &str, count: usize) -> bool {
        let Some((columns, rows)) = self.resize_capacity else {
            return false;
        };
        match key {
            "|" => {
                let target = if count == 0 { columns } else { count.clamp(1, columns) };
                self.insets.0 = columns.saturating_sub(target);
            }
            "_" => {
                let target = if count == 0 { rows } else { count.clamp(1, rows) };
                self.insets.1 = rows.saturating_sub(target);
            }
            _ => return false,
        }
        true
    }

    pub fn workspace(&self) -> usize {
        self.workspace
    }

    pub fn content(&self) -> &Content {
        &self.content
    }

    /// Float-local alternate: callers must also validate registry membership
    /// and availability before opening it.
    pub fn alternate(&self) -> Option<&Buffer> {
        self.alternate.as_ref()
    }

    /// A saved cursor belongs to this floating window and this exact editor
    /// revision; changes from another view invalidate it rather than guessing.
    pub fn saved_file_view(&self, path: &Path, editor: &Editor) -> Option<(usize, usize, usize)> {
        let saved = self.saved_file_views.get(path)?;
        (self.detached && saved.editor.matches(editor) && saved.version == editor.version)
            .then_some((saved.row, saved.column, saved.left))
    }

    pub fn buffer_for_content(content: &Content) -> Buffer {
        match content {
            Content::File(path) => Buffer::File(path.clone()),
            Content::Terminal(id) => Buffer::Terminal(*id),
            Content::Explorer(id) => Buffer::Explorer(*id),
            Content::Agents(id) => Buffer::Agents(*id),
        }
    }

    /// Stable across buffer changes and distinct for every floating window.
    pub fn window_token(&self) -> Arc<()> {
        Arc::clone(&self.window_identity)
    }

    /// Identity of the originating pane's current content.
    pub fn content_token(&self) -> Arc<()> {
        Arc::clone(&self.content_identity)
    }

    pub fn view(&self) -> &ViewState {
        &self.view
    }

    /// The float is a separate window: edits rebase its marks with every
    /// layout, while movements record only in the window that has focus.
    pub fn jump_layout(&self) -> &Layout {
        &self.jump_layout
    }

    pub fn jump_layout_mut(&mut self) -> &mut Layout {
        &mut self.jump_layout
    }

    pub fn owns_editor(&self, workspace: usize, panes: &Panes, editor: &Editor) -> bool {
        self.is_current(workspace, panes)
            && self
                .editor_identity
                .as_ref()
                .is_some_and(|id| id.matches(editor))
    }

    pub fn close_guard(
        &self,
        workspace: usize,
        panes: &Panes,
        editor: &Editor,
    ) -> Option<ZoomCloseGuard> {
        self.owns_editor(workspace, panes, editor)
            .then(|| ZoomCloseGuard {
                incarnation: Arc::clone(&self.identity),
                editor: editor.identity(),
            })
    }

    pub fn accepts_close(
        &self,
        workspace: usize,
        panes: &Panes,
        editor: &Editor,
        guard: &ZoomCloseGuard,
    ) -> bool {
        Arc::ptr_eq(&self.identity, &guard.incarnation)
            && guard.editor.matches(editor)
            && self.owns_editor(workspace, panes, editor)
    }

    /// Prepare a completed save-as or first save on copies. A stale async
    /// result cannot rename a different floating window or editor.
    pub fn stage_detached_file_rename(
        &self, workspace: usize, panes: &Panes, editor: &Editor,
        guard: &ZoomCloseGuard, old: &Path, new: &Path, save_as: bool,
    ) -> Option<(Panes, Self)> {
        if !self.detached || self.content != Content::File(old.to_owned())
            || !self.accepts_close(workspace, panes, editor, guard)
            || self.saved_file_views.contains_key(new)
        {
            return None;
        }
        let revision = if self.saved_file_views.contains_key(old) {
            Some(self.saved_views_revision.checked_add(1)?)
        } else {
            None
        };
        let mut next_panes = panes.clone();
        if !next_panes.rename_detached_floating_file(
            self.parent, &self.content_identity, &self.window_identity, self.view.buffer, old, new,
        ) {
            return None;
        }
        let mut next_zoom = self.clone();
        next_zoom.content = Content::File(new.to_owned());
        if save_as {
            next_zoom.alternate = Some(Buffer::File(old.to_owned()));
        } else if next_zoom.alternate.as_ref() == Some(&Buffer::File(old.to_owned())) {
            next_zoom.alternate = Some(Buffer::File(new.to_owned()));
        }
        if let Some(view) = next_zoom.saved_file_views.remove(old) {
            next_zoom.saved_file_views.insert(new.to_owned(), view);
            next_zoom.saved_views_revision = revision?;
        }
        next_zoom.is_current(workspace, &next_panes).then_some((next_panes, next_zoom))
    }

    /// A recycled attachment or vector index must not acquire the old zoom's
    /// PTY input or dimensions after a disconnect/reconnect.
    pub fn owns_terminal(&self, workspace: usize, panes: &Panes, instance: &Arc<()>) -> bool {
        self.is_current(workspace, panes) && self.terminal_instance_matches(instance)
    }

    pub fn terminal_instance_matches(&self, instance: &Arc<()>) -> bool {
        self.terminal_identity
            .as_ref()
            .is_some_and(|id| Arc::ptr_eq(id, instance))
    }

    /// Capture this floating render, not the underlay's pane measurement.
    pub fn render_guard(&self, render: u64) -> ZoomRenderGuard {
        ZoomRenderGuard {
            identity: Arc::clone(&self.identity),
            render,
            insets: self.insets,
        }
    }

    /// Compare session identity without treating an old render as current.
    pub fn same_session(&self, guard: &ZoomRenderGuard) -> bool {
        Arc::ptr_eq(&self.identity, &guard.identity)
    }

    /// Capture the exact attached float before its origin closes synchronously.
    pub fn prepare_origin_close(&self, workspace: usize, panes: &Panes) -> Option<ZoomOriginCloseGuard> {
        (!self.detached && self.is_current(workspace, panes)).then(|| ZoomOriginCloseGuard {
            session: Arc::clone(&self.identity),
            origin: self.parent,
            focus: self.focus.clone(),
        })
    }

    /// Stage the close on cloned layout and zoom state so failed validation
    /// leaves the live layout unchanged. Nothing is published by this method.
    pub fn stage_origin_close(
        &self, workspace: usize, panes: &Panes, registry: &BufferRegistry, owner: RebindOwner<'_>,
    ) -> Option<StagedZoomOriginClose> {
        let current = Self::buffer_for_content(&self.content);
        if !registry.knows(&current) {
            return None;
        }
        let source_owner = match (&self.content, owner) {
            (Content::File(_), RebindOwner::File(editor)) if !editor.saving && self.owns_editor(workspace, panes, editor) => {
                ZoomOwnerSnapshot::File { identity: editor.identity(), version: editor.version, cursor: editor.cursor, mode: editor.mode }
            }
            (Content::Terminal(_), RebindOwner::Terminal(instance)) if self.owns_terminal(workspace, panes, &instance) => {
                ZoomOwnerSnapshot::Terminal(instance)
            }
            (Content::Explorer(_) | Content::Agents(_), RebindOwner::List) => ZoomOwnerSnapshot::List,
            _ => return None,
        };
        let guard = self.prepare_origin_close(workspace, panes)?;
        let mut staged_panes = panes.clone();
        let previous = staged_panes.layout.clone();
        let (closed, neighbor) = staged_panes.layout.close_at(self.parent).ok()?;
        staged_panes.topology_after_close(&previous, neighbor);
        staged_panes.sizes.remove(&closed);
        let mut staged_zoom = self.clone();
        // While the overlay is attached, movements still use the parent
        // history. Preserve that current history before removing the parent.
        // The GUI must synchronize the active editor before staging.
        staged_zoom.jump_layout = panes.layout.clone_active_window().ok()?;
        staged_zoom.alternate = staged_zoom.alternate.filter(|alternate| {
            registry.knows(alternate) && alternate != &current
        });
        let mut staged_registry = registry.clone();
        staged_registry.close_pane(closed);
        if !staged_zoom.after_origin_close(workspace, &mut staged_panes, guard) {
            return None;
        }
        Some(StagedZoomOriginClose {
            panes: staged_panes, zoom: staged_zoom, registry: staged_registry, closed, neighbor,
            source_panes: panes.clone(), source_zoom: self.clone(),
            source_registry: registry.clone(), source_owner,
        })
    }

    /// Complete only that exact immediate close. Buffer/PTy lifetime, input
    /// and model focus remain the caller's responsibility before enabling it.
    pub fn after_origin_close(
        &mut self,
        workspace: usize,
        panes: &mut Panes,
        guard: ZoomOriginCloseGuard,
    ) -> bool {
        if self.detached
            || workspace != self.workspace
            || guard.origin != self.parent
            || !Arc::ptr_eq(&guard.session, &self.identity)
            || !panes.layout.focus_guard_direct_close(&guard.focus, self.parent)
            || !panes.layout.focus_guard_direct_close(&self.focus, self.parent)
            || !panes.detach_floating_after_origin_close(
                self.parent, &self.content_identity, &self.window_identity,
            )
        {
            return false;
        }
        self.focus = panes.layout.focus_guard();
        self.detached = true;
        true
    }

    /// Revalidate a surviving parent after an ordinary background window is
    /// closed. The caller must have captured the current session immediately
    /// before that synchronous close and confirmed the closed pane differs.
    pub fn after_background_close(
        &mut self,
        workspace: usize,
        panes: &mut Panes,
        guard: &ZoomRenderGuard,
    ) -> bool {
        if !self.same_session(guard)
            || workspace != self.workspace
            || panes.layout.active() != self.parent
            || !panes.layout.focus_guard_window_exists(&self.focus, self.parent)
            || !panes.content_identity_is_current(self.parent, &self.content_identity)
            || panes.content(self.parent) != Some(&self.content)
            || !panes.refresh_floating_focus(self.parent, &self.content_identity, &self.window_identity)
        {
            return false;
        }
        self.focus = panes.layout.focus_guard();
        true
    }

    /// Refresh the exact detached float after closing unrelated ordinary panes.
    pub fn after_detached_background_close(
        &mut self, workspace: usize, original: &Panes, staged: &mut Panes,
        guard: &ZoomRenderGuard,
    ) -> bool {
        let anchor = original.layout.active();
        if !self.detached || !self.same_session(guard) || !self.is_current(workspace, original)
            || staged.layout.active() != anchor
            || !staged.layout.focus_guard_window_exists(&self.focus, anchor)
            || !staged.refresh_detached_floating_focus(
                self.parent, &self.content_identity, &self.window_identity, anchor,
            )
        { return false; }
        self.focus = staged.layout.focus_guard();
        true
    }

    /// Switch a detached float to an already loaded, known buffer on clones.
    /// The caller must resolve both owners from their actual paths/attachments,
    /// fence pending operations, and update the surrounding UI model in the
    /// same synchronous transaction before exposing this operation.
    pub fn switch_detached_loaded(
        &mut self,
        workspace: usize,
        panes: &mut Panes,
        registry: &mut BufferRegistry,
        target: Content,
        source_owner: RebindOwner<'_>,
        target_owner: RebindOwner<'_>,
        target_position: Option<(usize, usize)>,
        top: usize,
        left: usize,
        relist: bool,
        jump: Option<(PaneJump, JumpLocation)>,
    ) -> bool {
        if !self.detached || !self.is_current(workspace, panes) {
            return false;
        }
        let (source_valid, saved_source) = match (&self.content, source_owner) {
            (Content::File(path), RebindOwner::File(editor)) => {
                let (row, column) = editor.position();
                (!editor.saving && self.owns_editor(workspace, panes, editor), Some((path.clone(), ZoomFileView {
                    editor: editor.identity(), version: editor.version, row, column, left: self.view.left,
                })))
            }
            (Content::Terminal(_), RebindOwner::Terminal(instance)) => (self.owns_terminal(workspace, panes, &instance), None),
            (Content::Explorer(_) | Content::Agents(_), RebindOwner::List) => (true, None),
            _ => (false, None),
        };
        let current = Self::buffer_for_content(&self.content);
        let destination = Self::buffer_for_content(&target);
        if !source_valid || !registry.knows(&current) || !registry.knows(&destination) {
            return false;
        }
        if target == self.content {
            if jump.is_some() {
                return false;
            }
            return match (&target, target_owner) {
                (Content::File(_), RebindOwner::File(editor)) => !editor.saving && self.owns_editor(workspace, panes, editor),
                (Content::Terminal(_), RebindOwner::Terminal(instance)) => self.owns_terminal(workspace, panes, &instance),
                (Content::Explorer(_) | Content::Agents(_), RebindOwner::List) => true,
                _ => false,
            };
        }
        let (editor_identity, terminal_identity, row, column, target_top, target_left) =
            match (&target, target_owner) {
                (Content::File(path), RebindOwner::File(editor)) if !editor.saving && !registry.is_unloaded(path) => {
                    let (row, column) = target_position.unwrap_or_else(|| editor.position());
                    (Some(editor.identity()), None, row, column, top, left)
                }
                (Content::Terminal(_), RebindOwner::Terminal(instance)) => (None, Some(instance), 0, 0, 0, 0),
                (Content::Explorer(_) | Content::Agents(_), RebindOwner::List) => (None, None, 0, 0, 0, 0),
                _ => return false,
            };
        let mut next_panes = panes.clone();
        let mut next_registry = registry.clone();
        let Some((buffer, token)) = next_panes.rebind_detached_floating_wrap(
            self.parent, &self.content_identity, &self.window_identity, &target, target_top,
        ) else {
            return false;
        };
        let mut next_zoom = self.clone();
        if let Some((plan, origin)) = jump
            && !next_zoom.jump_layout.commit_jump(plan, origin)
        {
            return false;
        }
        if let Some((path, view)) = saved_source {
            let Some(revision) = next_zoom.saved_views_revision.checked_add(1) else { return false; };
            if next_zoom.saved_file_views.len() >= 4096 && !next_zoom.saved_file_views.contains_key(&path) {
                next_zoom.saved_file_views.pop_first();
            }
            next_zoom.saved_file_views.insert(path, view);
            next_zoom.saved_views_revision = revision;
        }
        next_zoom.identity = Arc::new(());
        next_zoom.resize_capacity = None;
        next_zoom.content_identity = token;
        next_zoom.alternate = Some(current);
        next_zoom.content = target;
        next_zoom.editor_identity = editor_identity;
        next_zoom.terminal_identity = terminal_identity;
        next_zoom.view = ViewState { buffer, row, column, top: target_top, left: target_left };
        if relist {
            next_registry.relist(&destination);
        }
        next_registry.touch(&destination);
        *panes = next_panes;
        *registry = next_registry;
        *self = next_zoom;
        true
    }

    /// Rebind only when the same focused pane has committed a different
    /// buffer and the caller has resolved its current editor or PTY owner.
    pub fn rebind(
        &mut self,
        workspace: usize,
        panes: &mut Panes,
        owner: RebindOwner<'_>,
        top: usize,
        left: usize,
    ) -> bool {
        if workspace != self.workspace || !panes.layout.focus_guard_is_current(&self.focus) {
            return false;
        }
        let Some(content) = panes.content(self.parent).cloned() else {
            return false;
        };
        if content == self.content {
            return false;
        }
        let Some(mut view) = panes.layout.panes().get(&self.parent).cloned() else {
            return false;
        };
        let (editor_identity, terminal_identity) = match (&content, owner) {
            (Content::File(_), RebindOwner::File(editor)) => {
                let (row, column) = editor.position();
                view.row = row;
                view.column = column;
                view.top = top;
                view.left = left;
                (Some(editor.identity()), None)
            }
            (Content::Terminal(_), RebindOwner::Terminal(instance)) => (None, Some(instance)),
            (Content::Explorer(_) | Content::Agents(_), RebindOwner::List) => (None, None),
            _ => return false,
        };
        let Some(next) = panes.track_content_identity(self.parent) else {
            return false;
        };
        if !panes.rebind_floating_wrap(self.parent, &self.content_identity, &self.window_identity, next.clone(), top) {
            return false;
        }
        self.identity = Arc::new(());
        // A new content owner may have a different text area or cell grid.
        self.resize_capacity = None;
        self.content_identity = next;
        self.alternate = Some(Self::buffer_for_content(&self.content));
        self.content = content;
        self.editor_identity = editor_identity;
        self.terminal_identity = terminal_identity;
        self.view = view;
        true
    }

    /// Ignore callbacks from an earlier render or a closed/reopened zoom.
    /// Renderer and terminal identities and dimensions need separate checks.
    pub fn accepts_render(
        &self,
        workspace: usize,
        panes: &Panes,
        render: u64,
        guard: &ZoomRenderGuard,
    ) -> bool {
        self.same_session(guard)
            && guard.render == render
            && guard.insets == self.insets
            && self.is_current(workspace, panes)
    }

    /// Synchronize the live editor cursor, retaining the parent's viewport.
    /// The floating viewport is kept independently for the next zoom render.
    pub fn sync_editor(
        &mut self,
        workspace: usize,
        panes: &mut Panes,
        editor: &Editor,
        top: usize,
        left: usize,
    ) -> bool {
        if !self.owns_editor(workspace, panes, editor) {
            return false;
        }
        let (row, column) = editor.position();
        if !self.detached {
            let Ok(parent) = panes.layout.view_mut(self.parent) else {
                return false;
            };
            parent.row = row;
            parent.column = column;
        }
        self.view.row = row;
        self.view.column = column;
        self.view.top = top;
        self.view.left = left;
        true
    }
}
