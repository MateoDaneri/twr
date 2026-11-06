//! Reveal keyboard selection once, preserving manual wheel position on refresh.
use crate::workspace::AgentRow;

#[derive(Default)]
pub struct Follow {
    cursor: Option<(AgentRow, usize)>,
    active: bool,
    viewport: (f32, f32),
}
impl Follow {
    pub fn update(
        &mut self,
        active: bool,
        cursor: Option<(AgentRow, usize)>,
        viewport: (f32, f32),
        requested: bool,
    ) -> Option<usize> {
        let reveal = active
            && (requested || !self.active || self.cursor != cursor || self.viewport != viewport);
        self.cursor = cursor;
        self.active = active;
        self.viewport = viewport;
        reveal
            .then(|| self.cursor.as_ref().map(|(_, row)| *row))
            .flatten()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_refresh_reorder_resize_and_boundary_reveal() {
        let mut follow = Follow::default();
        let cursor = |row| Some((AgentRow::Task("same-task".into()), row));
        for row in [0, 90, 199, 4] {
            assert_eq!(
                follow.update(true, cursor(row), (800., 600.), false),
                Some(row)
            );
            // A provider update/redraw after manual scrolling must not jump back.
            assert_eq!(follow.update(true, cursor(row), (800., 600.), false), None);
        }
        assert_eq!(follow.update(true, cursor(4), (800., 600.), true), Some(4));
        assert_eq!(follow.update(true, cursor(4), (600., 400.), false), Some(4));
        assert_eq!(follow.update(false, cursor(4), (600., 400.), true), None);
        assert_eq!(follow.update(true, cursor(4), (600., 400.), false), Some(4));
        assert_eq!(follow.update(true, None, (600., 400.), true), None);
    }
}
