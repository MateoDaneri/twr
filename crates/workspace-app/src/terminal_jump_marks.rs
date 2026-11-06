//! Bounded opaque marks for terminal jump histories. No terminal text is stored
//! by this container; the owner supplies and validates the actual mark type.
use std::collections::{HashMap, HashSet};
use workspace_core::editor_jumplist::{AnchorId, Location};

const MAX_MARKS: usize = 1024;

struct Entry<T> {
    workspace: usize,
    location: Location,
    value: T,
}

/// Identities are never reused, even after pruning. A caller must pass every
/// visible/parked history and outstanding async request to `retain_reachable`.
pub struct MarkBank<T> {
    next: Option<AnchorId>,
    entries: HashMap<AnchorId, Entry<T>>,
}

impl<T> Default for MarkBank<T> {
    fn default() -> Self {
        Self {
            next: AnchorId::new(1),
            entries: HashMap::new(),
        }
    }
}

impl<T> MarkBank<T> {
    /// Returns the original value on capacity or identity exhaustion.
    pub fn insert(
        &mut self,
        workspace: usize,
        location: Location,
        value: T,
    ) -> Result<AnchorId, T> {
        if self.entries.len() >= MAX_MARKS {
            return Err(value);
        }
        let Some(id) = self.next else {
            return Err(value);
        };
        self.next = id.get().checked_add(1).and_then(AnchorId::new);
        self.entries.insert(
            id,
            Entry {
                workspace,
                location,
                value,
            },
        );
        Ok(id)
    }

    /// A token alone never authorizes a workspace or a different location.
    pub fn get(&self, id: AnchorId, workspace: usize, location: Location) -> Option<&T> {
        let entry = self.entries.get(&id)?;
        (entry.workspace == workspace && entry.location == location).then_some(&entry.value)
    }

    pub fn retain_reachable(&mut self, anchors: impl IntoIterator<Item = AnchorId>) {
        let mut keep = HashSet::new();
        for id in anchors {
            if self.entries.contains_key(&id) {
                keep.insert(id);
            }
        }
        self.entries.retain(|id, _| keep.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use workspace_core::{
        editor_jumplist::{JumpList, View},
        editor_panes::{Axis, Layout, ViewState},
    };

    fn location(buffer: u64, row: usize) -> Location {
        Location {
            buffer,
            row,
            column: 1,
            view: View::default(),
        }
    }

    #[test]
    fn tokens_are_scoped_non_reused_and_preserved_when_panes_split() {
        let mut bank = MarkBank::default();
        let original = location(2, 3);
        let first = bank.insert(7, original, "first").unwrap();
        let second = bank.insert(7, original, "second").unwrap();
        assert_ne!(first, second);
        assert_eq!(bank.get(first, 7, original), Some(&"first"));
        assert_eq!(bank.get(first, 8, original), None);
        assert_eq!(bank.get(first, 7, location(2, 4)), None);
        let mut layout = Layout::new(ViewState {
            buffer: 1,
            row: 0,
            column: 0,
            top: 0,
            left: 0,
        });
        let left = layout.active();
        let guard = layout.focus_guard();
        assert!(layout.record_jump_guarded_with_anchor(&guard, original, Some(first)));
        let right = layout.split(Axis::Row, true).unwrap();
        layout.jump_list_mut(left).unwrap().clear();
        bank.retain_reachable(layout.panes().keys().flat_map(|pane| {
            layout
                .jump_list(*pane)
                .unwrap()
                .anchors()
                .iter()
                .flatten()
                .copied()
        }));
        assert_eq!(bank.get(first, 7, original), Some(&"first"));
        assert_eq!(bank.get(second, 7, original), None);
        layout.jump_list_mut(right).unwrap().clear();
        bank.retain_reachable(std::iter::empty());
        assert_eq!(bank.get(first, 7, original), None);
        let third = bank.insert(7, original, "third").unwrap();
        assert!(third > second);
    }

    #[test]
    fn capacity_and_identifier_exhaustion_fail_without_overwrite() {
        let mut bank = MarkBank::default();
        let pos = location(9, 3);
        let first = bank.insert(0, pos, 1).unwrap();
        for n in 1..MAX_MARKS {
            bank.insert(0, location(9, n), n).unwrap();
        }
        assert_eq!(bank.insert(0, pos, 9999), Err(9999));
        assert_eq!(bank.get(first, 0, pos), Some(&1));
        bank.retain_reachable(std::iter::once(first));
        bank.next = AnchorId::new(u64::MAX);
        let last = bank.insert(0, pos, 3).unwrap();
        assert_eq!(last.get(), u64::MAX);
        assert_eq!(bank.insert(0, pos, 4), Err(4));
        assert_eq!(bank.get(first, 0, pos), Some(&1));
    }

    #[test]
    fn pending_tokens_can_stay_reachable_before_history_commit() {
        let mut bank = MarkBank::default();
        let pos = location(2, 1);
        let pending = bank.insert(0, pos, "pending").unwrap();
        let mut history = JumpList::default();
        bank.retain_reachable(history.anchors().iter().flatten().copied().chain([pending]));
        history.record_with_anchor(pos, Some(pending));
        assert_eq!(bank.get(pending, 0, pos), Some(&"pending"));
    }
}
