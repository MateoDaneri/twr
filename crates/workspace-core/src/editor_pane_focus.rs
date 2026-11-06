//! Directional selection from caller-supplied pane geometry and visible cursor.
//! The renderer must use a consistent coordinate space for every rectangle.
use super::{Axis, Error, Layout, Node, PaneId};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i64,
    pub y: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub pane: PaneId,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}
impl Bounds {
    fn right(self) -> i64 {
        self.x + self.width
    }
    fn bottom(self) -> i64 {
        self.y + self.height
    }
    fn valid(self) -> bool {
        const LIMIT: i64 = 1_000_000_000;
        (-LIMIT..=LIMIT).contains(&self.x)
            && (-LIMIT..=LIMIT).contains(&self.y)
            && (1..=LIMIT).contains(&self.width)
            && (1..=LIMIT).contains(&self.height)
    }
    fn contains(self, point: Point) -> bool {
        self.x <= point.x && point.x < self.right() && self.y <= point.y && point.y < self.bottom()
    }
    fn overlaps(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}
fn pane_path(node: &Node, pane: PaneId, path: &mut Vec<usize>) -> bool {
    match node {
        Node::Pane(id) => *id == pane,
        Node::Split(_, children) => {
            for (index, child) in children.iter().enumerate() {
                path.push(index);
                if pane_path(child, pane, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
    }
}

fn node_at<'a>(mut node: &'a Node, path: &[usize]) -> Option<&'a Node> {
    for &index in path {
        let Node::Split(_, children) = node else {
            return None;
        };
        node = children.get(index)?;
    }
    Some(node)
}

fn subtree_range(node: &Node, bounds: &[Bounds], axis: Axis) -> Option<(i64, i64)> {
    match node {
        Node::Pane(id) => bounds.iter().find(|b| b.pane == *id).map(|b| {
            if axis == Axis::Row {
                (b.x, b.right())
            } else {
                (b.y, b.bottom())
            }
        }),
        Node::Split(_, children) => {
            let mut ranges = children
                .iter()
                .map(|child| subtree_range(child, bounds, axis));
            let (mut start, mut end) = ranges.next()??;
            for range in ranges {
                let (child_start, child_end) = range?;
                start = start.min(child_start);
                end = end.max(child_end);
            }
            Some((start, end))
        }
    }
}

fn geometry_matches_tree(node: &Node, bounds: &[Bounds]) -> bool {
    let Node::Split(axis, children) = node else {
        return true;
    };
    let mut previous_end = None;
    for child in children {
        let Some((start, end)) = subtree_range(child, bounds, *axis) else {
            return false;
        };
        if previous_end.is_some_and(|previous| previous > start)
            || !geometry_matches_tree(child, bounds)
        {
            return false;
        }
        previous_end = Some(end);
    }
    !children.is_empty()
}

impl Layout {
    /// Check that measurements cover the current tree without overlapping or
    /// contradicting its sibling order. This cannot detect all stale frames.
    pub fn geometry_valid(&self, bounds: &[Bounds]) -> bool {
        if bounds.len() != self.panes().len() || bounds.iter().any(|b| !b.valid()) {
            return false;
        }
        let ids: BTreeSet<_> = bounds.iter().map(|b| b.pane).collect();
        if ids.len() != bounds.len() || ids != self.panes().keys().copied().collect() {
            return false;
        }
        for (index, item) in bounds.iter().enumerate() {
            if bounds[index + 1..]
                .iter()
                .any(|other| item.overlaps(*other))
            {
                return false;
            }
        }
        geometry_matches_tree(self.root(), bounds)
    }
    /// Returns false at a boundary, leaving focus/history untouched. This lets
    /// the future mode adapter preserve a Visual range on an unsuccessful move.
    /// Geometry must contain every current pane once, with no overlapping content
    /// rectangles. Borders/status lines may leave gaps between those rectangles.
    pub fn focus_direction(
        &mut self,
        direction: Direction,
        bounds: &[Bounds],
        cursor: Point,
    ) -> Result<bool, Error> {
        self.focus_direction_counted(direction, bounds, cursor, 1)
    }

    /// Follow at most `count` neighbors using the original cursor's screen
    /// position, then focus once so intermediate windows do not enter history.
    pub fn focus_direction_counted(
        &mut self,
        direction: Direction,
        bounds: &[Bounds],
        cursor: Point,
        count: usize,
    ) -> Result<bool, Error> {
        if !self.geometry_valid(bounds) {
            return Err(Error::InvalidGeometry);
        }
        let source = *bounds
            .iter()
            .find(|b| b.pane == self.active())
            .ok_or(Error::InvalidGeometry)?;
        if !source.contains(cursor) {
            return Err(Error::InvalidGeometry);
        }
        let (move_axis, backwards) = match direction {
            Direction::Left => (Axis::Row, true),
            Direction::Right => (Axis::Row, false),
            Direction::Up => (Axis::Column, true),
            Direction::Down => (Axis::Column, false),
        };
        let mut path = Vec::new();
        if !pane_path(self.root(), source.pane, &mut path) {
            return Err(Error::InvalidGeometry);
        }
        let mut target = source.pane;
        // Neovim ascends to a sibling frame, then descends to the appropriate
        // leaf using the original cursor's perpendicular screen coordinate.
        for _ in 0..count.max(1).min(bounds.len()) {
            let mut parent_path = path.clone();
            let mut neighbor = None;
            while let Some(index) = parent_path.pop() {
                let Node::Split(axis, children) =
                    node_at(self.root(), &parent_path).ok_or(Error::InvalidGeometry)?
                else {
                    return Err(Error::InvalidGeometry);
                };
                let next = if backwards {
                    index.checked_sub(1)
                } else {
                    index.checked_add(1).filter(|next| *next < children.len())
                };
                if *axis == move_axis
                    && let Some(next) = next
                {
                    parent_path.push(next);
                    neighbor = Some(parent_path);
                    break;
                }
            }
            let Some(mut next_path) = neighbor else {
                break;
            };
            loop {
                match node_at(self.root(), &next_path).ok_or(Error::InvalidGeometry)? {
                    Node::Pane(id) => {
                        target = *id;
                        path = next_path;
                        break;
                    }
                    Node::Split(axis, children) => {
                        if children.is_empty() {
                            return Err(Error::InvalidGeometry);
                        }
                        let next = if *axis == move_axis {
                            if backwards { children.len() - 1 } else { 0 }
                        } else {
                            let coordinate = if *axis == Axis::Row {
                                cursor.x
                            } else {
                                cursor.y
                            };
                            let starts: Vec<_> = children
                                .iter()
                                .map(|child| {
                                    subtree_range(child, bounds, *axis).map(|range| range.0)
                                })
                                .collect::<Option<_>>()
                                .ok_or(Error::InvalidGeometry)?;
                            if starts.windows(2).any(|pair| pair[0] >= pair[1]) {
                                return Err(Error::InvalidGeometry);
                            }
                            starts
                                .iter()
                                .rposition(|start| *start <= coordinate)
                                .unwrap_or(0)
                        };
                        next_path.push(next);
                    }
                }
            }
        }
        if target != self.active() {
            self.focus(target)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Axis, ViewState};
    use super::*;
    struct Step {
        keys: &'static [&'static str],
        layout: &'static str,
        before: u64,
        active: u64,
        previous: u64,
        point: Point,
        bounds: &'static [Bounds],
    }
    struct Case {
        name: &'static str,
        steps: &'static [Step],
    }
    include!("../../../Resources/Fixtures/nvim-editor-pane-focus.rs");
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
    fn directions_match_neovim_geometry_cursor_and_failed_move_history() {
        for case in CASES {
            let mut layout = initial();
            for step in case.steps {
                assert_eq!(
                    layout.active().0,
                    step.before,
                    "{} / {:?}",
                    case.name,
                    step.keys
                );
                let direction = match step.keys {
                    ["<C-w>", "h"] => Some(Direction::Left),
                    ["<C-w>", "j"] => Some(Direction::Down),
                    ["<C-w>", "k"] => Some(Direction::Up),
                    ["<C-w>", "l"] => Some(Direction::Right),
                    _ => None,
                };
                if let Some(direction) = direction {
                    let changed = layout
                        .focus_direction(direction, step.bounds, step.point)
                        .unwrap();
                    assert_eq!(
                        changed,
                        step.before != step.active,
                        "{} / {:?}",
                        case.name,
                        step.keys
                    );
                } else {
                    match step.keys {
                        ["<C-w>", "v"] => {
                            layout.split(Axis::Row, true).unwrap();
                        }
                        ["<C-w>", "s"] => {
                            layout.split(Axis::Column, true).unwrap();
                        }
                        ["<C-w>", "t"] => {
                            layout.focus_nth(1).unwrap();
                        }
                        ["<C-w>", "b"] => {
                            layout.focus_nth(usize::MAX).unwrap();
                        }
                        ["<C-w>", "w"] => {
                            layout.cycle(true).unwrap();
                        }
                        ["<C-w>", "W"] => {
                            layout.cycle(false).unwrap();
                        }
                        ["<C-w>", "p"] => {
                            layout.focus_previous().unwrap();
                        }

                        // Buffer motions belong to the editor; only their observed
                        // visible cursor geometry is input to this model's API.
                        keys if keys == ["g", "g"]
                            || ((keys.last() == Some(&"G") || keys.last() == Some(&"l"))
                                && keys[..keys.len() - 1].iter().all(|key| {
                                    key.len() == 1 && key.as_bytes()[0].is_ascii_digit()
                                })) => {}
                        _ => panic!("Unknown reference command"),
                    }
                }
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
                assert_eq!(
                    layout.root().to_string(),
                    step.layout,
                    "{} / {:?}",
                    case.name,
                    step.keys
                );
            }
        }
    }
    #[test]
    fn vertical_move_prefers_adjacent_pane_over_closer_diagonal_sidebar() {
        // Clean Neovim 0.12.1: terminal and file split vertically, then an
        // empty pane below the terminal and a sidebar split into two panes.
        // From the bottom-left pane, <C-w>k goes to the terminal above.
        let mut layout = initial();
        let terminal = layout.active();
        let file = layout.split(Axis::Row, true).unwrap();
        let explorer = layout.split(Axis::Row, true).unwrap();
        let agents = layout.split(Axis::Column, true).unwrap();
        layout.focus(terminal).unwrap();
        let below = layout.split(Axis::Column, true).unwrap();
        let bounds = [
            Bounds {
                pane: terminal,
                x: 0,
                y: 0,
                width: 80,
                height: 49,
            },
            Bounds {
                pane: below,
                x: 0,
                y: 53,
                width: 80,
                height: 47,
            },
            Bounds {
                pane: file,
                x: 81,
                y: 0,
                width: 79,
                height: 100,
            },
            Bounds {
                pane: explorer,
                x: 161,
                y: 0,
                width: 19,
                height: 51,
            },
            Bounds {
                pane: agents,
                x: 161,
                y: 53,
                width: 19,
                height: 47,
            },
        ];
        assert_eq!(layout.active(), below);
        assert_eq!(
            layout.focus_direction(Direction::Up, &bounds, Point { x: 5, y: 55 }),
            Ok(true)
        );
        assert_eq!(layout.active(), terminal);
        assert_eq!(layout.previous(), Some(below));
    }

    #[test]
    fn invalid_or_stale_geometry_never_changes_focus_or_history() {
        let mut layout = initial();
        layout.split(Axis::Row, true).unwrap();
        let valid = [
            Bounds {
                pane: PaneId(1),
                x: 0,
                y: 0,
                width: 10,
                height: 20,
            },
            Bounds {
                pane: PaneId(2),
                x: 11,
                y: 0,
                width: 10,
                height: 20,
            },
        ];
        let bad = [
            vec![],
            vec![valid[0], valid[0]],
            vec![
                valid[0],
                Bounds {
                    pane: PaneId(9),
                    ..valid[1]
                },
            ],
            vec![
                valid[0],
                Bounds {
                    width: 0,
                    ..valid[1]
                },
            ],
            vec![
                valid[0],
                Bounds {
                    x: i64::MAX,
                    ..valid[1]
                },
            ],
            vec![
                valid[0],
                Bounds {
                    y: i64::MIN,
                    ..valid[1]
                },
            ],
            vec![
                valid[0],
                Bounds {
                    width: i64::MAX,
                    ..valid[1]
                },
            ],
            vec![
                valid[0],
                Bounds {
                    height: -1,
                    ..valid[1]
                },
            ],
            vec![valid[0], Bounds { x: 5, ..valid[1] }],
        ];
        let before = layout.clone();
        for bounds in bad {
            assert_eq!(
                layout.focus_direction(Direction::Left, &bounds, Point { x: 12, y: 5 }),
                Err(Error::InvalidGeometry)
            );
            assert_eq!(layout.active(), before.active());
            assert_eq!(layout.previous(), before.previous());
            assert_eq!(layout.root(), before.root());
            assert_eq!(layout.panes(), before.panes());
        }
        assert_eq!(
            layout.focus_direction(Direction::Left, &valid, Point { x: 999, y: 5 }),
            Err(Error::InvalidGeometry)
        );
        layout.close().unwrap();
        assert_eq!(
            layout.focus_direction(Direction::Left, &valid, Point { x: 2, y: 5 }),
            Err(Error::InvalidGeometry)
        );
    }
    #[test]
    fn translation_and_input_order_do_not_change_the_target() {
        for shift in [-1000, 0, 1000] {
            let mut layout = initial();
            layout.split(Axis::Row, true).unwrap();
            let bounds = [
                Bounds {
                    pane: PaneId(2),
                    x: 11 + shift,
                    y: shift,
                    width: 10,
                    height: 20,
                },
                Bounds {
                    pane: PaneId(1),
                    x: shift,
                    y: shift,
                    width: 10,
                    height: 20,
                },
            ];
            assert!(
                layout
                    .focus_direction(
                        Direction::Left,
                        &bounds,
                        Point {
                            x: 12 + shift,
                            y: 5 + shift
                        }
                    )
                    .unwrap()
            );
            assert_eq!(layout.active(), PaneId(1));
        }
    }
}
