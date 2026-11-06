//! Bounded, opt-in edit metadata for rebasing window-local jump marks.
//! The journal contains coordinates and versions only, never buffer text.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
};

const MAX_CHANGES: usize = 128;
const MAX_TRACKED_BUFFERS: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Impact {
    /// A jump's origin within the editor and its captured pane viewport.
    Jump {
        row: usize,
        column: usize,
        view: workspace_core::editor_jumplist::View,
    },
    /// A line replacement using Neovim's mark gravity, in zero-based rows.
    /// When both counts are zero, only `start` may have changed; callers must
    /// use `Unmapped` if they cannot prove the edit was limited to that line.
    Lines {
        start: usize,
        removed: usize,
        added: usize,
    },
    /// No exact mapping has been proved. The owner must invalidate marks.
    Unmapped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub before: u64,
    pub after: u64,
    pub impact: Impact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    /// Coordinates could not be verified; affected marks were removed.
    Invalidated,
    /// No stable buffer identity was supplied; navigation must stay disabled.
    Unavailable,
}

/// The guard is captured before dispatching input to this editor.
#[derive(Clone, Debug)]
pub struct Origin {
    pub layout_index: usize,
    pub focus: workspace_core::editor_panes::FocusGuard,
}

/// Binds the journal to one editor instance and one non-reused buffer ID.
/// The owner must resolve the ID again before every synchronization.
pub struct Bridge {
    buffer: u64,
    expected_version: u64,
    journal_identity: Arc<()>,
    active: bool,
}

/// One bridge per stable catalog identity, shared by every workspace view.
/// Ownership and the identity lookup are checked by the caller before sync.
#[derive(Default)]
pub struct LiveTracking {
    bridges: HashMap<u64, Bridge>,
    suspended: HashSet<u64>,
}

impl LiveTracking {
    /// A caller preparing a jump must first synchronize every involved editor.
    pub fn is_current(&self, buffer: u64, editor: &super::Editor) -> bool {
        self.bridges.get(&buffer).is_some_and(|bridge| {
            bridge.active
                && bridge.expected_version == editor.version
                && editor.jump_changes.as_ref().is_some_and(|journal| {
                    Arc::ptr_eq(&journal.identity, &bridge.journal_identity)
                        && journal.pending.is_none()
                        && !journal.has_changes()
                })
        })
    }

    /// Drop marks only after the owner has committed unloading this buffer.
    pub fn forget(
        &mut self,
        buffer: u64,
        editor: Option<&mut super::Editor>,
        layouts: &mut [&mut workspace_core::editor_panes::Layout],
    ) {
        for layout in layouts.iter_mut() {
            layout.forget_jump_buffer(buffer);
        }
        self.bridges.remove(&buffer);
        self.suspended.remove(&buffer);
        if let Some(editor) = editor {
            editor.disable_jump_changes();
        }
    }

    /// Preserve marks only for an owner-committed unload, after applying every
    /// pending edit. If no bridge can prove provenance, discard that buffer's
    /// marks rather than preserving potentially stale coordinates.
    pub fn suspend(
        &mut self,
        buffer: u64,
        editor: Option<&mut super::Editor>,
        layouts: &mut [&mut workspace_core::editor_panes::Layout],
    ) {
        if buffer == 0 {
            return;
        }
        if !self.bridges.contains_key(&buffer)
            && !self.suspended.contains(&buffer)
            && self.bridges.len().saturating_add(self.suspended.len()) >= MAX_TRACKED_BUFFERS
        {
            for layout in layouts.iter_mut() {
                layout.forget_jump_buffer(buffer);
            }
            if let Some(editor) = editor {
                editor.disable_jump_changes();
            }
            return;
        }
        if let Some(editor) = editor {
            if self.bridges.contains_key(&buffer) {
                let _ = self.sync(buffer, editor, None, layouts);
            } else {
                for layout in layouts.iter_mut() {
                    layout.forget_jump_buffer(buffer);
                }
            }
            editor.disable_jump_changes();
        } else if !self.suspended.contains(&buffer) {
            for layout in layouts.iter_mut() {
                layout.forget_jump_buffer(buffer);
            }
        }
        self.bridges.remove(&buffer);
        self.suspended.insert(buffer);
    }

    /// Called only after a guarded loader created the new instance for the
    /// same catalog identity. Marks remain window-local and retain their rows.
    pub fn resume(&mut self, buffer: u64, editor: &mut super::Editor) -> bool {
        if !self.suspended.contains(&buffer)
            || self.bridges.contains_key(&buffer)
            || self.bridges.len() >= MAX_TRACKED_BUFFERS
        {
            return false;
        }
        let Some(bridge) = Bridge::attach(editor, buffer) else {
            return false;
        };
        self.bridges.insert(buffer, bridge);
        self.suspended.remove(&buffer);
        true
    }

    pub fn is_suspended(&self, buffer: u64) -> bool {
        self.suspended.contains(&buffer)
    }
    pub fn sync(
        &mut self,
        buffer: u64,
        editor: &mut super::Editor,
        origin: Option<&Origin>,
        layouts: &mut [&mut workspace_core::editor_panes::Layout],
    ) -> ApplyOutcome {
        if buffer == 0 {
            return ApplyOutcome::Unavailable;
        }
        if self.suspended.contains(&buffer) {
            return ApplyOutcome::Unavailable;
        }
        if !self.bridges.contains_key(&buffer) {
            if self.bridges.len().saturating_add(self.suspended.len()) >= MAX_TRACKED_BUFFERS {
                return ApplyOutcome::Unavailable;
            }
            let bridge = match Bridge::attach(editor, buffer) {
                Some(bridge) => bridge,
                None => {
                    // A journal without its original owner cannot be safely
                    // rebound, even if the path looks unchanged.
                    for layout in layouts.iter_mut() {
                        layout.forget_jump_buffer(buffer);
                    }
                    editor.disable_jump_changes();
                    let Some(bridge) = Bridge::attach(editor, buffer) else {
                        return ApplyOutcome::Unavailable;
                    };
                    bridge
                }
            };
            self.bridges.insert(buffer, bridge);
        }
        let outcome =
            self.bridges
                .get_mut(&buffer)
                .unwrap()
                .sync(editor, Some(buffer), origin, layouts);
        if outcome == ApplyOutcome::Unavailable {
            self.bridges.remove(&buffer);
            editor.disable_jump_changes();
        }
        outcome
    }

    /// A catalog failure invalidates every history that used its IDs. Disable
    /// all journals as well, so a later fresh catalog cannot reuse them.
    pub fn reset(
        &mut self,
        editors: &mut HashMap<PathBuf, super::Editor>,
        layouts: &mut [&mut workspace_core::editor_panes::Layout],
    ) {
        for layout in layouts.iter_mut() {
            layout.clear_jump_history();
        }
        self.bridges.clear();
        self.suspended.clear();
        for editor in editors.values_mut() {
            editor.disable_jump_changes();
        }
    }
}

impl Bridge {
    pub fn attach(editor: &mut super::Editor, buffer: u64) -> Option<Self> {
        if buffer == 0 {
            return None;
        }
        let expected_version = editor.enable_jump_changes()?;
        let journal_identity = editor.jump_changes.as_ref()?.identity.clone();
        Some(Self {
            buffer,
            expected_version,
            journal_identity,
            active: true,
        })
    }

    pub fn sync(
        &mut self,
        editor: &mut super::Editor,
        resolved_buffer: Option<u64>,
        origin: Option<&Origin>,
        layouts: &mut [&mut workspace_core::editor_panes::Layout],
    ) -> ApplyOutcome {
        if !self.active {
            return ApplyOutcome::Unavailable;
        }
        let valid_identity = editor
            .jump_changes
            .as_ref()
            .is_some_and(|journal| Arc::ptr_eq(&journal.identity, &self.journal_identity));
        if resolved_buffer != Some(self.buffer) || !valid_identity {
            for layout in layouts.iter_mut() {
                layout.forget_jump_buffer(self.buffer);
            }
            self.active = false;
            return ApplyOutcome::Unavailable;
        }
        let changes = editor.take_jump_changes();
        let outcome = apply_to_layouts(
            self.buffer,
            self.expected_version,
            editor.version,
            &changes,
            origin,
            layouts,
        );
        if outcome == ApplyOutcome::Unavailable {
            self.active = false;
        } else {
            self.expected_version = editor.version;
        }
        outcome
    }
}

/// Apply one editor's ordered changes to every owning workspace in one
/// transaction. The caller supplies a stable, workspace-authorized buffer ID
/// and must keep the editor and all layouts borrowed for the full operation.
pub fn apply_to_layouts(
    buffer: u64,
    expected_version: u64,
    current_version: u64,
    changes: &[Change],
    origin: Option<&Origin>,
    layouts: &mut [&mut workspace_core::editor_panes::Layout],
) -> ApplyOutcome {
    if buffer == 0 {
        return ApplyOutcome::Unavailable;
    }
    if changes.is_empty() && expected_version == current_version {
        return ApplyOutcome::Applied;
    }
    let mut version = expected_version;
    let mut valid = true;
    for change in changes {
        if change.before != version || change.impact == Impact::Unmapped {
            valid = false;
        }
        let expected_after = if matches!(change.impact, Impact::Jump { .. }) {
            change.before
        } else {
            change.before.wrapping_add(1)
        };
        if change.impact != Impact::Unmapped && change.after != expected_after {
            valid = false;
        }
        version = change.after;
    }
    valid &= version == current_version;
    let mut staged: Vec<_> = layouts.iter().map(|layout| (**layout).clone()).collect();
    if valid {
        for change in changes {
            match change.impact {
                Impact::Lines {
                    start,
                    removed,
                    added,
                } => {
                    for layout in &mut staged {
                        if !layout.adjust_jump_lines(buffer, start, removed, added) {
                            valid = false;
                        }
                    }
                }
                Impact::Jump { row, column, view } => {
                    let recorded = origin.and_then(|origin| {
                        staged.get_mut(origin.layout_index).map(|layout| {
                            layout.record_jump_guarded(
                                &origin.focus,
                                workspace_core::editor_jumplist::Location {
                                    buffer,
                                    row,
                                    column,
                                    view,
                                },
                            )
                        })
                    });
                    if recorded != Some(true) {
                        valid = false;
                    }
                }
                Impact::Unmapped => unreachable!(),
            }
        }
    }
    if !valid {
        // An untracked edit must not leave an apparently precise old mark.
        for layout in &mut staged {
            layout.forget_jump_buffer(buffer);
        }
    }
    for (target, next) in layouts.iter_mut().zip(staged) {
        **target = next;
    }
    if valid {
        ApplyOutcome::Applied
    } else {
        ApplyOutcome::Invalidated
    }
}

#[derive(Default)]
pub(super) struct Journal {
    pending: Option<Impact>,
    changes: VecDeque<Change>,
    identity: Arc<()>,
}

impl Journal {
    pub(super) fn prepare(&mut self, impact: Impact) {
        self.pending = Some(if self.pending.is_some() {
            Impact::Unmapped
        } else {
            impact
        });
    }

    pub(super) fn changed(&mut self, before: u64, after: u64) {
        let impact = self.pending.take().unwrap_or(Impact::Unmapped);
        self.push(Change {
            before,
            after,
            impact,
        });
    }

    pub(super) fn jump(
        &mut self,
        version: u64,
        row: usize,
        column: usize,
        view: workspace_core::editor_jumplist::View,
    ) {
        self.push(Change {
            before: version,
            after: version,
            impact: Impact::Jump { row, column, view },
        });
    }

    fn push(&mut self, change: Change) {
        if self.changes.len() == MAX_CHANGES {
            let start = self
                .changes
                .front()
                .map_or(change.before, |first| first.before);
            self.changes.clear();
            self.changes.push_back(Change {
                before: start,
                after: change.after,
                impact: Impact::Unmapped,
            });
        } else {
            self.changes.push_back(change);
        }
    }

    pub(super) fn drain(&mut self) -> Vec<Change> {
        self.changes.drain(..).collect()
    }

    pub(super) fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }
}

impl super::Editor {
    /// Read bounded coordinate metadata before the single owner drains it.
    /// A pending edit is incomplete and must never be projected speculatively.
    pub fn peek_jump_changes(&self) -> Option<Vec<Change>> {
        let journal = self.jump_changes.as_ref()?;
        journal
            .pending
            .is_none()
            .then(|| journal.changes.iter().copied().collect())
    }

    /// Enable before recording a window's first mark in this buffer.
    pub fn enable_jump_changes(&mut self) -> Option<u64> {
        if self.jump_changes.is_some() {
            return None;
        }
        self.jump_changes = Some(Journal::default());
        Some(self.version)
    }

    pub fn disable_jump_changes(&mut self) {
        self.jump_changes = None;
    }

    /// A single owner must distribute the ordered batch to all workspaces.
    pub fn take_jump_changes(&mut self) -> Vec<Change> {
        self.jump_changes
            .as_mut()
            .map(Journal::drain)
            .unwrap_or_default()
    }

    pub(super) fn prepare_jump_change(&mut self, impact: Impact) {
        if let Some(journal) = &mut self.jump_changes {
            journal.prepare(impact);
        }
    }

    pub fn set_jump_history_view(&mut self, view: workspace_core::editor_jumplist::View) {
        self.jump_history_view = view;
    }

    pub(super) fn record_history_jump(&mut self) {
        if self.jump_changes.is_none() {
            return;
        }
        let (row, column) = self.position();
        let view = self.jump_history_view;
        if let Some(journal) = &mut self.jump_changes {
            journal.jump(self.version, row, column, view);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use workspace_core::editor_jumplist::{JumpList, Location, View};

    #[test]
    fn live_tracking_uses_catalog_identity_across_workspaces_and_rename() {
        use crate::{buffer_numbers::BufferTracking, buffer_registry::Buffer};
        use workspace_core::editor_panes::{Layout, ViewState};
        let a = Buffer::File("/synthetic/a".into());
        let b = Buffer::File("/synthetic/b".into());
        let mut numbers = BufferTracking::default();
        numbers.register(1, a.clone());
        numbers.register(2, a.clone());
        let id = numbers.number_for(1, &a).unwrap();
        assert_eq!(numbers.number_for(2, &a), Ok(id));
        let view = ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        };
        let mut first = Layout::new(view.clone());
        let mut second = Layout::new(view);
        let mut editor = Editor::new("one\ntwo\nthree\nfour".into());
        let mut live = LiveTracking::default();
        assert_eq!(
            live.sync(id, &mut editor, None, &mut [&mut first, &mut second]),
            ApplyOutcome::Applied
        );
        assert!(live.is_current(id, &editor));
        editor.goto(2, 0);
        let origin = Origin {
            layout_index: 0,
            focus: first.focus_guard(),
        };
        editor.key("G");
        assert!(!live.is_current(id, &editor));
        assert_eq!(
            live.sync(
                id,
                &mut editor,
                Some(&origin),
                &mut [&mut first, &mut second]
            ),
            ApplyOutcome::Applied
        );
        assert!(live.is_current(id, &editor));
        assert_eq!(first.jump_list(first.active()).unwrap().entries()[0].row, 2);
        assert!(
            second
                .jump_list(second.active())
                .unwrap()
                .entries()
                .is_empty()
        );
        second
            .jump_list_mut(second.active())
            .unwrap()
            .record(Location {
                buffer: id,
                row: 2,
                column: 0,
                view: View::default(),
            });
        editor.goto(0, 0);
        editor.key("i");
        editor.key("<Enter>");
        assert!(!live.is_current(id, &editor));
        assert_eq!(
            live.sync(id, &mut editor, None, &mut [&mut first, &mut second]),
            ApplyOutcome::Applied
        );
        assert_eq!(first.jump_list(first.active()).unwrap().entries()[0].row, 3);
        assert_eq!(
            second.jump_list(second.active()).unwrap().entries()[0].row,
            3
        );
        numbers.rebind(&a, b.clone(), None);
        assert_eq!(numbers.number_for(1, &b), Ok(id));
        assert_eq!(
            live.sync(id, &mut editor, None, &mut [&mut first, &mut second]),
            ApplyOutcome::Applied
        );
        second
            .jump_list_mut(second.active())
            .unwrap()
            .record(Location {
                buffer: 999,
                row: 0,
                column: 0,
                view: View::default(),
            });
        let mut editors = HashMap::from([(PathBuf::from("/synthetic/b"), editor)]);
        live.reset(&mut editors, &mut [&mut first, &mut second]);
        assert!(
            first
                .jump_list(first.active())
                .unwrap()
                .entries()
                .is_empty()
        );
        assert!(
            second
                .jump_list(second.active())
                .unwrap()
                .entries()
                .is_empty()
        );
        assert!(
            editors
                .get_mut(&PathBuf::from("/synthetic/b"))
                .unwrap()
                .enable_jump_changes()
                .is_some()
        );
    }

    #[test]
    fn live_tracking_forgets_marks_if_editor_identity_is_replaced() {
        use workspace_core::editor_panes::{Layout, ViewState};
        let mut pane = Layout::new(ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let mut live = LiveTracking::default();
        let mut original = Editor::new("a\nb".into());
        assert_eq!(
            live.sync(8, &mut original, None, &mut [&mut pane]),
            ApplyOutcome::Applied
        );
        pane.jump_list_mut(pane.active()).unwrap().record(Location {
            buffer: 8,
            row: 1,
            column: 0,
            view: View::default(),
        });
        let mut replacement = Editor::new("unrelated".into());
        assert_eq!(
            live.sync(8, &mut replacement, None, &mut [&mut pane]),
            ApplyOutcome::Unavailable
        );
        assert!(pane.jump_list(pane.active()).unwrap().entries().is_empty());
        assert_eq!(
            live.sync(8, &mut replacement, None, &mut [&mut pane]),
            ApplyOutcome::Applied
        );
    }

    #[test]
    fn live_tracking_has_a_fixed_buffer_limit() {
        let mut live = LiveTracking::default();
        let mut first = Editor::new(String::new());
        assert_eq!(
            live.sync(1, &mut first, None, &mut []),
            ApplyOutcome::Applied
        );
        for id in 2..=MAX_TRACKED_BUFFERS as u64 {
            let mut editor = Editor::new(String::new());
            assert_eq!(
                live.sync(id, &mut editor, None, &mut []),
                ApplyOutcome::Applied
            );
        }
        let mut next = Editor::new(String::new());
        assert_eq!(
            live.sync(MAX_TRACKED_BUFFERS as u64 + 1, &mut next, None, &mut []),
            ApplyOutcome::Unavailable
        );
        live.suspend(1, Some(&mut first), &mut []);
        assert!(live.is_suspended(1));
        assert_eq!(
            live.sync(MAX_TRACKED_BUFFERS as u64 + 1, &mut next, None, &mut []),
            ApplyOutcome::Unavailable
        );
        let mut reloaded = Editor::new(String::new());
        assert!(live.resume(1, &mut reloaded));
        let mut editors = HashMap::new();
        live.reset(&mut editors, &mut []);
        assert_eq!(
            live.sync(MAX_TRACKED_BUFFERS as u64 + 1, &mut next, None, &mut []),
            ApplyOutcome::Applied
        );
    }

    #[test]
    fn unload_preserves_marks_with_jumpoptions_view_but_wipe_removes_them() {
        use workspace_core::editor_panes::{Layout, ViewState};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-unload.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let state = ViewState {
                buffer: 2,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            };
            let mut layout = Layout::new(state);
            let mut other = Layout::new(ViewState {
                buffer: 2,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            });
            let mut editor = Editor::new("a1\na2\na3".into());
            let mut live = LiveTracking::default();
            assert_eq!(
                live.sync(1, &mut editor, None, &mut [&mut layout, &mut other]),
                ApplyOutcome::Applied
            );
            let mark = Location {
                buffer: 1,
                row: 1,
                column: 1,
                view: View::default(),
            };
            let other_mark = Location {
                buffer: 2,
                row: 2,
                column: 0,
                view: View::default(),
            };
            layout.jump_list_mut(layout.active()).unwrap().record(mark);
            other.jump_list_mut(other.active()).unwrap().record(mark);
            other
                .jump_list_mut(other.active())
                .unwrap()
                .record(other_mark);
            let current = Location {
                buffer: 2,
                row: 0,
                column: 0,
                view: View::default(),
            };
            let old_plan = layout.prepare_jump(current, true, 1).unwrap();
            if name == "bwipeout" {
                live.forget(1, Some(&mut editor), &mut [&mut layout, &mut other]);
                assert!(!layout.commit_jump(old_plan, current));
                assert!(!live.is_current(1, &editor));
                assert_eq!(
                    other.jump_list(other.active()).unwrap().entries(),
                    &[other_mark]
                );
            } else if name != "none" {
                live.suspend(1, Some(&mut editor), &mut [&mut layout, &mut other]);
                assert!(live.is_suspended(1));
                assert!(!live.is_current(1, &editor));
                let mut loaded = Editor::new("a1\na2\na3".into());
                assert!(live.resume(1, &mut loaded));
                assert!(live.is_current(1, &loaded));
                assert_eq!(old_plan.target(), Some(mark));
                assert_eq!(
                    other.jump_list(other.active()).unwrap().entries(),
                    &[mark, other_mark]
                );
            }
            let actual: Vec<_> = layout
                .jump_list(layout.active())
                .unwrap()
                .entries()
                .iter()
                .map(|entry| (entry.row + 1, entry.column))
                .collect();
            let expected: Vec<_> = case["before"]["jumps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    (
                        entry["row"].as_u64().unwrap() as usize,
                        entry["col"].as_u64().unwrap() as usize,
                    )
                })
                .collect();
            assert_eq!(actual, expected, "{name}");
        }
    }

    #[test]
    fn suspension_rebases_pending_edits_and_rejects_unowned_jump_events() {
        use workspace_core::editor_panes::{Layout, ViewState};
        let mut pane = Layout::new(ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let mut live = LiveTracking::default();
        let mut editor = Editor::new("one\ntwo\nthree".into());
        assert_eq!(
            live.sync(1, &mut editor, None, &mut [&mut pane]),
            ApplyOutcome::Applied
        );
        pane.jump_list_mut(pane.active()).unwrap().record(Location {
            buffer: 1,
            row: 2,
            column: 0,
            view: View::default(),
        });
        editor.key("i");
        editor.key("<Enter>");
        live.suspend(1, Some(&mut editor), &mut [&mut pane]);
        assert_eq!(pane.jump_list(pane.active()).unwrap().entries()[0].row, 3);
        let mut loaded = Editor::new("\none\ntwo\nthree".into());
        assert!(!live.resume(2, &mut loaded));
        assert!(live.resume(1, &mut loaded));
        assert!(!live.resume(1, &mut loaded));
        loaded.key("G");
        live.suspend(1, Some(&mut loaded), &mut [&mut pane]);
        assert!(pane.jump_list(pane.active()).unwrap().entries().is_empty());
    }

    #[test]
    fn untracked_changes_and_overflow_fail_closed_without_buffer_text() {
        let mut journal = Journal::default();
        for version in 0..=MAX_CHANGES as u64 {
            journal.prepare(Impact::Lines {
                start: 0,
                removed: 0,
                added: 0,
            });
            journal.changed(version, version + 1);
        }
        assert_eq!(
            journal.drain(),
            vec![Change {
                before: 0,
                after: MAX_CHANGES as u64 + 1,
                impact: Impact::Unmapped
            }]
        );
        journal.changed(129, 130);
        assert_eq!(journal.drain()[0].impact, Impact::Unmapped);
        let mut editor = Editor::new("one\ntwo".into());
        assert_eq!(editor.enable_jump_changes(), Some(0));
        assert_eq!(editor.enable_jump_changes(), None);
        editor.replace_formatted("changed".into());
        assert_eq!(editor.take_jump_changes()[0].impact, Impact::Unmapped);
    }

    #[test]
    fn editor_typed_line_events_rebase_neovim_reference_marks() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        for case in fixture["typed_lines"].as_array().unwrap() {
            let mut editor = Editor::new("a1\na2\n\na4\na5\n\na7\na8".into());
            let row = case["row"].as_u64().unwrap() as usize - 1;
            editor.goto(row, 0);
            let mut history = JumpList::default();
            for mark in case["before"]["jumps"].as_array().unwrap() {
                history.record(Location {
                    buffer: 1,
                    row: mark["row"].as_u64().unwrap() as usize - 1,
                    column: mark["col"].as_u64().unwrap() as usize,
                    view: View::default(),
                });
            }
            assert_eq!(editor.enable_jump_changes(), Some(0));
            let keys = case["edit"]
                .as_str()
                .unwrap()
                .replace("<Esc>", "\u{1b}")
                .replace("<CR>", "\n");
            for key in keys.chars() {
                match key {
                    '\u{1b}' => {
                        editor.key("<Esc>");
                    }
                    '\n' => {
                        editor.key("<Enter>");
                    }
                    key if editor.mode == crate::editor::Mode::Insert => {
                        editor.insert(&key.to_string());
                    }
                    key => {
                        editor.key(&key.to_string());
                    }
                }
            }
            let changes = editor.take_jump_changes();
            assert!(!changes.is_empty(), "{}", case["edit"]);
            for (i, change) in changes.iter().enumerate() {
                if i > 0 {
                    assert_eq!(changes[i - 1].after, change.before);
                }
                match change.impact {
                    Impact::Lines {
                        start,
                        removed,
                        added,
                    } => assert!(history.adjust_lines(1, start, removed, added)),
                    Impact::Unmapped => panic!("unmapped {}", case["edit"]),
                    Impact::Jump { .. } => panic!("unexpected jump {}", case["edit"]),
                }
            }
            let after = &case["after"];
            let position = after["cursor"].as_array().unwrap();
            history.normalize(Location {
                buffer: 1,
                row: position[0].as_u64().unwrap() as usize - 1,
                column: position[1].as_u64().unwrap() as usize,
                view: View::default(),
            });
            let actual: Vec<_> = history
                .entries()
                .iter()
                .map(|m| (m.row + 1, m.column))
                .collect();
            let expected: Vec<_> = after["jumps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| {
                    (
                        m["row"].as_u64().unwrap() as usize,
                        m["col"].as_u64().unwrap() as usize,
                    )
                })
                .collect();
            assert_eq!(actual, expected, "{}", case["edit"]);
        }
    }

    #[test]
    fn multiline_paste_and_undo_are_explicitly_unmapped() {
        let mut editor = Editor::new("first\nsecond".into());
        assert_eq!(editor.enable_jump_changes(), Some(0));
        editor.key("i");
        editor.paste("one\ntwo");
        assert_eq!(editor.take_jump_changes()[0].impact, Impact::Unmapped);
        editor.key("<Esc>");
        editor.key("u");
        assert!(
            editor
                .take_jump_changes()
                .iter()
                .any(|change| change.impact == Impact::Unmapped)
        );
    }

    #[test]
    fn editor_events_update_all_workspaces_and_unmapped_edits_remove_only_affected_marks() {
        use workspace_core::editor_panes::{Layout, ViewState};
        let view = ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        };
        let mut first = Layout::new(view.clone());
        let mut second = Layout::new(view);
        let mark = Location {
            buffer: 1,
            row: 3,
            column: 1,
            view: View::default(),
        };
        let unrelated = Location {
            buffer: 2,
            row: 7,
            ..mark
        };
        first.jump_list_mut(first.active()).unwrap().record(mark);
        second.jump_list_mut(second.active()).unwrap().record(mark);
        second
            .jump_list_mut(second.active())
            .unwrap()
            .record(unrelated);
        let mut editor = Editor::new("a1\na2\n\na4\na5\n\na7\na8".into());
        let base = editor.enable_jump_changes().unwrap();
        editor.goto(2, 0);
        editor.key("o");
        editor.key("<Esc>");
        let changes = editor.take_jump_changes();
        assert_eq!(
            apply_to_layouts(
                1,
                base,
                editor.version,
                &changes,
                None,
                &mut [&mut first, &mut second]
            ),
            ApplyOutcome::Applied
        );
        for layout in [&first, &second] {
            assert_eq!(
                layout.jump_list(layout.active()).unwrap().entries()[0].row,
                4
            );
        }
        let base = editor.version;
        editor.key("i");
        editor.paste("new\nlines");
        let changes = editor.take_jump_changes();
        assert_eq!(
            apply_to_layouts(
                1,
                base,
                editor.version,
                &changes,
                None,
                &mut [&mut first, &mut second]
            ),
            ApplyOutcome::Invalidated
        );
        assert!(
            first
                .jump_list(first.active())
                .unwrap()
                .entries()
                .is_empty()
        );
        assert_eq!(
            second.jump_list(second.active()).unwrap().entries(),
            &[unrelated]
        );
    }

    #[test]
    fn missing_versions_and_bad_identity_never_leave_untrusted_marks_usable() {
        use workspace_core::editor_panes::{Layout, ViewState};
        let mut layout = Layout::new(ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let mark = Location {
            buffer: 1,
            row: 3,
            column: 0,
            view: View::default(),
        };
        layout.jump_list_mut(layout.active()).unwrap().record(mark);
        assert_eq!(
            apply_to_layouts(0, 0, 0, &[], None, &mut [&mut layout]),
            ApplyOutcome::Unavailable
        );
        assert_eq!(
            layout.jump_list(layout.active()).unwrap().entries(),
            &[mark]
        );
        assert_eq!(
            apply_to_layouts(1, 0, 1, &[], None, &mut [&mut layout]),
            ApplyOutcome::Invalidated
        );
        assert!(
            layout
                .jump_list(layout.active())
                .unwrap()
                .entries()
                .is_empty()
        );
    }

    #[test]
    fn paragraph_operator_records_jump_before_rebasing_edits_in_its_own_pane() {
        use workspace_core::editor_panes::{Layout, ViewState};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        let case = &fixture["typed_operators"][0]; // d} at the first line.
        let view = ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        };
        let mut first = Layout::new(view.clone());
        let mut second = Layout::new(view);
        for mark in case["before"]["jumps"].as_array().unwrap() {
            let mark = Location {
                buffer: 1,
                row: mark["row"].as_u64().unwrap() as usize - 1,
                column: mark["col"].as_u64().unwrap() as usize,
                view: View::default(),
            };
            first.jump_list_mut(first.active()).unwrap().record(mark);
            second.jump_list_mut(second.active()).unwrap().record(mark);
        }
        let mut editor = Editor::new("a1\na2\n\na4\na5\n\na7\na8".into());
        assert_eq!(editor.enable_jump_changes(), Some(0));
        let captured_view = View {
            top: 0,
            left: 2,
            skip: 0,
            valid: true,
        };
        editor.set_jump_history_view(captured_view);
        let origin = Origin {
            layout_index: 0,
            focus: first.focus_guard(),
        };
        editor.key("d");
        editor.key("}");
        let changes = editor.take_jump_changes();
        assert!(
            matches!(changes[0].impact, Impact::Jump { row: 0, column: 0, view } if view == captured_view)
        );
        assert!(matches!(changes[1].impact, Impact::Lines { .. }));
        assert_eq!(
            apply_to_layouts(
                1,
                0,
                editor.version,
                &changes,
                Some(&origin),
                &mut [&mut first, &mut second]
            ),
            ApplyOutcome::Applied
        );
        let after = &case["after"];
        let cursor = after["cursor"].as_array().unwrap();
        let current = Location {
            buffer: 1,
            row: cursor[0].as_u64().unwrap() as usize - 1,
            column: cursor[1].as_u64().unwrap() as usize,
            view: View::default(),
        };
        first
            .jump_list_mut(first.active())
            .unwrap()
            .normalize(current);
        let actual: Vec<_> = first
            .jump_list(first.active())
            .unwrap()
            .entries()
            .iter()
            .map(|m| (m.row + 1, m.column))
            .collect();
        let expected: Vec<_> = after["jumps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["row"].as_u64().unwrap() as usize,
                    m["col"].as_u64().unwrap() as usize,
                )
            })
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(
            second.jump_list(second.active()).unwrap().entries().len(),
            2
        );
    }

    #[test]
    fn paragraph_change_events_match_neovim_typed_references() {
        use workspace_core::editor_panes::{Layout, ViewState};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        for case in fixture["typed_changes"].as_array().unwrap() {
            let mut layout = Layout::new(ViewState {
                buffer: 1,
                row: 0,
                column: 0,
                top: 0,
                left: 0,
            });
            for mark in case["before"]["jumps"].as_array().unwrap() {
                layout
                    .jump_list_mut(layout.active())
                    .unwrap()
                    .record(Location {
                        buffer: 1,
                        row: mark["row"].as_u64().unwrap() as usize - 1,
                        column: mark["col"].as_u64().unwrap() as usize,
                        view: View::default(),
                    });
            }
            let mut editor = Editor::new("a1\na2\n\na4\na5\n\na7\na8".into());
            editor.goto(case["row"].as_u64().unwrap() as usize - 1, 0);
            let mut bridge = Bridge::attach(&mut editor, 1).unwrap();
            let origin = Origin {
                layout_index: 0,
                focus: layout.focus_guard(),
            };
            let keys = case["edit"]
                .as_str()
                .unwrap()
                .replace("<Esc>", "\u{1b}")
                .replace("<CR>", "\n");
            for key in keys.chars() {
                match key {
                    '\u{1b}' => {
                        editor.key("<Esc>");
                    }
                    '\n' => {
                        editor.key("<Enter>");
                    }
                    key if editor.mode == crate::editor::Mode::Insert => {
                        editor.insert(&key.to_string());
                    }
                    key => {
                        editor.key(&key.to_string());
                    }
                }
                assert_eq!(
                    bridge.sync(&mut editor, Some(1), Some(&origin), &mut [&mut layout]),
                    ApplyOutcome::Applied,
                    "{}",
                    case["edit"]
                );
            }
            assert!(editor.contents().contains('X'), "{}", case["edit"]);
            if case["edit"].as_str().unwrap().contains('Y') {
                assert!(editor.contents().contains('Y'), "{}", case["edit"]);
            }
            let after = &case["after"];
            let cursor = after["cursor"].as_array().unwrap();
            layout
                .jump_list_mut(layout.active())
                .unwrap()
                .normalize(Location {
                    buffer: 1,
                    row: cursor[0].as_u64().unwrap() as usize - 1,
                    column: cursor[1].as_u64().unwrap() as usize,
                    view: View::default(),
                });
            let actual: Vec<_> = layout
                .jump_list(layout.active())
                .unwrap()
                .entries()
                .iter()
                .map(|mark| (mark.row + 1, mark.column))
                .collect();
            let expected: Vec<_> = after["jumps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|mark| {
                    (
                        mark["row"].as_u64().unwrap() as usize,
                        mark["col"].as_u64().unwrap() as usize,
                    )
                })
                .collect();
            assert_eq!(
                actual, expected,
                "mark {} row {} edit {}",
                case["mark"], case["row"], case["edit"]
            );
        }
    }

    #[test]
    fn failed_paragraph_motion_emits_no_jump_and_yank_keeps_version() {
        let mut editor = Editor::new("one\n\ntwo".into());
        assert_eq!(editor.enable_jump_changes(), Some(0));
        editor.key("9");
        editor.key("}");
        assert!(editor.take_jump_changes().is_empty());
        editor.key("y");
        editor.key("}");
        let events = editor.take_jump_changes();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].impact, Impact::Jump { row: 0, .. }));
        assert_eq!((events[0].before, events[0].after), (0, 0));
    }

    #[test]
    fn stale_focus_guard_never_records_jump_in_another_pane() {
        use workspace_core::editor_panes::{Axis, Layout, ViewState};
        let mut layout = Layout::new(ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let first = layout.active();
        let origin = Origin {
            layout_index: 0,
            focus: layout.focus_guard(),
        };
        let second = layout.split(Axis::Row, true).unwrap();
        let event = Change {
            before: 0,
            after: 0,
            impact: Impact::Jump {
                row: 0,
                column: 0,
                view: View::default(),
            },
        };
        assert_eq!(
            apply_to_layouts(1, 0, 0, &[event], Some(&origin), &mut [&mut layout]),
            ApplyOutcome::Invalidated
        );
        assert!(layout.jump_list(first).unwrap().entries().is_empty());
        assert!(layout.jump_list(second).unwrap().entries().is_empty());
    }

    #[test]
    fn replay_yields_between_jump_and_edit_so_view_can_be_refreshed() {
        let mut recording = Editor::new("one\n\ntwo\nthree\nfour".into());
        for key in ["q", "a", "}", "d", "d", "q"] {
            recording.key(key);
        }
        let mut editor = Editor::with_registers(
            "one\n\ntwo\nthree\nfour".into(),
            recording.registers.clone(),
        );
        assert_eq!(editor.enable_jump_changes(), Some(0));
        editor.key("@");
        editor.key("a");
        assert!(editor.replaying());
        assert_eq!(editor.contents(), "one\n\ntwo\nthree\nfour");
        let first = editor.take_jump_changes();
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0].impact, Impact::Jump { row: 0, .. }));
        editor.advance_replay();
        let second = editor.take_jump_changes();
        assert_eq!(second.len(), 1);
        assert!(matches!(second[0].impact, Impact::Lines { .. }));
        assert_eq!(editor.contents(), "one\ntwo\nthree\nfour");
        while editor.replaying() {
            editor.advance_replay();
        }
    }

    #[test]
    fn bridge_records_actual_motion_and_rejects_a_replaced_editor() {
        use workspace_core::editor_panes::{Layout, ViewState};
        let mut layout = Layout::new(ViewState {
            buffer: 7,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let mut editor = Editor::new("one\n\ntwo".into());
        assert!(Bridge::attach(&mut editor, 0).is_none());
        let mut bridge = Bridge::attach(&mut editor, 7).unwrap();
        let origin = Origin {
            layout_index: 0,
            focus: layout.focus_guard(),
        };
        editor.set_jump_history_view(View {
            valid: true,
            ..View::default()
        });
        editor.key("}");
        assert_eq!(
            bridge.sync(&mut editor, Some(7), Some(&origin), &mut [&mut layout]),
            ApplyOutcome::Applied
        );
        assert_eq!(
            layout.jump_list(layout.active()).unwrap().entries()[0].row,
            0
        );
        let mut replacement = Editor::new("new buffer".into());
        assert_eq!(
            bridge.sync(&mut replacement, Some(7), Some(&origin), &mut [&mut layout]),
            ApplyOutcome::Unavailable
        );
        assert!(
            layout
                .jump_list(layout.active())
                .unwrap()
                .entries()
                .is_empty()
        );
        let fresh = Location {
            buffer: 7,
            row: 5,
            column: 0,
            view: View::default(),
        };
        layout.jump_list_mut(layout.active()).unwrap().record(fresh);
        assert_eq!(
            bridge.sync(&mut editor, Some(7), Some(&origin), &mut [&mut layout]),
            ApplyOutcome::Unavailable
        );
        assert_eq!(
            layout.jump_list(layout.active()).unwrap().entries(),
            &[fresh]
        );
    }

    #[test]
    fn bracket_percentage_and_named_marks_emit_the_prejump_location() {
        let mut editor = Editor::new("(\ninside\n)\nlast".into());
        assert_eq!(editor.enable_jump_changes(), Some(0));
        editor.key("%");
        assert_eq!(editor.position().0, 2);
        let events = editor.take_jump_changes();
        assert!(matches!(
            events.as_slice(),
            [Change {
                impact: Impact::Jump { row: 0, .. },
                ..
            }]
        ));
        editor.key("m");
        editor.key("a");
        editor.key("G");
        assert_eq!(editor.position().0, 3);
        assert!(matches!(
            editor.take_jump_changes()[0].impact,
            Impact::Jump { row: 2, .. }
        ));
        editor.key("'");
        editor.key("a");
        assert_eq!(editor.position().0, 2);
        assert!(matches!(
            editor.take_jump_changes()[0].impact,
            Impact::Jump { row: 3, .. }
        ));
        editor.key("5");
        editor.key("0");
        editor.key("%");
        assert!(matches!(
            editor.take_jump_changes()[0].impact,
            Impact::Jump { row: 2, .. }
        ));
    }
}
