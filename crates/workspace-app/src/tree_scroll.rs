//! Reveal a tree cursor on navigation, without undoing manual wheel scrolling.
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub root: PathBuf,
    pub path: PathBuf,
    pub row: usize,
}

#[derive(Default)]
pub struct Follow {
    cursor: Option<Cursor>,
    active: bool,
    viewport: (f32, f32),
    agents_visible: bool,
}

impl Follow {
    /// Return a one-shot child index for GPUI's minimal reveal strategy.
    /// Geometry comes from the current layout, never an assumed row height.
    pub fn update(
        &mut self,
        active: bool,
        cursor: Option<Cursor>,
        viewport: (f32, f32),
        agents_visible: bool,
        requested: bool,
    ) -> Option<usize> {
        let changed = self.cursor != cursor
            || !self.active
            || self.viewport != viewport
            || self.agents_visible != agents_visible;
        let reveal = active && (requested || changed);
        self.cursor = cursor;
        self.active = active;
        self.viewport = viewport;
        self.agents_visible = agents_visible;
        if reveal {
            self.cursor.as_ref().map(|cursor| cursor.row)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(row: usize) -> Option<Cursor> {
        Some(Cursor {
            root: "/synthetic/tree".into(),
            path: format!("/synthetic/tree/file-{row:03}").into(),
            row,
        })
    }

    #[test]
    fn long_tree_follows_counts_in_both_directions_without_repeating_on_redraw() {
        let mut follow = Follow::default();
        for row in [0, 1, 98, 199, 198, 2, 0] {
            assert_eq!(
                follow.update(true, cursor(row), (800., 480.), true, true),
                Some(row)
            );
            // A wheel event only changes GPUI's offset. Background redraws
            // must not issue a fresh reveal of the unchanged selection.
            assert_eq!(
                follow.update(true, cursor(row), (800., 480.), true, false),
                None
            );
        }
        // Boundary motion still reveals a cursor moved offscreen by wheel.
        assert_eq!(
            follow.update(true, cursor(0), (800., 480.), true, true),
            Some(0)
        );
    }

    #[test]
    fn focus_resize_and_agents_layout_reveal_once() {
        let mut follow = Follow::default();
        for size in [(1024., 768.), (640., 360.), (640., 220.), (800., 600.)] {
            assert_eq!(
                follow.update(true, cursor(150), size, true, false),
                Some(150)
            );
            assert_eq!(follow.update(true, cursor(150), size, true, false), None);
        }
        assert_eq!(
            follow.update(false, cursor(150), (800., 600.), true, false),
            None
        );
        assert_eq!(
            follow.update(true, cursor(150), (800., 600.), true, false),
            Some(150)
        );
        assert_eq!(
            follow.update(true, cursor(150), (800., 600.), false, false),
            Some(150)
        );
    }

    #[test]
    fn collapse_refresh_and_project_identity_use_current_visible_row() {
        let mut follow = Follow::default();
        let mut current = cursor(75).unwrap();
        assert_eq!(
            follow.update(true, Some(current.clone()), (800., 480.), true, false),
            Some(75)
        );
        current.row = 12; // Same node after a preceding branch collapses.
        assert_eq!(
            follow.update(true, Some(current.clone()), (800., 480.), true, false),
            Some(12)
        );
        current.root = "/synthetic/other".into();
        assert_eq!(
            follow.update(true, Some(current), (800., 480.), true, false),
            Some(12)
        );
        assert_eq!(follow.update(true, None, (800., 480.), true, true), None);
    }
}
