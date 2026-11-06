//! Headless editor window model. No files, processes, UI or buffer mutation.
//! A split shares a buffer identity and clones view state; subsequent view
//! changes belong to that pane. The GUI/buffer bank adapter is still pending.
use crate::editor_jumplist::{AnchorId, JumpList, Location as JumpLocation, PendingJump};
use std::{collections::BTreeMap, fmt, sync::Arc};
#[path = "editor_pane_focus.rs"]
mod spatial;
pub use spatial::{Bounds, Direction, Point};

pub const MAX_PANES: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PaneId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Row,
    Column,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Pane(PaneId),
    Split(Axis, Vec<Node>),
}
impl Node {
    fn rotate(&mut self, active: PaneId, count: usize, right: bool) -> Result<bool, Error> {
        let Self::Split(_, children) = self else {
            return Ok(false);
        };
        if children.contains(&Self::Pane(active)) {
            if children.iter().any(|node| !matches!(node, Self::Pane(_))) {
                return Err(Error::CannotRotateSplit);
            }
            let steps = count.max(1) % children.len();
            if right {
                children.rotate_right(steps);
            } else {
                children.rotate_left(steps);
            }
            return Ok(true);
        }
        for child in children {
            if child.rotate(active, count, right)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn exchange(&mut self, active: PaneId, count: usize) -> Result<Option<(Axis, PaneId)>, Error> {
        let Self::Split(axis, children) = self else {
            return Ok(None);
        };
        if let Some(index) = children.iter().position(|node| *node == Self::Pane(active)) {
            let target_index = if count == 0 {
                if index + 1 < children.len() {
                    index + 1
                } else {
                    index.saturating_sub(1)
                }
            } else if count <= children.len() {
                count - 1
            } else {
                return Err(Error::InvalidRange);
            };
            let Node::Pane(target) = children[target_index] else {
                return Ok(None);
            };
            if target == active {
                return Ok(None);
            }
            children.swap(index, target_index);
            return Ok(Some((*axis, target)));
        }
        for child in children {
            if let Some(found) = child.exchange(active, count)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    // Prefer the adjacent sibling in the innermost split, as Neovim does
    // when closing a view. A global traversal can jump into another column.
    fn close_neighbor(&self, target: PaneId) -> Option<PaneId> {
        let Self::Split(_, children) = self else {
            return None;
        };
        for (index, child) in children.iter().enumerate() {
            let mut leaves = Vec::new();
            child.order(&mut leaves);
            if !leaves.contains(&target) {
                continue;
            }
            if let Some(nested) = child.close_neighbor(target) {
                return Some(nested);
            }
            let neighbor = if index > 0 {
                &children[index - 1]
            } else {
                children.get(1)?
            };
            let mut candidates = Vec::new();
            neighbor.order(&mut candidates);
            return candidates.first().copied();
        }
        None
    }
    fn order(&self, output: &mut Vec<PaneId>) {
        match self {
            Self::Pane(id) => output.push(*id),
            Self::Split(_, children) => {
                for child in children {
                    child.order(output);
                }
            }
        }
    }
    fn insert(&mut self, target: PaneId, new: PaneId, axis: Axis, after: bool) -> bool {
        match self {
            Self::Pane(id) if *id == target => {
                let old = Self::Pane(*id);
                let new = Self::Pane(new);
                *self = Self::Split(
                    axis,
                    if after {
                        vec![old, new]
                    } else {
                        vec![new, old]
                    },
                );
                true
            }
            Self::Split(current, children) => {
                if *current == axis
                    && let Some(index) = children
                        .iter()
                        .position(|node| matches!(node, Self::Pane(id) if *id == target))
                {
                    children.insert(index + usize::from(after), Self::Pane(new));
                    return true;
                }
                children
                    .iter_mut()
                    .any(|child| child.insert(target, new, axis, after))
            }
            _ => false,
        }
    }
    fn remove(self, target: PaneId) -> Option<Self> {
        match self {
            Self::Pane(id) => (id != target).then_some(Self::Pane(id)),
            Self::Split(axis, children) => {
                let mut remaining = Vec::new();
                for child in children
                    .into_iter()
                    .filter_map(|child| child.remove(target))
                {
                    match child {
                        Self::Split(child_axis, nested) if child_axis == axis => {
                            remaining.extend(nested)
                        }
                        child => remaining.push(child),
                    }
                }
                match remaining.len() {
                    0 => None,
                    1 => remaining.pop(),
                    _ => Some(Self::Split(axis, remaining)),
                }
            }
        }
    }
}
impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pane(id) => write!(f, "{}", id.0),
            Self::Split(axis, children) => {
                write!(f, "{}(", if *axis == Axis::Row { "row" } else { "col" })?;
                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{child}")?;
                }
                f.write_str(")")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewState {
    pub buffer: u64,
    /// Opaque buffer coordinates supplied by the editor adapter. Reference
    /// fixtures use ASCII; this model performs no byte/character/cell conversion.
    pub row: usize,
    pub column: usize,
    pub top: usize,
    pub left: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownPane,
    LastPane,
    Limit,
    ExhaustedIds,
    InvalidGeometry,
    InvalidRange,
    CannotRotateSplit,
}

#[derive(Clone, Debug)]
pub struct Layout {
    root: Node,
    panes: BTreeMap<PaneId, ViewState>,
    jumps: BTreeMap<PaneId, JumpList>,
    active: PaneId,
    previous: Option<PaneId>,
    next_id: u64,
    focus_epoch: u64,
    identity: Arc<()>,
}

/// A navigation request is scoped to the active pane and its focus epoch.
/// Workspace ownership and buffer load generation remain the caller's guards.
#[derive(Clone, Debug)]
pub struct PaneJump {
    pane: PaneId,
    focus_epoch: u64,
    layout_identity: Arc<()>,
    pending: PendingJump,
}

/// Immutable capture of one layout's active pane and focus epoch.
#[derive(Clone, Debug)]
pub struct FocusGuard {
    pane: PaneId,
    epoch: u64,
    identity: Arc<()>,
}
impl PaneJump {
    pub fn target(&self) -> Option<JumpLocation> {
        self.pending.target()
    }
    pub fn target_anchor(&self) -> Option<AnchorId> {
        self.pending.target_anchor()
    }
}
impl Layout {
    pub fn new(view: ViewState) -> Self {
        let id = PaneId(1);
        Self {
            root: Node::Pane(id),
            panes: BTreeMap::from([(id, view)]),
            jumps: BTreeMap::from([(id, JumpList::default())]),
            active: id,
            previous: None,
            next_id: 2,
            focus_epoch: 0,
            identity: Arc::new(()),
        }
    }
    pub fn root(&self) -> &Node {
        &self.root
    }
    pub fn panes(&self) -> &BTreeMap<PaneId, ViewState> {
        &self.panes
    }
    pub fn active(&self) -> PaneId {
        self.active
    }
    pub fn previous(&self) -> Option<PaneId> {
        self.previous
    }
    pub fn view_mut(&mut self, id: PaneId) -> Result<&mut ViewState, Error> {
        self.panes.get_mut(&id).ok_or(Error::UnknownPane)
    }
    pub fn jump_list(&self, id: PaneId) -> Result<&JumpList, Error> {
        self.jumps.get(&id).ok_or(Error::UnknownPane)
    }
    pub fn jump_list_mut(&mut self, id: PaneId) -> Result<&mut JumpList, Error> {
        self.jumps.get_mut(&id).ok_or(Error::UnknownPane)
    }
    pub fn focus_guard(&self) -> FocusGuard {
        FocusGuard {
            pane: self.active,
            epoch: self.focus_epoch,
            identity: self.identity.clone(),
        }
    }
    pub fn focus_guard_is_current(&self, guard: &FocusGuard) -> bool {
        self.active == guard.pane
            && self.focus_epoch == guard.epoch
            && Arc::ptr_eq(&self.identity, &guard.identity)
    }
    /// For an explicit recovery action, allow a focus round trip but never a
    /// newly created layout that happens to reuse the same numeric pane ID.
    pub fn focus_guard_same_window(&self, guard: &FocusGuard) -> bool {
        self.active == guard.pane && Arc::ptr_eq(&self.identity, &guard.identity)
    }
    /// Check a saved window's identity without focusing it.
    pub fn focus_guard_window_exists(&self, guard: &FocusGuard, pane: PaneId) -> bool {
        guard.pane == pane
            && self.panes.contains_key(&pane)
            && Arc::ptr_eq(&self.identity, &guard.identity)
    }
    pub fn record_jump_guarded(&mut self, guard: &FocusGuard, origin: JumpLocation) -> bool {
        self.record_jump_guarded_with_anchor(guard, origin, None)
    }
    pub fn record_jump_guarded_with_anchor(
        &mut self,
        guard: &FocusGuard,
        origin: JumpLocation,
        anchor: Option<AnchorId>,
    ) -> bool {
        if !self.focus_guard_is_current(guard) {
            return false;
        }
        let Some(history) = self.jumps.get_mut(&guard.pane) else {
            return false;
        };
        history.record_with_anchor(origin, anchor);
        true
    }
    /// All visible windows see the same line edit. Apply transactionally so
    /// one unrepresentable coordinate cannot leave pane histories diverged.
    pub fn adjust_jump_lines(
        &mut self,
        buffer: u64,
        start: usize,
        removed: usize,
        added: usize,
    ) -> bool {
        let mut next = self.jumps.clone();
        if next
            .values_mut()
            .any(|jumps| !jumps.adjust_lines(buffer, start, removed, added))
        {
            return false;
        }
        self.jumps = next;
        true
    }
    pub fn forget_jump_buffer(&mut self, buffer: u64) {
        for jumps in self.jumps.values_mut() {
            jumps.forget_buffer(buffer);
        }
    }
    pub fn clear_jump_history(&mut self) {
        for jumps in self.jumps.values_mut() {
            jumps.clear();
        }
    }
    pub fn prepare_jump(
        &self,
        current: JumpLocation,
        older: bool,
        count: usize,
    ) -> Option<PaneJump> {
        self.prepare_jump_with_anchor(current, None, older, count)
    }
    pub fn prepare_jump_with_anchor(
        &self,
        current: JumpLocation,
        current_anchor: Option<AnchorId>,
        older: bool,
        count: usize,
    ) -> Option<PaneJump> {
        let pane = self.active;
        Some(PaneJump {
            pane,
            focus_epoch: self.focus_epoch,
            layout_identity: self.identity.clone(),
            pending: self.jumps.get(&pane)?.prepare_with_anchor(
                current,
                current_anchor,
                older,
                count,
            )?,
        })
    }
    pub fn commit_jump(&mut self, plan: PaneJump, origin: JumpLocation) -> bool {
        if self.active != plan.pane
            || self.focus_epoch != plan.focus_epoch
            || !Arc::ptr_eq(&self.identity, &plan.layout_identity)
        {
            return false;
        }
        self.jumps
            .get_mut(&plan.pane)
            .is_some_and(|jumps| jumps.commit(plan.pending, origin))
    }
    pub fn order(&self) -> Vec<PaneId> {
        let mut order = Vec::new();
        self.root.order(&mut order);
        order
    }
    pub fn focus(&mut self, id: PaneId) -> Result<(), Error> {
        if !self.panes.contains_key(&id) {
            return Err(Error::UnknownPane);
        }
        if self.active != id {
            self.previous = Some(self.active);
            self.active = id;
            self.focus_epoch = self.focus_epoch.wrapping_add(1);
        }
        Ok(())
    }
    pub fn focus_previous(&mut self) -> Result<(), Error> {
        if let Some(previous) = self.previous {
            self.focus(previous)
        } else {
            Ok(())
        }
    }
    pub fn focus_nth(&mut self, number: usize) -> Result<(), Error> {
        let order = self.order();
        self.focus(order[number.saturating_sub(1).min(order.len() - 1)])
    }
    pub fn cycle(&mut self, forward: bool) -> Result<(), Error> {
        let order = self.order();
        let index = order
            .iter()
            .position(|id| *id == self.active)
            .ok_or(Error::UnknownPane)?;
        let next = if forward {
            (index + 1) % order.len()
        } else {
            (index + order.len() - 1) % order.len()
        };
        self.focus(order[next])
    }
    /// Exchange direct sibling windows; nested sibling groups are not windows.
    pub fn exchange(&mut self, count: usize) -> Result<Option<(Axis, PaneId, PaneId)>, Error> {
        let old = self.active;
        let Some((axis, other)) = self.root.exchange(old, count)? else {
            return Ok(None);
        };
        self.focus(other)?;
        Ok(Some((axis, old, other)))
    }
    pub fn rotate(&mut self, count: usize, right: bool) -> Result<(), Error> {
        if self.root.rotate(self.active, count, right)? {
            self.focus_epoch = self.focus_epoch.wrapping_add(1);
        }
        Ok(())
    }
    /// Move the existing view to a full-width or full-height outer edge.
    pub fn move_to_edge(&mut self, direction: Direction) -> Result<bool, Error> {
        if self.panes.len() == 1 {
            return Ok(false);
        }
        let remaining = self
            .root
            .clone()
            .remove(self.active)
            .ok_or(Error::UnknownPane)?;
        let (axis, before) = match direction {
            Direction::Left => (Axis::Row, true),
            Direction::Right => (Axis::Row, false),
            Direction::Up => (Axis::Column, true),
            Direction::Down => (Axis::Column, false),
        };
        let mut children = match remaining {
            Node::Split(existing, children) if existing == axis => children,
            node => vec![node],
        };
        if before {
            children.insert(0, Node::Pane(self.active));
        } else {
            children.push(Node::Pane(self.active));
        }
        self.root = Node::Split(axis, children);
        self.focus_epoch = self.focus_epoch.wrapping_add(1);
        Ok(true)
    }
    /// Remove views only; the owner retains buffers and terminal processes.
    pub fn only(&mut self) -> Vec<PaneId> {
        let removed: Vec<_> = self
            .order()
            .into_iter()
            .filter(|id| *id != self.active)
            .collect();
        self.root = Node::Pane(self.active);
        self.panes.retain(|id, _| *id == self.active);
        self.jumps.retain(|id, _| *id == self.active);
        self.previous = None;
        if !removed.is_empty() {
            self.focus_epoch = self.focus_epoch.wrapping_add(1);
        }
        removed
    }
    /// `after` follows splitright/splitbelow. This changes topology only; sizes,
    /// cursor visibility and interactive resizing belong to the future renderer.
    pub fn split(&mut self, axis: Axis, after: bool) -> Result<PaneId, Error> {
        if self.panes.len() >= MAX_PANES {
            return Err(Error::Limit);
        }
        let next_id = self.next_id.checked_add(1).ok_or(Error::ExhaustedIds)?;
        let new = PaneId(self.next_id);
        let state = self
            .panes
            .get(&self.active)
            .ok_or(Error::UnknownPane)?
            .clone();
        let jumps = self
            .jumps
            .get(&self.active)
            .ok_or(Error::UnknownPane)?
            .clone();
        if !self.root.insert(self.active, new, axis, after) {
            return Err(Error::UnknownPane);
        }
        self.panes.insert(new, state);
        self.jumps.insert(new, jumps);
        self.next_id = next_id;
        self.focus(new)?;
        Ok(new)
    }
    /// Removes a view only. The caller must preflight unsaved buffer policy;
    /// the model never deletes buffer contents or writes a file.
    pub fn close(&mut self) -> Result<PaneId, Error> {
        self.close_at(self.active).map(|(closed, _)| closed)
    }
    /// Close a specific view without changing focus when another view is
    /// active. Return the neighboring view that receives the freed space.
    pub fn close_at(&mut self, pane: PaneId) -> Result<(PaneId, PaneId), Error> {
        if !self.panes.contains_key(&pane) {
            return Err(Error::UnknownPane);
        }
        if self.panes.len() == 1 {
            return Err(Error::LastPane);
        }
        let next = self.root.close_neighbor(pane).ok_or(Error::UnknownPane)?;
        let root = self.root.clone().remove(pane).ok_or(Error::LastPane)?;
        self.root = root;
        self.panes.remove(&pane);
        self.jumps.remove(&pane);
        if self.active == pane {
            self.active = next;
        }
        self.focus_epoch = self.focus_epoch.wrapping_add(1);
        // Neovim keeps the prior window when it survives, even if it is now
        // active. Closing a view does not turn that removed view into history.
        if self.previous == Some(pane) {
            self.previous = None;
        }
        Ok((pane, next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Step {
        keys: &'static [&'static str],
        layout: &'static str,
        active: u64,
        previous: u64,
        cursors: &'static [(u64, usize, usize)],
        error: bool,
    }
    struct Case {
        name: &'static str,
        steps: &'static [Step],
    }
    include!("../../../Resources/Fixtures/nvim-editor-panes.rs");
    include!("../../../Resources/Fixtures/nvim-editor-panes-independent.rs");
    fn initial() -> Layout {
        Layout::new(ViewState {
            buffer: 1,
            row: 4,
            column: 2,
            top: 0,
            left: 0,
        })
    }
    #[test]
    fn explicit_recovery_accepts_a_focus_round_trip_but_not_a_recreated_layout() {
        let mut original = initial();
        let pane = original.active();
        let guard = original.focus_guard();
        assert!(original.focus_guard_is_current(&guard));
        original.split(Axis::Row, true).unwrap();
        original.focus(pane).unwrap();
        assert!(!original.focus_guard_is_current(&guard));
        assert!(original.focus_guard_same_window(&guard));
        let replacement = initial();
        assert_eq!(replacement.active(), pane);
        assert!(!replacement.focus_guard_same_window(&guard));
    }
    #[test]
    fn exchange_matches_neovim_sibling_windows_and_focus() {
        let mut layout = initial();
        let a = layout.active();
        let b = layout.split(Axis::Row, true).unwrap();
        let c = layout.split(Axis::Column, true).unwrap();
        layout.view_mut(b).unwrap().buffer = 12;
        layout.view_mut(c).unwrap().buffer = 13;
        assert_eq!(layout.exchange(0), Ok(Some((Axis::Column, c, b))));
        assert_eq!(layout.order(), vec![a, c, b]);
        assert_eq!(layout.active(), b);
        assert_eq!(layout.previous(), Some(c));
        assert_eq!(layout.panes()[&b].buffer, 12);
        assert_eq!(layout.panes()[&c].buffer, 13);
        assert_eq!(layout.exchange(9), Err(Error::InvalidRange));
        assert_eq!(layout.order(), vec![a, c, b]);
        layout.focus(a).unwrap();
        assert_eq!(layout.exchange(0), Ok(None));
        assert_eq!(layout.order(), vec![a, c, b]);
    }
    #[test]
    fn rotate_matches_neovim_and_rejects_split_siblings() {
        let mut layout = initial();
        let a = layout.active();
        let b = layout.split(Axis::Row, true).unwrap();
        let c = layout.split(Axis::Row, true).unwrap();
        layout.focus(b).unwrap();
        let history = layout.previous();
        layout.rotate(2, true).unwrap();
        assert_eq!(layout.order(), vec![b, c, a]);
        assert_eq!(layout.active(), b);
        assert_eq!(layout.previous(), history);
        layout.rotate(2, false).unwrap();
        assert_eq!(layout.order(), vec![a, b, c]);
        layout.focus(a).unwrap();
        layout.split(Axis::Column, true).unwrap();
        layout.focus(c).unwrap();
        let before = layout.order();
        assert_eq!(layout.rotate(0, true), Err(Error::CannotRotateSplit));
        assert_eq!(layout.order(), before);
        assert_eq!(layout.active(), c);
    }
    #[test]
    fn moving_a_view_to_each_edge_matches_neovim_topology() {
        for (direction, expected) in [
            (Direction::Left, "row(3,1,2)"),
            (Direction::Down, "col(row(1,2),3)"),
            (Direction::Up, "col(3,row(1,2))"),
            (Direction::Right, "row(1,2,3)"),
        ] {
            let mut layout = initial();
            layout.split(Axis::Row, true).unwrap();
            let active = layout.split(Axis::Column, true).unwrap();
            let previous = layout.previous();
            layout.view_mut(active).unwrap().buffer = 11;
            assert_eq!(layout.move_to_edge(direction), Ok(true));
            assert_eq!(layout.root().to_string(), expected);
            assert_eq!(layout.active(), active);
            assert_eq!(layout.previous(), previous);
            assert_eq!(layout.panes()[&active].buffer, 11);
        }
    }
    #[test]
    fn only_retains_active_view_and_removes_only_other_window_ids() {
        let mut layout = initial();
        let a = layout.active();
        let b = layout.split(Axis::Row, true).unwrap();
        let c = layout.split(Axis::Column, true).unwrap();
        layout.view_mut(c).unwrap().buffer = 42;
        assert_eq!(layout.only(), vec![a, b]);
        assert_eq!(layout.root(), &Node::Pane(c));
        assert_eq!(layout.active(), c);
        assert_eq!(layout.panes()[&c].buffer, 42);
        assert_eq!(layout.previous(), None);
        assert!(layout.only().is_empty());
    }
    #[test]
    fn topology_history_and_independent_cursors_match_clean_neovim() {
        for case in CASES.iter().chain(INDEPENDENT_CASES) {
            let mut layout = initial();
            for step in case.steps {
                let result = match step.keys {
                    ["<C-w>", "v"] => layout.split(Axis::Row, true).map(|_| ()),
                    ["<C-w>", "s"] => layout.split(Axis::Column, true).map(|_| ()),
                    ["<C-w>", "c"] => layout.close().map(|_| ()),
                    ["<C-w>", "t"] => layout.focus_nth(1),
                    ["<C-w>", "b"] => layout.focus_nth(usize::MAX),
                    ["<C-w>", "p"] => layout.focus_previous(),
                    ["<C-w>", "w"] => layout.cycle(true),
                    ["<C-w>", "W"] => layout.cycle(false),
                    ["j"] => {
                        let id = layout.active();
                        layout.view_mut(id).unwrap().row += 1;
                        Ok(())
                    }
                    _ => panic!("Unknown fixture command"),
                };
                assert_eq!(
                    result.is_err(),
                    step.error,
                    "{} / {:?}",
                    case.name,
                    step.keys
                );
                assert_eq!(
                    layout.root().to_string(),
                    step.layout,
                    "{} / {:?}",
                    case.name,
                    step.keys
                );
                assert_eq!(
                    layout.active().0,
                    step.active,
                    "{} / {:?}",
                    case.name,
                    step.keys
                );
                assert_eq!(
                    layout.previous().map(|id| id.0).unwrap_or(0),
                    step.previous,
                    "{} / {:?}",
                    case.name,
                    step.keys
                );
                let cursors: Vec<_> = layout
                    .panes()
                    .iter()
                    .map(|(id, state)| {
                        assert_eq!(state.buffer, 1);
                        (id.0, state.row, state.column)
                    })
                    .collect();
                assert_eq!(cursors, step.cursors, "{} / {:?}", case.name, step.keys);
            }
        }
    }
    #[test]
    fn closing_middle_flat_pane_focuses_previous_pane() {
        let mut layout = initial();
        let left = layout.active();
        let middle = layout.split(Axis::Row, true).unwrap();
        let right = layout.split(Axis::Row, true).unwrap();
        layout.focus(middle).unwrap();
        assert_eq!(layout.order(), vec![left, middle, right]);
        layout.close().unwrap();
        assert_eq!(layout.active(), left);
    }
    #[test]
    fn closing_right_pane_focuses_first_leaf_of_adjacent_column() {
        let mut layout = initial();
        let upper = layout.active();
        let right = layout.split(Axis::Row, true).unwrap();
        layout.focus(upper).unwrap();
        let lower = layout.split(Axis::Column, true).unwrap();
        layout.focus(right).unwrap();
        assert_eq!(layout.order(), vec![upper, lower, right]);
        layout.close().unwrap();
        assert_eq!(layout.active(), upper);
    }
    #[test]
    fn closing_lower_nested_pane_focuses_its_sibling_not_next_column() {
        let mut layout = initial();
        let terminal = layout.active();
        let sidebar = layout.split(Axis::Row, true).unwrap();
        layout.focus(terminal).unwrap();
        let lower = layout.split(Axis::Column, true).unwrap();
        assert_eq!(layout.order(), vec![terminal, lower, sidebar]);
        layout.close().unwrap();
        assert_eq!(layout.active(), terminal);
    }
    #[test]
    fn split_limit_and_stale_ids_never_retarget_another_view() {
        let mut layout = initial();
        let first = layout.active();
        let second = layout.split(Axis::Row, false).unwrap();
        assert_eq!(layout.order(), vec![second, first]);
        layout.close().unwrap();
        assert_eq!(layout.focus(second), Err(Error::UnknownPane));
        let third = layout.split(Axis::Column, false).unwrap();
        assert!(third.0 > second.0);
        layout.view_mut(third).unwrap().top = 123;
        assert_eq!(layout.panes()[&first].top, 0);
        for _ in 2..MAX_PANES {
            layout.split(Axis::Row, true).unwrap();
        }
        let before = layout.root().clone();
        assert_eq!(layout.split(Axis::Row, true), Err(Error::Limit));
        assert_eq!(layout.root(), &before);
    }
    #[test]
    fn mixed_operations_keep_topology_ids_and_history_valid() {
        fn check(layout: &Layout) {
            fn check_node(current: &Node) {
                if let Node::Split(axis, children) = current {
                    assert!(children.len() >= 2);
                    for child in children {
                        if let Node::Split(child_axis, _) = child {
                            assert_ne!(axis, child_axis);
                        }
                        check_node(child);
                    }
                }
            }
            let order = layout.order();
            let mut unique = order.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(order.len(), unique.len());
            assert_eq!(unique, layout.panes.keys().copied().collect::<Vec<_>>());
            assert!(layout.panes.contains_key(&layout.active));
            assert!(
                layout
                    .previous
                    .is_none_or(|id| layout.panes.contains_key(&id))
            );
            assert!(layout.panes.len() <= MAX_PANES && !layout.panes.is_empty());
            assert!(layout.panes.keys().all(|id| id.0 < layout.next_id));
            check_node(&layout.root);
        }
        for seed in 1..=12u64 {
            let mut random = seed;
            let mut layout = initial();
            for _ in 0..1000 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                match random % 16 {
                    0..=5 => {
                        let _ = layout.split(
                            if random & 1 == 0 {
                                Axis::Row
                            } else {
                                Axis::Column
                            },
                            random & 2 == 0,
                        );
                    }
                    6 => {
                        let _ = layout.close();
                    }
                    7 => {
                        layout.focus_nth((random % 32) as usize).unwrap();
                    }
                    8 => {
                        layout.cycle(random & 1 == 0).unwrap();
                    }
                    9 => {
                        layout.focus_previous().unwrap();
                    }
                    10 => {
                        let _ = layout.exchange((random % 20) as usize);
                    }
                    11 => {
                        let _ = layout.rotate((random % 20) as usize, random & 1 == 0);
                    }
                    12 => {
                        let direction = [
                            Direction::Left,
                            Direction::Right,
                            Direction::Up,
                            Direction::Down,
                        ][(random as usize >> 8) % 4];
                        layout.move_to_edge(direction).unwrap();
                    }
                    13 if random & 0xf00 == 0 => {
                        layout.only();
                    }
                    _ => {
                        let id = layout.active();
                        layout.view_mut(id).unwrap().top = (random % 1000) as usize;
                    }
                }
                check(&layout);
            }
        }
        let mut layout = initial();
        layout.next_id = u64::MAX;
        let root = layout.root().clone();
        assert_eq!(layout.split(Axis::Row, true), Err(Error::ExhaustedIds));
        assert_eq!(layout.root(), &root);
        assert_eq!(layout.close(), Err(Error::LastPane));
    }
}
