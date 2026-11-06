//! Reveal picker navigation once; redraws must preserve manual wheel scrolling.
#[derive(Default)]
pub struct Follow {
    cursor: Option<(u64, String, usize)>,
    viewport: (f32, f32),
}
impl Follow {
    pub fn update(
        &mut self,
        cursor: Option<(u64, String, usize)>,
        viewport: (f32, f32),
        requested: bool,
    ) -> Option<usize> {
        let reveal = requested || cursor != self.cursor || viewport != self.viewport;
        self.cursor = cursor;
        self.viewport = viewport;
        reveal.then(|| self.cursor.as_ref().map(|c| c.2)).flatten()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_results_reorder_resize_and_boundary_follow_without_wheel_snapback() {
        let mut follow = Follow::default();
        let cursor = |generation, row| Some((generation, format!("choice-{row}"), row));
        for row in [0, 80, 199, 2, 0] {
            assert_eq!(
                follow.update(cursor(1, row), (800., 600.), false),
                Some(row)
            );
            assert_eq!(follow.update(cursor(1, row), (800., 600.), false), None);
        }
        assert_eq!(follow.update(cursor(1, 0), (800., 600.), true), Some(0));
        assert_eq!(follow.update(cursor(2, 0), (800., 600.), false), Some(0));
        assert_eq!(follow.update(cursor(2, 0), (640., 300.), false), Some(0));
        assert_eq!(follow.update(None, (640., 300.), false), None);
        assert_eq!(follow.update(cursor(3, 0), (640., 300.), false), Some(0));
    }
}
