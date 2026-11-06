//! Window-local jump history. This model never reads files or opens buffers.
//!
//! Buffer numbers must be supplied by an authoritative, non-reused catalog.
//! The caller owns permission checks, editing marks and asynchronous loading.

const MAX_JUMPS: usize = 100;
/// Opaque, nonzero identity owned by the application. The core never opens
/// or interprets the corresponding external resource.
pub type AnchorId = std::num::NonZeroU64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub top: usize,
    pub left: usize,
    pub skip: usize,
    /// False means the saved top cannot safely be restored.
    pub valid: bool,
}

impl View {
    /// Neovim saves a top-line offset and skip column, but not leftcol.
    /// The caller must additionally validate skip against folds and line width.
    pub fn restore_over(self, current: View) -> Option<View> {
        self.valid.then_some(View {
            left: current.left,
            ..self
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub buffer: u64,
    pub row: usize,
    pub column: usize,
    pub view: View,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JumpList {
    entries: Vec<Location>,
    anchors: Vec<Option<AnchorId>>,
    index: usize,
    generation: u64,
}

/// A caller must verify the pane, buffer identity, permissions and loading
/// outcome before committing. A stale history or changed origin rejects it.
#[derive(Clone, Debug)]
pub struct PendingJump {
    generation: u64,
    origin: Location,
    next: JumpList,
    target: Option<Location>,
    target_anchor: Option<AnchorId>,
}

impl PendingJump {
    pub fn target(&self) -> Option<Location> {
        self.target
    }
    pub fn target_anchor(&self) -> Option<AnchorId> {
        self.target_anchor
    }
}

impl JumpList {
    fn push(&mut self, location: Location, anchor: Option<AnchorId>) {
        if self.entries.len() == MAX_JUMPS {
            self.entries.remove(0);
            self.anchors.remove(0);
        }
        self.entries.push(location);
        self.anchors.push(anchor);
        self.index = self.entries.len();
    }

    /// Record an actual jump's origin after the owning operation succeeds.
    pub fn record(&mut self, origin: Location) {
        self.record_with_anchor(origin, None);
    }

    pub fn record_with_anchor(&mut self, origin: Location, anchor: Option<AnchorId>) {
        self.push(origin, anchor);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Neovim deduplicates by buffer and line, keeping the newest occurrence.
    /// Its public getjumplist operation also removes the current-line tail.
    pub fn normalize(&mut self, current: Location) {
        let mut kept = Vec::with_capacity(self.entries.len());
        let mut kept_anchors = Vec::with_capacity(self.anchors.len());
        let mut index = 0;
        for (i, entry) in self.entries.iter().enumerate() {
            let duplicate = self.entries[i + 1..]
                .iter()
                .any(|later| later.buffer == entry.buffer && later.row == entry.row);
            if !duplicate {
                if i < self.index {
                    index += 1;
                }
                kept.push(*entry);
                kept_anchors.push(self.anchors[i]);
            }
        }
        if index == kept.len()
            && kept
                .last()
                .is_some_and(|last| last.buffer == current.buffer && last.row == current.row)
        {
            kept.pop();
            kept_anchors.pop();
            index -= 1;
        }
        if kept != self.entries || index != self.index {
            self.entries = kept;
            self.anchors = kept_anchors;
            self.index = index;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    pub fn entries(&self) -> &[Location] {
        &self.entries
    }
    pub fn anchors(&self) -> &[Option<AnchorId>] {
        &self.anchors
    }

    pub fn index(&self) -> usize {
        self.index
    }

    /// A catalog reset invalidates even previously prepared empty-list plans.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.anchors.clear();
        self.index = 0;
        self.generation = self.generation.wrapping_add(1);
    }

    /// A wiped identity must never be rebound to a later buffer with a
    /// similar name. Unloading is different and is handled by the caller.
    pub fn forget_buffer(&mut self, buffer: u64) {
        let index = self
            .entries
            .iter()
            .take(self.index)
            .filter(|entry| entry.buffer != buffer)
            .count();
        let previous = self.entries.len();
        let mut index_in_old = 0;
        self.anchors.retain(|_| {
            let keep = self.entries[index_in_old].buffer != buffer;
            index_in_old += 1;
            keep
        });
        self.entries.retain(|entry| entry.buffer != buffer);
        if self.entries.len() != previous {
            self.index = index;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// Update marks for a known line replacement. Rows inside the removed
    /// range move to its beginning, even if that row is now beyond EOF; the
    /// editor must clamp after resolving the target buffer. Neovim stores a
    /// top-line offset from the mark rather than a second independent mark.
    pub fn adjust_lines(
        &mut self,
        buffer: u64,
        start: usize,
        removed: usize,
        added: usize,
    ) -> bool {
        let Some(end) = start.checked_add(removed) else {
            return false;
        };
        let mut next = self.entries.clone();
        let mut touched = false;
        for entry in &mut next {
            if entry.buffer != buffer {
                continue;
            }
            touched = true;
            let top_offset = entry.row.checked_sub(entry.view.top);
            if entry.row >= end {
                let Some(row) = entry
                    .row
                    .checked_sub(removed)
                    .and_then(|row| row.checked_add(added))
                else {
                    return false;
                };
                entry.row = row;
            } else if entry.row >= start {
                entry.row = start;
            }
            if entry.view.valid {
                if let Some(top) = top_offset.and_then(|offset| entry.row.checked_sub(offset)) {
                    entry.view.top = top;
                } else {
                    entry.view.valid = false;
                }
            }
        }
        if touched {
            self.entries = next;
            self.generation = self.generation.wrapping_add(1);
        }
        true
    }

    /// Compute a navigation without changing live history. A missing target,
    /// failed file load or outdated origin can then be rejected atomically.
    pub fn prepare(&self, current: Location, older: bool, count: usize) -> Option<PendingJump> {
        self.prepare_with_anchor(current, None, older, count)
    }

    pub fn prepare_with_anchor(
        &self,
        current: Location,
        current_anchor: Option<AnchorId>,
        older: bool,
        count: usize,
    ) -> Option<PendingJump> {
        let count = count.max(1);
        let mut next = self.clone();
        next.normalize(current);
        if next.entries.is_empty() {
            return (next != *self).then_some(PendingJump {
                generation: self.generation,
                origin: current,
                next,
                target: None,
                target_anchor: None,
            });
        }
        let candidate = if older {
            next.index.checked_sub(count)
        } else {
            next.index.checked_add(count)
        };
        if candidate.is_none_or(|index| index >= next.entries.len()) {
            // Even a failed move may have cleaned duplicates in Neovim.
            return (next != *self).then_some(PendingJump {
                generation: self.generation,
                origin: current,
                next,
                target: None,
                target_anchor: None,
            });
        }
        if next.index == next.entries.len() {
            next.push(current, current_anchor);
            next.index -= 1;
        }
        let destination = if older {
            next.index.checked_sub(count)
        } else {
            next.index.checked_add(count)
        };
        let target = destination.and_then(|index| next.entries.get(index).copied());
        let target_anchor = destination
            .and_then(|index| next.anchors.get(index).copied())
            .flatten();
        if let Some(index) = destination.filter(|_| target.is_some()) {
            next.index = index;
        }
        Some(PendingJump {
            generation: self.generation,
            origin: current,
            next,
            target,
            target_anchor,
        })
    }

    /// Commit only after a caller has safely resolved and selected the target.
    pub fn commit(&mut self, pending: PendingJump, current: Location) -> bool {
        if self.generation != pending.generation || current != pending.origin {
            return false;
        }
        *self = pending.next;
        self.generation = self.generation.wrapping_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_anchors_follow_entries_through_normalize_forget_and_eviction() {
        let anchor = |id| Some(AnchorId::new(id).unwrap());
        let terminal = Location {
            buffer: 1,
            row: 2,
            column: 1,
            view: View::default(),
        };
        let file = Location {
            buffer: 2,
            row: 3,
            column: 0,
            view: View::default(),
        };
        let current = Location {
            buffer: 3,
            row: 4,
            column: 0,
            view: View::default(),
        };
        let mut history = JumpList::default();
        history.record_with_anchor(terminal, anchor(1));
        history.record(file);
        history.record_with_anchor(terminal, anchor(2));
        history.normalize(current);
        assert_eq!(history.entries(), &[file, terminal]);
        assert_eq!(history.anchors(), &[None, anchor(2)]);
        let pending = history.prepare(current, true, 1).unwrap();
        assert_eq!(pending.target(), Some(terminal));
        assert_eq!(pending.target_anchor(), anchor(2));
        assert!(history.commit(pending, current));
        history.forget_buffer(2);
        assert_eq!(history.entries().len(), history.anchors().len());
        assert!(!history.anchors().contains(&anchor(1)));
        assert!(history.anchors().contains(&anchor(2)));
        history.clear();
        for n in 1..=101 {
            history.record_with_anchor(
                Location {
                    row: n as usize,
                    ..terminal
                },
                anchor(n),
            );
        }
        assert_eq!(history.entries().len(), 100);
        assert_eq!(history.anchors().len(), 100);
        assert_eq!(history.anchors()[0], anchor(2));
        assert_eq!(history.anchors()[99], anchor(101));
    }

    #[test]
    fn terminal_origin_anchor_survives_backward_and_forward_navigation() {
        let file = Location {
            buffer: 1,
            row: 1,
            column: 0,
            view: View::default(),
        };
        let terminal = Location {
            buffer: 2,
            row: 8,
            column: 4,
            view: View::default(),
        };
        let token = AnchorId::new(7).unwrap();
        let mut history = JumpList::default();
        history.record(file);
        let back = history
            .prepare_with_anchor(terminal, Some(token), true, 1)
            .unwrap();
        assert_eq!(back.target(), Some(file));
        assert_eq!(back.target_anchor(), None);
        assert!(history.commit(back, terminal));
        let forward = history.prepare(file, false, 1).unwrap();
        assert_eq!(forward.target(), Some(terminal));
        assert_eq!(forward.target_anchor(), Some(token));
        assert!(history.commit(forward, file));
    }

    #[test]
    fn split_clones_anchors_and_later_changes_are_window_local() {
        use crate::editor_panes::{Axis, Layout, ViewState};
        let mut layout = Layout::new(ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let initial = layout.active();
        let terminal = Location {
            buffer: 2,
            row: 8,
            column: 4,
            view: View::default(),
        };
        let current = Location {
            buffer: 1,
            row: 0,
            column: 0,
            view: View::default(),
        };
        let id = AnchorId::new(42).unwrap();
        let guard = layout.focus_guard();
        assert!(layout.record_jump_guarded_with_anchor(&guard, terminal, Some(id)));
        let split = layout.split(Axis::Row, true).unwrap();
        let pending = layout.prepare_jump(current, true, 1).unwrap();
        assert_eq!(pending.target(), Some(terminal));
        assert_eq!(pending.target_anchor(), Some(id));
        assert!(layout.commit_jump(pending, current));
        assert_eq!(layout.jump_list(initial).unwrap().anchors(), &[Some(id)]);
        layout.jump_list_mut(split).unwrap().clear();
        assert!(layout.jump_list(split).unwrap().anchors().is_empty());
        assert_eq!(layout.jump_list(initial).unwrap().anchors(), &[Some(id)]);
        assert!(!layout.record_jump_guarded_with_anchor(&guard, terminal, Some(id)));
    }

    #[test]
    fn reset_rejects_plans_prepared_before_the_catalog_was_cleared() {
        let mut history = JumpList::default();
        let first = Location {
            buffer: 1,
            row: 0,
            column: 0,
            view: View::default(),
        };
        let current = Location {
            buffer: 1,
            row: 3,
            column: 0,
            view: View::default(),
        };
        history.record(first);
        let pending = history.prepare(current, true, 1).unwrap();
        history.clear();
        history.record(first);
        assert!(!history.commit(pending, current));
        assert_eq!(history.entries(), &[first]);
    }

    fn location(state: &serde_json::Value) -> Location {
        let cursor = state["cursor"].as_array().unwrap();
        let view = state["view"].as_array().unwrap();
        Location {
            buffer: match state["buffer"].as_str().unwrap() {
                "alpha.txt" => 1,
                "beta.txt" => 2,
                "long.txt" => 3,
                name => panic!("unexpected fixture buffer {name}"),
            },
            row: cursor[0].as_u64().unwrap() as usize - 1,
            column: cursor[1].as_u64().unwrap() as usize,
            view: View {
                top: view[0].as_u64().unwrap() as usize - 1,
                left: view[1].as_u64().unwrap() as usize,
                skip: view[2].as_u64().unwrap() as usize,
                valid: true,
            },
        }
    }

    #[test]
    fn per_window_cross_buffer_history_replays_neovim() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        assert_eq!(fixture["jumpoptions"], "view");
        let mut failures = Vec::new();
        for (case, states) in fixture["scenarios"].as_array().unwrap().iter().enumerate() {
            let states = states.as_array().unwrap();
            let mut histories = [JumpList::default(), JumpList::default()];
            let mut window = 0;
            let mut before = location(&states[0]);
            for state in &states[1..] {
                let action = state["action"].as_str().unwrap();
                let current = location(state);
                match action {
                    "split" => {
                        histories[1] = histories[0].clone();
                        window = 1;
                    }
                    "original focus" => window = 0,
                    "split focus" => window = 1,
                    "}" | "G" | "gg" | "edit B" | "edit A" => histories[window].record(before),
                    _ if action.contains("<C-o>")
                        || action.contains("Ctrl-o")
                        || action.contains("<C-i>")
                        || action.contains("Ctrl-i") =>
                    {
                        let older = action.contains("<C-o>") || action.contains("Ctrl-o");
                        let count = action
                            .chars()
                            .next()
                            .and_then(|c| c.to_digit(10))
                            .unwrap_or(1) as usize;
                        if let Some(plan) = histories[window].prepare(before, older, count) {
                            if plan.target().is_some_and(|target| {
                                target.buffer != current.buffer
                                    || target.row != current.row
                                    || target.column != current.column
                            }) || (plan.target().is_none() && before != current)
                            {
                                failures.push(format!(
                                    "case {case} {action} target {:?}/{current:?}",
                                    plan.target()
                                ));
                            }
                            assert!(histories[window].commit(plan, before));
                        } else if (before.buffer, before.row, before.column)
                            != (current.buffer, current.row, current.column)
                        {
                            failures.push(format!(
                                "case {case} {action} missing target {before:?}/{current:?}"
                            ));
                        }
                    }
                    _ => panic!("unexpected {action}"),
                }
                histories[window].normalize(current);
                let actual: Vec<_> = histories[window]
                    .entries()
                    .iter()
                    .map(|entry| (entry.buffer, entry.row + 1, entry.column))
                    .collect();
                let expected: Vec<_> = state["jumps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| {
                        (
                            if entry["buffer"] == "alpha.txt" { 1 } else { 2 },
                            entry["row"].as_u64().unwrap() as usize,
                            entry["col"].as_u64().unwrap() as usize,
                        )
                    })
                    .collect();
                if actual != expected
                    || histories[window].index() != state["index"].as_u64().unwrap() as usize
                {
                    failures.push(format!(
                        "case {case} {action}: {actual:?}/{expected:?}, index {}/{}",
                        histories[window].index(),
                        state["index"]
                    ));
                }
                before = current;
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn navigation_without_intermediate_jumplist_queries_matches_neovim() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        let mut failures = Vec::new();
        for (case, scenario) in fixture["unobserved"].as_array().unwrap().iter().enumerate() {
            let trace = scenario["trace"].as_array().unwrap();
            let mut history = JumpList::default();
            let mut before = location(&trace[0]);
            for state in &trace[1..] {
                let action = state["action"].as_str().unwrap();
                let current = location(state);
                if action.contains("<C-o>") || action.contains("<C-i>") {
                    let older = action.contains("<C-o>");
                    let count = action
                        .chars()
                        .next()
                        .and_then(|c| c.to_digit(10))
                        .unwrap_or(1) as usize;
                    if let Some(plan) = history.prepare(before, older, count) {
                        if plan.target().is_some_and(|target| {
                            (target.buffer, target.row, target.column)
                                != (current.buffer, current.row, current.column)
                        }) || (plan.target().is_none() && before != current)
                        {
                            failures.push(format!(
                                "case {case} {action} target {:?}/{current:?}",
                                plan.target()
                            ));
                        }
                        assert!(history.commit(plan, before));
                    } else if (before.buffer, before.row, before.column)
                        != (current.buffer, current.row, current.column)
                    {
                        failures.push(format!("case {case} {action} missing target"));
                    }
                } else {
                    history.record(before);
                }
                before = current;
            }
            history.normalize(before);
            let actual: Vec<_> = history
                .entries()
                .iter()
                .map(|entry| (entry.buffer, entry.row + 1, entry.column))
                .collect();
            let expected: Vec<_> = scenario["final"]["jumps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    (
                        if entry["buffer"] == "alpha.txt" { 1 } else { 2 },
                        entry["row"].as_u64().unwrap() as usize,
                        entry["col"].as_u64().unwrap() as usize,
                    )
                })
                .collect();
            if actual != expected
                || history.index() != scenario["final"]["index"].as_u64().unwrap() as usize
            {
                failures.push(format!(
                    "case {case}: {actual:?}/{expected:?}, index {}/{}",
                    history.index(),
                    scenario["final"]["index"]
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn stale_plan_and_changed_origin_never_move_history() {
        let a = Location {
            buffer: 1,
            row: 0,
            column: 0,
            view: View::default(),
        };
        let b = Location { row: 3, ..a };
        let mut history = JumpList::default();
        history.record(a);
        let plan = history.prepare(b, true, 1).unwrap();
        let unchanged = history.clone();
        assert!(!history.commit(plan.clone(), Location { column: 1, ..b }));
        assert_eq!(history, unchanged);
        history.record(b);
        let changed = history.clone();
        assert!(!history.commit(plan, b));
        assert_eq!(history, changed);
    }

    #[test]
    fn split_copies_then_isolates_history_and_close_discards_only_its_view() {
        use crate::editor_panes::{Axis, Error, Layout, ViewState};
        let view = ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        };
        let mut layout = Layout::new(view);
        let first = layout.active();
        let a = Location {
            buffer: 1,
            row: 0,
            column: 0,
            view: View::default(),
        };
        let b = Location { row: 3, ..a };
        layout.jump_list_mut(first).unwrap().record(a);
        let second = layout.split(Axis::Row, true).unwrap();
        assert_eq!(layout.jump_list(first), layout.jump_list(second));
        layout.jump_list_mut(second).unwrap().record(b);
        assert_ne!(layout.jump_list(first), layout.jump_list(second));
        let first_history = layout.jump_list(first).unwrap().clone();
        layout.close().unwrap();
        assert_eq!(layout.jump_list(second), Err(Error::UnknownPane));
        assert_eq!(layout.jump_list(first), Ok(&first_history));
    }

    #[test]
    fn pending_jump_rejects_focus_round_trip_and_other_panes() {
        use crate::editor_panes::{Axis, Layout, ViewState};
        let mut layout = Layout::new(ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let first = layout.active();
        let a = Location {
            buffer: 1,
            row: 0,
            column: 0,
            view: View::default(),
        };
        let b = Location { row: 3, ..a };
        layout.jump_list_mut(first).unwrap().record(a);
        let second = layout.split(Axis::Row, true).unwrap();
        layout.focus(first).unwrap();
        let plan = layout.prepare_jump(b, true, 1).unwrap();
        assert_eq!(plan.target(), Some(a));
        layout.focus(second).unwrap();
        assert!(!layout.commit_jump(plan.clone(), b));
        layout.focus(first).unwrap();
        assert!(!layout.commit_jump(plan, b));
        let fresh = layout.prepare_jump(b, true, 1).unwrap();
        assert!(layout.commit_jump(fresh, b));
    }

    #[test]
    fn pending_jump_cannot_cross_layouts_with_identical_pane_ids() {
        use crate::editor_panes::{Layout, ViewState};
        let view = ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        };
        let mut first = Layout::new(view.clone());
        let mut other = Layout::new(view);
        let a = Location {
            buffer: 1,
            row: 0,
            column: 0,
            view: View::default(),
        };
        let b = Location { row: 3, ..a };
        first.jump_list_mut(first.active()).unwrap().record(a);
        other.jump_list_mut(other.active()).unwrap().record(a);
        let pending = first.prepare_jump(b, true, 1).unwrap();
        assert!(!other.commit_jump(pending.clone(), b));
        let mut staging_copy = first.clone();
        assert!(staging_copy.commit_jump(pending.clone(), b));
        assert!(first.commit_jump(pending, b));
    }

    #[test]
    fn edit_updates_both_pane_histories_or_neither() {
        use crate::editor_panes::{Axis, Layout, ViewState};
        let mut layout = Layout::new(ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let first = layout.active();
        let mark = Location {
            buffer: 1,
            row: 6,
            column: 2,
            view: View {
                valid: true,
                ..View::default()
            },
        };
        layout.jump_list_mut(first).unwrap().record(mark);
        let second = layout.split(Axis::Row, true).unwrap();
        assert!(!layout.adjust_jump_lines(1, usize::MAX, 1, 0));
        assert_eq!(layout.jump_list(first).unwrap().entries()[0], mark);
        assert_eq!(layout.jump_list(second).unwrap().entries()[0], mark);
        assert!(layout.adjust_jump_lines(1, 2, 0, 3));
        for pane in [first, second] {
            let updated = layout.jump_list(pane).unwrap().entries()[0];
            assert_eq!(updated.row, 9);
            assert!(updated.view.valid);
            assert_eq!(updated.view.top, 3);
        }
    }

    #[test]
    fn edited_line_marks_follow_neovim_and_invalidate_stale_plans() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        for case in fixture["edits"].as_array().unwrap() {
            let mut history = JumpList::default();
            let before = location(&case["before"]);
            for mark in case["before"]["jumps"].as_array().unwrap() {
                history.record(Location {
                    buffer: 1,
                    row: mark["row"].as_u64().unwrap() as usize - 1,
                    column: mark["col"].as_u64().unwrap() as usize,
                    view: before.view,
                });
            }
            let (start, removed, added) = match case["edit"].as_str().unwrap() {
                "insert before" => (0, 0, 2),
                "delete before" => (1, 2, 0),
                "delete marked" | "delete marked column" => (2, 3, 0),
                "delete to EOF" => (2, 6, 0),
                "replace marked" => (2, 3, 1),
                "replace same count" => (2, 3, 3),
                "replace more" => (2, 3, 4),
                other => panic!("unexpected edit {other}"),
            };
            let pending = history.prepare(before, true, 1).unwrap();
            assert!(history.adjust_lines(1, start, removed, added));
            assert!(
                !history.commit(pending, before),
                "stale plan {}",
                case["edit"]
            );
            history.normalize(location(&case["after"]));
            let actual: Vec<_> = history
                .entries()
                .iter()
                .map(|entry| (entry.row + 1, entry.column))
                .collect();
            let expected: Vec<_> = case["after"]["jumps"]
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
            assert_eq!(actual, expected, "{}", case["edit"]);
            assert_eq!(
                history.index(),
                case["after"]["index"].as_u64().unwrap() as usize
            );
        }
    }

    #[test]
    fn line_adjustments_are_atomic_and_do_not_touch_other_buffers() {
        let mut history = JumpList::default();
        let first = Location {
            buffer: 1,
            row: 5,
            column: 2,
            view: View {
                valid: true,
                ..View::default()
            },
        };
        let other = Location {
            buffer: 2,
            row: 2,
            ..first
        };
        history.record(first);
        history.record(other);
        let original = history.clone();
        assert!(!history.adjust_lines(1, usize::MAX, 1, 0));
        assert_eq!(history, original);
        assert!(history.adjust_lines(1, 1, 2, 0));
        assert_eq!(history.entries()[0].row, 3);
        assert!(!history.entries()[0].view.valid);
        assert_eq!(history.entries()[1], other);
    }

    #[test]
    fn typed_line_changes_match_neovim_mark_gravity() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        for case in fixture["typed_lines"].as_array().unwrap() {
            let row = case["row"].as_u64().unwrap() as usize;
            let key = case["edit"].as_str().unwrap();
            let (start, removed, added) = match (row, key) {
                (3, "o<Esc>") | (4, "O<Esc>") => (3, 0, 1),
                (4, "0i<CR><Esc>") => (4, 0, 1),
                (4, "dd") => (3, 1, 0),
                (3, "2dd") => (2, 2, 0),
                (2, "dd") => (1, 1, 0),
                other => panic!("unexpected typed change {other:?}"),
            };
            let mut history = JumpList::default();
            for mark in case["before"]["jumps"].as_array().unwrap() {
                history.record(Location {
                    buffer: 1,
                    row: mark["row"].as_u64().unwrap() as usize - 1,
                    column: mark["col"].as_u64().unwrap() as usize,
                    view: View::default(),
                });
            }
            assert!(history.adjust_lines(1, start, removed, added));
            history.normalize(location(&case["after"]));
            let actual: Vec<_> = history
                .entries()
                .iter()
                .map(|mark| (mark.row + 1, mark.column))
                .collect();
            let expected: Vec<_> = case["after"]["jumps"]
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
            assert_eq!(actual, expected, "row {row} {key}");
        }
    }

    #[test]
    fn same_line_change_invalidates_pending_plan_without_moving_marks() {
        let a = Location {
            buffer: 1,
            row: 3,
            column: 1,
            view: View::default(),
        };
        let current = Location { row: 6, ..a };
        let mut history = JumpList::default();
        history.record(a);
        let pending = history.prepare(current, true, 1).unwrap();
        assert!(history.adjust_lines(1, 3, 0, 0));
        assert_eq!(history.entries(), &[a]);
        assert!(!history.commit(pending, current));
    }

    #[test]
    fn full_history_keeps_neovim_failed_count_side_effect() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        let states = fixture["capacity"].as_array().unwrap();
        let mut history = JumpList::default();
        let base = Location {
            buffer: 3,
            row: 0,
            column: 0,
            view: View::default(),
        };
        for row in 0..111 {
            history.record(Location { row, ..base });
        }
        let mut current = Location { row: 111, ..base };
        history.normalize(current);
        for (i, state) in states.iter().enumerate() {
            if i > 0 {
                let count = if i == 1 { 100 } else { 99 };
                let pending = history.prepare(current, true, count).unwrap();
                let target = pending.target();
                assert!(history.commit(pending, current));
                if let Some(target) = target {
                    current = target;
                }
            }
            history.normalize(current);
            let actual: Vec<_> = history.entries().iter().map(|mark| mark.row + 1).collect();
            let expected: Vec<_> = state["jumps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|mark| mark["row"].as_u64().unwrap() as usize)
                .collect();
            assert_eq!(actual, expected, "{}", state["action"]);
            assert_eq!(history.index(), state["index"].as_u64().unwrap() as usize);
            assert_eq!(
                current.row + 1,
                state["cursor"][0].as_u64().unwrap() as usize
            );
        }
    }

    #[test]
    fn jumpoptions_view_restores_the_captured_view_for_unchanged_buffers() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        let states = fixture["views"].as_array().unwrap();
        let mut history = JumpList::default();
        let mut before = location(&states[0]);
        for state in &states[1..] {
            let action = state["action"].as_str().unwrap();
            let current = location(state);
            match action {
                "30G" | "70G" => history.record(before),
                "<C-o>" | "<C-i>" => {
                    let pending = history.prepare(before, action == "<C-o>", 1).unwrap();
                    assert_eq!(pending.target(), Some(current), "{action}");
                    assert!(history.commit(pending, before));
                }
                "zt" => (),
                _ => panic!("unexpected {action}"),
            }
            history.normalize(current);
            before = current;
        }
    }

    #[test]
    fn inserted_lines_preserve_neovim_view_offset_and_current_horizontal_scroll() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        let mark = location(&fixture["views"][2]); // 30G followed by zt.
        let current = location(&fixture["view_edits"][1]);
        let expected = location(&fixture["view_edits"][2]);
        let mut history = JumpList::default();
        history.record(mark);
        assert!(history.adjust_lines(3, 0, 0, 2));
        let plan = history.prepare(current, true, 1).unwrap();
        let target = plan.target().unwrap();
        assert_eq!(
            (target.buffer, target.row, target.column),
            (expected.buffer, expected.row, expected.column)
        );
        assert_eq!(target.view.restore_over(current.view), Some(expected.view));
        let horizontal = View {
            left: 17,
            ..current.view
        };
        assert_eq!(target.view.restore_over(horizontal).unwrap().left, 17);
    }

    #[test]
    fn wiped_buffer_is_removed_without_retargeting_its_old_marks() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-windows.json"
        ))
        .unwrap();
        let states = fixture["removed"].as_array().unwrap();
        let before = &states[0];
        let mut history = JumpList::default();
        for mark in before["jumps"].as_array().unwrap() {
            history.record(Location {
                buffer: if mark["buffer"] == "alpha.txt" { 1 } else { 2 },
                row: mark["row"].as_u64().unwrap() as usize - 1,
                column: mark["col"].as_u64().unwrap() as usize,
                view: View::default(),
            });
        }
        let current = location(&states[1]);
        let pending = history.prepare(current, true, 1).unwrap();
        history.forget_buffer(2);
        assert!(!history.commit(pending, current));
        history.normalize(current);
        let rows: Vec<_> = history
            .entries()
            .iter()
            .map(|entry| (entry.buffer, entry.row + 1))
            .collect();
        let expected: Vec<_> = states[1]["jumps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|mark| {
                (
                    if mark["buffer"] == "alpha.txt" { 1 } else { 2 },
                    mark["row"].as_u64().unwrap() as usize,
                )
            })
            .collect();
        assert_eq!(rows, expected);
        assert_eq!(
            history.index(),
            states[1]["index"].as_u64().unwrap() as usize
        );
    }
}
