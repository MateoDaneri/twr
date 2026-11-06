//! Experimental visual positions for wrapped lines. Not yet wired to the UI.
use super::Editor;
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrapPosition {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

/// A visible cell run and the scalar range that produced it. The editor can
/// use the source range for selection, highlights and mouse hit testing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedCell {
    pub source: Range<usize>,
    pub columns: Range<usize>,
    pub tab: bool,
    pub standalone_mark: bool,
    pub padding: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrappedTop {
    pub source_line: usize,
    pub visual_row: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedViewportRow {
    pub source: Range<usize>,
    pub visual_row: usize,
    pub folded_lines: Option<usize>,
    pub end_cursor: Option<usize>,
    pub cells: Vec<WrappedCell>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedCursorCell {
    pub row: usize,
    pub columns: Range<usize>,
}

/// A wrapped cursor that may be inside a tab or beyond the end of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrappedVirtualCursor {
    pub cursor: WrapCursor,
    pub column: usize,
    pub cell_width: usize,
    pub coladd: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedMouseHit {
    pub cursor: WrapCursor,
    /// Desired zero-based virtual column, including preceding wrapped rows.
    pub wanted_column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedSelectionCells {
    pub row: usize,
    pub columns: Range<usize>,
}

/// Translate an absolute scalar selection into visible cells. Fold summaries
/// and linewise or blockwise extension are handled by their own adapters.
pub fn wrapped_selection_cells(
    viewport: &[WrappedViewportRow],
    selection: Range<usize>,
) -> Vec<WrappedSelectionCells> {
    let mut output: Vec<WrappedSelectionCells> = Vec::new();
    for (row, visible) in viewport.iter().enumerate() {
        if visible.folded_lines.is_some() {
            continue;
        }
        for cell in &visible.cells {
            let start = visible.source.start.saturating_add(cell.source.start);
            let end = visible.source.start.saturating_add(cell.source.end);
            if start >= selection.end || end <= selection.start {
                continue;
            }
            if let Some(last) = output.last_mut()
                && last.row == row
                && cell.columns.start <= last.columns.end
            {
                last.columns.end = last.columns.end.max(cell.columns.end);
            } else {
                output.push(WrappedSelectionCells {
                    row,
                    columns: cell.columns.clone(),
                });
            }
        }
    }
    output
}

impl WrappedCell {
    pub fn append_text(&self, text: &[char], output: &mut String) {
        if self.tab {
            output.extend(std::iter::repeat_n(' ', self.columns.len()));
        } else if self.source.end <= text.len() {
            if self.standalone_mark {
                output.push(' ');
            }
            output.extend(&text[self.source.clone()]);
            output.extend(std::iter::repeat_n(' ', self.padding));
        }
    }
}

/// Extract one visual row without scanning preceding off-screen scalars.
pub fn wrapped_row_cells(
    text: &[char],
    positions: &[WrapPosition],
    row: usize,
    width: usize,
) -> Result<Vec<WrappedCell>, &'static str> {
    if width == 0 || positions.len() != text.len() {
        return Err("Wrap: posiciones o ancho inválidos");
    }
    let first =
        positions.partition_point(|p| p.row.saturating_add(p.end.saturating_sub(1) / width) < row);
    let mut cells: Vec<WrappedCell> = Vec::new();
    for index in first..positions.len() {
        let position = positions[index];
        if position.row > row {
            break;
        }
        let offset = row.saturating_sub(position.row).saturating_mul(width);
        if position.end <= offset {
            continue;
        }
        let start = position.start.saturating_sub(offset).min(width);
        let end = position.end.saturating_sub(offset).min(width);
        if start >= end {
            continue;
        }
        let ch = text[index];
        if ch.width() == Some(0)
            && index > 0
            && position == positions[index - 1]
            && let Some(previous) = cells.last_mut()
            && previous.source.end == index
            && !previous.tab
        {
            previous.source.end += 1;
            continue;
        }
        let tab = ch == '\t';
        let cell_width = ch.width().unwrap_or(1).max(1);
        cells.push(WrappedCell {
            source: index..index + 1,
            columns: start..end,
            tab,
            standalone_mark: ch.width() == Some(0),
            padding: if tab {
                0
            } else {
                (end - start).saturating_sub(cell_width)
            },
        });
    }
    Ok(cells)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrapCursor {
    pub line: usize,
    pub scalar: usize,
    /// Zero-based visual row within the logical line. A tab can cover
    /// several rows while the scalar cursor remains on the same character.
    pub visual_row: usize,
}

fn nearest_on_row(positions: &[WrapPosition], row: usize, desired: usize) -> Option<usize> {
    let mut best = None;
    for (index, position) in positions.iter().enumerate().filter(|(_, p)| p.row == row) {
        if position.start <= desired {
            if best.is_none_or(|(_, start)| position.start > start) {
                best = Some((index, position.start));
            }
        } else if best.is_none() {
            best = Some((index, position.start));
            break;
        }
    }
    best.map(|(index, _)| index)
}

fn nearest_visible_on_row(
    positions: &[WrapPosition],
    row: usize,
    desired: usize,
    width: usize,
) -> Option<usize> {
    let first =
        positions.partition_point(|p| p.row.saturating_add(p.end.saturating_sub(1) / width) < row);
    let mut nearest = None;
    for (index, position) in positions.iter().enumerate().skip(first) {
        if position.row > row {
            break;
        }
        let start = if row == position.row {
            position.start
        } else {
            0
        };
        let distance = desired.saturating_sub(start);
        if start <= desired && nearest.is_none_or(|(_, old)| distance < old) {
            nearest = Some((index, distance));
        } else if nearest.is_none() {
            nearest = Some((index, usize::MAX));
        }
    }
    nearest.map(|(index, _)| index)
}

/// Move one visual row through a sequence of unfolded logical lines.
/// Empty logical lines have one visual row and cursor scalar zero.
/// `visual_row` keeps track of positions within a multirow tab.
pub fn move_in_wrapped_lines(
    lines: &[Vec<WrapPosition>],
    cursor: WrapCursor,
    desired: usize,
    down: bool,
    width: usize,
) -> WrapCursor {
    if width == 0 {
        return cursor;
    }
    let Some(positions) = lines.get(cursor.line) else {
        return cursor;
    };
    if positions.is_empty() && cursor.scalar == 0 && cursor.visual_row == 0 {
    } else if let Some(position) = positions.get(cursor.scalar) {
        let end_row = position.row + position.end.saturating_sub(1) / width;
        if cursor.visual_row < position.row || cursor.visual_row > end_row {
            return cursor;
        }
    } else {
        return cursor;
    }
    let last_row = |line: &[WrapPosition]| {
        line.iter()
            .map(|p| p.row + p.end.saturating_sub(1) / width)
            .max()
            .unwrap_or(0)
    };
    let current_row = cursor.visual_row;
    let line_last_row = last_row(positions);
    let target = if down && current_row < line_last_row {
        Some((cursor.line, current_row + 1))
    } else if !down && current_row > 0 {
        Some((cursor.line, current_row - 1))
    } else if down && cursor.line + 1 < lines.len() {
        Some((cursor.line + 1, 0))
    } else if !down && cursor.line > 0 {
        let line = cursor.line - 1;
        Some((line, last_row(&lines[line])))
    } else {
        None
    };
    let Some((line, row)) = target else {
        return cursor;
    };
    WrapCursor {
        line,
        scalar: nearest_visible_on_row(&lines[line], row, desired, width).unwrap_or(0),
        visual_row: row,
    }
}

/// Move within one wrapped logical line; crossing logical lines and folded
/// ranges belongs to the future viewport adapter.
pub fn move_in_wrapped_line(
    positions: &[WrapPosition],
    current: usize,
    desired: usize,
    down: bool,
) -> usize {
    let Some(at) = positions.get(current) else {
        return current;
    };
    let target = if down {
        at.row.saturating_add(1)
    } else if let Some(row) = at.row.checked_sub(1) {
        row
    } else {
        return current;
    };
    nearest_on_row(positions, target, desired).unwrap_or(current)
}

fn break_character(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\t' | '!' | '@' | '*' | '-' | '+' | ';' | ':' | ',' | '.' | '/' | '?'
    )
}

impl Editor {
    /// Experimental exclusive range for an operator over visual rows.
    /// A forward motion ending at column zero can become linewise, as in Vim.
    /// The caller must still validate window, buffer, version and operator.
    pub fn wrapped_operator_range(
        &self,
        origin: usize,
        target: usize,
    ) -> Result<Option<(Range<usize>, bool)>, &'static str> {
        if origin > self.text.len() || target > self.text.len() {
            return Err("Wrap: rango de operador fuera del buffer");
        }
        if origin == target {
            return Ok(None);
        }
        let start = origin.min(target);
        let end = origin.max(target);
        let line_start = self.line_start(start);
        let first_nonblank = line_start
            + self.text[line_start..self.line_end(start)]
                .iter()
                .take_while(|ch| matches!(ch, ' ' | '\t'))
                .count();
        let ends_at_line_start =
            end == self.line_start(end) && self.source_line_at(end) > self.source_line_at(start);
        let linewise = ends_at_line_start && start <= first_nonblank;
        Ok(Some((
            if linewise {
                line_start..end
            } else if ends_at_line_start {
                start..end.saturating_sub(1)
            } else if target > origin && self.folded_line_count(target).is_some() {
                let last = self.folded_last_row(self.source_line_at(target));
                start..self.line_end(self.line_starts[last])
            } else {
                start..end
            },
            linewise,
        )))
    }

    /// Complete a synchronous view-resolved operator only against its origin.
    pub fn commit_wrapped_operator(
        &mut self,
        op: char,
        origin: usize,
        version: u64,
        target: usize,
    ) -> bool {
        if !matches!(op, 'd' | 'c' | 'y')
            || self.mode != super::Mode::Normal
            || self.cursor != origin
            || self.version != version
        {
            return false;
        }
        let Ok(range) = self.wrapped_operator_range(origin, target) else {
            return false;
        };
        if let Some((range, linewise)) = range {
            let start = range.start;
            self.operate(op, range, linewise);
            if op == 'y' {
                self.cursor = start;
                self.clamp();
            }
        } else {
            self.chosen_register = None;
        }
        self.finish_change();
        true
    }
    /// Initial desired column for a wrapped vertical motion in this pane.
    pub fn wrapped_cursor_column(&self, cursor: WrapCursor, width: usize) -> Option<usize> {
        if width == 0 || width > 200 || cursor.line >= self.line_starts.len() {
            return None;
        }
        let start = self.line_starts[cursor.line];
        let text = &self.text[start..self.line_end(start)];
        if cursor.scalar > text.len() {
            return None;
        }
        if self.folded_line_count(start).is_some() {
            return Some(
                self.display_text_column(&text[..cursor.scalar])
                    .min(width - 1),
            );
        }
        let positions = self.wrapped_positions(text, width).ok()?;
        if cursor.scalar == text.len() {
            return Some(positions.last().map_or(0, |p| p.end % width));
        }
        let position = positions.get(cursor.scalar)?;
        let offset = cursor
            .visual_row
            .checked_sub(position.row)?
            .checked_mul(width)?;
        if position.end <= offset {
            return None;
        }
        let start = position.start.saturating_sub(offset).min(width - 1);
        let end = position.end.saturating_sub(offset).min(width);
        Some(if text[cursor.scalar] == '\t' {
            end.saturating_sub(1)
        } else {
            start
        })
    }

    /// Move one visual row without allocating a model for the entire buffer.
    /// The caller owns the desired column and per-view virtual cursor state.
    pub fn move_wrapped_cursor(
        &self,
        cursor: WrapCursor,
        desired: usize,
        down: bool,
        width: usize,
    ) -> Result<(usize, WrapCursor), &'static str> {
        self.move_wrapped_cursor_count(cursor, desired, down, width, 1)
    }

    /// Move by visual rows while computing positions only once per source line.
    pub fn move_wrapped_cursor_count(
        &self,
        cursor: WrapCursor,
        desired: usize,
        down: bool,
        width: usize,
        count: usize,
    ) -> Result<(usize, WrapCursor), &'static str> {
        if width == 0 || width > 200 || cursor.line >= self.line_starts.len() {
            return Err("Wrap: cursor o ancho fuera de límites");
        }
        let visible = self.visible_row(cursor.line);
        if self.source_row(visible) != cursor.line {
            return Err("Wrap: cursor dentro de pliegue cerrado");
        }
        let make_line = |line: usize| {
            let start = self.line_starts[line];
            if self.folded_line_count(start).is_some() {
                if self.line_end(start).saturating_sub(start) > 131_072 {
                    return Err("Wrap: línea o ancho fuera de límites");
                }
                let mut column = 0usize;
                let mut positions: Vec<WrapPosition> = Vec::new();
                for &ch in &self.text[start..self.line_end(start)] {
                    let cells = self.display_cell_width(ch, column);
                    if cells == 0 {
                        positions.push(positions.last().copied().unwrap_or(WrapPosition {
                            row: 0,
                            start: 0,
                            end: 1,
                        }));
                        continue;
                    }
                    let begin = column.min(width - 1);
                    column = column.saturating_add(cells);
                    positions.push(WrapPosition {
                        row: 0,
                        start: begin,
                        end: column.min(width).max(begin + 1),
                    });
                }
                Ok(positions)
            } else {
                self.wrapped_positions(&self.text[start..self.line_end(start)], width)
            }
        };
        let mut positions = make_line(cursor.line)?;
        let valid = if positions.is_empty() {
            cursor.scalar == 0 && cursor.visual_row == 0
        } else {
            positions.get(cursor.scalar).is_some_and(|p| {
                cursor.visual_row >= p.row
                    && cursor.visual_row <= p.row + p.end.saturating_sub(1) / width
            })
        };
        if !valid {
            return Err("Wrap: cursor visual inválido");
        }
        let mut line = cursor.line;
        let mut row = cursor.visual_row;
        let mut remaining = count.min(9999);
        let mut moved = false;
        while remaining > 0 {
            let last = positions
                .last()
                .map_or(0, |p| p.row + p.end.saturating_sub(1) / width);
            let available = if down { last.saturating_sub(row) } else { row };
            let step = available.min(remaining);
            if step > 0 {
                row = if down { row + step } else { row - step };
                remaining -= step;
                moved = true;
                continue;
            }
            let current_visible = self.visible_row(line);
            let neighbor = self.source_row(if down {
                current_visible.saturating_add(1)
            } else {
                current_visible.saturating_sub(1)
            });
            if neighbor == line {
                break;
            }
            line = neighbor;
            positions = make_line(line)?;
            row = if down {
                0
            } else {
                positions
                    .last()
                    .map_or(0, |p| p.row + p.end.saturating_sub(1) / width)
            };
            remaining -= 1;
            moved = true;
        }
        if !moved {
            let offset = self.line_starts[cursor.line] + cursor.scalar;
            return Ok((offset, cursor));
        }
        let scalar = nearest_visible_on_row(&positions, row, desired, width).unwrap_or(0);
        let offset = self.line_starts[line]
            .saturating_add(scalar)
            .min(self.line_end(self.line_starts[line]));
        Ok((
            offset,
            WrapCursor {
                line,
                scalar,
                visual_row: row,
            },
        ))
    }

    /// Keep a wrapped cursor visible with smooth scrolling and zero scrolloff.
    /// Other Neovim scrolloff and nosmoothscroll policies are not modeled here.
    pub fn follow_wrapped_cursor(
        &self,
        top: WrappedTop,
        cursor: WrapCursor,
        rows: usize,
        columns: usize,
    ) -> Result<WrappedTop, &'static str> {
        if rows == 0 || rows > 100 {
            return Err("Wrap: viewport fuera de límites");
        }
        let viewport = self.wrapped_viewport_for_cursor(top, rows, columns, cursor)?;
        let Some(first) = viewport.first() else {
            return Err("Wrap: viewport vacío");
        };
        let normalized = WrappedTop {
            source_line: self.source_line_at(first.source.start),
            visual_row: first.visual_row,
        };
        if self.wrapped_cursor_cell(&viewport, cursor).is_some() {
            return Ok(normalized);
        }
        let mut next = WrappedTop {
            source_line: cursor.line,
            visual_row: cursor.visual_row,
        };
        if (cursor.line, cursor.visual_row) >= (normalized.source_line, normalized.visual_row) {
            let mut remaining = rows - 1;
            while remaining > 0 {
                // Moving within one logical line does not require rebuilding
                // its complete wrapped positions for every visual row.
                let within_line = next.visual_row.min(remaining);
                if within_line > 0 {
                    next.visual_row -= within_line;
                    remaining -= within_line;
                    continue;
                }
                let previous = self.step_wrapped_top_for_cursor(next, false, columns, cursor)?;
                if previous == next {
                    break;
                }
                next = previous;
                remaining -= 1;
            }
        }
        let viewport = self.wrapped_viewport_for_cursor(next, rows, columns, cursor)?;
        if self.wrapped_cursor_cell(&viewport, cursor).is_none() {
            return Err("Wrap: cursor fuera del viewport");
        }
        Ok(next)
    }

    /// Calculate the centered visual top for a wrapped cursor line.
    ///
    /// This is the smoothscroll/`zz` model only. Oversized lines retain the
    /// existing visual top when the cursor fits; other viewport and cursor
    /// corrections need separate Neovim references before UI dispatch.
    pub fn wrapped_center_top(
        &self,
        top: WrappedTop,
        cursor: WrapCursor,
        rows: usize,
        columns: usize,
    ) -> Result<WrappedTop, &'static str> {
        if rows == 0 || rows > 100 || columns == 0 || columns > 200 {
            return Err("Wrap: viewport fuera de límites");
        }
        if cursor.line >= self.line_starts.len() {
            return Err("Wrap: cursor fuera de límites");
        }
        let height = self.wrapped_final_row(cursor.line, columns, Some(cursor))? + 1;
        if height > rows {
            return self.follow_wrapped_cursor(top, cursor, rows, columns);
        }
        let mut centered = WrappedTop {
            source_line: cursor.line,
            visual_row: 0,
        };
        for _ in 0..((rows - height) / 2) {
            let next = self.step_wrapped_top(centered, false, columns)?;
            if next == centered {
                break;
            }
            centered = next;
        }
        Ok(centered)
    }

    /// Calculate the top anchor for `zt` with smooth scrolling and no filler
    /// lines. Whole lines are considered for the requested scrolloff, as in
    /// Neovim; an existing partial top row is retained when the line stays.
    pub fn wrapped_align_top(
        &self,
        top: WrappedTop,
        cursor: WrapCursor,
        rows: usize,
        columns: usize,
        scrolloff: usize,
    ) -> Result<WrappedTop, &'static str> {
        if rows == 0
            || rows > 100
            || columns == 0
            || columns > 200
            || cursor.line >= self.line_starts.len()
        {
            return Err("Wrap: viewport o cursor fuera de límites");
        }
        let visible = self.visible_row(cursor.line);
        let mut new_line = self.source_row(visible);
        let mut previous = visible;
        let mut following = visible.saturating_add(1);
        let mut extra = 0usize;
        let mut used = self.wrapped_final_row(new_line, columns, Some(cursor))? + 1;
        while previous > 0 {
            if extra >= scrolloff {
                break;
            }
            previous -= 1;
            let line = self.source_row(previous);
            let height = self.wrapped_final_row(line, columns, None)? + 1;
            used = used.saturating_add(height);
            let below = self.source_row(following);
            let after_below = self.source_row(following.saturating_add(1));
            if extra.saturating_add(height) <= scrolloff
                && below != after_below
                && below != new_line
            {
                used = used.saturating_add(self.wrapped_final_row(below, columns, None)? + 1);
            }
            if used > rows {
                let fallback = self.wrapped_halfway_bounded(cursor, rows, columns)?;
                if fallback.source_line == top.source_line && top.visual_row > 0 {
                    return self.follow_wrapped_cursor(top, cursor, rows, columns);
                }
                return Ok(fallback);
            }
            extra = extra.saturating_add(height);
            new_line = line;
            following = following.saturating_add(1);
        }
        let visual_row = if new_line == top.source_line {
            top.visual_row
        } else {
            0
        };
        Ok(WrappedTop {
            source_line: new_line,
            visual_row,
        })
    }

    /// Experimental `zb` anchor and cursor correction, pending reference
    /// comparisons across small views, folds and Unicode.
    pub fn wrapped_align_bottom(
        &self,
        old_top: WrappedTop,
        cursor: WrapCursor,
        rows: usize,
        columns: usize,
        scrolloff: usize,
    ) -> Result<(WrappedTop, WrapCursor), &'static str> {
        if rows == 0
            || rows > 100
            || columns == 0
            || columns > 200
            || cursor.line >= self.line_starts.len()
        {
            return Err("Wrap: viewport o cursor fuera de límites");
        }
        let final_row = self.wrapped_final_row(cursor.line, columns, Some(cursor))?;
        let mut top = WrappedTop {
            source_line: cursor.line,
            visual_row: final_row,
        };
        let mut remaining = rows - 1;
        while remaining > 0 {
            let within_line = top.visual_row.min(remaining);
            if within_line > 0 {
                top.visual_row -= within_line;
                remaining -= within_line;
                continue;
            }
            let next = self.step_wrapped_top(top, false, columns)?;
            if next == top {
                break;
            }
            top = next;
            remaining -= 1;
        }
        let filled = rows - remaining;
        // In the set-top/bottom path Neovim resets an unchanged skip column
        // before cursor_correct_sms applies the visible-range correction.
        let next_line = self.source_row(self.visible_row(cursor.line).saturating_add(1));
        let reset_partial = top.visual_row > 0
            && top.visual_row == old_top.visual_row
            && (top.source_line == cursor.line || (scrolloff == 0 && next_line != cursor.line));
        if reset_partial {
            top.visual_row = 0;
        }
        // After placing the logical cursor line at the bottom, Neovim counts
        // full neighboring lines for scrolloff before scrolling visual rows.
        let visible = self.visible_row(cursor.line);
        let mut above = visible;
        let mut below = visible;
        let mut used = final_row + 1;
        let mut extra = 0usize;
        let mut scrolled = 0isize;
        let empty = rows.saturating_sub(filled);
        while above > 0 {
            let below_line = self.source_row(below);
            let next_below = self.source_row(below.saturating_add(1));
            if extra >= scrolloff || next_below == below_line {
                break;
            }
            above -= 1;
            let preceding = self.source_row(above);
            used = used.saturating_add(self.wrapped_final_row(preceding, columns, None)? + 1);
            if used > rows {
                break;
            }
            below += 1;
            let height = self.wrapped_final_row(next_below, columns, None)? + 1;
            used = used.saturating_add(height);
            if used > rows {
                break;
            }
            if extra < scrolloff {
                extra = extra.saturating_add(height);
                scrolled += height as isize;
                if below == visible + 1 {
                    scrolled -= empty as isize;
                }
            }
        }
        if scrolled > 0 && used > rows {
            top = self.wrapped_halfway_bounded(cursor, rows, columns)?;
        } else {
            for _ in 0..scrolled.max(0) {
                let next = self.step_wrapped_top(top, true, columns)?;
                if next == top {
                    break;
                }
                top = next;
            }
        }
        // A reset of the old partial top can put the cursor below the screen.
        // With zero scrolloff Neovim's next redraw counts whole lines below
        // the approximate botline, then scrolls or falls back to halfway.
        if reset_partial && scrolloff == 0 && top.source_line != cursor.line {
            let viewport = self.wrapped_viewport_for_cursor(top, rows, columns, cursor)?;
            if self.wrapped_cursor_cell(&viewport, cursor).is_none()
                && let Some(last) = viewport.last()
            {
                let line = self.source_line_at(last.source.start);
                let last_row = self.wrapped_final_row(line, columns, Some(cursor))?;
                let bot = self.visible_row(line) + usize::from(last.visual_row == last_row);
                let mut above = self.visible_row(cursor.line);
                let mut below = above;
                let mut used = final_row + 1;
                let mut scrolled = used;
                while above > bot {
                    above -= 1;
                    let height = self.wrapped_final_row(self.source_row(above), columns, None)? + 1;
                    used = used.saturating_add(height);
                    if used > rows {
                        break;
                    }
                    if above >= bot {
                        scrolled = scrolled.saturating_add(height);
                    }
                    let current = self.source_row(below);
                    let next = self.source_row(below.saturating_add(1));
                    if current != next {
                        below += 1;
                        used =
                            used.saturating_add(self.wrapped_final_row(next, columns, None)? + 1);
                        if used > rows {
                            break;
                        }
                    }
                }
                if used > rows {
                    top = self.wrapped_halfway_bounded(cursor, rows, columns)?;
                } else {
                    for _ in 0..scrolled {
                        let next = self.step_wrapped_top(top, true, columns)?;
                        if next == top {
                            break;
                        }
                        top = next;
                    }
                }
            }
        }
        let mut moved = cursor;
        if top.source_line == cursor.line {
            let mut so = scrolloff.saturating_mul(columns);
            let available = rows.saturating_sub(1).saturating_mul(columns);
            if top.source_line == 0 && top.visual_row == 0 {
                so = 0;
            } else {
                so = so.min(available / 2);
            }
            let line_start = self.line_starts[cursor.line];
            let line_len =
                self.display_text_column(&self.text[line_start..self.line_end(line_start)]);
            while so > line_len && so >= columns {
                so -= columns;
            }
            let marker = if top.visual_row > 0 && so == 0 { 3 } else { 0 };
            let lower = top
                .visual_row
                .saturating_mul(columns)
                .saturating_add(if so != 0 { so } else { marker });
            let upper = top
                .visual_row
                .saturating_mul(columns)
                .saturating_add(rows.saturating_mul(columns))
                .saturating_sub(so);
            let column = self
                .wrapped_cursor_column(cursor, columns)
                .ok_or("Wrap: columna inválida")?;
            let current = cursor
                .visual_row
                .saturating_mul(columns)
                .saturating_add(column);
            let mut desired = current;
            while desired < lower {
                desired = desired.saturating_add(columns);
            }
            while desired >= upper && desired >= columns {
                desired -= columns;
            }
            if desired != current {
                let target_row = desired / columns;
                let (_, new_cursor) = self.move_wrapped_cursor_count(
                    cursor,
                    column,
                    target_row > cursor.visual_row,
                    columns,
                    target_row.abs_diff(cursor.visual_row),
                )?;
                moved = new_cursor;
            }
        }
        // When the marker clips a long topline, the redraw keeps the corrected
        // cursor inside the upper scrolloff margin if that can be done without
        // exposing an earlier source line.
        if top.source_line == moved.line && top.visual_row > 0 {
            let margin = scrolloff.min(rows / 2);
            let above = moved.visual_row.saturating_sub(top.visual_row);
            top.visual_row = top.visual_row.saturating_sub(margin.saturating_sub(above));
        }
        Ok((top, moved))
    }

    /// The fallback for insufficient space counts whole visible lines on both
    /// sides before choosing a partial first line. It differs from explicit
    /// `zz`, which centers even when the cursor is at the end of the file.
    fn wrapped_halfway_bounded(
        &self,
        cursor: WrapCursor,
        rows: usize,
        columns: usize,
    ) -> Result<WrappedTop, &'static str> {
        let visible = self.visible_row(cursor.line);
        let mut previous = visible;
        let mut following = visible;
        let mut used = self.wrapped_final_row(cursor.line, columns, Some(cursor))? + 1;
        let mut top = WrappedTop {
            source_line: cursor.line,
            visual_row: 0,
        };
        while previous > 0 {
            previous -= 1;
            let line = self.source_row(previous);
            let height = self.wrapped_final_row(line, columns, None)? + 1;
            used = used.saturating_add(height);
            let below = self.source_row(following);
            let next = self.source_row(following.saturating_add(1));
            if next != below {
                following += 1;
                used = used.saturating_add(self.wrapped_final_row(next, columns, None)? + 1);
            }
            if used > rows {
                if used.saturating_sub(height) < rows {
                    top = WrappedTop {
                        source_line: line,
                        visual_row: used - rows,
                    };
                }
                break;
            }
            top = WrappedTop {
                source_line: line,
                visual_row: 0,
            };
        }
        Ok(top)
    }

    /// Experimental smooth-scroll margin around the cursor, bounded by rows.
    pub fn follow_wrapped_cursor_with_margin(
        &self,
        top: WrappedTop,
        cursor: WrapCursor,
        rows: usize,
        columns: usize,
        scrolloff: usize,
    ) -> Result<WrappedTop, &'static str> {
        let mut top = self.follow_wrapped_cursor(top, cursor, rows, columns)?;
        let viewport = self.wrapped_viewport_for_cursor(top, rows, columns, cursor)?;
        let row = self
            .wrapped_cursor_cell(&viewport, cursor)
            .ok_or("Wrap: cursor fuera del viewport")?
            .row;
        let upper = scrolloff.min(rows / 2);
        let lower = scrolloff.min(rows.saturating_sub(1) / 2);
        let (down, distance) = if row < upper {
            (false, upper - row)
        } else if row > rows.saturating_sub(1 + lower) {
            (true, row - rows.saturating_sub(1 + lower))
        } else {
            return Ok(top);
        };
        // Do not introduce empty rows below the file just to satisfy the
        // requested margin. Count available rows from the current viewport's
        // bottom in logical-line chunks, without laying out their cells again.
        let distance = if down && viewport.len() == rows {
            let last = viewport.last().ok_or("Wrap: viewport vacío")?;
            let mut line = self.source_line_at(last.source.start);
            let mut visual = last.visual_row;
            let mut available = 0;
            while available < distance {
                let final_row = self.wrapped_final_row(line, columns, Some(cursor))?;
                let advance = final_row.saturating_sub(visual).min(distance - available);
                available += advance;
                if available == distance {
                    break;
                }
                let next = self.source_row(self.visible_row(line).saturating_add(1));
                if next == line {
                    break;
                }
                line = next;
                visual = 0;
                available += 1;
            }
            available
        } else if down {
            0
        } else {
            distance
        };
        for _ in 0..distance {
            let next = self.step_wrapped_top_for_cursor(top, down, columns, cursor)?;
            if next == top {
                break;
            }
            top = next;
        }
        Ok(top)
    }

    /// Project a logical cursor hidden by a closed fold onto its summary for
    /// painting and pane focus, without moving the editor's logical cursor.
    pub fn wrapped_display_cursor_at(
        &self,
        offset: usize,
        width: usize,
        allow_end: bool,
    ) -> Result<Option<WrapCursor>, &'static str> {
        let cursor = self.wrapped_cursor_at(offset, width, allow_end)?;
        if cursor.is_some() {
            return Ok(cursor);
        }
        let row = self.source_line_at(offset);
        let summary = self.source_row(self.visible_row(row));
        if summary != row && self.folded_line_count(self.line_starts[summary]).is_some() {
            return Ok(Some(WrapCursor {
                line: summary,
                scalar: 0,
                visual_row: 0,
            }));
        }
        Ok(None)
    }

    /// Convert an absolute scalar offset into a wrapped cursor location.
    /// A cursor on a tab starts at its last displayed cell, as in Normal mode.
    pub fn wrapped_cursor_at(
        &self,
        offset: usize,
        width: usize,
        allow_end: bool,
    ) -> Result<Option<WrapCursor>, &'static str> {
        if width == 0 || width > 200 || offset > self.text.len() {
            return Err("Wrap: cursor o ancho fuera de límites");
        }
        let line = self.source_line_at(offset);
        if self.source_row(self.visible_row(line)) != line {
            return Ok(None);
        }
        let start = self.line_starts[line];
        let end = self.line_end(start);
        let scalar = offset.saturating_sub(start);
        if offset > end || (offset == end && end > start && !allow_end) {
            return Ok(None);
        }
        if self.folded_line_count(start).is_some() {
            return Ok(Some(WrapCursor {
                line,
                scalar,
                visual_row: 0,
            }));
        }
        let positions = self.wrapped_positions(&self.text[start..end], width)?;
        let visual_row = if offset == end && allow_end {
            positions
                .last()
                .map_or(0, |position| position.row + position.end / width)
        } else if let Some(position) = positions.get(scalar) {
            position.row + position.end.saturating_sub(1) / width
        } else {
            0
        };
        Ok(Some(WrapCursor {
            line,
            scalar,
            visual_row,
        }))
    }

    /// Resolve a cursor and Neovim-style virtual cell offset into a wrapped row.
    /// Returns None for a cursor hidden in a fold; the caller owns the virtual
    /// offset and may keep it beyond the end of a short line.
    pub fn wrapped_virtual_cursor_at(
        &self,
        offset: usize,
        coladd: usize,
        width: usize,
    ) -> Result<Option<WrappedVirtualCursor>, &'static str> {
        if width == 0 || width > 200 || offset > self.text.len() {
            return Err("Wrap: cursor o ancho fuera de límites");
        }
        let line = self.source_line_at(offset);
        if self.source_row(self.visible_row(line)) != line {
            return Ok(None);
        }
        let start = self.line_starts[line];
        let end = self.line_end(start);
        if offset > end || self.folded_line_count(start).is_some() {
            return Ok(None);
        }
        let scalar = offset - start;
        let text = &self.text[start..end];
        let positions = self.wrapped_positions(text, width)?;
        let (base, glyph_width, extra) = if let Some(position) = positions.get(scalar) {
            let ch = text[scalar];
            let cells = if ch == '\t' {
                1
            } else {
                ch.width().unwrap_or(1).max(1)
            };
            let extra = if ch != '\t' && cells > 1 && coladd < cells {
                0
            } else {
                coladd
            };
            let glyph_width = if extra == 0 && ch != '\t' { cells } else { 1 };
            (position.row * width + position.start, glyph_width, extra)
        } else {
            (
                positions
                    .last()
                    .map_or(0, |position| position.row * width + position.end),
                1,
                coladd,
            )
        };
        let absolute = base
            .checked_add(extra)
            .ok_or("Wrap: columna virtual fuera de límites")?;
        Ok(Some(WrappedVirtualCursor {
            cursor: WrapCursor {
                line,
                scalar,
                visual_row: absolute / width,
            },
            column: absolute % width,
            cell_width: glyph_width,
            coladd,
        }))
    }

    /// Map an already resolved virtual cursor to a row in a bounded viewport.
    pub fn wrapped_virtual_cursor_cell(
        &self,
        viewport: &[WrappedViewportRow],
        cursor: WrappedVirtualCursor,
        width: usize,
    ) -> Option<WrappedCursorCell> {
        if width == 0 || cursor.column >= width {
            return None;
        }
        viewport.iter().enumerate().find_map(|(row, visible)| {
            (visible.folded_lines.is_none()
                && self.source_line_at(visible.source.start) == cursor.cursor.line
                && visible.visual_row == cursor.cursor.visual_row)
                .then_some(WrappedCursorCell {
                    row,
                    columns: cursor.column
                        ..cursor.column.saturating_add(cursor.cell_width).min(width),
                })
        })
    }

    /// Locate a Normal-mode mouse click in a visible wrapped row. A click past
    /// the text retains its desired column but targets the last scalar.
    pub fn wrapped_mouse_hit(
        &self,
        viewport: &[WrappedViewportRow],
        row: usize,
        column: usize,
        width: usize,
    ) -> Option<WrappedMouseHit> {
        if width == 0 || column >= width {
            return None;
        }
        let visible = viewport.get(row)?;
        if visible.folded_lines.is_some() {
            return None;
        }
        let scalar = visible
            .cells
            .iter()
            .find(|cell| cell.columns.contains(&column))
            .or_else(|| {
                visible
                    .cells
                    .iter()
                    .rev()
                    .find(|cell| cell.columns.start <= column)
            })
            .or_else(|| visible.cells.first())
            .map_or(0, |cell| cell.source.start);
        Some(WrappedMouseHit {
            cursor: WrapCursor {
                line: self.source_line_at(visible.source.start),
                scalar,
                visual_row: visible.visual_row,
            },
            wanted_column: visible
                .visual_row
                .saturating_mul(width)
                .saturating_add(column),
        })
    }

    /// Clip a block's virtual columns to visible rows, including padding after
    /// text. Fold summaries remain separate.
    pub fn wrapped_block_selection(
        &self,
        viewport: &[WrappedViewportRow],
        source_lines: Range<usize>,
        block_columns: Range<usize>,
        width: usize,
    ) -> Vec<WrappedSelectionCells> {
        if width == 0 || block_columns.is_empty() {
            return Vec::new();
        }
        viewport
            .iter()
            .enumerate()
            .filter_map(|(row, visible)| {
                if visible.folded_lines.is_some()
                    || !source_lines.contains(&self.source_line_at(visible.source.start))
                {
                    return None;
                }
                let base = visible.visual_row.saturating_mul(width);
                let start = block_columns.start.saturating_sub(base).min(width);
                let end = block_columns.end.saturating_sub(base).min(width);
                (start < end).then_some(WrappedSelectionCells {
                    row,
                    columns: start..end,
                })
            })
            .collect()
    }

    /// Map selected source lines to their visible text cells. Fold summary
    /// highlighting and empty-line cursor styling remain separate.
    pub fn wrapped_line_selection(
        &self,
        viewport: &[WrappedViewportRow],
        source_lines: Range<usize>,
    ) -> Vec<WrappedSelectionCells> {
        viewport
            .iter()
            .enumerate()
            .filter_map(|(row, visible)| {
                if visible.folded_lines.is_some()
                    || !source_lines.contains(&self.source_line_at(visible.source.start))
                {
                    return None;
                }
                let end = visible.cells.last()?.columns.end;
                (end > 0).then_some(WrappedSelectionCells {
                    row,
                    columns: 0..end,
                })
            })
            .collect()
    }

    /// Locate a scalar cursor in the already bounded viewport. Returns None
    /// for hidden, offscreen and unsupported end-of-line cursor positions.
    pub fn wrapped_cursor_cell(
        &self,
        viewport: &[WrappedViewportRow],
        cursor: WrapCursor,
    ) -> Option<WrappedCursorCell> {
        for (row, visible) in viewport.iter().enumerate() {
            if self.source_line_at(visible.source.start) != cursor.line
                || visible.visual_row != cursor.visual_row
            {
                continue;
            }
            if visible.folded_lines.is_some() {
                return (cursor.scalar < visible.source.len()
                    || (visible.source.is_empty() && cursor.scalar == 0))
                    .then_some(WrappedCursorCell { row, columns: 0..1 });
            }
            if visible.source.is_empty() && cursor.scalar == 0 {
                return Some(WrappedCursorCell { row, columns: 0..1 });
            }
            if cursor.scalar == visible.source.len()
                && let Some(column) = visible.end_cursor
            {
                return Some(WrappedCursorCell {
                    row,
                    columns: column..column + 1,
                });
            }
            if let Some(cell) = visible
                .cells
                .iter()
                .find(|cell| cell.source.contains(&cursor.scalar))
            {
                return Some(WrappedCursorCell {
                    row,
                    columns: cell.columns.clone(),
                });
            }
        }
        None
    }

    fn wrapped_final_row(
        &self,
        source_line: usize,
        columns: usize,
        cursor: Option<WrapCursor>,
    ) -> Result<usize, &'static str> {
        let start = self.line_starts[source_line];
        if self.folded_line_count(start).is_some() {
            return Ok(0);
        }
        let text = &self.text[start..self.line_end(start)];
        let positions = self.wrapped_positions(text, columns)?;
        let mut last = positions
            .iter()
            .map(|p| p.row + p.end.saturating_sub(1) / columns)
            .max()
            .unwrap_or(0);
        if cursor.is_some_and(|c| c.line == source_line && c.scalar == text.len())
            && let Some(position) = positions.last()
        {
            last = last.max(position.row + position.end / columns);
        }
        Ok(last)
    }

    /// Advance or retreat one displayed row, skipping closed fold interiors.
    pub fn step_wrapped_top(
        &self,
        top: WrappedTop,
        down: bool,
        columns: usize,
    ) -> Result<WrappedTop, &'static str> {
        self.step_wrapped_top_inner(top, down, columns, None)
    }

    /// Include the extra end-of-line row only for the focused Insert cursor.
    pub fn step_wrapped_top_for_cursor(
        &self,
        top: WrappedTop,
        down: bool,
        columns: usize,
        cursor: WrapCursor,
    ) -> Result<WrappedTop, &'static str> {
        self.step_wrapped_top_inner(top, down, columns, Some(cursor))
    }

    fn step_wrapped_top_inner(
        &self,
        top: WrappedTop,
        down: bool,
        columns: usize,
        cursor: Option<WrapCursor>,
    ) -> Result<WrappedTop, &'static str> {
        if columns == 0 || columns > 200 {
            return Err("Wrap: viewport fuera de límites");
        }
        let visible = self.visible_row(top.source_line.min(self.line_starts.len() - 1));
        let source_line = self.source_row(visible);
        let last = self.wrapped_final_row(source_line, columns, cursor)?;
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
                self.wrapped_final_row(next_line, columns, cursor)?
            },
        })
    }

    /// Produce only the rows needed for a native editor viewport. Folded
    /// ranges remain one summary row; callers supply their own fold display.
    pub fn wrapped_viewport(
        &self,
        top: WrappedTop,
        rows: usize,
        columns: usize,
    ) -> Result<Vec<WrappedViewportRow>, &'static str> {
        self.wrapped_viewport_inner(top, rows, columns, None)
    }

    /// Include the cursor cell after a line for the focused Insert view.
    pub fn wrapped_viewport_for_cursor(
        &self,
        top: WrappedTop,
        rows: usize,
        columns: usize,
        cursor: WrapCursor,
    ) -> Result<Vec<WrappedViewportRow>, &'static str> {
        self.wrapped_viewport_inner(top, rows, columns, Some(cursor))
    }

    fn wrapped_viewport_inner(
        &self,
        top: WrappedTop,
        rows: usize,
        columns: usize,
        cursor: Option<WrapCursor>,
    ) -> Result<Vec<WrappedViewportRow>, &'static str> {
        if rows > 100 || columns == 0 || columns > 200 {
            return Err("Wrap: viewport fuera de límites");
        }
        let mut output = Vec::with_capacity(rows);
        for source in self.visible_lines(top.source_line, rows) {
            if output.len() == rows {
                break;
            }
            if let Some(folded_lines) = self.folded_line_count(source.start) {
                output.push(WrappedViewportRow {
                    source,
                    visual_row: 0,
                    folded_lines: Some(folded_lines),
                    end_cursor: None,
                    cells: Vec::new(),
                });
                continue;
            }
            let text = &self.text[source.clone()];
            let positions = self.wrapped_positions(text, columns)?;
            let mut last = positions
                .iter()
                .map(|p| p.row + p.end.saturating_sub(1) / columns)
                .max()
                .unwrap_or(0);
            let eol = cursor
                .filter(|cursor| {
                    cursor.line == self.source_line_at(source.start) && cursor.scalar == text.len()
                })
                .map(|_| {
                    positions.last().map_or((0, 0), |position| {
                        (
                            position.row + position.end / columns,
                            position.end % columns,
                        )
                    })
                });
            if let Some((row, _)) = eol {
                last = last.max(row);
            }
            let first = if output.is_empty() {
                top.visual_row.min(last)
            } else {
                0
            };
            for row in first..=last {
                output.push(WrappedViewportRow {
                    source: source.clone(),
                    visual_row: row,
                    folded_lines: None,
                    end_cursor: eol
                        .filter(|(eol_row, _)| *eol_row == row)
                        .map(|(_, col)| col),
                    cells: wrapped_row_cells(text, &positions, row, columns)?,
                });
                if output.len() == rows {
                    break;
                }
            }
        }
        Ok(output)
    }

    /// Scalar positions for one line with wrap and linebreak enabled.
    /// A hard cap keeps a pathological line from monopolizing the UI thread.
    pub fn wrapped_positions(
        &self,
        text: &[char],
        width: usize,
    ) -> Result<Vec<WrapPosition>, &'static str> {
        if text.len() > 131_072 || width == 0 {
            return Err("Wrap: línea o ancho fuera de límites");
        }
        let mut result: Vec<WrapPosition> = Vec::with_capacity(text.len());
        let (mut row, mut col) = (0usize, 0usize);
        let mut seen_nonbreak = false;
        for (index, &ch) in text.iter().enumerate() {
            let vcol = row.saturating_mul(width).saturating_add(col);
            let mut cells = self.display_cell_width(ch, vcol);
            if cells == 0 && index > 0 && text[index - 1] == '\t' {
                // A combining mark cannot attach to a tab; Neovim renders it
                // in its own cell instead.
                cells = 1;
            }
            if break_character(ch)
                && seen_nonbreak
                && text.get(index + 1).is_some_and(|c| !break_character(*c))
            {
                // Neovim's linebreak sizing uses the displayed virtual column,
                // and includes the trailing separators after the next word.
                let v = vcol as i128;
                let w = width as i128;
                let adjustment = cells.saturating_sub(1) as i128;
                let mut threshold = w - adjustment;
                if v >= threshold {
                    threshold = w + ((v - w) / w + 1) * w - adjustment;
                }
                let mut next = vcol;
                let mut after_separator = false;
                let mut previous = ch;
                for &next_char in &text[index + 1..] {
                    let separator = break_character(next_char);
                    if self.display_cell_width(next_char, next) == 0 && previous != '\t' {
                        continue; // Combining marks remain attached to the preceding character.
                    }
                    if after_separator && !separator {
                        break;
                    }
                    let mut next_cells = self.display_cell_width(next_char, next);
                    if next_cells == 0 && previous == '\t' {
                        next_cells = 1;
                    }
                    next = next.saturating_add(next_cells);
                    if next as i128 >= threshold {
                        cells = (threshold - v + adjustment).max(1) as usize;
                        break;
                    }
                    after_separator = separator;
                    previous = next_char;
                }
            }
            seen_nonbreak |= !break_character(ch);
            if width == 1 && cells > 1 && ch != '\t' {
                return Err("Wrap: carácter ancho en columna única");
            }
            if cells == 0 {
                let previous = result.last().copied().unwrap_or(WrapPosition {
                    row,
                    start: col,
                    end: col,
                });
                result.push(previous);
                continue;
            }
            if col == width || (cells > 1 && ch != '\t' && col.saturating_add(cells) > width) {
                row += 1;
                col = 0;
            }
            let start = col;
            let occupied = cells.min(width - col);
            col += occupied;
            result.push(WrapPosition {
                row,
                start,
                end: if ch == '\t' && cells > occupied {
                    start.saturating_add(cells)
                } else {
                    col
                },
            });
            // A tab may occupy cells in the next visual row.
            if cells > occupied {
                let remaining = cells - occupied;
                row += 1 + (remaining - 1) / width;
                col = 1 + (remaining - 1) % width;
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct OperatorReference {
        width: usize,
        lines: Vec<String>,
        start: [usize; 2],
        keys: String,
        after: Vec<String>,
        register: String,
        #[serde(rename = "type")]
        register_type: String,
        cursor: [usize; 5],
    }

    #[derive(Deserialize)]
    struct ChangeReference {
        width: usize,
        lines: Vec<String>,
        start: [usize; 2],
        keys: String,
        after: Vec<String>,
        register: String,
        #[serde(rename = "type")]
        register_type: String,
        cursor: [usize; 5],
        undo: Vec<String>,
    }

    #[test]
    fn wrapped_change_and_undo_match_neovim_reference() {
        let fixture = include_str!("../../../Resources/Fixtures/nvim-wrap-change.jsonl");
        for raw in fixture.lines() {
            let reference: ChangeReference = serde_json::from_str(raw).unwrap();
            let mut editor = Editor::new(reference.lines.join("\n"));
            editor.goto(reference.start[0] - 1, reference.start[1] - 1);
            let origin = editor.cursor;
            let mut effect = super::super::Effect::None;
            for key in reference.keys.chars().take(3) {
                effect = editor.key_with_wrap(&key.to_string());
            }
            let super::super::Effect::WrappedOperator(op, down, count) = effect else {
                panic!("change did not route: {}", reference.keys);
            };
            let cursor = editor
                .wrapped_cursor_at(origin, reference.width, false)
                .unwrap()
                .unwrap();
            let desired = editor
                .wrapped_cursor_column(cursor, reference.width)
                .unwrap()
                % reference.width;
            let (target, _) = editor
                .move_wrapped_cursor_count(cursor, desired, down, reference.width, count)
                .unwrap();
            assert!(editor.commit_wrapped_operator(op, origin, editor.version, target));
            assert_eq!(editor.mode, super::super::Mode::Insert);
            editor.insert(&reference.keys.chars().skip(3).collect::<String>());
            editor.key_with_wrap("<Esc>");
            assert_eq!(
                editor.contents(),
                reference.after.join("\n"),
                "{}",
                reference.keys
            );
            assert_eq!(
                editor.position(),
                (reference.cursor[1] - 1, reference.cursor[2] - 1)
            );
            let (text, linewise) = editor.registers.get('"').unwrap_or_default();
            assert_eq!(text, reference.register);
            assert_eq!(linewise, reference.register_type == "V");
            editor.key_with_wrap("u");
            assert_eq!(
                editor.contents(),
                reference.undo.join("\n"),
                "{}",
                reference.keys
            );
        }
    }

    #[test]
    fn wrapped_change_over_closed_fold_matches_neovim() {
        let fixture = include_str!("../../../Resources/Fixtures/nvim-wrap-fold-change.jsonl");
        for raw in fixture.lines() {
            let reference: ChangeReference = serde_json::from_str(raw).unwrap();
            let text = reference.lines.join("\n");
            let mut editor =
                Editor::new(text.clone()).for_path(std::path::Path::new("synthetic.rs"));
            let mut engine = crate::syntax_folds::Engine::default();
            let folds = engine
                .compute(
                    crate::syntax::Language::Rust,
                    &text,
                    &std::sync::atomic::AtomicUsize::new(0),
                )
                .unwrap();
            assert!(editor.apply_parser_folds(editor.version, Ok(folds)));
            editor.key("z");
            editor.key("M");
            assert_eq!(editor.folded_line_count(editor.line_starts[1]), Some(3));
            editor.goto(reference.start[0] - 1, reference.start[1] - 1);
            let origin = editor.cursor;
            let mut effect = super::super::Effect::None;
            for key in reference.keys.chars().take(3) {
                effect = editor.key_with_wrap(&key.to_string());
            }
            let super::super::Effect::WrappedOperator(op, down, count) = effect else {
                panic!("change did not route: {}", reference.keys);
            };
            let cursor = editor
                .wrapped_cursor_at(origin, reference.width, false)
                .unwrap()
                .unwrap();
            let desired = editor
                .wrapped_cursor_column(cursor, reference.width)
                .unwrap()
                % reference.width;
            let (target, _) = editor
                .move_wrapped_cursor_count(cursor, desired, down, reference.width, count)
                .unwrap();
            assert!(editor.commit_wrapped_operator(op, origin, editor.version, target));
            assert_eq!(editor.mode, super::super::Mode::Insert);
            editor.insert(&reference.keys.chars().skip(3).collect::<String>());
            editor.key_with_wrap("<Esc>");
            assert_eq!(
                editor.contents(),
                reference.after.join("\n"),
                "{} at {:?}",
                reference.keys,
                reference.start
            );
            assert_eq!(
                editor.position(),
                (reference.cursor[1] - 1, reference.cursor[2] - 1)
            );
            let (text, linewise) = editor.registers.get('"').unwrap_or_default();
            assert_eq!(text, reference.register);
            assert_eq!(linewise, reference.register_type == "V");
            editor.key_with_wrap("u");
            assert_eq!(
                editor.contents(),
                reference.undo.join("\n"),
                "{} at {:?}",
                reference.keys,
                reference.start
            );
        }
    }

    #[test]
    fn wrapped_operator_ranges_match_neovim_reference() {
        let fixture = include_str!("../../../Resources/Fixtures/nvim-wrap-operators.jsonl");
        for raw in fixture.lines() {
            let reference: OperatorReference = serde_json::from_str(raw).unwrap();
            let mut editor = Editor::new(reference.lines.join("\n"));
            editor.goto(reference.start[0] - 1, reference.start[1] - 1);
            let origin = editor.cursor;
            let mut effect = super::super::Effect::None;
            for key in reference.keys.chars() {
                effect = editor.key_with_wrap(&key.to_string());
            }
            let super::super::Effect::WrappedOperator(op, down, count) = effect else {
                panic!("operator did not route: {}", reference.keys);
            };
            let cursor = editor
                .wrapped_cursor_at(origin, reference.width, false)
                .unwrap()
                .unwrap();
            let desired = editor
                .wrapped_cursor_column(cursor, reference.width)
                .unwrap()
                % reference.width;
            let (target, _) = editor
                .move_wrapped_cursor_count(cursor, desired, down, reference.width, count)
                .unwrap();
            assert!(editor.commit_wrapped_operator(op, origin, editor.version, target));
            assert_eq!(
                editor.contents(),
                reference.after.join("\n"),
                "{}",
                reference.keys
            );
            let (text, linewise) = editor.registers.get('"').unwrap_or_default();
            assert_eq!(text, reference.register, "{}", reference.keys);
            assert_eq!(
                linewise,
                reference.register_type == "V",
                "{}",
                reference.keys
            );
            assert_eq!(
                editor.position(),
                (reference.cursor[1] - 1, reference.cursor[2] - 1),
                "{}",
                reference.keys
            );
        }
    }

    #[test]
    fn wrapped_operators_over_closed_folds_match_neovim_reference() {
        let fixture = include_str!("../../../Resources/Fixtures/nvim-wrap-fold-operators.jsonl");
        for raw in fixture.lines() {
            let reference: OperatorReference = serde_json::from_str(raw).unwrap();
            let text = reference.lines.join("\n");
            let mut editor =
                Editor::new(text.clone()).for_path(std::path::Path::new("synthetic.rs"));
            let mut engine = crate::syntax_folds::Engine::default();
            let folds = engine
                .compute(
                    crate::syntax::Language::Rust,
                    &text,
                    &std::sync::atomic::AtomicUsize::new(0),
                )
                .unwrap();
            assert!(editor.apply_parser_folds(editor.version, Ok(folds)));
            editor.key("z");
            editor.key("M");
            assert_eq!(editor.folded_line_count(editor.line_starts[1]), Some(3));
            editor.goto(reference.start[0] - 1, reference.start[1] - 1);
            let origin = editor.cursor;
            let mut effect = super::super::Effect::None;
            for key in reference.keys.chars() {
                effect = editor.key_with_wrap(&key.to_string());
            }
            let super::super::Effect::WrappedOperator(op, down, count) = effect else {
                panic!("operator did not route: {}", reference.keys);
            };
            let cursor = editor
                .wrapped_cursor_at(origin, reference.width, false)
                .unwrap()
                .unwrap();
            let desired = editor
                .wrapped_cursor_column(cursor, reference.width)
                .unwrap()
                % reference.width;
            let (target, _) = editor
                .move_wrapped_cursor_count(cursor, desired, down, reference.width, count)
                .unwrap();
            assert!(editor.commit_wrapped_operator(op, origin, editor.version, target));
            assert_eq!(
                editor.contents(),
                reference.after.join("\n"),
                "{} at {:?}",
                reference.keys,
                reference.start
            );
            let (text, linewise) = editor.registers.get('"').unwrap_or_default();
            assert_eq!(
                text, reference.register,
                "{} at {:?}",
                reference.keys, reference.start
            );
            assert_eq!(linewise, reference.register_type == "V");
            assert_eq!(
                editor.position(),
                (reference.cursor[1] - 1, reference.cursor[2] - 1)
            );
        }
    }

    #[derive(Deserialize)]
    struct WrapReference {
        line: String,
        width: usize,
        tabstop: usize,
        positions: Vec<ScreenReference>,
    }
    #[derive(Deserialize)]
    struct ScreenReference {
        row: usize,
        col: usize,
        endcol: usize,
    }
    #[derive(Deserialize)]
    struct RowReference {
        line: String,
        width: usize,
        tabstop: usize,
        rows: Vec<Vec<String>>,
    }

    #[test]
    fn rendered_rows_match_neovim_screen_cells() {
        let fixture = include_str!("../../../Resources/Fixtures/nvim-wrap-rows.jsonl");
        for raw in fixture.lines() {
            let expected: RowReference = serde_json::from_str(raw).unwrap();
            let mut editor = Editor::new(String::new());
            editor.indent_style.tabstop = expected.tabstop;
            let chars: Vec<_> = expected.line.chars().collect();
            let positions = editor.wrapped_positions(&chars, expected.width).unwrap();
            let last_row = positions
                .iter()
                .map(|p| p.row + p.end.saturating_sub(1) / expected.width)
                .max()
                .unwrap_or(0);
            for row in 0..=last_row {
                let cells = wrapped_row_cells(&chars, &positions, row, expected.width).unwrap();
                let mut actual = String::new();
                let mut column = 0;
                for cell in &cells {
                    actual.extend(std::iter::repeat_n(
                        ' ',
                        cell.columns.start.saturating_sub(column),
                    ));
                    cell.append_text(&chars, &mut actual);
                    column = cell.columns.end;
                }
                actual.extend(std::iter::repeat_n(
                    ' ',
                    expected.width.saturating_sub(column),
                ));
                assert_eq!(
                    actual,
                    expected.rows[row].concat(),
                    "line {:?}, tabstop {}, row {row}",
                    expected.line,
                    expected.tabstop
                );
            }
        }
    }

    #[test]
    fn wrap_positions_match_isolated_neovim_matrix() {
        let fixture = include_str!("../../../Resources/Fixtures/nvim-wrap-positions.jsonl");
        for raw in fixture.lines() {
            let expected: WrapReference = serde_json::from_str(raw).unwrap();
            let mut editor = Editor::new(String::new());
            editor.indent_style.tabstop = expected.tabstop;
            let chars: Vec<_> = expected.line.chars().collect();
            let positions = editor.wrapped_positions(&chars, expected.width).unwrap();
            assert_eq!(positions.len(), expected.positions.len());
            for (index, (position, reference)) in
                positions.iter().zip(&expected.positions).enumerate()
            {
                if reference.row == 0 || reference.col == 0 {
                    continue; // screenpos reports zero for rows outside the attached UI.
                }
                assert_eq!(
                    *position,
                    WrapPosition {
                        row: reference.row - 1,
                        start: reference.col - 1,
                        end: reference.endcol
                    },
                    "line {:?}, tabstop {}, scalar {index}",
                    expected.line,
                    expected.tabstop
                );
            }
        }
    }
    #[test]
    fn clean_neovim_reference_word_boundary_wide_glyph_and_combining_mark() {
        let e = Editor::new(String::new());
        let word: Vec<_> = "1234567890123456 wordnext".chars().collect();
        let p = e.wrapped_positions(&word, 20).unwrap();
        assert_eq!(
            p[16],
            WrapPosition {
                row: 0,
                start: 16,
                end: 20
            }
        );
        assert_eq!(
            p[17],
            WrapPosition {
                row: 1,
                start: 0,
                end: 1
            }
        );
        for text in ["1234567890123456-wordnext", "1234567890123456/wordnext"] {
            let chars: Vec<_> = text.chars().collect();
            let p = e.wrapped_positions(&chars, 20).unwrap();
            assert_eq!(
                p[16],
                WrapPosition {
                    row: 0,
                    start: 16,
                    end: 20
                }
            );
            assert_eq!(
                p[17],
                WrapPosition {
                    row: 1,
                    start: 0,
                    end: 1
                }
            );
        }
        let spaces: Vec<_> = "1234567890123456  wordnext".chars().collect();
        let p = e.wrapped_positions(&spaces, 20).unwrap();
        assert_eq!(
            p[17],
            WrapPosition {
                row: 0,
                start: 17,
                end: 20
            }
        );
        assert_eq!(
            p[18],
            WrapPosition {
                row: 1,
                start: 0,
                end: 1
            }
        );
        let motion: Vec<_> = "1234567890123456 wordnext MORE words again"
            .chars()
            .collect();
        let p = e.wrapped_positions(&motion, 20).unwrap();
        let second = move_in_wrapped_line(&p, 0, 0, true);
        assert_eq!(second, 17);
        let third = move_in_wrapped_line(&p, second, 0, true);
        assert_eq!(third, 37);
        assert_eq!(move_in_wrapped_line(&p, third, 0, false), second);
        let wide: Vec<_> = "1234567890123456789界ZZ".chars().collect();
        let p = e.wrapped_positions(&wide, 20).unwrap();
        assert_eq!(
            p[19],
            WrapPosition {
                row: 1,
                start: 0,
                end: 2
            }
        );
        let combined: Vec<_> = "e\u{301} xxxxxxxxxxxxxxxxxxxxxxxxx".chars().collect();
        let p = e.wrapped_positions(&combined, 20).unwrap();
        assert_eq!(p[0], p[1]);
        assert_eq!(
            p[2],
            WrapPosition {
                row: 0,
                start: 1,
                end: 20
            }
        );
        assert_eq!(
            p[3],
            WrapPosition {
                row: 1,
                start: 0,
                end: 1
            }
        );
        let mut tabs = Editor::new(String::new());
        tabs.indent_style.tabstop = 8;
        let line: Vec<_> = "1234567890123456789\tX".chars().collect();
        let p = tabs.wrapped_positions(&line, 20).unwrap();
        assert_eq!(
            p[19],
            WrapPosition {
                row: 0,
                start: 19,
                end: 24
            }
        );
        assert_eq!(
            p[20],
            WrapPosition {
                row: 1,
                start: 4,
                end: 5
            }
        );
        assert!(e.wrapped_positions(&word, 0).is_err());
        assert!(e.wrapped_positions(&['界'], 1).is_err());
    }

    #[test]
    fn tab_can_span_more_than_two_visual_rows() {
        // Clean Neovim 0.12.1, 12 columns, tabstop=32: screenpos for X
        // in "\tX" is row 3, column 9 (one-based).
        let mut e = Editor::new(String::new());
        e.indent_style.tabstop = 32;
        let positions = e.wrapped_positions(&['\t', 'X'], 12).unwrap();
        assert_eq!(
            positions[1],
            WrapPosition {
                row: 2,
                start: 8,
                end: 9
            }
        );
        e.indent_style.tabstop = 64;
        for (prefix, row) in [(1, 2), (9, 2), (13, 2), (24, 3)] {
            let chars: Vec<_> = format!("{}\tX", "x".repeat(prefix)).chars().collect();
            let positions = e.wrapped_positions(&chars, 12).unwrap();
            assert_eq!(positions[prefix + 1].row, row, "prefix length {prefix}");
            assert_eq!(positions[prefix + 1].start, 0, "prefix length {prefix}");
        }
    }

    #[test]
    fn visual_rows_preserve_source_ranges_tabs_and_combining_marks() {
        let mut editor = Editor::new(String::new());
        let chars: Vec<_> = "1234567890123456 wordnext".chars().collect();
        let positions = editor.wrapped_positions(&chars, 20).unwrap();
        let row = wrapped_row_cells(&chars, &positions, 0, 20).unwrap();
        let mut text = String::new();
        for cell in &row {
            cell.append_text(&chars, &mut text);
        }
        assert_eq!(text, "1234567890123456    ");
        assert_eq!(row.last().unwrap().source, 16..17);
        assert_eq!(row.last().unwrap().columns, 16..20);
        let row = wrapped_row_cells(&chars, &positions, 1, 20).unwrap();
        let mut text = String::new();
        for cell in &row {
            cell.append_text(&chars, &mut text);
        }
        assert_eq!(text, "wordnext");
        assert_eq!(row[0].source, 17..18);

        editor.indent_style.tabstop = 32;
        let chars: Vec<_> = "\tX".chars().collect();
        let positions = editor.wrapped_positions(&chars, 12).unwrap();
        for (row_index, expected) in [(0, 12), (1, 12), (2, 8)] {
            let row = wrapped_row_cells(&chars, &positions, row_index, 12).unwrap();
            let mut text = String::new();
            row[0].append_text(&chars, &mut text);
            assert_eq!(text, " ".repeat(expected));
            assert_eq!(row[0].source, 0..1);
        }
        let row = wrapped_row_cells(&chars, &positions, 2, 12).unwrap();
        assert_eq!(row[1].source, 1..2);
        assert_eq!(row[1].columns, 8..9);

        let chars: Vec<_> = "e\u{301}界".chars().collect();
        let positions = editor.wrapped_positions(&chars, 12).unwrap();
        let row = wrapped_row_cells(&chars, &positions, 0, 12).unwrap();
        assert_eq!(row[0].source, 0..2);
        assert_eq!(row[0].columns, 0..1);
        assert_eq!(row[1].source, 2..3);
        assert_eq!(row[1].columns, 1..3);
        assert!(wrapped_row_cells(&chars, &positions, 0, 0).is_err());
        assert!(wrapped_row_cells(&chars[..1], &positions, 0, 12).is_err());

        // Neovim renders a standalone combining mark after a tab on its own
        // space cell, rather than attaching it to a cell belonging to the tab.
        editor.indent_style.tabstop = 8;
        let chars: Vec<_> = "aa\t\u{301}X".chars().collect();
        let positions = editor.wrapped_positions(&chars, 20).unwrap();
        let row = wrapped_row_cells(&chars, &positions, 0, 20).unwrap();
        let mut text = String::new();
        for cell in &row {
            cell.append_text(&chars, &mut text);
        }
        assert_eq!(text, "aa       \u{301}X");
        assert!(row[3].standalone_mark);
    }

    #[test]
    fn viewport_is_bounded_and_continues_across_lines_and_folds() {
        let editor =
            Editor::new("short\n1234567890123456 wordnext MORE words again\n\nlast".into());
        let rows = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 1,
                    visual_row: 1,
                },
                3,
                20,
            )
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].source.clone(), rows[0].visual_row), (6..48, 1));
        assert_eq!((rows[1].source.clone(), rows[1].visual_row), (6..48, 2));
        assert_eq!((rows[2].source.clone(), rows[2].visual_row), (49..49, 0));
        assert!(
            editor
                .wrapped_viewport(
                    WrappedTop {
                        source_line: 0,
                        visual_row: 0
                    },
                    101,
                    20
                )
                .is_err()
        );
        assert!(
            editor
                .wrapped_viewport(
                    WrappedTop {
                        source_line: 0,
                        visual_row: 0
                    },
                    3,
                    0
                )
                .is_err()
        );
        assert!(
            editor
                .wrapped_viewport(
                    WrappedTop {
                        source_line: 0,
                        visual_row: 0
                    },
                    0,
                    20
                )
                .unwrap()
                .is_empty()
        );

        let text = "fn demo() {\n  let x = 1;\n}\nnext";
        let mut editor = Editor::new(text.into()).for_path(std::path::Path::new("synthetic.rs"));
        let mut engine = crate::syntax_folds::Engine::default();
        let folds = engine
            .compute(
                crate::syntax::Language::Rust,
                text,
                &std::sync::atomic::AtomicUsize::new(0),
            )
            .unwrap();
        assert!(editor.apply_parser_folds(editor.version, Ok(folds)));
        editor.key("z");
        editor.key("M");
        let rows = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                5,
                20,
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].folded_lines, Some(3));
        assert_eq!(rows[1].folded_lines, None);
        assert_eq!(&editor.text[rows[1].source.clone()], &['n', 'e', 'x', 't']);
        let folded = WrappedTop {
            source_line: 0,
            visual_row: 0,
        };
        let after = editor.step_wrapped_top(folded, true, 20).unwrap();
        assert_eq!(
            after,
            WrappedTop {
                source_line: 3,
                visual_row: 0
            }
        );
        assert_eq!(editor.step_wrapped_top(after, false, 20).unwrap(), folded);
    }

    #[test]
    fn viewport_steps_one_visual_row_and_stays_bounded() {
        let editor =
            Editor::new("short\n1234567890123456 wordnext MORE words again\n\nlast".into());
        let mut top = WrappedTop {
            source_line: 0,
            visual_row: 0,
        };
        let start = top;
        for expected in [
            WrappedTop {
                source_line: 1,
                visual_row: 0,
            },
            WrappedTop {
                source_line: 1,
                visual_row: 1,
            },
            WrappedTop {
                source_line: 1,
                visual_row: 2,
            },
            WrappedTop {
                source_line: 2,
                visual_row: 0,
            },
            WrappedTop {
                source_line: 3,
                visual_row: 0,
            },
        ] {
            top = editor.step_wrapped_top(top, true, 20).unwrap();
            assert_eq!(top, expected);
        }
        assert_eq!(editor.step_wrapped_top(top, true, 20).unwrap(), top);
        for _ in 0..5 {
            top = editor.step_wrapped_top(top, false, 20).unwrap();
        }
        assert_eq!(top, start);
        assert_eq!(editor.step_wrapped_top(top, false, 20).unwrap(), top);
        assert!(editor.step_wrapped_top(top, true, 0).is_err());
        let resized = editor
            .step_wrapped_top(
                WrappedTop {
                    source_line: 1,
                    visual_row: 99,
                },
                false,
                100,
            )
            .unwrap();
        assert_eq!(
            resized,
            WrappedTop {
                source_line: 0,
                visual_row: 0
            }
        );
    }

    #[test]
    fn viewport_locates_visible_cursor_and_rejects_offscreen_positions() {
        let editor =
            Editor::new("short\n1234567890123456 wordnext MORE words again\n\nlast".into());
        let viewport = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 1,
                    visual_row: 1,
                },
                3,
                20,
            )
            .unwrap();
        assert_eq!(
            editor.wrapped_cursor_cell(
                &viewport,
                WrapCursor {
                    line: 1,
                    scalar: 17,
                    visual_row: 1
                }
            ),
            Some(WrappedCursorCell {
                row: 0,
                columns: 0..1
            })
        );
        assert_eq!(
            editor.wrapped_cursor_cell(
                &viewport,
                WrapCursor {
                    line: 1,
                    scalar: 37,
                    visual_row: 2
                }
            ),
            Some(WrappedCursorCell {
                row: 1,
                columns: 0..1
            })
        );
        assert_eq!(
            editor.wrapped_cursor_cell(
                &viewport,
                WrapCursor {
                    line: 2,
                    scalar: 0,
                    visual_row: 0
                }
            ),
            Some(WrappedCursorCell {
                row: 2,
                columns: 0..1
            })
        );
        assert_eq!(
            editor.wrapped_cursor_cell(
                &viewport,
                WrapCursor {
                    line: 0,
                    scalar: 0,
                    visual_row: 0
                }
            ),
            None
        );

        let mut tabs = Editor::new("\tX".into());
        tabs.indent_style.tabstop = 32;
        let viewport = tabs
            .wrapped_viewport(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                3,
                12,
            )
            .unwrap();
        assert_eq!(
            tabs.wrapped_cursor_cell(
                &viewport,
                WrapCursor {
                    line: 0,
                    scalar: 0,
                    visual_row: 2
                }
            ),
            Some(WrappedCursorCell {
                row: 2,
                columns: 0..8
            })
        );
    }

    #[test]
    fn buffer_offsets_map_to_wrapped_rows_and_insert_end() {
        let editor = Editor::new("1234567890123456 wordnext MORE words again".into());
        assert_eq!(
            editor.wrapped_cursor_at(17, 20, false).unwrap(),
            Some(WrapCursor {
                line: 0,
                scalar: 17,
                visual_row: 1
            })
        );
        assert_eq!(
            editor.wrapped_cursor_at(37, 20, false).unwrap(),
            Some(WrapCursor {
                line: 0,
                scalar: 37,
                visual_row: 2
            })
        );
        assert_eq!(
            editor
                .wrapped_cursor_at(editor.text.len(), 20, false)
                .unwrap(),
            None
        );

        let editor = Editor::new("12345678901234567890".into());
        assert_eq!(
            editor.wrapped_cursor_at(20, 20, true).unwrap(),
            Some(WrapCursor {
                line: 0,
                scalar: 20,
                visual_row: 1
            })
        );
        let mut editor = Editor::new("\tX".into());
        editor.indent_style.tabstop = 32;
        assert_eq!(
            editor.wrapped_cursor_at(0, 12, false).unwrap(),
            Some(WrapCursor {
                line: 0,
                scalar: 0,
                visual_row: 2
            })
        );
        assert!(editor.wrapped_cursor_at(0, 0, false).is_err());
    }

    #[test]
    fn wrapped_motion_initial_column_handles_words_and_multirow_tabs() {
        let editor = Editor::new("1234567890123456 wordnext".into());
        let cursor = editor.wrapped_cursor_at(19, 20, false).unwrap().unwrap();
        assert_eq!(editor.wrapped_cursor_column(cursor, 20), Some(2));
        let mut editor = Editor::new("\tX".into());
        editor.indent_style.tabstop = 32;
        let cursor = editor.wrapped_cursor_at(0, 12, false).unwrap().unwrap();
        assert_eq!(editor.wrapped_cursor_column(cursor, 12), Some(7));
        assert_eq!(editor.wrapped_cursor_column(cursor, 0), None);
    }

    #[test]
    fn normal_mouse_hits_match_clean_neovim_wrapped_rows() {
        let editor = Editor::new("1234567890123456 wordnext".into());
        let rows = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                3,
                20,
            )
            .unwrap();
        for (row, column, scalar, wanted) in [(0, 18, 16, 18), (1, 2, 19, 22), (1, 15, 24, 35)] {
            let hit = editor.wrapped_mouse_hit(&rows, row, column, 20).unwrap();
            assert_eq!(
                hit.cursor,
                WrapCursor {
                    line: 0,
                    scalar,
                    visual_row: row
                }
            );
            assert_eq!(hit.wanted_column, wanted);
        }
        assert!(editor.wrapped_mouse_hit(&rows, 0, 20, 20).is_none());

        let mut editor = Editor::new("\tX".into());
        editor.indent_style.tabstop = 32;
        let rows = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                3,
                20,
            )
            .unwrap();
        for (row, column, scalar, wanted) in [(0, 4, 0, 4), (1, 4, 0, 24), (1, 12, 1, 32)] {
            let hit = editor.wrapped_mouse_hit(&rows, row, column, 20).unwrap();
            assert_eq!(
                hit.cursor,
                WrapCursor {
                    line: 0,
                    scalar,
                    visual_row: row
                }
            );
            assert_eq!(hit.wanted_column, wanted);
        }
    }

    #[test]
    fn insert_cursor_at_end_matches_clean_neovim_wrap_boundaries() {
        // Clean Neovim 0.12.1, width 20, Insert after A: rows and columns
        // are zero-based here. A full-width line creates an extra cursor row.
        for (line, visual_row, column) in [
            ("short", 0, 5),
            ("1234567890123456789", 0, 19),
            ("12345678901234567890", 1, 0),
            ("1234567890123456 wordnext", 1, 8),
        ] {
            let editor = Editor::new(line.into());
            let cursor = WrapCursor {
                line: 0,
                scalar: line.chars().count(),
                visual_row,
            };
            let rows = editor
                .wrapped_viewport_for_cursor(
                    WrappedTop {
                        source_line: 0,
                        visual_row: 0,
                    },
                    4,
                    20,
                    cursor,
                )
                .unwrap();
            assert_eq!(
                editor.wrapped_cursor_cell(&rows, cursor),
                Some(WrappedCursorCell {
                    row: visual_row,
                    columns: column..column + 1
                }),
                "{line:?}"
            );
        }
    }

    #[test]
    fn viewport_step_can_reach_insert_cursor_only_row() {
        let editor = Editor::new("12345678901234567890".into());
        let start = WrappedTop {
            source_line: 0,
            visual_row: 0,
        };
        let cursor = WrapCursor {
            line: 0,
            scalar: 20,
            visual_row: 1,
        };
        assert_eq!(editor.step_wrapped_top(start, true, 20).unwrap(), start);
        let next = editor
            .step_wrapped_top_for_cursor(start, true, 20, cursor)
            .unwrap();
        assert_eq!(
            next,
            WrappedTop {
                source_line: 0,
                visual_row: 1
            }
        );
        assert_eq!(
            editor
                .step_wrapped_top_for_cursor(next, false, 20, cursor)
                .unwrap(),
            start
        );
        let viewport = editor
            .wrapped_viewport_for_cursor(next, 2, 20, cursor)
            .unwrap();
        assert_eq!(
            editor.wrapped_cursor_cell(&viewport, cursor),
            Some(WrappedCursorCell {
                row: 0,
                columns: 0..1
            })
        );
    }

    #[test]
    fn selection_crosses_visual_rows_and_preserves_tab_spans() {
        let editor = Editor::new("1234567890123456 wordnext".into());
        let rows = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                3,
                20,
            )
            .unwrap();
        // Clean Neovim shows Visual from scalar 15 through 20 on the last
        // five cells of row 0 and first four of row 1; cursor styling is separate.
        assert_eq!(
            wrapped_selection_cells(&rows, 15..21),
            vec![
                WrappedSelectionCells {
                    row: 0,
                    columns: 15..20
                },
                WrappedSelectionCells {
                    row: 1,
                    columns: 0..4
                },
            ]
        );
        assert!(wrapped_selection_cells(&rows, 0..0).is_empty());

        let mut editor = Editor::new("\tX".into());
        editor.indent_style.tabstop = 32;
        let rows = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                3,
                12,
            )
            .unwrap();
        assert_eq!(
            wrapped_selection_cells(&rows, 0..1),
            vec![
                WrappedSelectionCells {
                    row: 0,
                    columns: 0..12
                },
                WrappedSelectionCells {
                    row: 1,
                    columns: 0..12
                },
                WrappedSelectionCells {
                    row: 2,
                    columns: 0..8
                },
            ]
        );
    }

    #[test]
    fn linewise_selection_marks_each_wrapped_row_through_its_text() {
        let editor = Editor::new("1234567890123456 wordnext\nother".into());
        let rows = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                4,
                20,
            )
            .unwrap();
        // Clean Neovim Visual line highlights all text on both visual rows;
        // its cursor cell uses a distinct screen attribute.
        assert_eq!(
            editor.wrapped_line_selection(&rows, 0..1),
            vec![
                WrappedSelectionCells {
                    row: 0,
                    columns: 0..20
                },
                WrappedSelectionCells {
                    row: 1,
                    columns: 0..8
                },
            ]
        );
        assert_eq!(
            editor.wrapped_line_selection(&rows, 1..2),
            vec![WrappedSelectionCells {
                row: 2,
                columns: 0..5
            },]
        );
    }

    #[test]
    fn block_selection_crosses_wrapped_rows_in_two_lines() {
        let editor = Editor::new("1234567890123456 wordnext\nabcdefghijklmnopqrstuvwx".into());
        let rows = editor
            .wrapped_viewport(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                5,
                20,
            )
            .unwrap();
        // Isolated Neovim 0.12.1, block from display column 16 to 21;
        // the cursor has a separate screen attribute in the fourth row.
        assert_eq!(
            editor.wrapped_block_selection(&rows, 0..2, 15..21, 20),
            vec![
                WrappedSelectionCells {
                    row: 0,
                    columns: 15..20
                },
                WrappedSelectionCells {
                    row: 1,
                    columns: 0..1
                },
                WrappedSelectionCells {
                    row: 2,
                    columns: 15..20
                },
                WrappedSelectionCells {
                    row: 3,
                    columns: 0..1
                },
            ]
        );
        assert!(
            editor
                .wrapped_block_selection(&rows, 0..2, 15..21, 0)
                .is_empty()
        );
    }

    #[test]
    fn viewport_step_matches_smoothscroll_topline_reference() {
        // Clean Neovim 0.12.1, width 20, smoothscroll and scrolloff=0,
        // line `ab ` repeated 100 times: five Ctrl-e then three Ctrl-y
        // produce skipcol 20,40,60,80,100,80,60,40 respectively.
        let editor = Editor::new("ab ".repeat(100));
        let mut top = WrappedTop {
            source_line: 0,
            visual_row: 0,
        };
        for (down, row) in [
            (true, 1),
            (true, 2),
            (true, 3),
            (true, 4),
            (true, 5),
            (false, 4),
            (false, 3),
            (false, 2),
        ] {
            top = editor.step_wrapped_top(top, down, 20).unwrap();
            assert_eq!(
                top,
                WrappedTop {
                    source_line: 0,
                    visual_row: row
                }
            );
        }
    }

    #[test]
    fn smoothscroll_follow_matches_neovim_zero_scrolloff_reference() {
        // Clean Neovim 0.12.1, 20 columns, eight visible rows, smoothscroll,
        // scrolloff=0. Scalar offsets are zero-based because the line is ASCII.
        let editor = Editor::new("ab ".repeat(100));
        let mut top = WrappedTop {
            source_line: 0,
            visual_row: 0,
        };
        for (offset, expected_top) in [
            (18, 0),
            (36, 0),
            (54, 0),
            (72, 0),
            (90, 0),
            (108, 0),
            (126, 0),
            (144, 1),
            (162, 2),
            (180, 3),
            (198, 4),
            (216, 5),
            (198, 5),
            (180, 5),
            (162, 5),
            (144, 5),
            (126, 5),
        ] {
            let cursor = editor
                .wrapped_cursor_at(offset, 20, false)
                .unwrap()
                .unwrap();
            top = editor.follow_wrapped_cursor(top, cursor, 8, 20).unwrap();
            assert_eq!(
                top,
                WrappedTop {
                    source_line: 0,
                    visual_row: expected_top
                },
                "offset {offset}"
            );
        }
        assert!(
            editor
                .follow_wrapped_cursor(
                    top,
                    WrapCursor {
                        line: 0,
                        scalar: 0,
                        visual_row: 0
                    },
                    0,
                    20
                )
                .is_err()
        );
    }

    #[test]
    fn smoothscroll_four_margin_matches_clean_neovim_reference() {
        // Neovim 0.12.1, width 20, eight rows, scrolloff=4, smoothscroll.
        // Twelve gj then five gk on `ab ` repeated 100 times.
        let editor = Editor::new("ab ".repeat(100));
        let mut top = WrappedTop {
            source_line: 0,
            visual_row: 0,
        };
        for (offset, expected_top) in [
            (18, 0),
            (36, 0),
            (54, 0),
            (72, 0),
            (90, 1),
            (108, 2),
            (126, 3),
            (144, 4),
            (162, 5),
            (180, 6),
            (198, 7),
            (216, 8),
            (198, 7),
            (180, 6),
            (162, 5),
            (144, 4),
            (126, 3),
        ] {
            let cursor = editor
                .wrapped_cursor_at(offset, 20, false)
                .unwrap()
                .unwrap();
            top = editor
                .follow_wrapped_cursor_with_margin(top, cursor, 8, 20, 4)
                .unwrap();
            assert_eq!(top.visual_row, expected_top, "cursor at {offset}");
        }
    }

    #[test]
    #[ignore = "Neovim recentering near file boundaries needs further modeling"]
    fn scroll_margin_matches_neovim_on_mixed_and_short_buffers() {
        #[derive(Deserialize)]
        struct View {
            topline: usize,
            skipcol: usize,
        }
        #[derive(Deserialize)]
        struct Step {
            key: String,
            cursor: [usize; 5],
            view: View,
        }
        #[derive(Deserialize)]
        struct Ref {
            lines: Vec<String>,
            initial: View,
            steps: Vec<Step>,
        }
        let fixture = include_str!("../../../Resources/Fixtures/nvim-wrap-follow-limits.jsonl");
        for raw in fixture.lines() {
            let reference: Ref = serde_json::from_str(raw).unwrap();
            let editor = Editor::new(reference.lines.join("\n"));
            let mut top = WrappedTop {
                source_line: reference.initial.topline - 1,
                visual_row: reference.initial.skipcol / 20,
            };
            for (index, step) in reference.steps.iter().enumerate() {
                let line = step.cursor[1] - 1;
                let offset = editor.line_starts[line] + step.cursor[2] - 1;
                let cursor = editor
                    .wrapped_cursor_at(offset, 20, false)
                    .unwrap()
                    .unwrap();
                top = editor
                    .follow_wrapped_cursor_with_margin(top, cursor, 8, 20, 4)
                    .unwrap();
                assert_eq!(
                    top,
                    WrappedTop {
                        source_line: step.view.topline - 1,
                        visual_row: step.view.skipcol / 20
                    },
                    "step {index} {}, input {:?}",
                    step.key,
                    reference.lines
                );
            }
        }
    }

    #[test]
    fn following_a_bounded_long_line_keeps_cursor_visible() {
        let editor = Editor::new("a".repeat(131_072));
        let cursor = editor
            .wrapped_cursor_at(131_071, 80, false)
            .unwrap()
            .unwrap();
        let top = editor
            .follow_wrapped_cursor(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                cursor,
                100,
                80,
            )
            .unwrap();
        let viewport = editor
            .wrapped_viewport_for_cursor(top, 100, 80, cursor)
            .unwrap();
        assert!(editor.wrapped_cursor_cell(&viewport, cursor).is_some());
    }

    #[test]
    fn optimized_follow_crosses_logical_lines_like_single_row_steps() {
        let editor = Editor::new(format!("first\n{}\nlast", "abc ".repeat(50)));
        let cursor = editor
            .wrapped_cursor_at(editor.text.len() - 2, 20, false)
            .unwrap()
            .unwrap();
        for rows in [1, 2, 8, 16, 40] {
            let mut expected = WrappedTop {
                source_line: cursor.line,
                visual_row: cursor.visual_row,
            };
            for _ in 1..rows {
                let previous = editor
                    .step_wrapped_top_for_cursor(expected, false, 20, cursor)
                    .unwrap();
                if previous == expected {
                    break;
                }
                expected = previous;
            }
            let actual = editor
                .follow_wrapped_cursor(
                    WrappedTop {
                        source_line: 0,
                        visual_row: 0,
                    },
                    cursor,
                    rows,
                    20,
                )
                .unwrap();
            assert_eq!(actual, expected, "height {rows}");
        }
    }

    #[test]
    fn gj_gk_cross_wrapped_and_empty_lines_like_clean_neovim() {
        let editor = Editor::new(String::new());
        let lines: Vec<Vec<WrapPosition>> = [
            "short",
            "1234567890123456 wordnext MORE words again",
            "",
            "final",
        ]
        .iter()
        .map(|line| {
            editor
                .wrapped_positions(&line.chars().collect::<Vec<_>>(), 20)
                .unwrap()
        })
        .collect();
        let mut cursor = WrapCursor {
            line: 0,
            scalar: 0,
            visual_row: 0,
        };
        for expected in [
            WrapCursor {
                line: 1,
                scalar: 0,
                visual_row: 0,
            },
            WrapCursor {
                line: 1,
                scalar: 17,
                visual_row: 1,
            },
            WrapCursor {
                line: 1,
                scalar: 37,
                visual_row: 2,
            },
            WrapCursor {
                line: 2,
                scalar: 0,
                visual_row: 0,
            },
            WrapCursor {
                line: 3,
                scalar: 0,
                visual_row: 0,
            },
        ] {
            cursor = move_in_wrapped_lines(&lines, cursor, 0, true, 20);
            assert_eq!(cursor, expected);
        }
        for expected in [
            WrapCursor {
                line: 2,
                scalar: 0,
                visual_row: 0,
            },
            WrapCursor {
                line: 1,
                scalar: 37,
                visual_row: 2,
            },
        ] {
            cursor = move_in_wrapped_lines(&lines, cursor, 0, false, 20);
            assert_eq!(cursor, expected);
        }
    }

    #[test]
    fn editor_moves_across_wrapped_and_empty_lines_without_losing_offset() {
        let editor =
            Editor::new("short\n1234567890123456 wordnext MORE words again\n\nlast".into());
        let mut cursor = editor.wrapped_cursor_at(0, 20, false).unwrap().unwrap();
        for (down, expected_offset) in [
            (true, 6),
            (true, 23),
            (true, 43),
            (true, 49),
            (true, 50),
            (false, 49),
            (false, 43),
        ] {
            let (offset, next) = editor.move_wrapped_cursor(cursor, 0, down, 20).unwrap();
            assert_eq!(offset, expected_offset);
            cursor = next;
        }
    }

    #[test]
    fn counted_visual_motion_matches_neovim_and_repeated_steps() {
        let editor = Editor::new("ab ".repeat(100));
        let cursor = editor.wrapped_cursor_at(0, 20, false).unwrap().unwrap();
        let (offset, after) = editor
            .move_wrapped_cursor_count(cursor, 0, true, 20, 3)
            .unwrap();
        assert_eq!(offset, 54); // Clean Neovim 3gj reports byte column 55.
        assert_eq!(
            editor
                .move_wrapped_cursor_count(after, 0, false, 20, 99)
                .unwrap()
                .0,
            0
        );
        assert_eq!(
            editor
                .move_wrapped_cursor_count(cursor, 0, true, 20, 0)
                .unwrap(),
            (0, cursor)
        );

        let editor =
            Editor::new("short\n1234567890123456 wordnext MORE words again\n\nlast".into());
        let cursor = editor.wrapped_cursor_at(0, 20, false).unwrap().unwrap();
        for count in 1..=10 {
            let batched = editor
                .move_wrapped_cursor_count(cursor, 0, true, 20, count)
                .unwrap();
            let mut repeated = (0, cursor);
            for _ in 0..count {
                repeated = editor.move_wrapped_cursor(repeated.1, 0, true, 20).unwrap();
            }
            assert_eq!(batched, repeated, "count {count}");
        }

        let editor = Editor::new("a".repeat(131_072));
        let cursor = editor.wrapped_cursor_at(0, 80, false).unwrap().unwrap();
        let (offset, target) = editor
            .move_wrapped_cursor_count(cursor, 0, true, 80, 9999)
            .unwrap();
        assert!(offset < editor.text.len());
        assert_eq!(target.visual_row, 1638);
    }

    #[test]
    fn visual_motion_crosses_closed_fold_as_one_row() {
        let text = "first\nfn demo() {\n  x();\n}\nlast";
        let mut editor = Editor::new(text.into()).for_path(std::path::Path::new("synthetic.rs"));
        let mut engine = crate::syntax_folds::Engine::default();
        let folds = engine
            .compute(
                crate::syntax::Language::Rust,
                text,
                &std::sync::atomic::AtomicUsize::new(0),
            )
            .unwrap();
        assert!(editor.apply_parser_folds(editor.version, Ok(folds)));
        editor.key("z");
        editor.key("M");
        let mut cursor = editor.wrapped_cursor_at(0, 20, false).unwrap().unwrap();
        for (down, line) in [(true, 1), (true, 4), (false, 1), (false, 0)] {
            let (offset, next) = editor.move_wrapped_cursor(cursor, 0, down, 20).unwrap();
            assert_eq!(next.line, line);
            assert_eq!(offset, editor.line_starts[line]);
            cursor = next;
        }
        let mut cursor = editor.wrapped_cursor_at(4, 20, false).unwrap().unwrap();
        for (down, line, scalar) in [(true, 1, 4), (true, 4, 3), (false, 1, 4), (false, 0, 4)] {
            let (offset, next) = editor.move_wrapped_cursor(cursor, 4, down, 20).unwrap();
            assert_eq!((next.line, next.scalar), (line, scalar));
            assert_eq!(offset, editor.line_starts[line] + scalar);
            if line == 1 {
                let viewport = editor
                    .wrapped_viewport(
                        WrappedTop {
                            source_line: 0,
                            visual_row: 0,
                        },
                        4,
                        20,
                    )
                    .unwrap();
                assert_eq!(
                    editor.wrapped_cursor_cell(&viewport, next),
                    Some(WrappedCursorCell {
                        row: 1,
                        columns: 0..1
                    })
                );
            }
            cursor = next;
        }
    }

    #[test]
    fn gj_gk_preserve_virtual_row_within_multirow_tab() {
        let mut editor = Editor::new(String::new());
        editor.indent_style.tabstop = 32;
        let lines: Vec<_> = ["\tX", "end"]
            .iter()
            .map(|line| {
                editor
                    .wrapped_positions(&line.chars().collect::<Vec<_>>(), 12)
                    .unwrap()
            })
            .collect();
        // Clean Neovim starts at the tab's last cell. gj reaches the short
        // next line, then two gk motions visit two rows of the same tab.
        let tab = WrapCursor {
            line: 0,
            scalar: 0,
            visual_row: 2,
        };
        let next = move_in_wrapped_lines(&lines, tab, 7, true, 12);
        assert_eq!(
            next,
            WrapCursor {
                line: 1,
                scalar: 2,
                visual_row: 0
            }
        );
        let back = move_in_wrapped_lines(&lines, next, 7, false, 12);
        assert_eq!(back, tab);
        let above = move_in_wrapped_lines(&lines, back, 7, false, 12);
        assert_eq!(
            above,
            WrapCursor {
                line: 0,
                scalar: 0,
                visual_row: 1
            }
        );
    }
}

#[cfg(test)]
mod actual_viewport_audit {
    use super::*;
    #[test]
    fn research_center_top_matches_five_clean_neovim_references() {
        // The source's smoothscroll/atend path counts half the space left
        // after the full cursor line, then walks displayed rows upward. This
        // experiment covers only these finite synthetic cases; it is not wired.
        let text = (1..=30)
            .map(|i| format!("line{i:02} {}", "x".repeat((i % 3) * 30)))
            .collect::<Vec<_>>()
            .join("\n");
        let editor = Editor::new(text);
        for (one_based_line, expected_line, expected_row) in
            [(1, 1, 0), (3, 1, 1), (6, 4, 1), (15, 13, 1), (29, 27, 0)]
        {
            let line = one_based_line - 1;
            let height = editor.wrapped_final_row(line, 30, None).unwrap() + 1;
            let above = 10usize.saturating_sub(height) / 2;
            let mut top = WrappedTop {
                source_line: line,
                visual_row: 0,
            };
            for _ in 0..above {
                top = editor.step_wrapped_top(top, false, 30).unwrap();
            }
            assert_eq!(
                top,
                WrappedTop {
                    source_line: expected_line - 1,
                    visual_row: expected_row
                },
                "line {one_based_line}"
            );
        }
    }

    #[test]
    fn centered_top_matches_attached_neovim_ui_references() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-visual-recenter-ui.json"
        ))
        .unwrap();
        let rows = fixture["rows"].as_u64().unwrap() as usize;
        let columns = fixture["columns"].as_u64().unwrap() as usize;
        let text = (1..=30)
            .map(|i| {
                if i == 10 {
                    "L".repeat(600)
                } else {
                    format!("line{i:02} {}", "x".repeat((i % 3) * 30))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let editor = Editor::new(text);
        let mut cases = 0;
        for case in fixture["cases"].as_array().unwrap() {
            if case["command"] != "zz" {
                continue;
            }
            let line = case["line"].as_u64().unwrap() as usize - 1;
            let scalar = case["col"].as_u64().unwrap() as usize;
            let cursor = editor
                .wrapped_cursor_at(editor.line_starts[line] + scalar, columns, false)
                .unwrap()
                .unwrap();
            let before = WrappedTop {
                source_line: case["before"][0].as_u64().unwrap() as usize - 1,
                visual_row: case["before"][1].as_u64().unwrap() as usize / columns,
            };
            let expected = WrappedTop {
                source_line: case["after"][0].as_u64().unwrap() as usize - 1,
                visual_row: case["after"][1].as_u64().unwrap() as usize / columns,
            };
            assert_eq!(
                editor
                    .wrapped_center_top(before, cursor, rows, columns)
                    .unwrap(),
                expected,
                "line {line} scalar {scalar}"
            );
            cases += 1;
        }
        assert_eq!(cases, 19);
    }

    #[test]
    fn centered_top_matches_additional_neovim_geometries() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-recenter-geometries.json"
        ))
        .unwrap();
        let text = (1..=30)
            .map(|i| {
                if i == 10 {
                    "L".repeat(600)
                } else {
                    format!("line{i:02} {}", "x".repeat((i % 3) * 30))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let editor = Editor::new(text);
        let mut mismatches = Vec::new();
        let mut count = 0;
        for geometry in fixture.as_array().unwrap() {
            let rows = geometry["rows"].as_u64().unwrap() as usize;
            let columns = geometry["columns"].as_u64().unwrap() as usize;
            for case in geometry["cases"].as_array().unwrap() {
                if case["command"] != "zz" {
                    continue;
                }
                let line = case["line"].as_u64().unwrap() as usize - 1;
                let scalar = case["col"].as_u64().unwrap() as usize;
                let cursor = editor
                    .wrapped_cursor_at(editor.line_starts[line] + scalar, columns, false)
                    .unwrap()
                    .unwrap();
                let before = WrappedTop {
                    source_line: case["before"][0].as_u64().unwrap() as usize - 1,
                    visual_row: case["before"][1].as_u64().unwrap() as usize / columns,
                };
                let expected = WrappedTop {
                    source_line: case["after"][0].as_u64().unwrap() as usize - 1,
                    visual_row: case["after"][1].as_u64().unwrap() as usize / columns,
                };
                let actual = editor
                    .wrapped_center_top(before, cursor, rows, columns)
                    .unwrap();
                if actual != expected {
                    mismatches.push(format!("{columns}x{rows} line={} col={scalar} before={before:?} expected={expected:?} actual={actual:?}", line+1));
                }
                count += 1;
            }
        }
        assert_eq!(count, 154);
        assert!(
            mismatches.is_empty(),
            "{} mismatches: {mismatches:?}",
            mismatches.len()
        );
    }

    #[test]
    fn aligned_top_matches_neovim_geometries() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-recenter-geometries.json"
        ))
        .unwrap();
        let original: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-visual-recenter-ui.json"
        ))
        .unwrap();
        let mut geometries = fixture.as_array().unwrap().clone();
        geometries.push(serde_json::json!({
            "rows": original["rows"],
            "columns": original["columns"],
            "scrolloff": 4,
            "cases": original["cases"],
        }));
        let text = (1..=30)
            .map(|i| {
                if i == 10 {
                    "L".repeat(600)
                } else {
                    format!("line{i:02} {}", "x".repeat((i % 3) * 30))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let editor = Editor::new(text);
        let mut mismatches = Vec::new();
        let mut count = 0;
        for geometry in &geometries {
            let rows = geometry["rows"].as_u64().unwrap() as usize;
            let columns = geometry["columns"].as_u64().unwrap() as usize;
            let margin = geometry["scrolloff"].as_u64().unwrap() as usize;
            for case in geometry["cases"].as_array().unwrap() {
                if case["command"] != "zt" {
                    continue;
                }
                let line = case["line"].as_u64().unwrap() as usize - 1;
                let scalar = case["col"].as_u64().unwrap() as usize;
                let cursor = editor
                    .wrapped_cursor_at(editor.line_starts[line] + scalar, columns, false)
                    .unwrap()
                    .unwrap();
                let before = WrappedTop {
                    source_line: case["before"][0].as_u64().unwrap() as usize - 1,
                    visual_row: case["before"][1].as_u64().unwrap() as usize / columns,
                };
                let expected = WrappedTop {
                    source_line: case["after"][0].as_u64().unwrap() as usize - 1,
                    visual_row: case["after"][1].as_u64().unwrap() as usize / columns,
                };
                let actual = editor
                    .wrapped_align_top(before, cursor, rows, columns, margin)
                    .unwrap();
                if actual != expected {
                    mismatches.push(format!("{columns}x{rows} so={margin} line={} col={scalar} before={before:?} expected={expected:?} actual={actual:?}", line+1));
                }
                count += 1;
            }
        }
        assert_eq!(count, 173);
        assert!(
            mismatches.is_empty(),
            "{} mismatches: {mismatches:?}",
            mismatches.len()
        );
    }

    #[test]
    fn aligned_bottom_matches_neovim_geometries() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-recenter-geometries.json"
        ))
        .unwrap();
        let original: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-visual-recenter-ui.json"
        ))
        .unwrap();
        let mut geometries = fixture.as_array().unwrap().clone();
        geometries.push(serde_json::json!({"rows":original["rows"],"columns":original["columns"],"scrolloff":4,"cases":original["cases"]}));
        let text = (1..=30)
            .map(|i| {
                if i == 10 {
                    "L".repeat(600)
                } else {
                    format!("line{i:02} {}", "x".repeat((i % 3) * 30))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let editor = Editor::new(text);
        let mut mismatches = Vec::new();
        let mut count = 0;
        for geometry in &geometries {
            let rows = geometry["rows"].as_u64().unwrap() as usize;
            let columns = geometry["columns"].as_u64().unwrap() as usize;
            let margin = geometry["scrolloff"].as_u64().unwrap() as usize;
            for case in geometry["cases"].as_array().unwrap() {
                if case["command"] != "zb" {
                    continue;
                }
                let line = case["line"].as_u64().unwrap() as usize - 1;
                let scalar = case["col"].as_u64().unwrap() as usize;
                let cursor = editor
                    .wrapped_cursor_at(editor.line_starts[line] + scalar, columns, false)
                    .unwrap()
                    .unwrap();
                let expected = WrappedTop {
                    source_line: case["after"][0].as_u64().unwrap() as usize - 1,
                    visual_row: case["after"][1].as_u64().unwrap() as usize / columns,
                };
                let expected_cursor = (
                    case["cursor"][0].as_u64().unwrap() as usize - 1,
                    case["cursor"][1].as_u64().unwrap() as usize - 1,
                );
                let before = WrappedTop {
                    source_line: case["before"][0].as_u64().unwrap() as usize - 1,
                    visual_row: case["before"][1].as_u64().unwrap() as usize / columns,
                };
                let (actual, moved) = editor
                    .wrapped_align_bottom(before, cursor, rows, columns, margin)
                    .unwrap();
                if actual != expected || (moved.line, moved.scalar) != expected_cursor {
                    mismatches.push(format!("{columns}x{rows} so={margin} line={} col={scalar} expected={expected:?}/{expected_cursor:?} actual={actual:?}/{:?}",line+1,(moved.line,moved.scalar)));
                }
                count += 1;
            }
        }
        assert_eq!(count, 173);
        assert!(
            mismatches.is_empty(),
            "{} mismatches: {mismatches:?}",
            mismatches.len()
        );
    }

    #[test]
    fn recenter_matches_unicode_and_tab_references() {
        check_unicode_recenter();
    }

    #[test]
    fn bottom_redraw_matches_partial_top_grid() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-bottom-redraw.json"
        ))
        .unwrap();
        let text = (1..=30)
            .map(|i| {
                if i == 10 {
                    "añ界\tZ ".repeat(100)
                } else {
                    format!("line{i:02} {}", "añ界\tZ ".repeat((i % 3) * 5))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::new(text);
        editor.indent_style.tabstop = 2;
        let mut redraw_changed_top = 0;
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 10);
        for case in cases {
            let cursor = editor
                .wrapped_cursor_at(editor.line_starts[2], 17, false)
                .unwrap()
                .unwrap();
            let read_top = |name: &str| WrappedTop {
                source_line: case[name][0].as_u64().unwrap() as usize - 1,
                visual_row: case[name][1].as_u64().unwrap() as usize / 17,
            };
            let expected = read_top("after");
            if read_top("instant") != expected {
                redraw_changed_top += 1;
            }
            let (actual, moved) = editor
                .wrapped_align_bottom(read_top("before"), cursor, 8, 17, 0)
                .unwrap();
            assert_eq!(actual, expected, "forced={}", case["forced"]);
            assert_eq!((moved.line, moved.scalar), (2, 0));
        }
        assert_eq!(redraw_changed_top, 4);
        let ascii = (1..=30)
            .map(|i| {
                if i == 10 {
                    "L".repeat(600)
                } else {
                    format!("line{i:02} {}", "x".repeat((i % 3) * 30))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let editor = Editor::new(ascii);
        let cases = fixture["ascii_cases"].as_array().unwrap();
        assert_eq!(cases.len(), 10);
        for case in cases {
            let cursor = editor
                .wrapped_cursor_at(editor.line_starts[2], 17, false)
                .unwrap()
                .unwrap();
            let read_top = |name: &str| WrappedTop {
                source_line: case[name][0].as_u64().unwrap() as usize - 1,
                visual_row: case[name][1].as_u64().unwrap() as usize / 17,
            };
            let (actual, moved) = editor
                .wrapped_align_bottom(read_top("before"), cursor, 8, 17, 0)
                .unwrap();
            assert_eq!(actual, read_top("after"), "ASCII forced={}", case["forced"]);
            assert_eq!((moved.line, moved.scalar), (2, 0));
        }
    }

    fn check_unicode_recenter() {
        let geometries: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-recenter-unicode.json"
        ))
        .unwrap();
        let text = (1..=30)
            .map(|i| {
                if i == 10 {
                    "añ界\tZ ".repeat(100)
                } else {
                    format!("line{i:02} {}", "añ界\tZ ".repeat((i % 3) * 5))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::new(text);
        let mut failures = Vec::new();
        let mut total = 0;
        for geometry in geometries.as_array().unwrap() {
            let rows = geometry["rows"].as_u64().unwrap() as usize;
            let cols = geometry["columns"].as_u64().unwrap() as usize;
            let margin = geometry["scrolloff"].as_u64().unwrap() as usize;
            editor.indent_style.tabstop = geometry["tabstop"].as_u64().unwrap() as usize;
            for case in geometry["cases"].as_array().unwrap() {
                let line = case["line"].as_u64().unwrap() as usize - 1;
                let scalar = case["col"].as_u64().unwrap() as usize;
                let command = case["command"].as_str().unwrap();
                let cursor = editor
                    .wrapped_cursor_at(editor.line_starts[line] + scalar, cols, false)
                    .unwrap()
                    .unwrap();
                let before = WrappedTop {
                    source_line: case["before"][0].as_u64().unwrap() as usize - 1,
                    visual_row: case["before"][1].as_u64().unwrap() as usize / cols,
                };
                let expected = WrappedTop {
                    source_line: case["after"][0].as_u64().unwrap() as usize - 1,
                    visual_row: case["after"][1].as_u64().unwrap() as usize / cols,
                };
                let expected_cursor = (
                    case["cursor"][0].as_u64().unwrap() as usize - 1,
                    case["cursor"][1].as_u64().unwrap() as usize,
                );
                let (top, moved) = match command {
                    "zt" => (
                        editor
                            .wrapped_align_top(before, cursor, rows, cols, margin)
                            .unwrap(),
                        cursor,
                    ),
                    "zz" => (
                        editor
                            .wrapped_center_top(before, cursor, rows, cols)
                            .unwrap(),
                        cursor,
                    ),
                    "zb" => editor
                        .wrapped_align_bottom(before, cursor, rows, cols, margin)
                        .unwrap(),
                    _ => panic!("unknown command"),
                };
                if top != expected || (moved.line, moved.scalar) != expected_cursor {
                    failures.push(format!("{command} {cols}x{rows} so={margin} line={} col={scalar} before={before:?} expected={expected:?}/{expected_cursor:?} actual={top:?}/{:?}",line+1,(moved.line,moved.scalar)));
                }
                total += 1;
            }
        }
        assert_eq!(total, 342);
        assert!(
            failures.is_empty(),
            "{} mismatches: {failures:?}",
            failures.len()
        );
    }

    #[test]
    fn bottom_cursor_correction_preserves_visual_anchor_and_buffer() {
        let text = format!("{}\nnext", "x".repeat(600));
        let mut editor = Editor::new(text.clone());
        editor.key("v");
        let cursor = editor.wrapped_cursor_at(0, 30, false).unwrap().unwrap();
        let (top, moved) = editor
            .wrapped_align_bottom(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                cursor,
                10,
                30,
                4,
            )
            .unwrap();
        assert_eq!(
            top,
            WrappedTop {
                source_line: 0,
                visual_row: 10
            }
        );
        assert_eq!(moved.scalar, 420);
        assert!(editor.commit_wrapped_motion(moved.scalar));
        assert_eq!(editor.mode, super::super::Mode::Visual);
        assert_eq!(editor.selection(), Some(0..421));
        assert_eq!(editor.contents(), text);
        assert!(!editor.dirty());
    }
    #[test]
    #[ignore = "Neovim wrapped viewport recentering is not implemented yet"]
    fn actual_short_reference_diagnostic() {
        let matrix: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-short-matrix.json"
        ))
        .unwrap();
        let mut mismatches = 0;
        let mut steps = 0;
        let mut bad_cases = 0;
        let mut examples = Vec::new();
        for case in matrix["cases"].as_array().unwrap() {
            let rows = case["rows"].as_u64().unwrap() as usize;
            let margin = case["scrolloff"].as_u64().unwrap() as usize;
            if margin != 4 {
                continue;
            }
            let count = case["line_count"].as_u64().unwrap() as usize;
            let text = (0..count)
                .map(|n| format!("line {n:02}"))
                .collect::<Vec<_>>()
                .join("\n");
            let editor = Editor::new(text);
            let mut top = WrappedTop {
                source_line: case["initial_top"].as_u64().unwrap() as usize,
                visual_row: 0,
            };
            let mut prior_cursor = 0usize;
            let mut case_bad = false;
            for (index, item) in case["steps"].as_array().unwrap().iter().enumerate() {
                let cursor_line = item[1].as_u64().unwrap() as usize;
                let expected = item[2].as_u64().unwrap() as usize;
                if cursor_line != prior_cursor {
                    let cursor = WrapCursor {
                        line: cursor_line,
                        scalar: 0,
                        visual_row: 0,
                    };
                    top = editor
                        .follow_wrapped_cursor_with_margin(top, cursor, rows, 20, margin)
                        .unwrap();
                }
                prior_cursor = cursor_line;
                steps += 1;
                if top.source_line != expected || top.visual_row != 0 {
                    case_bad = true;
                    mismatches += 1;
                    if examples.len() < 5 {
                        examples.push(format!("h={rows} n={count} step={index} key={} cursor={cursor_line} expected={expected} actual={:?}", item[0], top));
                    }
                }
            }
            if case_bad {
                bad_cases += 1;
            }
        }
        assert_eq!(
            mismatches, 0,
            "Neovim: {mismatches} mismatched steps of {steps}, {bad_cases} cases; examples: {examples:?}"
        );
    }
    #[test]
    fn bounded_follow_matches_direct_row_recalculation() {
        // Compare the fast row-counting path against a deliberately simple
        // per-step viewport reconstruction, including tabs and wide glyphs.
        fn reference(
            editor: &Editor,
            top: WrappedTop,
            cursor: WrapCursor,
            rows: usize,
            margin: usize,
        ) -> WrappedTop {
            let mut top = editor.follow_wrapped_cursor(top, cursor, rows, 20).unwrap();
            let viewport = editor
                .wrapped_viewport_for_cursor(top, rows, 20, cursor)
                .unwrap();
            let row = editor.wrapped_cursor_cell(&viewport, cursor).unwrap().row;
            let upper = margin.min(rows / 2);
            let lower = margin.min(rows.saturating_sub(1) / 2);
            let (down, distance) = if row < upper {
                (false, upper - row)
            } else if row > rows.saturating_sub(1 + lower) {
                (true, row - rows.saturating_sub(1 + lower))
            } else {
                return top;
            };
            for _ in 0..distance {
                let next = editor
                    .step_wrapped_top_for_cursor(top, down, 20, cursor)
                    .unwrap();
                if next == top
                    || (down
                        && editor
                            .wrapped_viewport_for_cursor(next, rows, 20, cursor)
                            .unwrap()
                            .len()
                            < rows)
                {
                    break;
                }
                top = next;
            }
            top
        }
        let documents = [
            "first\nsecond\nthird\nend".to_string(),
            format!("short\n{}\n\nlast", "ab ".repeat(100)),
            "first\n\tá界 hello\nend\n".to_string(),
        ];
        for text in documents {
            let editor = Editor::new(text);
            for rows in [2, 3, 5, 8] {
                for margin in [0, 1, 4] {
                    for line in 0..editor.line_starts.len() {
                        let cursor = editor
                            .wrapped_cursor_at(editor.line_starts[line], 20, false)
                            .unwrap()
                            .unwrap();
                        for top_line in 0..editor.line_starts.len() {
                            let top = WrappedTop {
                                source_line: top_line,
                                visual_row: 0,
                            };
                            let expected = reference(&editor, top, cursor, rows, margin);
                            let actual = editor
                                .follow_wrapped_cursor_with_margin(top, cursor, rows, 20, margin)
                                .unwrap();
                            assert_eq!(
                                actual, expected,
                                "rows={rows} margin={margin} cursor={cursor:?} top={top:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mixed_wrapped_follow_avoids_empty_rows_at_end_of_file() {
        // These five clean-Neovim traces cover short lines, long lines,
        // mixed heights, multiple viewport sizes, and an empty line.
        check_mixed_reference(Some(&[0, 7, 8, 15, 17]));
    }

    #[test]
    #[ignore = "Neovim mixed-height viewport recentering is not implemented yet"]
    fn actual_mixed_reference_diagnostic() {
        check_mixed_reference(None);
    }

    fn check_mixed_reference(selected: Option<&[usize]>) {
        let matrix: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-mixed-matrix.json"
        ))
        .unwrap();
        let mut mismatches = 0;
        let mut steps = 0;
        let mut bad_cases = 0;
        let mut examples = Vec::new();
        let cases = matrix["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 18);
        for (case_index, case) in cases.iter().enumerate() {
            if selected.is_some_and(|selected| !selected.contains(&case_index)) {
                continue;
            }
            let rows = case["rows"].as_u64().unwrap() as usize;
            let margin = case["scrolloff"].as_u64().unwrap() as usize;
            let lines: Vec<_> = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap())
                .collect();
            let editor = Editor::new(lines.join("\n"));
            let mut top = WrappedTop {
                source_line: case["initial_topline"].as_u64().unwrap() as usize - 1,
                visual_row: case["initial_skipcol"].as_u64().unwrap() as usize / 20,
            };
            let mut prior_cursor = editor.wrapped_cursor_at(0, 20, false).unwrap().unwrap();
            let mut case_bad = false;
            for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let pos = step["cursor"].as_array().unwrap();
                let line = pos[1].as_u64().unwrap() as usize - 1;
                // This fixture is ASCII: Neovim byte columns equal scalar offsets.
                let scalar = pos[2].as_u64().unwrap() as usize - 1;
                let cursor = editor
                    .wrapped_cursor_at(editor.line_starts[line] + scalar, 20, false)
                    .unwrap()
                    .unwrap();
                if cursor != prior_cursor {
                    top = editor
                        .follow_wrapped_cursor_with_margin(top, cursor, rows, 20, margin)
                        .unwrap();
                }
                prior_cursor = cursor;
                let expected = WrappedTop {
                    source_line: step["topline"].as_u64().unwrap() as usize - 1,
                    visual_row: step["skipcol"].as_u64().unwrap() as usize / 20,
                };
                steps += 1;
                if top != expected {
                    case_bad = true;
                    mismatches += 1;
                    if examples.len() < 5 {
                        examples.push(format!("h={rows} step={index} key={} cursor={cursor:?} expected={expected:?} actual={top:?}", step["key"]));
                    }
                }
            }
            if case_bad {
                bad_cases += 1;
            }
        }
        assert_eq!(
            mismatches, 0,
            "Neovim mixed: {mismatches} mismatched steps of {steps}, {bad_cases} cases; examples: {examples:?}"
        );
    }
}

#[cfg(test)]
mod virtual_cursor_reference {
    use super::*;

    #[test]
    fn wrapped_virtual_spans_match_neovim_block_reference() {
        let data: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-wrap-wheel-block-vspan.json"
        ))
        .unwrap();
        let mut steps = 0;
        for (case_index, case) in data["cases"].as_array().unwrap().iter().enumerate() {
            let lines: Vec<_> = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect();
            let mut editor = Editor::new(lines.join("\n"));
            editor.indent_style.tabstop = 8;
            let width = case["width"].as_u64().unwrap() as usize;
            for (step_index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let cursor = step["cursor"].as_array().unwrap();
                let line = cursor[1].as_u64().unwrap() as usize - 1;
                let byte = cursor[2].as_u64().unwrap() as usize - 1;
                let coladd = cursor[3].as_u64().unwrap() as usize;
                let scalar = lines[line]
                    .char_indices()
                    .take_while(|(index, _)| *index < byte)
                    .count();
                let resolved = editor
                    .wrapped_virtual_cursor_at(editor.line_starts[line] + scalar, coladd, width)
                    .unwrap()
                    .unwrap();
                let start = resolved.cursor.visual_row * width + resolved.column + 1;
                let end = start + resolved.cell_width - 1;
                let expected = step["virtspan"].as_array().unwrap();
                assert_eq!(
                    (start, end),
                    (
                        expected[0].as_u64().unwrap() as usize,
                        expected[1].as_u64().unwrap() as usize
                    ),
                    "case={case_index} step={step_index}"
                );
                steps += 1;
            }
        }
        assert_eq!(steps, 288);
    }

    #[test]
    fn virtual_cursor_respects_wide_cells_and_tab_offsets() {
        let wide = Editor::new("界".into());
        let within = wide.wrapped_virtual_cursor_at(0, 1, 8).unwrap().unwrap();
        assert_eq!((within.column, within.cell_width), (0, 2));
        let beyond = wide.wrapped_virtual_cursor_at(0, 2, 8).unwrap().unwrap();
        assert_eq!((beyond.column, beyond.cell_width), (2, 1));
        let tab = Editor::new("\t".into());
        let in_tab = tab.wrapped_virtual_cursor_at(0, 1, 8).unwrap().unwrap();
        assert_eq!((in_tab.column, in_tab.cell_width), (1, 1));
    }

    #[test]
    fn virtual_cursor_cell_can_occupy_padding_after_text() {
        let editor = Editor::new("abc".into());
        let virtual_cursor = editor.wrapped_virtual_cursor_at(3, 1, 8).unwrap().unwrap();
        let viewport = editor
            .wrapped_viewport_for_cursor(
                WrappedTop {
                    source_line: 0,
                    visual_row: 0,
                },
                1,
                8,
                virtual_cursor.cursor,
            )
            .unwrap();
        assert_eq!(
            editor.wrapped_virtual_cursor_cell(&viewport, virtual_cursor, 8),
            Some(WrappedCursorCell {
                row: 0,
                columns: 4..5
            })
        );
        assert_eq!(editor.contents(), "abc");
    }
}
