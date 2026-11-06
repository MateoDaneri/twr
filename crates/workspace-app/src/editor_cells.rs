//! Scalar ranges clipped in display-cell space; shared by paint, jump and IME.
use super::*;

#[derive(Debug, PartialEq, Eq)]
pub struct CellPiece {
    pub source: Range<usize>,
    pub columns: Range<usize>,
    pub visible: Range<usize>,
    pub tab: bool,
    pub clipped: bool,
}
impl CellPiece {
    pub fn append_text(&self, text: &[char], output: &mut String) {
        if self.tab || self.clipped || self.source.end > text.len() {
            output.extend(std::iter::repeat_n(' ', self.visible.len()));
        } else {
            output.extend(&text[self.source.clone()]);
        }
    }
}
impl Editor {
    pub fn display_text_column(&self, text: &[char]) -> usize {
        text.iter().fold(0usize, |column, ch| {
            column.saturating_add(self.display_cell_width(*ch, column))
        })
    }

    pub fn display_cells(
        &self,
        text: &[char],
        left: usize,
        columns: usize,
        padding: usize,
    ) -> Vec<CellPiece> {
        let right = left.saturating_add(columns);
        let mut pieces: Vec<CellPiece> = Vec::new();
        if left == right {
            return pieces;
        }
        let mut column = 0usize;
        for (index, ch) in text
            .iter()
            .copied()
            .chain(std::iter::repeat_n(' ', padding))
            .enumerate()
        {
            let width = self.display_cell_width(ch, column);
            if width == 0 {
                // Keep combining marks with the visible base, including at the
                // right edge. A clipped base is rendered as padding instead.
                if let Some(last) = pieces.last_mut()
                    && last.source.end == index
                {
                    last.source.end += 1;
                }
                continue;
            }
            if column >= right {
                break;
            }
            let end = column.saturating_add(width);
            if end > left {
                let visible = column.max(left)..end.min(right);
                pieces.push(CellPiece {
                    source: index..index + 1,
                    clipped: visible.start != column || visible.end != end,
                    columns: column..end,
                    visible,
                    tab: ch == '\t',
                });
            }
            column = end;
        }
        pieces
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(
        editor: &Editor,
        text: &[char],
        left: usize,
        columns: usize,
        padding: usize,
    ) -> String {
        let mut output = String::new();
        for piece in editor.display_cells(text, left, columns, padding) {
            piece.append_text(text, &mut output);
        }
        output
    }

    #[test]
    fn clipped_wide_glyphs_tabs_and_combining_marks_use_cell_boundaries() {
        let editor = Editor::new(String::new());
        let text: Vec<_> = "界\te\u{301}XYZ".chars().collect();
        assert_eq!(editor.display_text_column(&text), 8);
        assert_eq!(rendered(&editor, &text, 1, 5, 0), "   e\u{301}X");
        assert_eq!(rendered(&editor, &text, 0, 1, 0), " ");
        assert_eq!(rendered(&editor, &text, 4, 1, 0), "e\u{301}");
        assert_eq!(rendered(&editor, &text, 3, 2, 0), " e\u{301}");
        let pieces = editor.display_cells(&text, 1, 5, 0);
        assert_eq!(pieces[0].source, 0..1);
        assert!(pieces[0].clipped);
        assert_eq!(pieces[2].source, 2..4);
        assert_eq!(pieces.iter().map(|p| p.visible.len()).sum::<usize>(), 5);
    }

    #[test]
    fn virtual_padding_and_extreme_viewports_remain_bounded() {
        let editor = Editor::new(String::new());
        let text: Vec<_> = "界".chars().collect();
        assert_eq!(rendered(&editor, &text, 2, 3, 3), "   ");
        assert!(editor.display_cells(&text, 0, 0, 0).is_empty());
        assert!(editor.display_cells(&text, usize::MAX, 10, 0).is_empty());
        let pieces = editor.display_cells(&text, 0, usize::MAX, 0);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].visible, 0..2);
    }

    #[test]
    fn wide_line_caret_is_painted_inside_the_measured_cell_viewport() {
        let mut editor = Editor::new("界".repeat(200));
        editor.goto(0, 100);
        assert_eq!(editor.display_cell_column(), 200);
        let left = followed_column(0, editor.display_cell_column(), 80);
        let pieces = editor.display_cells(&editor.text, left, 80, 1);
        assert_eq!(left, 160);
        assert_eq!(pieces.first().unwrap().source.start, 80);
        assert_eq!(pieces.last().unwrap().source.end, 120);
        assert_eq!(pieces.iter().map(|p| p.visible.len()).sum::<usize>(), 80);
        assert!(
            pieces
                .iter()
                .any(|piece| piece.source.contains(&editor.cursor))
        );
    }
}
