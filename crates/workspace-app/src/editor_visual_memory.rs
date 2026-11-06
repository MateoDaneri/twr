//! Previous Visual area: positions per buffer, never text or external state.
use super::*;

#[derive(Clone, Copy, Debug)]
struct Point {
    row: usize,
    column: usize,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Area {
    mode: Mode,
    block: Option<block::Selection>,
    anchor: Point,
    cursor: Point,
}
impl Editor {
    pub(super) fn goto_line_column(&mut self, number: usize) {
        let column = self
            .desired_column
            .filter(|(expected, _)| *expected == self.cursor)
            .map_or_else(|| self.tab_column(self.cursor), |(_, column)| column);
        let last = self.line_start(self.text.len().saturating_sub(1));
        let start = self.line_number(number).min(last);
        self.cursor = self.offset_at_tab_column(start, column);
        self.clamp();
        self.desired_column = Some((self.cursor, column));
    }
    fn visual_point(&self, offset: usize) -> Point {
        let row = self
            .line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        Point {
            row,
            column: offset.saturating_sub(self.line_starts[row]),
        }
    }
    fn visual_offset(&self, point: Point) -> usize {
        let start = self.line_starts[point.row.min(self.line_starts.len() - 1)];
        start + point.column.min(self.line_end(start) - start)
    }
    pub(super) fn visual_area(&self) -> Option<Area> {
        matches!(
            self.mode,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        )
        .then(|| Area {
            mode: self.mode,
            block: self.block,
            anchor: self.visual_point(self.anchor),
            cursor: self.visual_point(self.cursor),
        })
    }
    pub(super) fn reselect_visual(&mut self) {
        let Some(previous) = self.last_visual else {
            self.message = "No hay selección visual anterior".into();
            return;
        };
        let current = self.visual_area();
        self.anchor = self.visual_offset(previous.anchor);
        self.cursor = self.visual_offset(previous.cursor);
        self.mode = previous.mode;
        self.block = previous.block;
        self.restore_block_cursor(previous.anchor.row, previous.cursor.row);
        self.count = 0;
        self.desired_column = None;
        if let Some(current) = current {
            self.remember_visual(current);
        }
    }
    pub(super) fn remember_visual(&mut self, area: Area) {
        self.last_visual = Some(area);
        self.visual_generation = self.visual_generation.wrapping_add(1);
    }
    pub(super) fn remember_visual_put(&mut self, mode: Mode, anchor: usize, cursor: usize) {
        self.remember_visual(Area {
            mode,
            block: None,
            anchor: self.visual_point(anchor.min(self.text.len())),
            cursor: self.visual_point(cursor.min(self.text.len())),
        });
    }
    pub(super) fn track_visual_edit(
        &mut self,
        range: Range<usize>,
        added_rows: usize,
        before_line: bool,
    ) {
        let Some(mut area) = self.last_visual else {
            return;
        };
        let start = self.visual_point(range.start).row;
        let end = self.visual_point(range.end).row;
        let removed = end - start;
        if removed == 0 && added_rows == 0 {
            return;
        }
        for point in [&mut area.anchor, &mut area.cursor] {
            if before_line && removed == 0 && point.row >= start {
                point.row = point.row.saturating_add(added_rows);
            } else if point.row > start {
                point.row = if point.row >= end {
                    point.row.saturating_sub(removed).saturating_add(added_rows)
                } else {
                    start
                };
            }
        }
        self.last_visual = Some(area);
    }
    pub(super) fn track_visual_replacement(&mut self, next: &[char]) {
        if self.last_visual.is_none() {
            return;
        }
        let prefix = self
            .text
            .iter()
            .zip(next)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.text[prefix..]
            .iter()
            .rev()
            .zip(next[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_end = self.text.len() - suffix;
        let new_end = next.len() - suffix;
        self.track_visual_edit(
            prefix..old_end,
            next[prefix..new_end].iter().filter(|c| **c == '\n').count(),
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(e: &mut Editor, text: &str) {
        for ch in text.chars() {
            e.key(&ch.to_string());
        }
    }
    fn sequence(e: &mut Editor, input: &str) {
        for (i, part) in input.split("<Esc>").enumerate() {
            if i > 0 {
                e.key("<Esc>");
            }
            keys(e, part);
        }
    }
    #[test]
    fn reselect_and_exchange_match_clean_neovim_0121() {
        for (input, mode, anchor, cursor) in [
            ("lvll<Esc>0gv", Mode::Visual, 1, 3),
            ("3lvhh<Esc>0gv", Mode::Visual, 3, 1),
            ("Vj<Esc>Ggv", Mode::VisualLine, 0, 7),
            ("lvll<Esc>Gvlgv", Mode::Visual, 1, 3),
            ("lvll<Esc>Gvlgvgv", Mode::Visual, 17, 18),
            ("lvly0gv", Mode::Visual, 1, 2),
            ("3lvhhy0gv", Mode::Visual, 3, 1),
            ("lvldgv", Mode::Visual, 1, 2),
            ("lvllUgv", Mode::Visual, 1, 3),
            ("lvllrXgv", Mode::Visual, 1, 3),
            ("lvldugv", Mode::Visual, 1, 2),
        ] {
            let mut e = Editor::new("abcdef\nsecond\nthird".into());
            sequence(&mut e, input);
            assert_eq!(
                (e.mode, e.anchor, e.cursor),
                (mode, anchor, cursor),
                "{input}"
            );
        }
        let mut e = Editor::new("abc".into());
        keys(&mut e, "gv");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.contents(), "abc");
    }
    #[test]
    fn put_selects_inserted_text_and_keeps_original_visual_mode() {
        for (input, mode, selection) in [
            ("lvll\"apgv", Mode::Visual, 1..4),
            ("V\"apgv", Mode::VisualLine, 0..4),
        ] {
            let mut e = Editor::new("abcdef\nsecond\nthird".into());
            e.registers.capture('a', 'y', "XYZ".into(), false).unwrap();
            sequence(&mut e, input);
            assert_eq!(e.mode, mode);
            assert_eq!(e.selection(), Some(selection));
            e.key("<Esc>");
            e.key("u");
            assert_eq!(e.contents(), "abcdef\nsecond\nthird");
        }
    }
    #[test]
    fn remembered_rows_follow_line_edits_without_copying_buffer_on_motion() {
        let mut e = Editor::new("abcdef\nsecond\nthird".into());
        sequence(&mut e, "jlvll<Esc>ggO");
        e.insert("new");
        e.key("<Esc>");
        keys(&mut e, "gv");
        assert_eq!(e.selection(), Some(12..15));
        let mut e = Editor::new("abcdef\nsecond\nthird".into());
        sequence(&mut e, "jlvll<Esc>ggddgv");
        assert_eq!(e.selection(), Some(1..4));
        let mut e = Editor::new("abcdef\nsecond\nthird".into());
        sequence(&mut e, "lvll<Esc>0i");
        e.insert("ZZ");
        e.key("<Esc>");
        keys(&mut e, "gv");
        assert_eq!(e.selection(), Some(1..4));
        assert_eq!(e.text[1..4].iter().collect::<String>(), "Zab");
        let mut e = Editor::new("abcdef\nsecond\nthird".into());
        sequence(&mut e, "lvll<Esc>O");
        e.insert("new");
        e.key("<Esc>");
        e.key("u");
        e.key("<C-r>");
        keys(&mut e, "gv");
        assert_eq!(e.selection(), Some(5..8));
        let mut e = Editor::new("abcdef\nsecond\nthird".into());
        keys(&mut e, "Vdugv");
        assert_eq!(e.mode, Mode::VisualLine);
        assert_eq!(e.selection(), Some(0..7));
    }
    #[test]
    fn reselect_is_buffer_local_unicode_safe_and_records_macros() {
        let mut e = Editor::new("ñ😀abc".into());
        sequence(&mut e, "qavll<Esc>0gvyq");
        assert_eq!(e.registers.get('"').unwrap().0, "ñ😀a");
        keys(&mut e, "@a");
        assert_eq!(e.registers.get('"').unwrap().0, "ñ😀a");
        let mut other = Editor::new("unrelated".into()).using_registers(e.registers.clone());
        keys(&mut other, "gv");
        assert_eq!(other.selection(), None);
        let mut e = Editor::new("abc\ndef".into());
        keys(&mut e, "Vjv");
        assert_eq!(e.anchor, 0);
        assert_eq!(e.mode, Mode::Visual);
    }
}
