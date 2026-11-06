//! Per-event wrapped wheel geometry, compared with clean Neovim fixtures.
use super::{Editor, Mode, WrapCursor, WrapPosition, WrappedTop, WrappedVirtualCursor};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug)]
pub struct WrappedWheelRequest {
    pub top: WrappedTop,
    pub cursor: WrapCursor,
    pub goal: Option<usize>,
    pub rows: usize,
    pub columns: usize,
    pub delta: isize,
    pub mode: Mode,
    pub coladd: usize,
}

impl Editor {
    fn native_wheel_positions_cached<'a>(
        &self,
        cache: &'a mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        line: usize,
        cols: usize,
    ) -> Result<&'a [WrapPosition], &'static str> {
        if !cache.contains_key(&line) {
            let start = self.line_starts[line];
            let positions =
                self.wrapped_positions(&self.text[start..self.line_end(start)], cols)?;
            let used: usize = cache.values().map(Vec::len).sum();
            if used.saturating_add(positions.len()) > 262_144 {
                cache.clear();
            }
            cache.insert(line, positions);
        }
        Ok(cache.get(&line).unwrap())
    }
    fn native_wheel_vcol_cached(
        &self,
        cache: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        cursor: WrapCursor,
        cols: usize,
        insert: bool,
    ) -> Result<usize, &'static str> {
        let start = self.line_starts[cursor.line];
        let text = &self.text[start..self.line_end(start)];
        if cursor.scalar > text.len() {
            return Err("invalid wheel cursor");
        }
        if self.folded_line_count(start).is_some() {
            return Ok(cursor.visual_row * cols
                + self
                    .display_text_column(&text[..cursor.scalar])
                    .min(cols - 1));
        }
        let pos = self.native_wheel_positions_cached(cache, cursor.line, cols)?;
        if cursor.scalar == text.len() {
            return Ok(cursor.visual_row * cols + pos.last().map_or(0, |p| p.end % cols));
        }
        let p = pos.get(cursor.scalar).ok_or("invalid wheel cursor")?;
        let offset = cursor
            .visual_row
            .checked_sub(p.row)
            .ok_or("bad row")?
            .checked_mul(cols)
            .ok_or("overflow")?;
        if p.end <= offset {
            return Err("invalid wheel cursor");
        }
        let begin = p.start.saturating_sub(offset).min(cols - 1);
        let end = p.end.saturating_sub(offset).min(cols);
        Ok(cursor.visual_row * cols
            + if text[cursor.scalar] == '\t' && !insert {
                end.saturating_sub(1)
            } else {
                begin
            })
    }
    fn native_wheel_coladvance_cached(
        &self,
        cache: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        line: usize,
        wanted: usize,
        cols: usize,
        one_more: bool,
    ) -> Result<WrapCursor, &'static str> {
        let pos = self.native_wheel_positions_cached(cache, line, cols)?;
        if one_more && pos.last().is_some_and(|p| wanted >= p.row * cols + p.end) {
            let p = pos.last().unwrap();
            return Ok(WrapCursor {
                line,
                scalar: pos.len(),
                visual_row: p.row + p.end / cols,
            });
        }
        let mut scalar = pos
            .partition_point(|p| p.row * cols + p.start <= wanted)
            .saturating_sub(1);
        while scalar > 0 && pos[scalar - 1] == pos[scalar] {
            scalar -= 1;
        }
        Ok(WrapCursor {
            line,
            scalar,
            visual_row: pos
                .get(scalar)
                .map_or(0, |p| p.row + p.end.saturating_sub(1) / cols),
        })
    }
    fn native_wheel_retain_byte_cached(
        &self,
        cache: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        cursor: WrapCursor,
        target: usize,
        cols: usize,
    ) -> Result<WrapCursor, &'static str> {
        if self.source_row(self.visible_row(target)) != target {
            return Err("bad retained cursor");
        }
        let origin_start = self.line_starts[cursor.line];
        let origin = &self.text[origin_start..self.line_end(origin_start)];
        let byte_col: usize = origin
            .iter()
            .take(cursor.scalar)
            .map(|c| c.len_utf8())
            .sum();
        let start = self.line_starts[target];
        let chars = &self.text[start..self.line_end(start)];
        let mut bytes = 0;
        let mut scalar = 0;
        for c in chars {
            if bytes + c.len_utf8() > byte_col {
                break;
            }
            bytes += c.len_utf8();
            scalar += 1;
        }
        scalar = scalar.min(chars.len());
        if self.folded_line_count(start).is_some() {
            return Ok(WrapCursor {
                line: target,
                scalar,
                visual_row: 0,
            });
        }
        let pos = self.native_wheel_positions_cached(cache, target, cols)?;
        let visual_row = if scalar == chars.len() {
            pos.last().map_or(0, |p| p.row + p.end / cols)
        } else {
            pos.get(scalar)
                .map_or(0, |p| p.row + p.end.saturating_sub(1) / cols)
        };
        Ok(WrapCursor {
            line: target,
            scalar,
            visual_row,
        })
    }
    fn native_wheel_final_cached(
        &self,
        heights: &mut std::collections::HashMap<(usize, bool), usize>,
        layouts: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        line: usize,
        cols: usize,
        cursor: WrapCursor,
    ) -> Result<usize, &'static str> {
        let start = self.line_starts[line];
        if self.folded_line_count(start).is_some() {
            return Ok(0);
        }
        let eol = cursor.line == line && cursor.scalar == self.line_end(start) - start;
        if let Some(result) = heights.get(&(line, eol)) {
            return Ok(*result);
        }
        let positions = self.native_wheel_positions_cached(layouts, line, cols)?;
        let mut last = positions
            .iter()
            .map(|p| p.row + p.end.saturating_sub(1) / cols)
            .max()
            .unwrap_or(0);
        if eol && let Some(p) = positions.last() {
            last = last.max(p.row + p.end / cols);
        }
        heights.insert((line, eol), last);
        Ok(last)
    }
    fn native_wheel_step_cached(
        &self,
        heights: &mut std::collections::HashMap<(usize, bool), usize>,
        layouts: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        top: WrappedTop,
        down: bool,
        cols: usize,
        cursor: WrapCursor,
    ) -> Result<WrappedTop, &'static str> {
        let visible = self.visible_row(top.source_line.min(self.line_starts.len() - 1));
        let source_line = self.source_row(visible);
        let last = self.native_wheel_final_cached(heights, layouts, source_line, cols, cursor)?;
        let current = top.visual_row.min(last);
        if down && current < last {
            return Ok(WrappedTop {
                source_line,
                visual_row: current + 1,
            });
        }
        if !down && current > 0 {
            return Ok(WrappedTop {
                source_line,
                visual_row: current - 1,
            });
        }
        let next_visible = if down {
            visible.saturating_add(1)
        } else {
            visible.saturating_sub(1)
        };
        let next_line = self.source_row(next_visible);
        if next_line == source_line {
            return Ok(WrappedTop {
                source_line,
                visual_row: current,
            });
        }
        Ok(WrappedTop {
            source_line: next_line,
            visual_row: if down {
                0
            } else {
                self.native_wheel_final_cached(heights, layouts, next_line, cols, cursor)?
            },
        })
    }
    fn native_block_wheel_vcol(
        &self,
        cache: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        cursor: WrapCursor,
        add: usize,
        cols: usize,
    ) -> Result<usize, &'static str> {
        let start = self.line_starts[cursor.line];
        let text = &self.text[start..self.line_end(start)];
        let positions = self.native_wheel_positions_cached(cache, cursor.line, cols)?;
        let base = positions
            .get(cursor.scalar)
            .map(|p| p.row * cols + p.start)
            .unwrap_or_else(|| positions.last().map_or(0, |p| p.row * cols + p.end));
        let ch = text.get(cursor.scalar).copied();
        let cells = ch
            .filter(|c| *c != '\t')
            .and_then(|c| c.width())
            .unwrap_or(1)
            .max(1);
        let normalized = if cells > 1 && add < cells { 0 } else { add };
        base.checked_add(normalized)
            .ok_or("virtual column overflow")
    }
    fn native_block_wheel_coladvance(
        &self,
        cache: &mut std::collections::HashMap<usize, Vec<WrapPosition>>,
        line: usize,
        wanted: usize,
        cols: usize,
    ) -> Result<(WrapCursor, usize), &'static str> {
        let start = self.line_starts[line];
        let text = &self.text[start..self.line_end(start)];
        let pos = self.native_wheel_positions_cached(cache, line, cols)?;
        let size = pos.last().map_or(0, |p| p.row * cols + p.end);
        // Neovim treats MAXCOL as an end-of-line request, before applying
        // virtualedit's ordinary screen-row cap.
        if wanted >= i32::MAX as usize - 1 {
            return Ok((
                WrapCursor {
                    line,
                    scalar: pos.len(),
                    visual_row: size / cols,
                },
                0,
            ));
        }
        let mut target = wanted;
        if target >= cols {
            let last = size.saturating_sub(1);
            if target / cols > last / cols {
                target = (last / cols + 1) * cols - 1;
            }
        }
        if target >= size {
            let cursor = WrapCursor {
                line,
                scalar: pos.len(),
                visual_row: target / cols,
            };
            return Ok((cursor, target - size));
        }
        let mut scalar = pos
            .partition_point(|p| p.row * cols + p.start <= target)
            .saturating_sub(1);
        while scalar > 0 && pos[scalar - 1] == pos[scalar] {
            scalar -= 1;
        }
        let base = pos.get(scalar).map_or(0, |p| p.row * cols + p.start);
        let ch = text.get(scalar).copied();
        let cells = ch
            .filter(|c| *c != '\t')
            .and_then(|c| c.width())
            .unwrap_or(1)
            .max(1);
        let mut add = target.saturating_sub(base);
        if cells > 1 && add < cells {
            add = 0;
        }
        let vcol = base + add;
        Ok((
            WrapCursor {
                line,
                scalar,
                visual_row: vcol / cols,
            },
            add,
        ))
    }
    /// Move a virtual block cursor by displayed rows, retaining its screen column.
    pub fn move_wrapped_block_cursor(
        &self,
        virtual_cursor: WrappedVirtualCursor,
        desired: usize,
        down: bool,
        cols: usize,
        count: usize,
    ) -> Result<(WrapCursor, usize, usize), &'static str> {
        if self.mode != Mode::VisualBlock || cols == 0 || cols > 200 || desired >= cols {
            return Err("invalid wrapped block motion");
        }
        let mut cursor = virtual_cursor.cursor;
        if cursor.line >= self.line_starts.len() {
            return Err("bad wrapped line");
        }
        let mut coladd = virtual_cursor.coladd;
        let mut goal = cursor
            .visual_row
            .saturating_mul(cols)
            .saturating_add(desired);
        let mut heights = std::collections::HashMap::new();
        let mut layouts = std::collections::HashMap::new();
        for _ in 0..count.min(9_999) {
            let current = WrappedTop {
                source_line: cursor.line,
                visual_row: cursor.visual_row,
            };
            let next = self.native_wheel_step_cached(
                &mut heights,
                &mut layouts,
                current,
                down,
                cols,
                cursor,
            )?;
            if next == current {
                break;
            }
            goal = next.visual_row.saturating_mul(cols).saturating_add(desired);
            (cursor, coladd) =
                self.native_block_wheel_coladvance(&mut layouts, next.source_line, goal, cols)?;
        }
        Ok((cursor, coladd, goal))
    }

    pub fn calculate_wrapped_wheel(
        &self,
        request: WrappedWheelRequest,
    ) -> Result<(WrappedTop, WrapCursor, usize, usize), &'static str> {
        let WrappedWheelRequest {
            mut top,
            mut cursor,
            goal,
            rows,
            columns: cols,
            delta,
            mode,
            coladd: mut add,
        } = request;
        if top.source_line >= self.line_starts.len() || cursor.line >= self.line_starts.len() {
            return Err("bad wrapped line");
        }
        let start = self.line_starts[cursor.line];
        if cursor.scalar > self.line_end(start) - start {
            return Err("bad wrapped cursor");
        }
        let (one_more, insert, block) = match mode {
            Mode::Normal => (false, false, false),
            Mode::Visual | Mode::VisualLine => (true, false, false),
            Mode::Insert => (true, true, false),
            Mode::VisualBlock => (true, false, true),
            _ => return Err("unsupported wrapped wheel mode"),
        };
        if !block && add != 0 {
            return Err("virtual offset outside block mode");
        }
        if rows == 0 || rows > 100 || cols == 0 || cols > 200 {
            return Err("bad viewport");
        }
        let mut final_rows = std::collections::HashMap::new();
        let mut layouts = std::collections::HashMap::new();
        let down = delta > 0;
        let original_line = cursor.line;
        let original_view = self.wrapped_viewport_for_cursor(top, rows, cols, cursor)?;
        let cursor_row = self
            .wrapped_cursor_cell(&original_view, cursor)
            .map_or(0, |cell| cell.row);
        let original_vcol = if block {
            self.native_block_wheel_vcol(&mut layouts, cursor, add, cols)?
        } else {
            self.native_wheel_vcol_cached(&mut layouts, cursor, cols, insert)?
        };
        let mut goal = goal.unwrap_or(original_vcol);
        if delta == 0 {
            return Ok((top, cursor, goal, add));
        }
        let line_height = (self.native_wheel_final_cached(
            &mut final_rows,
            &mut layouts,
            cursor.line,
            cols,
            cursor,
        )? + 1)
            .min(rows);
        let mut scrolled = 0usize;
        // Editor counts are bounded to 9999. Mouse input is already bounded
        // by its accumulator; do not silently truncate keyboard counts to 100.
        for _ in 0..delta.unsigned_abs().min(9999) {
            let next = self.native_wheel_step_cached(
                &mut final_rows,
                &mut layouts,
                top,
                down,
                cols,
                cursor,
            )?;
            if next == top {
                break;
            }
            top = next;
            scrolled += 1;
        }
        if down && cursor.line < top.source_line {
            if block {
                (cursor, add) =
                    self.native_block_wheel_coladvance(&mut layouts, top.source_line, goal, cols)?;
            } else {
                cursor = self.native_wheel_coladvance_cached(
                    &mut layouts,
                    top.source_line,
                    goal,
                    cols,
                    one_more,
                )?;
            }
        } else if !down {
            let mut bottom = (cursor_row + scrolled + line_height - 1) as isize
                - (original_vcol / cols) as isize;
            let mut line = cursor.line;
            while bottom >= rows as isize && line > 0 {
                bottom -= (self.native_wheel_final_cached(
                    &mut final_rows,
                    &mut layouts,
                    line,
                    cols,
                    cursor,
                )? + 1)
                    .min(rows) as isize;
                let previous = self.source_row(self.visible_row(line).saturating_sub(1));
                if previous == line {
                    break;
                }
                line = previous;
            }
            if line != cursor.line {
                if block {
                    (cursor, add) =
                        self.native_block_wheel_coladvance(&mut layouts, line, goal, cols)?;
                } else {
                    cursor = self.native_wheel_coladvance_cached(
                        &mut layouts,
                        line,
                        goal,
                        cols,
                        one_more,
                    )?;
                }
            }
            if cursor.line < top.source_line {
                cursor = self.native_wheel_retain_byte_cached(
                    &mut layouts,
                    cursor,
                    top.source_line,
                    cols,
                )?;
            }
        }
        let view = self.wrapped_viewport_for_cursor(top, rows, cols, cursor)?;
        if view.is_empty() {
            return Err("empty viewport");
        }
        let last = view.last().unwrap();
        let last_line = self.source_line_at(last.source.start);
        let last_top = WrappedTop {
            source_line: last_line,
            visual_row: last.visual_row,
        };
        let at_end = self.native_wheel_step_cached(
            &mut final_rows,
            &mut layouts,
            last_top,
            true,
            cols,
            cursor,
        )? == last_top;
        let mut above_wanted = 4;
        let mut below_wanted = 4;
        if top.source_line == 0 {
            above_wanted = 0;
            below_wanted = below_wanted.min(rows / 2);
        }
        if at_end {
            below_wanted = 0;
            above_wanted = above_wanted.min(rows.saturating_sub(1) / 2);
        }
        let mut low = top.source_line;
        let mut high = last_line;
        if last.visual_row
            < self.native_wheel_final_cached(
                &mut final_rows,
                &mut layouts,
                last_line,
                cols,
                cursor,
            )?
            && high > low
        {
            high = self.source_row(self.visible_row(high).saturating_sub(1));
        }
        let (mut above, mut below) = (0usize, 0usize);
        while (above < above_wanted || below < below_wanted) && low < high {
            if below < below_wanted && (below <= above || above >= above_wanted) {
                below += self.native_wheel_final_cached(
                    &mut final_rows,
                    &mut layouts,
                    high,
                    cols,
                    cursor,
                )? + 1;
                high = self.source_row(self.visible_row(high).saturating_sub(1));
            }
            if above < above_wanted && (above < below || below >= below_wanted) && low < high {
                above += self.native_wheel_final_cached(
                    &mut final_rows,
                    &mut layouts,
                    low,
                    cols,
                    cursor,
                )? + 1;
                low = self.source_row(self.visible_row(low).saturating_add(1));
            }
        }
        let mut target = cursor.line;
        if low == high {
            target = low;
        } else {
            if cursor.line < low && top.source_line > 0 {
                target = low;
            }
            if cursor.line > high && !at_end {
                target = high;
            }
        }
        if target != cursor.line {
            cursor = self.native_wheel_retain_byte_cached(&mut layouts, cursor, target, cols)?;
        }
        if cursor.line == top.source_line {
            let positions = self.native_wheel_positions_cached(&mut layouts, cursor.line, cols)?;
            let size = positions.last().map_or(0, |p| p.row * cols + p.end);
            let mut so = 4 * cols;
            if top.source_line == 0 && top.visual_row == 0 {
                so = 0;
            } else {
                so = so.min((rows - 1) * cols / 2);
            }
            while so > size && so >= 2 * cols {
                so -= cols;
            }
            if so >= cols && so > size {
                so -= cols;
            }
            let min = top.visual_row * cols + so;
            let max = top.visual_row * cols + rows * cols - so;
            let previous_vcol = if block {
                self.native_block_wheel_vcol(&mut layouts, cursor, add, cols)?
            } else {
                self.native_wheel_vcol_cached(&mut layouts, cursor, cols, insert)?
            };
            let mut vcol = previous_vcol;
            if vcol < min {
                while vcol < min {
                    vcol += cols;
                }
            } else {
                while vcol >= max {
                    vcol = vcol.saturating_sub(cols);
                }
            }
            if vcol != previous_vcol {
                if block {
                    (cursor, add) =
                        self.native_block_wheel_coladvance(&mut layouts, cursor.line, vcol, cols)?;
                } else {
                    cursor = self.native_wheel_coladvance_cached(
                        &mut layouts,
                        cursor.line,
                        vcol,
                        cols,
                        one_more,
                    )?;
                }
                goal = vcol;
            }
        }
        if cursor.line != original_line {
            if block {
                (cursor, add) =
                    self.native_block_wheel_coladvance(&mut layouts, cursor.line, goal, cols)?;
            } else {
                cursor = self.native_wheel_coladvance_cached(
                    &mut layouts,
                    cursor.line,
                    goal,
                    cols,
                    one_more,
                )?;
            }
        }
        // Neovim recalculates skipcol for a cursor in a line taller than the window.
        if cursor.line == top.source_line {
            let line_rows = self.native_wheel_final_cached(
                &mut final_rows,
                &mut layouts,
                cursor.line,
                cols,
                cursor,
            )? + 1;
            if line_rows > rows && rows <= 8 {
                let vc = if block {
                    self.native_block_wheel_vcol(&mut layouts, cursor, add, cols)?
                } else {
                    self.native_wheel_vcol_cached(&mut layouts, cursor, cols, insert)?
                };
                let max_skip = line_rows.saturating_sub(rows);
                top.visual_row = (vc / cols).saturating_sub(rows / 2).min(max_skip);
            }
        }
        Ok((top, cursor, goal, add))
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    #[allow(clippy::too_many_arguments)]
    fn wheel(
        editor: &Editor,
        top: WrappedTop,
        cursor: WrapCursor,
        goal: usize,
        rows: usize,
        columns: usize,
        delta: isize,
        mode: Mode,
        coladd: usize,
    ) -> Result<(WrappedTop, WrapCursor, usize, usize), &'static str> {
        editor.calculate_wrapped_wheel(WrappedWheelRequest {
            top,
            cursor,
            goal: Some(goal),
            rows,
            columns,
            delta,
            mode,
            coladd,
        })
    }
    #[test]
    fn large_keyboard_scroll_does_not_truncate_at_mouse_limit() {
        let text = "short\n".repeat(12_050);
        let editor = Editor::new(text);
        let (top, _, _, _) = wheel(
            &editor,
            WrappedTop {
                source_line: 0,
                visual_row: 0,
            },
            WrapCursor {
                line: 0,
                scalar: 0,
                visual_row: 0,
            },
            0,
            6,
            13,
            9_999,
            Mode::Normal,
            0,
        )
        .unwrap();
        assert_eq!(top.source_line, 9_999);
        assert_eq!(top.visual_row, 0);
    }

    #[test]
    fn committing_wheel_preserves_mode_text_and_visual_anchor() {
        let mut editor = Editor::new("abc\ndef".into());
        editor.key("v");
        assert_eq!(editor.mode, Mode::Visual);
        let anchor = editor.anchor;
        let version = editor.version;
        assert!(editor.commit_wrapped_wheel(3, Mode::Visual));
        assert_eq!(editor.mode, Mode::Visual);
        assert_eq!(editor.anchor, anchor);
        assert_eq!(editor.selection(), Some(0..4));
        assert_eq!(editor.version, version);
        assert_eq!(editor.contents(), "abc\ndef");
        assert!(!editor.commit_wrapped_wheel(0, Mode::Insert));
        assert_eq!(editor.cursor, 3);

        let mut visual_line = Editor::new("abc\ndef".into());
        visual_line.key("V");
        assert!(visual_line.commit_wrapped_wheel(3, Mode::VisualLine));
        assert_eq!(visual_line.mode, Mode::VisualLine);
        assert_eq!(visual_line.selection(), Some(0..4));
        assert_eq!(visual_line.contents(), "abc\ndef");

        let mut insert = Editor::new("abc\ndef".into());
        insert.key("i");
        assert!(insert.commit_wrapped_wheel(3, Mode::Insert));
        assert_eq!((insert.mode, insert.cursor), (Mode::Insert, 3));
        assert_eq!(insert.contents(), "abc\ndef");
    }
    #[test]
    fn block_wheel_keeps_virtual_cursor_in_editor_state() {
        let mut steps = 0;
        for text in [
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-block-vspan.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-block-extra-vspan.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-block-mixed-vspan.json"),
        ] {
            let data: serde_json::Value = serde_json::from_str(text).unwrap();
            for case in data["cases"].as_array().unwrap() {
                let lines: Vec<_> = case["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                let mut editor = Editor::new(lines.join("\n"));
                editor.indent_style.tabstop = 8;
                let rows = case["rows"].as_u64().unwrap() as usize;
                let width = case["width"].as_u64().unwrap() as usize;
                let initial = case["initial_cursor"].as_array().unwrap();
                let initial_line = initial[1].as_u64().unwrap() as usize - 1;
                let initial_byte = initial[2].as_u64().unwrap() as usize - 1;
                let initial_scalar = lines[initial_line]
                    .char_indices()
                    .take_while(|(byte, _)| *byte < initial_byte)
                    .count();
                editor.goto(initial_line, initial_scalar);
                editor.key("<C-v>");
                assert_eq!(editor.mode, Mode::VisualBlock);
                let anchor = editor.block_anchor_for_test();
                let mut cursor = editor.wrapped_block_cursor(width).unwrap().unwrap().cursor;
                let mut top = WrappedTop {
                    source_line: case["initial_topline"].as_u64().unwrap() as usize - 1,
                    visual_row: case["initial_skipcol"].as_u64().unwrap() as usize / width,
                };
                let mut goal = initial[4].as_u64().unwrap() as usize - 1;
                let mut coladd = initial[3].as_u64().unwrap() as usize;
                let version = editor.version;
                for step in case["steps"].as_array().unwrap() {
                    let delta = if step["key"] == "down" { 3 } else { -3 };
                    (top, cursor, goal, coladd) = wheel(
                        &editor,
                        top,
                        cursor,
                        goal,
                        rows,
                        width,
                        delta,
                        Mode::VisualBlock,
                        coladd,
                    )
                    .unwrap();
                    let offset = editor.line_starts[cursor.line] + cursor.scalar;
                    assert!(editor.commit_wrapped_block_wheel(offset, coladd, width, goal));
                    let resolved = editor.wrapped_block_cursor(width).unwrap().unwrap();
                    let start = resolved.cursor.visual_row * width + resolved.column + 1;
                    let expected = step["virtspan"][0].as_u64().unwrap() as usize;
                    assert_eq!(start, expected, "case step {steps}");
                    assert_eq!(editor.mode, Mode::VisualBlock);
                    assert_eq!(editor.block_anchor_for_test(), anchor);
                    assert_eq!(editor.version, version);
                    steps += 1;
                }
                assert_eq!(editor.contents(), lines.join("\n"));
            }
        }
        assert_eq!(steps, 1_116);
    }

    fn block_mapped_key(
        editor: &mut Editor,
        key: &str,
        width: usize,
        desired: &mut Option<usize>,
    ) -> Option<usize> {
        if editor.mode == Mode::VisualBlock {
            if let super::super::Effect::WrappedMove(down, count) = editor.key_with_wrap(key) {
                let virtual_cursor = editor.wrapped_block_cursor(width).unwrap().unwrap();
                let column = *desired.get_or_insert(virtual_cursor.column);
                let (cursor, coladd, goal) = editor
                    .move_wrapped_block_cursor(virtual_cursor, column, down, width, count)
                    .unwrap();
                let offset = editor.line_starts[cursor.line] + cursor.scalar;
                assert!(editor.commit_wrapped_block_wheel(offset, coladd, width, goal));
                return Some(goal);
            }
        } else {
            editor.key(key);
        }
        *desired = None;
        None
    }

    #[test]
    fn block_visual_navigation_after_wheel_matches_lazyvim() {
        let mut cases = Vec::new();
        for text in [
            include_str!("../../../Resources/Fixtures/nvim-wrap-block-lazy-postkeys.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-block-eol-lazy-postkeys.json"),
        ] {
            let data: serde_json::Value = serde_json::from_str(text).unwrap();
            cases.extend(data["cases"].as_array().unwrap().clone());
        }
        for (case_index, case) in cases.iter().enumerate() {
            let lines: Vec<_> = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let mut editor = Editor::new(lines.join("\n"));
            editor.indent_style.tabstop = 8;
            let width = case["width"].as_u64().unwrap() as usize;
            let rows = case["rows"].as_u64().unwrap() as usize;
            let mut desired = None;
            let mut chars = case["keys"].as_str().unwrap().chars();
            while let Some(ch) = chars.next() {
                let mut key = ch.to_string();
                if ch == '<' {
                    for next in chars.by_ref() {
                        key.push(next);
                        if next == '>' {
                            break;
                        }
                    }
                }
                block_mapped_key(&mut editor, &key, width, &mut desired);
            }
            assert_eq!(editor.mode, Mode::VisualBlock);
            let initial = case["before_cursor"].as_array().unwrap();
            let before = &case["before_view"];
            let mut top = WrappedTop {
                source_line: before["topline"].as_u64().unwrap() as usize - 1,
                visual_row: before["skipcol"].as_u64().unwrap() as usize / width,
            };
            let mut cursor = editor.wrapped_block_cursor(width).unwrap().unwrap().cursor;
            let mut coladd = initial[3].as_u64().unwrap() as usize;
            let mut goal = initial[4].as_u64().unwrap() as usize - 1;
            let anchor = editor.block_anchor_for_test();
            for direction in case["wheels"].as_array().unwrap() {
                let delta = if direction == "down" { 3 } else { -3 };
                (top, cursor, goal, coladd) = wheel(
                    &editor,
                    top,
                    cursor,
                    goal,
                    rows,
                    width,
                    delta,
                    Mode::VisualBlock,
                    coladd,
                )
                .unwrap();
                assert!(editor.commit_wrapped_block_wheel(
                    editor.line_starts[cursor.line] + cursor.scalar,
                    coladd,
                    width,
                    goal
                ));
            }
            desired = None;
            for (index, expected) in case["post_moves"].as_array().unwrap().iter().enumerate() {
                let key = expected["key"].as_str().unwrap();
                let goal = block_mapped_key(&mut editor, key, width, &mut desired).unwrap();
                let resolved = editor.wrapped_block_cursor(width).unwrap().unwrap();
                let offset = editor.cursor;
                let byte = editor.text[editor.line_starts[resolved.cursor.line]..offset]
                    .iter()
                    .map(|ch| ch.len_utf8())
                    .sum::<usize>()
                    + 1;
                let actual = (resolved.cursor.line + 1, byte, resolved.coladd, goal + 1);
                let expected_cursor = expected["cursor"].as_array().unwrap();
                let expected = (
                    expected_cursor[1].as_u64().unwrap() as usize,
                    expected_cursor[2].as_u64().unwrap() as usize,
                    expected_cursor[3].as_u64().unwrap() as usize,
                    expected_cursor[4].as_u64().unwrap() as usize,
                );
                assert_eq!(actual, expected, "case {case_index} move {index}");
                assert_eq!(editor.block_anchor_for_test(), anchor);
            }
            assert_eq!(editor.contents(), lines.join("\n"));
            let virtual_cursor = editor.wrapped_block_cursor(width).unwrap().unwrap();
            let view = &case["view"];
            let final_top = WrappedTop {
                source_line: view["topline"].as_u64().unwrap() as usize - 1,
                visual_row: view["skipcol"].as_u64().unwrap() as usize / width,
            };
            let viewport = editor
                .wrapped_viewport_for_cursor(final_top, rows, width, virtual_cursor.cursor)
                .unwrap();
            let bounds = editor.wrapped_block_bounds(width).unwrap().unwrap();
            let selections =
                editor.wrapped_block_selection(&viewport, bounds.lines, bounds.columns, width);
            let mut actual = vec![vec![0u64; width]; rows];
            for selection in selections {
                for column in selection.columns {
                    actual[selection.row][column] = 1;
                }
            }
            if let Some(cursor) =
                editor.wrapped_virtual_cursor_cell(&viewport, virtual_cursor, width)
            {
                for column in cursor.columns {
                    actual[cursor.row][column] = 0;
                }
            }
            let expected: Vec<Vec<u64>> = case["selected"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    row.as_array()
                        .unwrap()
                        .iter()
                        .map(|cell| cell.as_u64().unwrap())
                        .collect()
                })
                .collect();
            assert_eq!(actual, expected, "selection after keys case {case_index}");
        }
    }

    #[test]
    fn invalid_inputs_do_not_change_the_editor() {
        let editor = Editor::new("alpha\nbeta".into());
        let cursor = editor.wrapped_cursor_at(0, 20, false).unwrap().unwrap();
        let top = WrappedTop {
            source_line: 0,
            visual_row: 0,
        };
        assert_eq!(
            wheel(&editor, top, cursor, 0, 10, 20, 0, Mode::Normal, 0),
            Ok((top, cursor, 0, 0))
        );
        assert!(wheel(&editor, top, cursor, 0, 0, 20, 3, Mode::Normal, 0).is_err());
        assert!(wheel(&editor, top, cursor, 0, 10, 20, 3, Mode::Search, 0).is_err());
        let invalid = WrapCursor {
            line: 100,
            ..cursor
        };
        assert!(wheel(&editor, top, invalid, 0, 10, 20, 3, Mode::Normal, 0).is_err());
        assert_eq!(editor.contents(), "alpha\nbeta");
    }
    #[test]
    fn compare_wheel_reference() {
        let mut cases = Vec::new();
        for text in [
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-wide-unicode.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-extra-geometries.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-mixed-directions.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-visual.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-visual-line.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-insert.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-matrix.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-control-scroll.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-block-vspan.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-block-extra-vspan.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-wheel-block-mixed-vspan.json"),
        ] {
            let data: serde_json::Value = serde_json::from_str(text).unwrap();
            for mut case in data["cases"].as_array().unwrap().clone() {
                if case.get("width").is_none() {
                    case["width"] = serde_json::json!(20);
                    case["initial_cursor"] = serde_json::json!([0, 1, 1, 0, 1]);
                }
                cases.push(case);
            }
        }
        let mut mismatches = 0;
        let mut bad_top = 0;
        let mut bad_cursor = 0;
        let mut bad_goal = 0;
        let mut steps = 0;
        let mut examples = Vec::new();
        for (case_idx, case) in cases.iter().enumerate() {
            let lines: Vec<_> = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let mut editor = Editor::new(lines.join("\n"));
            editor.indent_style.tabstop = 8;
            let mode = case["steps"][0]["mode"].as_str().unwrap_or("n");
            let one_more = matches!(mode, "v" | "V" | "i" | "\u{16}");
            let mode = match mode {
                "v" => Mode::Visual,
                "V" => Mode::VisualLine,
                "i" => Mode::Insert,
                "\u{16}" => Mode::VisualBlock,
                _ => Mode::Normal,
            };
            let rows = case["rows"].as_u64().unwrap() as usize;
            let width = case["width"].as_u64().unwrap() as usize;
            let mut top = WrappedTop {
                source_line: case["initial_topline"].as_u64().unwrap() as usize - 1,
                visual_row: case["initial_skipcol"].as_u64().unwrap() as usize / width,
            };
            let initial = case["initial_cursor"].as_array().unwrap();
            let start_line = initial[1].as_u64().unwrap() as usize - 1;
            let start_byte = initial[2].as_u64().unwrap() as usize - 1;
            let start_scalar = lines[start_line]
                .char_indices()
                .take_while(|(byte, _)| *byte < start_byte)
                .count();
            let mut cursor = editor
                .wrapped_cursor_at(
                    editor.line_starts[start_line] + start_scalar,
                    width,
                    one_more,
                )
                .unwrap()
                .unwrap();
            let mut goal = initial[4].as_u64().unwrap() as usize - 1;
            let mut coladd = initial[3].as_u64().unwrap() as usize;
            for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let amount = case["scroll_amount"].as_i64().unwrap_or(3) as isize;
                let delta = if step["key"] == "down" {
                    amount
                } else {
                    -amount
                };
                (top, cursor, goal, coladd) =
                    wheel(&editor, top, cursor, goal, rows, width, delta, mode, coladd).unwrap();
                let pos = step["cursor"].as_array().unwrap();
                let line = pos[1].as_u64().unwrap() as usize - 1;
                let byte = pos[2].as_u64().unwrap() as usize - 1;
                let scalar = lines[line]
                    .char_indices()
                    .take_while(|(byte_index, _)| *byte_index < byte)
                    .count();
                let mut expected = editor
                    .wrapped_cursor_at(editor.line_starts[line] + scalar, width, one_more)
                    .unwrap()
                    .unwrap();
                let expected_add = pos[3].as_u64().unwrap() as usize;
                if mode == Mode::VisualBlock {
                    expected.visual_row =
                        (step["virtspan"][0].as_u64().unwrap() as usize - 1) / width;
                }
                let expected_top = WrappedTop {
                    source_line: step["topline"].as_u64().unwrap() as usize - 1,
                    visual_row: step["skipcol"].as_u64().unwrap() as usize / width,
                };
                steps += 1;
                if goal + 1 != pos[4].as_u64().unwrap() as usize {
                    bad_goal += 1;
                    if examples.len() < 12 {
                        examples.push(format!("case={case_idx} step={index} goal={goal} expected={} cursor={cursor:?} top={top:?}", pos[4]));
                    }
                }
                if top != expected_top {
                    bad_top += 1;
                }
                if cursor != expected || coladd != expected_add {
                    bad_cursor += 1;
                }
                if top != expected_top || cursor != expected || coladd != expected_add {
                    mismatches += 1;
                    if examples.len() < 12 {
                        examples.push(format!("case={case_idx} step={index} key={} top={top:?}/{expected_top:?} cursor={cursor:?}/{expected:?}",step["key"]));
                    }
                }
            }
        }
        println!(
            "WHEEL_AUDIT steps={steps} mismatches={mismatches} top={bad_top} cursor={bad_cursor} goal={bad_goal} examples={examples:?}"
        );
        assert_eq!(steps, 4_154);
        assert_eq!(
            (mismatches, bad_goal),
            (0, 0),
            "wheel model differs from reference: {examples:?}"
        );
    }
}

#[cfg(test)]
mod wheel_cache_properties {
    use super::*;
    #[test]
    fn cached_positions_have_a_per_event_bound() {
        let line = "xy ".repeat(23_000);
        let editor =
            Editor::new([line.clone(), line.clone(), line.clone(), line.clone(), line].join("\n"));
        let mut cache = std::collections::HashMap::new();
        for index in 0..5 {
            assert_eq!(
                editor
                    .native_wheel_positions_cached(&mut cache, index, 80)
                    .unwrap()
                    .len(),
                69_000
            );
            assert!(cache.values().map(Vec::len).sum::<usize>() <= 262_144);
        }
    }
    #[test]
    fn positions_are_monotonic_for_binary_search() {
        let alphabet = ['a', ' ', '\t', '界', '\u{301}'];
        for width in [2, 3, 5, 13, 20] {
            for mut n in 0..15625usize {
                let mut line = Vec::new();
                for _ in 0..6 {
                    line.push(alphabet[n % 5]);
                    n /= 5;
                }
                let editor = Editor::new(String::new());
                let positions = editor.wrapped_positions(&line, width).unwrap();
                for pair in positions.windows(2) {
                    assert!(
                        pair[0].row * width + pair[0].start <= pair[1].row * width + pair[1].start,
                        "width={width} line={line:?} positions={positions:?}"
                    );
                }
            }
        }
    }
}
