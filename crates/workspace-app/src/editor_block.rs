//! Display-cell helpers and typed rectangular put. Never accesses a clipboard.
use super::*;
use unicode_width::UnicodeWidthChar;

#[derive(Debug)]
struct Glyph {
    text: String,
    start: usize,
    end: usize,
}
fn glyphs(line: &str, tabstop: usize, start_column: usize) -> Vec<Glyph> {
    let mut result: Vec<Glyph> = Vec::new();
    let mut column = start_column;
    for ch in line.chars() {
        let mut width = if ch == '\t' {
            tabstop - column % tabstop
        } else {
            ch.width().unwrap_or(1)
        };
        if width == 0 {
            if let Some(previous) = result.last_mut() {
                previous.text.push(ch);
                continue;
            }
            // A leading combining mark has no base glyph; retain it as a cell.
            width = 1;
        }
        result.push(Glyph {
            text: ch.to_string(),
            start: column,
            end: column + width,
        });
        column += width;
    }
    result
}
fn width_at(line: &str, tabstop: usize, column: usize) -> usize {
    glyphs(line, tabstop, column)
        .last()
        .map_or(0, |g| g.end - column)
}
/// Split boundary glyphs into spaces, preserving every full glyph outside the
/// selected columns. A fully selected tab remains a tab in the register.
fn split_cells(
    line: &str,
    tabstop: usize,
    left: usize,
    right: usize,
    pad_left: bool,
) -> (String, String, String) {
    let mut before = String::new();
    let mut inside = String::new();
    let mut after = String::new();
    let mut end = 0;
    for glyph in glyphs(line, tabstop, 0) {
        end = glyph.end;
        for (from, to, target) in [
            (0, left, &mut before),
            (left, right, &mut inside),
            (right, usize::MAX, &mut after),
        ] {
            let start = glyph.start.max(from);
            let stop = glyph.end.min(to);
            if start < stop {
                if start == glyph.start && stop == glyph.end {
                    target.push_str(&glyph.text);
                } else {
                    target.push_str(&" ".repeat(stop - start));
                }
            }
        }
    }
    if pad_left && end < left {
        before.push_str(&" ".repeat(left - end));
    }
    if end < right {
        inside.push_str(&" ".repeat(right - end.max(left)));
    }
    (before, inside, after)
}
impl Editor {
    pub(super) fn cell_column(&self, offset: usize) -> usize {
        let start = self.line_start(offset);
        width_at(
            &self.text[start..offset].iter().collect::<String>(),
            self.indent_style.tabstop,
            0,
        )
    }
    pub(super) fn cell_offset(&self, row: usize, column: usize) -> usize {
        let start = self.line_number(row + 1);
        let mut cell = 0;
        for offset in start..self.line_end(start) {
            let ch = self.text[offset];
            let width = self.display_cell_width(ch, cell);
            if cell + width > column {
                return offset;
            }
            cell += width;
        }
        self.line_end(start)
    }
    pub(super) fn put_block_register(
        &mut self,
        value: &str,
        width: usize,
        count: usize,
        after: bool,
    ) -> Effect {
        if self.mode != Mode::Normal {
            self.message =
                "Pegado rectangular requiere destino normal; selección y buffer conservados".into();
            return Effect::None;
        }
        let count = count.clamp(1, 9999);
        let rows: Vec<_> = value.split('\n').collect();
        let Some(cells) = width
            .checked_mul(count)
            .and_then(|width| width.checked_mul(rows.len()))
        else {
            self.message = "Pegado rectangular fuera de límites".into();
            return Effect::None;
        };
        if width == 0
            || rows.len() > 4096
            || cells > 2_000_000
            || value.len().saturating_mul(count) > 8 * 1024 * 1024
        {
            self.message = "Pegado rectangular fuera de límites".into();
            return Effect::None;
        }
        let row = self.position().0;
        let column = self.cell_column(self.cursor)
            + if after {
                self.text
                    .get(self.cursor)
                    .filter(|ch| **ch != '\n')
                    .map_or(0, |ch| {
                        if *ch == '\t' {
                            self.tab_padding(self.cell_column(self.cursor))
                        } else {
                            ch.width().unwrap_or(1)
                        }
                    })
            } else {
                0
            };
        if column.saturating_mul(rows.len()) > 2_000_000
            || row.saturating_add(rows.len()) > 2_000_000
        {
            self.message = "Pegado rectangular fuera de límites".into();
            return Effect::None;
        }
        let mut lines: Vec<_> = self.contents().split('\n').map(str::to_owned).collect();
        lines.resize(lines.len().max(row + rows.len()), String::new());
        let mut added = 0usize;
        for (index, text) in rows.iter().enumerate() {
            let (before, _, rest) = split_cells(
                &lines[row + index],
                self.indent_style.tabstop,
                column,
                column,
                true,
            );
            let mut inserted = String::new();
            let mut at = column;
            for repetition in 0..count {
                inserted.push_str(text);
                let used = width_at(text, self.indent_style.tabstop, at);
                at += used;
                if repetition + 1 < count {
                    let padding = width.saturating_sub(used);
                    inserted.push_str(&" ".repeat(padding));
                    at += padding;
                }
            }
            // Padding is only required between repetitions, not after the last.
            added = added.saturating_add(before.len() + inserted.len() + rest.len());
            if added > 8 * 1024 * 1024 {
                self.message = "Pegado rectangular excede 8 MiB".into();
                return Effect::None;
            }
            lines[row + index] = before + &inserted + &rest;
        }
        let text = lines.join("\n");
        if text.chars().count() > 2_000_000 || text.len() > 8 * 1024 * 1024 {
            self.message = "Pegado rectangular excede el límite de edición".into();
            return Effect::None;
        }
        self.checkpoint();
        self.text = text.chars().collect();
        self.changed();
        self.cursor = self.cell_offset(row, column);
        self.clamp();
        Effect::None
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedBlockBounds {
    pub lines: Range<usize>,
    pub columns: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Selection {
    anchor_row: usize,
    anchor_column: usize,
    anchor_width: usize,
    row: usize,
    column: usize,
    width: usize,
    expected_cursor: usize,
    wanted: usize,
    eol: bool,
}
pub(super) struct Insertion {
    rows: Vec<(usize, usize)>,
    column: usize,
    first: usize,
    prefix: String,
    suffix: String,
    lines: usize,
    return_column: usize,
}
#[derive(Clone, Copy)]
struct Bounds {
    top: usize,
    bottom: usize,
    left: usize,
    right: usize,
    width: usize,
}
impl Editor {
    fn block_bounds(&self) -> Option<Bounds> {
        let selection = self.block?;
        let top = self.folded_first_row(
            selection
                .anchor_row
                .min(selection.row)
                .min(self.line_starts.len() - 1),
        );
        let bottom = self.folded_last_row(
            selection
                .anchor_row
                .max(selection.row)
                .min(self.line_starts.len() - 1),
        );
        let left = selection.anchor_column.min(selection.column);
        let right = if selection.eol {
            (top..=bottom)
                .map(|row| {
                    let start = self.line_starts[row];
                    width_at(
                        &self.text[start..self.line_end(start)]
                            .iter()
                            .collect::<String>(),
                        self.indent_style.tabstop,
                        0,
                    )
                })
                .max()
                .unwrap_or(left)
                .saturating_add(1)
        } else {
            (selection.anchor_column + selection.anchor_width)
                .max(selection.column + selection.width)
        };
        Some(Bounds {
            top,
            bottom,
            left,
            right: right.max(left + 1),
            width: right
                .saturating_sub(left + usize::from(selection.eol))
                .max(1),
        })
    }
    pub fn block_row_columns(&self, start: usize) -> Option<(usize, usize)> {
        if self.mode != Mode::VisualBlock {
            return None;
        }
        let bounds = self.block_bounds()?;
        let row = self
            .line_starts
            .partition_point(|p| *p <= start)
            .saturating_sub(1);
        (row >= bounds.top && row <= bounds.bottom).then_some((bounds.left, bounds.right))
    }
    pub fn block_selected(&self, offset: usize) -> bool {
        let Some((left, right)) =
            self.block_row_columns(self.line_start(offset.min(self.text.len())))
        else {
            return false;
        };
        let column = self.cell_column(offset.min(self.text.len()));
        let width = self
            .text
            .get(offset)
            .filter(|ch| **ch != '\n')
            .map_or(1, |ch| {
                if *ch == '\t' {
                    self.tab_padding(column)
                } else {
                    ch.width().unwrap_or(1)
                }
            });
        column < right && column + width > left
    }
    pub(super) fn sync_block_cursor(&mut self) {
        let Some(mut block) = self.block else {
            return;
        };
        if self.mode != Mode::VisualBlock {
            self.block = None;
            return;
        }
        if block.expected_cursor != self.cursor {
            block.row = self.position().0;
            block.column = self.cell_column(self.cursor);
            block.width = self
                .text
                .get(self.cursor)
                .filter(|ch| **ch != '\n')
                .map_or(1, |ch| {
                    if *ch == '\t' {
                        self.tab_padding(block.column)
                    } else {
                        ch.width().unwrap_or(1).max(1)
                    }
                });
            block.expected_cursor = self.cursor;
            block.wanted = block.column;
            block.eol = false;
            self.block = Some(block);
        }
    }
    pub fn display_cell_width(&self, ch: char, column: usize) -> usize {
        if ch == '\t' {
            self.tab_padding(column)
        } else {
            let width = ch.width().unwrap_or(1);
            if width == 0 && column == 0 { 1 } else { width }
        }
    }
    /// Resolve the active block cursor using the real buffer offset and its
    /// virtual column. Do not use display_cursor(), which may numerically
    /// overlap the following logical line for virtual positions.
    pub fn wrapped_block_cursor(
        &self,
        width: usize,
    ) -> Result<Option<super::WrappedVirtualCursor>, &'static str> {
        let Some(block) = self.block.filter(|_| self.mode == Mode::VisualBlock) else {
            return Ok(None);
        };
        let coladd = block.column.saturating_sub(self.cell_column(self.cursor));
        self.wrapped_virtual_cursor_at(self.cursor, coladd, width)
    }

    #[cfg(test)]
    pub(super) fn block_anchor_for_test(&self) -> Option<(usize, usize, usize)> {
        self.block
            .map(|block| (block.anchor_row, block.anchor_column, block.anchor_width))
    }

    /// Whether the active block selection tracks the end of each line.
    pub fn block_wants_eol(&self) -> bool {
        self.mode == Mode::VisualBlock && self.block.is_some_and(|block| block.eol)
    }

    /// Resolve a rectangular selection into wrapped virtual columns.
    pub fn wrapped_block_bounds(
        &self,
        width: usize,
    ) -> Result<Option<WrappedBlockBounds>, &'static str> {
        let Some(block) = self.block.filter(|_| self.mode == Mode::VisualBlock) else {
            return Ok(None);
        };
        let last = self.line_starts.len() - 1;
        let top = self.folded_first_row(block.anchor_row.min(block.row).min(last));
        let bottom = self.folded_last_row(block.anchor_row.max(block.row).min(last));
        let resolve =
            |row: usize, column: usize| -> Result<super::WrappedVirtualCursor, &'static str> {
                let offset = self.cell_offset(row, column);
                let coladd = column.saturating_sub(self.cell_column(offset));
                self.wrapped_virtual_cursor_at(offset, coladd, width)?
                    .ok_or("Wrap: cursor de bloque oculto")
            };
        let anchor = resolve(block.anchor_row, block.anchor_column)?;
        let cursor = resolve(block.row, block.column)?;
        let a = anchor.cursor.visual_row * width + anchor.column;
        let b = cursor.cursor.visual_row * width + cursor.column;
        let left = a.min(b);
        let mut right = (a + anchor.cell_width).max(b + cursor.cell_width);
        if block.eol {
            for row in top..=bottom {
                let start = self.line_starts[row];
                let positions =
                    self.wrapped_positions(&self.text[start..self.line_end(start)], width)?;
                let end = positions
                    .last()
                    .map_or(0, |position| position.row * width + position.end);
                right = right.max(end.saturating_add(1));
            }
        }
        Ok(Some(WrappedBlockBounds {
            lines: top..bottom + 1,
            columns: left..right,
        }))
    }

    /// Move a block cursor without changing its anchor or the buffer. The
    /// virtual offset may refer to tab cells or padding after line end.
    pub fn commit_wrapped_block_wheel(
        &mut self,
        offset: usize,
        coladd: usize,
        width: usize,
        goal: usize,
    ) -> bool {
        if self.mode != Mode::VisualBlock || self.composing() || self.block.is_none() {
            return false;
        }
        let Ok(Some(resolved)) = self.wrapped_virtual_cursor_at(offset, coladd, width) else {
            return false;
        };
        let base = self.cell_column(offset);
        let glyph = self.text.get(offset).copied().filter(|ch| *ch != '\n');
        let glyph_width = glyph
            .filter(|ch| *ch != '\t')
            .and_then(|ch| ch.width())
            .unwrap_or(1)
            .max(1);
        let extra = if glyph.is_some_and(|ch| ch != '\t') && glyph_width > 1 && coladd < glyph_width
        {
            0
        } else {
            coladd
        };
        let Some(column) = base
            .checked_add(extra)
            .filter(|column| *column <= 2_000_000)
        else {
            return false;
        };
        self.cursor = offset;
        if let Some(block) = &mut self.block {
            block.row = resolved.cursor.line;
            block.column = column;
            block.width = if glyph == Some('\t') || extra > 0 {
                1
            } else {
                glyph_width
            };
            block.expected_cursor = offset;
            if block.eol && goal < i32::MAX as usize - 1 {
                block.eol = false;
            }
            if !block.eol {
                block.wanted = column;
            }
        }
        self.desired_column = None;
        true
    }

    pub fn block_cursor_column(&self, start: usize) -> Option<usize> {
        let block = self.block.filter(|_| self.mode == Mode::VisualBlock)?;
        (self.line_starts[block.row] == start).then_some(block.column)
    }
    pub(super) fn block_display_position(&self) -> Option<(usize, usize)> {
        (self.mode == Mode::VisualBlock)
            .then_some(self.block)
            .flatten()
            .map(|block| (block.row, block.column))
    }
    fn block_horizontal(
        &self,
        row: usize,
        mut column: usize,
        count: usize,
        forward: bool,
    ) -> usize {
        let start = self.line_starts[row];
        let text: String = self.text[start..self.line_end(start)].iter().collect();
        let cells = glyphs(&text, self.indent_style.tabstop, 0);
        for _ in 0..count {
            if forward {
                let i = cells
                    .partition_point(|cell| cell.start <= column)
                    .saturating_sub(1);
                column = cells
                    .get(i)
                    .filter(|g| g.start <= column && column < g.end && g.text != "\t")
                    .map_or(column.saturating_add(1), |g| g.end);
            } else {
                let i = cells
                    .partition_point(|cell| cell.start < column)
                    .saturating_sub(1);
                column = cells
                    .get(i)
                    .filter(|g| g.start < column && column <= g.end && g.text != "\t")
                    .map_or(column.saturating_sub(1), |g| g.start);
            }
            column = column.min(2_000_000);
        }
        column
    }
    fn block_move(&mut self, row: usize, column: usize, eol: bool) {
        let row = row.min(self.line_starts.len() - 1);
        let line = self.line_starts[row];
        let text: String = self.text[line..self.line_end(line)].iter().collect();
        let wanted = if eol {
            width_at(&text, self.indent_style.tabstop, 0)
        } else {
            column.min(2_000_000)
        };
        let column = glyphs(&text, self.indent_style.tabstop, 0)
            .iter()
            .find(|g| g.start <= wanted && wanted < g.end && g.text != "\t")
            .map_or(wanted, |g| g.start);
        let cursor = self.cell_offset(row, column);
        let width = self
            .text
            .get(cursor)
            .filter(|ch| **ch != '\n')
            .map_or(1, |ch| {
                if *ch == '\t' {
                    1
                } else {
                    ch.width().unwrap_or(1).max(1)
                }
            });
        self.cursor = cursor;
        if let Some(block) = &mut self.block {
            block.row = row;
            block.column = column.min(2_000_000);
            block.width = width;
            block.expected_cursor = cursor;
            block.wanted = wanted;
            block.eol = eol;
        }
    }
    fn finish_block(&mut self, row: usize, column: usize) {
        let eol = self.block.is_some_and(|block| block.eol);
        self.mode = Mode::Normal;
        self.block = None;
        self.cursor = self.cell_offset(row, column);
        self.pending.clear();
        self.count = 0;
        self.chosen_register = None;
        // virtualedit=block with $ retains the NUL-column cursor after deletion.
        if !eol {
            self.clamp();
        }
    }
    pub(super) fn block_selected_rows(&self) -> Option<Range<usize>> {
        let block = self.block.as_ref()?;
        Some(block.anchor_row.min(block.row)..block.anchor_row.max(block.row).saturating_add(1))
    }

    pub(super) fn block_key(&mut self, key: &str) -> Option<Effect> {
        if self.mode == Mode::VisualBlock
            && key != "<Esc>"
            && (key == "z" || self.pending.starts_with('z'))
        {
            return None;
        }
        if key == "<Esc>" && self.block_insert.is_some() {
            self.finish_block_insert();
            return Some(Effect::None);
        }
        if key == "<C-v>" && matches!(self.mode, Mode::Normal | Mode::Visual | Mode::VisualLine) {
            let wanted = self.cell_column(self.cursor);
            let mut anchor = if self.selection().is_some() {
                self.anchor
            } else {
                self.cursor
            };
            if self.mode == Mode::Normal
                && let Some(rows) = self.closed_fold_rows(self.position().0)
            {
                anchor = self.line_starts[rows.start];
                self.cursor = self.line_end(self.line_starts[rows.end - 1]);
            }
            let anchor_row = self
                .line_starts
                .partition_point(|p| *p <= anchor)
                .saturating_sub(1);
            let anchor_column = self.cell_column(anchor);
            let anchor_width = self
                .text
                .get(anchor)
                .filter(|ch| **ch != '\n')
                .map_or(1, |ch| {
                    if *ch == '\t' {
                        self.tab_padding(anchor_column)
                    } else {
                        ch.width().unwrap_or(1).max(1)
                    }
                });
            self.anchor = anchor;
            self.block = Some(Selection {
                anchor_row,
                anchor_column,
                anchor_width,
                row: self.position().0,
                column: self.cell_column(self.cursor),
                width: self
                    .text
                    .get(self.cursor)
                    .filter(|ch| **ch != '\n')
                    .map_or(1, |ch| {
                        if *ch == '\t' {
                            self.tab_padding(self.cell_column(self.cursor))
                        } else {
                            ch.width().unwrap_or(1).max(1)
                        }
                    }),
                expected_cursor: self.cursor,
                wanted,
                eol: false,
            });
            self.mode = Mode::VisualBlock;
            self.count = 0;
            self.pending.clear();
            return Some(Effect::None);
        }
        if self.mode != Mode::VisualBlock {
            if self.block_insert.is_some()
                && matches!(
                    key,
                    "<Left>" | "<Right>" | "<Up>" | "<Down>" | "<Home>" | "<End>"
                )
            {
                self.block_insert = None;
                self.message =
                    "Movimiento en Insert: réplica rectangular cancelada; edición conservada"
                        .into();
            }
            return None;
        }
        if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() && (key != "0" || self.count > 0) {
            self.count = (self.count * 10 + key.parse::<usize>().unwrap()).min(9999);
            return Some(Effect::None);
        }
        let explicit = self.count > 0;
        let count = std::mem::take(&mut self.count).max(1);
        let block = self.block?;
        match key {
            "<Esc>" | "<C-v>" => self.finish_block(block.row, block.column),
            "v" | "V" => {
                self.mode = if key == "v" {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
                self.block = None;
                self.clamp();
            }
            "j" | "<Down>" => self.block_move(
                self.source_row(self.visible_row(block.row).saturating_add(count)),
                block.wanted,
                block.eol,
            ),
            "k" | "<Up>" => self.block_move(
                self.source_row(self.visible_row(block.row).saturating_sub(count)),
                block.wanted,
                block.eol,
            ),
            "h" | "<Left>" => self.block_move(
                block.row,
                self.block_horizontal(block.row, block.column, count, false),
                false,
            ),
            "l" | "<Right>" => self.block_move(
                block.row,
                self.block_horizontal(block.row, block.column, count, true),
                false,
            ),
            "0" => self.block_move(block.row, 0, false),
            "$" => {
                let start = self.line_starts[block.row];
                let width = width_at(
                    &self.text[start..self.line_end(start)]
                        .iter()
                        .collect::<String>(),
                    self.indent_style.tabstop,
                    0,
                );
                self.block_move(block.row, width, true);
            }
            "<C-d>" | "<C-u>" | "<C-f>" | "<C-b>" | "<PageDown>" | "<PageUp>" | "<C-e>"
            | "<C-y>" => {
                let effect = self.scroll_command(key, count, explicit)?;
                if matches!(effect, Effect::WrappedScroll(..)) {
                    return Some(effect);
                }
                self.block_move(self.position().0, block.wanted, block.eol);
                return Some(effect);
            }
            "G" => {
                self.record_history_jump();
                self.block_move(
                    if explicit {
                        count - 1
                    } else {
                        self.line_starts.len() - 1
                    },
                    block.column,
                    block.eol,
                );
            }
            "o" | "O" => {
                let mut swapped = block;
                std::mem::swap(&mut swapped.anchor_column, &mut swapped.column);
                std::mem::swap(&mut swapped.anchor_width, &mut swapped.width);
                if key == "o" {
                    std::mem::swap(&mut swapped.anchor_row, &mut swapped.row);
                }
                self.cursor = self.cell_offset(swapped.row, swapped.column);
                swapped.expected_cursor = self.cursor;
                self.anchor = self.cell_offset(swapped.anchor_row, swapped.anchor_column);
                self.block = Some(swapped);
            }
            "y" | "Y" | "d" | "x" | "X" | "D" => {
                self.block_operate(if matches!(key, "y" | "Y") { 'y' } else { 'd' }, key == "D")
            }
            "c" | "s" | "C" => self.begin_block_insert('c', key == "C"),
            "I" | "A" => self.begin_block_insert(key.chars().next().unwrap(), false),
            "u" | "U" | "~" => self.block_case(key.chars().next().unwrap()),
            ">" | "<" => self.block_shift(key == ">", count),
            "S" | "R" => {
                let bounds = self.block_bounds()?;
                let start = self.line_starts[bounds.top];
                let end = (self.line_end(self.line_starts[bounds.bottom]) + 1).min(self.text.len());
                self.operate('c', start..end, true);
            }
            _ => {
                self.count = if explicit { count } else { 0 };
                return None;
            }
        }
        Some(Effect::None)
    }
    fn block_capture(&mut self, bounds: Bounds, tail: bool, op: char) -> bool {
        if (bounds.bottom - bounds.top + 1).saturating_mul(bounds.right.saturating_sub(bounds.left))
            > 2_000_000
        {
            self.message = "Selección rectangular excede el límite de edición".into();
            return false;
        }
        let mut rows = Vec::new();
        for row in bounds.top..=bounds.bottom {
            let start = self.line_starts[row];
            let line: String = self.text[start..self.line_end(start)].iter().collect();
            let right = if tail {
                width_at(&line, self.indent_style.tabstop, 0).max(bounds.left)
            } else {
                bounds.right
            };
            rows.push(split_cells(&line, self.indent_style.tabstop, bounds.left, right, false).1);
        }
        let value = rows.join("\n");
        let name = self.effective_register();
        self.chosen_register = None;
        if let Err(error) =
            self.registers
                .capture_kind(name, op, value, registers::Kind::Block(bounds.width))
        {
            self.message = error;
            return false;
        }
        if self.clipboard_enabled && matches!(name, '+' | '*') {
            self.clipboard_write = self
                .registers
                .get_kind(name)
                .map(|(text, _)| (text, false, Some(bounds.width)));
        }
        true
    }
    fn block_operate(&mut self, op: char, tail: bool) {
        let Some(mut bounds) = self.block_bounds() else {
            return;
        };
        if tail {
            bounds = self.block_tail_bounds(bounds);
            if let Some(block) = &mut self.block {
                block.eol = true;
            }
        }
        if !self.block_capture(bounds, false, op) {
            return;
        }
        if op == 'y' {
            self.finish_block(bounds.top, bounds.left);
            return;
        }
        let mut lines: Vec<_> = self.contents().split('\n').map(str::to_owned).collect();
        for line in &mut lines[bounds.top..=bounds.bottom] {
            let right = bounds.right;
            let (before, _, after) =
                split_cells(line, self.indent_style.tabstop, bounds.left, right, false);
            *line = before + &after;
        }
        let next: Vec<char> = lines.join("\n").chars().collect();
        if next != self.text {
            self.checkpoint();
            self.text = next;
            self.changed();
        }
        self.finish_block(bounds.top, bounds.left);
    }
    fn block_case(&mut self, op: char) {
        let Some(bounds) = self.block_bounds() else {
            return;
        };
        let mut lines: Vec<_> = self.contents().split('\n').map(str::to_owned).collect();
        for line in &mut lines[bounds.top..=bounds.bottom] {
            let end = width_at(line, self.indent_style.tabstop, 0).min(bounds.right);
            let (before, selected, after) = split_cells(
                line,
                self.indent_style.tabstop,
                bounds.left,
                end.max(bounds.left),
                false,
            );
            let selected: String = selected.chars().map(|ch| visual::convert(ch, op)).collect();
            *line = before + &selected + &after;
        }
        let next: Vec<char> = lines.join("\n").chars().collect();
        if next != self.text {
            self.checkpoint();
            self.text = next;
            self.changed();
        }
        self.finish_block(bounds.top, bounds.left);
    }
    pub(super) fn block_replace(&mut self, ch: char) {
        let Some(bounds) = self.block_bounds() else {
            return;
        };
        let cells = (bounds.bottom - bounds.top + 1).saturating_mul(bounds.right);
        if cells > 2_000_000 || self.text.len().saturating_add(cells) > 2_000_000 {
            self.message = "Reemplazo rectangular fuera de límites".into();
            return;
        }
        let mut lines: Vec<_> = self.contents().split('\n').map(str::to_owned).collect();
        let width = ch.width().unwrap_or(1).max(1);
        let value = ch.to_string().repeat((bounds.right - bounds.left) / width)
            + &" ".repeat((bounds.right - bounds.left) % width);
        for line in &mut lines[bounds.top..=bounds.bottom] {
            let (before, _, after) = split_cells(
                line,
                self.indent_style.tabstop,
                bounds.left,
                bounds.right,
                true,
            );
            *line = before + &value + &after;
        }
        let next: Vec<char> = lines.join("\n").chars().collect();
        if next != self.text {
            self.checkpoint();
            self.text = next;
            self.changed();
        }
        self.finish_block(bounds.top, bounds.left);
    }
}
impl Editor {
    fn begin_block_insert(&mut self, action: char, tail: bool) {
        let Some(mut bounds) = self.block_bounds() else {
            return;
        };
        if tail {
            bounds = self.block_tail_bounds(bounds);
        }
        if (bounds.bottom - bounds.top + 1).saturating_mul(bounds.right) > 2_000_000 {
            self.message = "Inserción rectangular excede el límite de edición".into();
            return;
        }
        let eol = self.block.is_some_and(|block| block.eol);
        let mut lines: Vec<_> = self.contents().split('\n').map(str::to_owned).collect();
        let rows: Vec<_> = (bounds.top..=bounds.bottom)
            .filter_map(|row| {
                let width = width_at(&lines[row], self.indent_style.tabstop, 0);
                if action == 'I' && width <= bounds.left {
                    return None;
                }
                let column = if action == 'A' {
                    if eol { width } else { bounds.right }
                } else {
                    bounds.left
                };
                Some((row, column))
            })
            .collect();
        let Some(&(first, column)) = rows.first() else {
            self.finish_block(bounds.top, bounds.left);
            return;
        };
        if action == 'c' && !self.block_capture(bounds, false, 'c') {
            return;
        }
        if action == 'c' {
            for line in &mut lines[bounds.top..=bounds.bottom] {
                let right = bounds.right;
                let (before, _, after) =
                    split_cells(line, self.indent_style.tabstop, bounds.left, right, true);
                *line = before + &after;
            }
        }
        let (prefix, _, suffix) = split_cells(
            &lines[first],
            self.indent_style.tabstop,
            column,
            column,
            true,
        );
        lines[first] = prefix.clone() + &suffix;
        let next = lines.join("\n");
        if next.chars().count() > 2_000_000 || next.len() > 8 * 1024 * 1024 {
            self.message = "Inserción rectangular excede el límite de edición".into();
            return;
        }
        self.checkpoint();
        self.mode = Mode::Insert;
        self.block = None;
        self.text = next.chars().collect();
        self.changed();
        self.cursor = self.line_number(first + 1) + prefix.chars().count();
        self.block_insert = Some(Insertion {
            rows,
            column,
            first,
            prefix,
            suffix,
            lines: self.line_starts.len(),
            return_column: bounds.left,
        });
        self.count = 0;
        self.pending.clear();
    }
    fn finish_block_insert(&mut self) {
        let Some(insert) = self.block_insert.take() else {
            return;
        };
        self.clear_automatic_indent();
        let mut lines: Vec<_> = self.contents().split('\n').map(str::to_owned).collect();
        let inserted = (lines.len() == insert.lines)
            .then(|| lines.get(insert.first))
            .flatten()
            .and_then(|line| line.strip_prefix(&insert.prefix))
            .and_then(|line| line.strip_suffix(&insert.suffix))
            .map(str::to_owned);
        if let Some(text) = inserted {
            let estimated = text.len().saturating_mul(insert.rows.len());
            if estimated <= 8 * 1024 * 1024 {
                for &(row, column) in insert.rows.iter().skip(1) {
                    let (before, _, after) =
                        split_cells(&lines[row], self.indent_style.tabstop, column, column, true);
                    lines[row] = before + &text + &after;
                }
                let next = lines.join("\n");
                if next.chars().count() <= 2_000_000 && next.len() <= 8 * 1024 * 1024 {
                    self.text = next.chars().collect();
                    self.changed();
                } else {
                    self.message = "Réplica rectangular fuera de límites; primera edición conservada, u deshace el conjunto".into();
                }
            } else {
                self.message =
                    "Réplica rectangular excede 8 MiB; primera edición conservada".into();
            }
        } else {
            self.message = "El salto de línea o edición fuera del punto inicial canceló la réplica; primera edición conservada".into();
        }
        self.finish_block(insert.first, insert.return_column.min(insert.column));
    }
}
impl Editor {
    pub(super) fn put_over_block(
        &mut self,
        value: &str,
        kind: registers::Kind,
        count: usize,
        replace_register: bool,
    ) -> Effect {
        let Some(bounds) = self.block_bounds() else {
            return Effect::None;
        };
        let count = count.clamp(1, 9999);
        let height = bounds.bottom - bounds.top + 1;
        let source_height = value.split('\n').count();
        let repeat_rows = if kind == registers::Kind::Character && !value.contains('\n') {
            height
        } else {
            1
        };
        if source_height > 4096
            || value
                .len()
                .saturating_mul(count)
                .saturating_mul(repeat_rows)
                > 8 * 1024 * 1024
            || bounds.left.saturating_mul(height.max(source_height)) > 2_000_000
        {
            self.message = "Reemplazo rectangular fuera de límites".into();
            return Effect::None;
        }
        let mut lines: Vec<_> = self.contents().split('\n').map(str::to_owned).collect();
        for line in &mut lines[bounds.top..=bounds.bottom] {
            let (before, _, after) = split_cells(
                line,
                self.indent_style.tabstop,
                bounds.left,
                bounds.right,
                false,
            );
            *line = before + &after;
        }
        let mut cursor_row = bounds.top;
        let mut cursor_column = bounds.left;
        match kind {
            registers::Kind::Block(width) => {
                if width.saturating_mul(count).saturating_mul(source_height) > 2_000_000 {
                    self.message = "Reemplazo rectangular fuera de límites".into();
                    return Effect::None;
                }
                lines.resize(lines.len().max(bounds.top + source_height), String::new());
                for (index, row) in value.split('\n').enumerate() {
                    let (before, _, after) = split_cells(
                        &lines[bounds.top + index],
                        self.indent_style.tabstop,
                        bounds.left,
                        bounds.left,
                        true,
                    );
                    let mut text = String::new();
                    let mut column = bounds.left;
                    for repetition in 0..count {
                        text.push_str(row);
                        let used = width_at(row, self.indent_style.tabstop, column);
                        column += used;
                        if repetition + 1 < count {
                            let padding = width.saturating_sub(used);
                            text.push_str(&" ".repeat(padding));
                            column += padding;
                        }
                    }
                    lines[bounds.top + index] = before + &text + &after;
                }
            }
            registers::Kind::Character if !value.contains('\n') => {
                let text = value.repeat(count);
                for line in &mut lines[bounds.top..=bounds.bottom] {
                    let (before, _, after) = split_cells(
                        line,
                        self.indent_style.tabstop,
                        bounds.left,
                        bounds.left,
                        true,
                    );
                    *line = before + &text + &after;
                }
            }
            registers::Kind::Character => {
                let (before, _, after) = split_cells(
                    &lines[bounds.top],
                    self.indent_style.tabstop,
                    bounds.left,
                    bounds.left,
                    true,
                );
                let text = before + &value.repeat(count) + &after;
                lines.splice(bounds.top..=bounds.top, text.split('\n').map(str::to_owned));
            }
            registers::Kind::Line => {
                let value = if value.ends_with('\n') {
                    value.to_owned()
                } else {
                    format!("{value}\n")
                };
                let text = value.repeat(count);
                let index = (bounds.bottom + 1).min(lines.len());
                lines.splice(
                    index..index,
                    text.strip_suffix('\n')
                        .unwrap_or(&text)
                        .split('\n')
                        .map(str::to_owned),
                );
                cursor_row = index;
                cursor_column = 0;
            }
        }
        let text = lines.join("\n");
        if text.len() > 8 * 1024 * 1024 || text.chars().count() > 2_000_000 {
            self.message = "Reemplazo rectangular excede el límite de edición".into();
            return Effect::None;
        }
        if replace_register && !self.block_capture(bounds, false, 'd') {
            return Effect::None;
        }
        self.checkpoint();
        self.text = text.chars().collect();
        self.changed();
        self.finish_block(cursor_row, cursor_column);
        Effect::None
    }
}
impl Editor {
    pub(super) fn restore_block_cursor(&mut self, anchor_row: usize, cursor_row: usize) {
        let Some(mut block) = self.block else {
            return;
        };
        if self.mode != Mode::VisualBlock {
            return;
        }
        block.anchor_row = anchor_row.min(self.line_starts.len() - 1);
        block.row = cursor_row.min(self.line_starts.len() - 1);
        self.anchor = self.cell_offset(block.anchor_row, block.anchor_column);
        self.cursor = self.cell_offset(block.row, block.column);
        block.expected_cursor = self.cursor;
        self.block = Some(block);
    }
}
impl Editor {
    fn block_tail_bounds(&self, mut bounds: Bounds) -> Bounds {
        let end = (bounds.top..=bounds.bottom)
            .map(|row| {
                let start = self.line_starts[row];
                width_at(
                    &self.text[start..self.line_end(start)]
                        .iter()
                        .collect::<String>(),
                    self.indent_style.tabstop,
                    0,
                )
            })
            .max()
            .unwrap_or(bounds.left);
        bounds.right = end.saturating_add(1).max(bounds.left + 1);
        bounds.width = end.saturating_sub(bounds.left).max(1);
        bounds
    }
    fn block_shift(&mut self, right: bool, count: usize) {
        let Some(bounds) = self.block_bounds() else {
            return;
        };
        let step = self.indent_style.width.saturating_mul(count);
        if step
            .saturating_mul(bounds.bottom - bounds.top + 1)
            .saturating_add(self.text.len())
            > 2_000_000
        {
            self.message = "Desplazamiento rectangular fuera de límites".into();
            return;
        }
        let mut lines: Vec<_> = self.contents().split('\n').map(str::to_owned).collect();
        for line in &mut lines[bounds.top..=bounds.bottom] {
            if width_at(line, self.indent_style.tabstop, 0) <= bounds.left {
                continue;
            }
            if right {
                let (before, _, after) = split_cells(
                    line,
                    self.indent_style.tabstop,
                    bounds.left,
                    bounds.left,
                    false,
                );
                let mut padding = String::new();
                let mut column = bounds.left;
                while column < bounds.left + step {
                    let tab = self.tab_padding(column);
                    if !self.indent_style.expand && tab <= bounds.left + step - column {
                        padding.push('\t');
                        column += tab;
                    } else {
                        padding.push(' ');
                        column += 1;
                    }
                }
                *line = before + &padding + &after;
            } else {
                let (_, _, after) = split_cells(
                    line,
                    self.indent_style.tabstop,
                    bounds.left,
                    bounds.left,
                    false,
                );
                let whitespace: String = after
                    .chars()
                    .take_while(|ch| matches!(ch, ' ' | '\t'))
                    .collect();
                let remove =
                    width_at(&whitespace, self.indent_style.tabstop, bounds.left).min(step);
                let (before, _, after) = split_cells(
                    line,
                    self.indent_style.tabstop,
                    bounds.left,
                    bounds.left + remove,
                    false,
                );
                *line = before + &after;
            }
        }
        let next: Vec<char> = lines.join("\n").chars().collect();
        if next != self.text {
            self.checkpoint();
            self.text = next;
            self.changed();
        }
        self.finish_block(bounds.top, bounds.left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reference_keys(e: &mut Editor, sequence: &str) {
        let mut keys = sequence.chars().peekable();
        while let Some(ch) = keys.next() {
            if ch == '<' {
                let mut key = String::from("<");
                for next in keys.by_ref() {
                    key.push(next);
                    if next == '>' {
                        break;
                    }
                }
                e.key(&key);
            } else if e.mode == Mode::Insert {
                e.insert(&ch.to_string());
            } else {
                e.key(&ch.to_string());
            }
        }
    }
    #[test]
    fn virtual_cursor_has_exactly_one_row_owner_even_when_offset_matches_next_line() {
        let mut e = Editor::new("abcde\nA\nxyz".into());
        reference_keys(&mut e, "ll<C-v>j");
        let second = e.line_number(2);
        let third = e.line_number(3);
        assert_eq!(e.display_position(), (1, 2));
        assert_eq!(e.display_cursor(), third);
        assert_eq!(e.display_cursor_in_line(second), Some(third));
        assert_eq!(e.display_cursor_in_line(third), None);
        assert_eq!(e.display_cursor_in_line(0), None);
        e.key("<Esc>");
        assert!(e.display_cursor_in_line(second).is_some());
        assert_eq!(e.display_cursor_in_line(third), None);
    }
    #[test]
    fn wrapped_block_cursor_keeps_virtual_column_on_the_original_line() {
        let mut editor = Editor::new("abcde\nA\nxyz".into());
        reference_keys(&mut editor, "ll<C-v>j");
        let cursor = editor.wrapped_block_cursor(8).unwrap().unwrap();
        assert_eq!(cursor.cursor.line, 1);
        assert_eq!(cursor.cursor.scalar, 1);
        assert_eq!(cursor.cursor.visual_row, 0);
        assert_eq!(cursor.column, 2);
        assert_eq!(editor.contents(), "abcde\nA\nxyz");
        editor.key("<Esc>");
        assert!(editor.wrapped_block_cursor(8).unwrap().is_none());
    }

    #[test]
    fn block_wheel_commit_preserves_anchor_and_virtual_padding() {
        let mut editor = Editor::new("abcde\nA\nxyz".into());
        reference_keys(&mut editor, "ll<C-v>j");
        let original_anchor = editor.block.unwrap().anchor_column;
        let version = editor.version;
        let end = editor.line_number(2) + 1;
        assert!(editor.commit_wrapped_block_wheel(end, 4, 8, 5));
        assert_eq!(editor.display_position(), (1, 5));
        assert_eq!(editor.block.unwrap().anchor_column, original_anchor);
        assert_eq!(editor.wrapped_block_cursor(8).unwrap().unwrap().column, 5);
        assert_eq!(editor.contents(), "abcde\nA\nxyz");
        assert_eq!(editor.version, version);
        editor.key("<Esc>");
        assert!(!editor.commit_wrapped_block_wheel(end, 1, 8, 2));

        let mut eol = Editor::new("abcde\nA\nxyz".into());
        reference_keys(&mut eol, "l<C-v>j$");
        assert!(eol.block_wants_eol());
        let target = eol.line_number(3) + 3;
        assert!(eol.commit_wrapped_block_wheel(target, 0, 8, i32::MAX as usize - 1));
        assert!(eol.block_wants_eol());
    }

    #[test]
    fn wrapped_block_selection_matches_neovim_screen_attributes() {
        let mut cases = Vec::new();
        for text in [
            include_str!("../../../Resources/Fixtures/nvim-wrap-block-paint.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-block-eol-paint.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-block-eol-wheel-paint.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-block-eol-wheel-intermediate.json"),
            include_str!("../../../Resources/Fixtures/nvim-wrap-block-wheel-paint.json"),
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
            reference_keys(&mut editor, case["keys"].as_str().unwrap());
            assert_eq!(editor.mode, Mode::VisualBlock, "case {case_index}");
            let width = case["width"].as_u64().unwrap() as usize;
            let rows = case["rows"].as_u64().unwrap() as usize;
            if let Some(wheels) = case.get("wheels").and_then(|v| v.as_array()) {
                let before = case["before_cursor"].as_array().unwrap();
                let before_view = &case["before_view"];
                let mut top = super::super::WrappedTop {
                    source_line: before_view["topline"].as_u64().unwrap() as usize - 1,
                    visual_row: before_view["skipcol"].as_u64().unwrap() as usize / width,
                };
                let mut cursor = editor.wrapped_block_cursor(width).unwrap().unwrap().cursor;
                let mut coladd = before[3].as_u64().unwrap() as usize;
                let mut goal = before[4].as_u64().unwrap() as usize - 1;
                let anchor = editor.block_anchor_for_test();
                for (step_index, direction) in wheels.iter().enumerate() {
                    let request = super::super::WrappedWheelRequest {
                        top,
                        cursor,
                        goal: Some(goal),
                        rows,
                        columns: width,
                        delta: if direction == "down" { 3 } else { -3 },
                        mode: Mode::VisualBlock,
                        coladd,
                    };
                    (top, cursor, goal, coladd) = editor.calculate_wrapped_wheel(request).unwrap();
                    let offset = editor.line_starts[cursor.line] + cursor.scalar;
                    assert!(editor.commit_wrapped_block_wheel(offset, coladd, width, goal));
                    assert_eq!(editor.block_anchor_for_test(), anchor);
                    if let Some(expected) = case
                        .get("intermediate")
                        .and_then(|v| v.as_array())
                        .and_then(|v| v.get(step_index))
                    {
                        let expected_cursor = expected["cursor"].as_array().unwrap();
                        let byte = editor.text[editor.line_starts[cursor.line]..offset]
                            .iter()
                            .map(|ch| ch.len_utf8())
                            .sum::<usize>()
                            + 1;
                        assert_eq!(
                            (cursor.line + 1, byte, coladd, goal + 1),
                            (
                                expected_cursor[1].as_u64().unwrap() as usize,
                                expected_cursor[2].as_u64().unwrap() as usize,
                                expected_cursor[3].as_u64().unwrap() as usize,
                                expected_cursor[4].as_u64().unwrap() as usize
                            ),
                            "intermediate case {case_index} step {step_index}"
                        );
                        assert_eq!(
                            (top.source_line + 1, top.visual_row * width),
                            (
                                expected["view"]["topline"].as_u64().unwrap() as usize,
                                expected["view"]["skipcol"].as_u64().unwrap() as usize
                            ),
                            "intermediate top case {case_index} step {step_index}"
                        );
                    }
                }
                assert_eq!(
                    (top.source_line + 1, top.visual_row * width),
                    (
                        case["view"]["topline"].as_u64().unwrap() as usize,
                        case["view"]["skipcol"].as_u64().unwrap() as usize
                    ),
                    "top case {case_index}"
                );
            }
            let expected_cursor = case["cursor"].as_array().unwrap();
            let line = editor.source_line_at(editor.cursor);
            let line_start = editor.line_starts[line];
            let byte = editor.text[line_start..editor.cursor]
                .iter()
                .map(|ch| ch.len_utf8())
                .sum::<usize>()
                + 1;
            let coladd = editor
                .block
                .unwrap()
                .column
                .saturating_sub(editor.cell_column(editor.cursor));
            assert_eq!(
                (line + 1, byte, coladd),
                (
                    expected_cursor[1].as_u64().unwrap() as usize,
                    expected_cursor[2].as_u64().unwrap() as usize,
                    expected_cursor[3].as_u64().unwrap() as usize
                ),
                "cursor case {case_index}"
            );
            let virtual_cursor = editor.wrapped_block_cursor(width).unwrap().unwrap();
            let view = &case["view"];
            let top = super::super::WrappedTop {
                source_line: view["topline"].as_u64().unwrap() as usize - 1,
                visual_row: view["skipcol"].as_u64().unwrap() as usize / width,
            };
            let viewport = editor
                .wrapped_viewport_for_cursor(top, rows, width, virtual_cursor.cursor)
                .unwrap();
            let bounds = editor.wrapped_block_bounds(width).unwrap().unwrap();
            let selections =
                editor.wrapped_block_selection(&viewport, bounds.lines, bounds.columns, width);
            let mut actual = vec![vec![0u64; width]; rows];
            for selection in selections {
                for col in selection.columns {
                    actual[selection.row][col] = 1;
                }
            }
            // Neovim draws its cursor separately from screenattr highlights.
            if let Some(cursor) =
                editor.wrapped_virtual_cursor_cell(&viewport, virtual_cursor, width)
            {
                for col in cursor.columns {
                    actual[cursor.row][col] = 0;
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
                        .map(|v| v.as_u64().unwrap())
                        .collect()
                })
                .collect();
            assert_eq!(actual, expected, "selection case {case_index}");
        }
    }

    #[test]
    fn rectangular_clipboard_metadata_survives_buffers_and_one_undo_restores_put() {
        let registers = RegisterBank::default();
        let mut source =
            Editor::with_registers("abcde\nABCDE".into(), registers.clone()).with_clipboard();
        reference_keys(&mut source, "l<C-v>jly");
        let clipboard = source.take_clipboard_write_typed().unwrap();
        assert_eq!(clipboard, ("bc\nBC".into(), false, Some(2)));
        let mut target = Editor::with_registers("xyz\nXYZ".into(), registers).with_clipboard();
        assert_eq!(target.key("p"), Effect::ClipboardRead);
        target.provide_clipboard_typed(Ok(clipboard));
        assert_eq!(target.contents(), "xbcyz\nXBCYZ");
        target.key("u");
        assert_eq!(target.contents(), "xyz\nXYZ");
        target.key("<C-r>");
        assert_eq!(target.contents(), "xbcyz\nXBCYZ");
    }
    #[test]
    fn rectangular_gv_dot_and_viewport_motions_keep_shape_and_virtual_columns() {
        let mut e = Editor::new("abcde\nABCDE\nxyz".into());
        reference_keys(&mut e, "l<C-v>jldgv");
        assert_eq!(e.mode, Mode::VisualBlock);
        assert_eq!(e.block_row_columns(0), Some((1, 3)));
        e.key("<Esc>");
        e.key("u");
        assert_eq!(e.contents(), "abcde\nABCDE\nxyz");
        let mut e = Editor::new("abcde\nABCDE\nxyz".into());
        reference_keys(&mut e, "l<C-v>jldj.");
        assert_eq!(e.contents(), "ade\nA\nx");
        let mut e = Editor::new(
            (0..20)
                .map(|i| if i == 0 { "abcde" } else { "x" })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        e.jump_viewport(0, 0, 10, 80);
        reference_keys(&mut e, "ll<C-v>");
        e.key("<C-d>");
        assert_eq!(e.display_position(), (5, 2));
        reference_keys(&mut e, "<Esc>gv");
        assert_eq!(e.display_position(), (5, 2));
        e.key("<C-u>");
        assert_eq!(e.display_position(), (0, 2));
    }
    #[test]
    fn rectangular_aliases_tail_shift_and_wide_replace_match_neovim() {
        for (name, before, keys, after, register, kind, cursor) in [
            (
                "X",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlX",
                "ade\nADE\nxyz",
                "bc\nBC",
                registers::Kind::Block(2),
                (0, 1),
            ),
            (
                "S",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlS_<Esc>",
                "_\nxyz",
                "abcde\nABCDE\n",
                registers::Kind::Line,
                (0, 0),
            ),
            (
                "R",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlR_<Esc>",
                "_\nxyz",
                "abcde\nABCDE\n",
                registers::Kind::Line,
                (0, 0),
            ),
            (
                "D",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlD",
                "a\nA\nxyz",
                "bcde \nBCDE ",
                registers::Kind::Block(4),
                (0, 1),
            ),
            (
                "C",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlC_<Esc>",
                "a_\nA_\nxyz",
                "bcde \nBCDE ",
                registers::Kind::Block(4),
                (0, 1),
            ),
            (
                "right",
                "abcde\nABCDE\nxyz",
                "l<C-v>jl>",
                "a  bcde\nA  BCDE\nxyz",
                "",
                registers::Kind::Character,
                (0, 1),
            ),
            (
                "right-short",
                "abcde\nA\nxyz",
                "ll<C-v>j>",
                "ab  cde\nA\nxyz",
                "",
                registers::Kind::Character,
                (0, 2),
            ),
            (
                "left",
                "a  bcde\nA  BCDE\nxyz",
                "3l<C-v>jl<",
                "a  bcde\nA  BCDE\nxyz",
                "",
                registers::Kind::Character,
                (0, 3),
            ),
            (
                "left-at-space",
                "a  bcde\nA  BCDE\nxyz",
                "l<C-v>jl<",
                "abcde\nABCDE\nxyz",
                "",
                registers::Kind::Character,
                (0, 1),
            ),
            (
                "wide-replace",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlré",
                "aééde\nAééDE\nxyz",
                "",
                registers::Kind::Character,
                (0, 1),
            ),
            (
                "double-replace",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlré",
                "aééde\nAééDE\nxyz",
                "",
                registers::Kind::Character,
                (0, 1),
            ),
            (
                "wide-replace3",
                "abcde\nABCDE\nxyz",
                "l<C-v>jllr界",
                "a界 e\nA界 E\nxyz",
                "",
                registers::Kind::Character,
                (0, 1),
            ),
        ] {
            let mut e = Editor::new(before.into());
            reference_keys(&mut e, keys);
            assert_eq!(e.contents(), after, "{name}: text");
            assert_eq!(e.position(), cursor, "{name}: cursor");
            if register.is_empty() {
                assert!(
                    e.registers.get_kind('"').is_none(),
                    "{name}: no register write"
                );
            } else {
                assert_eq!(
                    e.registers.get_kind('"'),
                    Some((register.into(), kind)),
                    "{name}: register"
                );
            }
            if before != after {
                e.key("u");
                assert_eq!(e.contents(), before, "{name}: one undo");
            }
        }
    }
    #[test]
    fn rectangular_replacement_and_uppercase_register_append_match_neovim() {
        for (name, value, kind, keys, after, named, named_kind, unnamed, unnamed_kind, cursor) in [
            (
                "block2",
                "Q\nR",
                registers::Kind::Block(1),
                "l<C-v>jl\"ap",
                "aQde\nARDE\nxyz",
                "Q\nR",
                registers::Kind::Block(1),
                "bc\nBC",
                registers::Kind::Block(2),
                (0, 1),
            ),
            (
                "char1",
                "Q",
                registers::Kind::Character,
                "l<C-v>jl\"ap",
                "aQde\nAQDE\nxyz",
                "Q",
                registers::Kind::Character,
                "bc\nBC",
                registers::Kind::Block(2),
                (0, 1),
            ),
            (
                "char2",
                "Q\nR",
                registers::Kind::Character,
                "l<C-v>jl\"ap",
                "aQ\nRde\nADE\nxyz",
                "Q\nR",
                registers::Kind::Character,
                "bc\nBC",
                registers::Kind::Block(2),
                (0, 1),
            ),
            (
                "line2",
                "Q\nR\n",
                registers::Kind::Line,
                "l<C-v>jl\"ap",
                "ade\nADE\nQ\nR\nxyz",
                "Q\nR\n",
                registers::Kind::Line,
                "bc\nBC",
                registers::Kind::Block(2),
                (2, 0),
            ),
            (
                "block1",
                "Q",
                registers::Kind::Block(1),
                "l<C-v>jl\"ap",
                "aQde\nADE\nxyz",
                "Q",
                registers::Kind::Block(1),
                "bc\nBC",
                registers::Kind::Block(2),
                (0, 1),
            ),
            (
                "block3",
                "Q\nR\nS",
                registers::Kind::Block(1),
                "l<C-v>jl\"ap",
                "aQde\nARDE\nxSyz",
                "Q\nR\nS",
                registers::Kind::Block(1),
                "bc\nBC",
                registers::Kind::Block(2),
                (0, 1),
            ),
            (
                "append2",
                "12\n34",
                registers::Kind::Block(2),
                "l<C-v>jl\"Ay",
                "abcde\nABCDE\nxyz",
                "12\n34\nbc\nBC",
                registers::Kind::Block(2),
                "12\n34\nbc\nBC",
                registers::Kind::Block(2),
                (0, 1),
            ),
            (
                "append3",
                "12\n34\n56",
                registers::Kind::Block(2),
                "l<C-v>jl\"Ay",
                "abcde\nABCDE\nxyz",
                "12\n34\n56\nbc\nBC",
                registers::Kind::Block(2),
                "12\n34\n56\nbc\nBC",
                registers::Kind::Block(2),
                (0, 1),
            ),
            (
                "append-char",
                "Z",
                registers::Kind::Character,
                "l<C-v>jl\"Ay",
                "abcde\nABCDE\nxyz",
                "Zbc\nBC",
                registers::Kind::Character,
                "Zbc\nBC",
                registers::Kind::Character,
                (0, 1),
            ),
        ] {
            let mut e = Editor::new("abcde\nABCDE\nxyz".into());
            e.registers
                .capture_kind('a', 'y', value.into(), kind)
                .unwrap();
            reference_keys(&mut e, keys);
            assert_eq!(e.contents(), after, "{name}: text");
            assert_eq!(e.position(), cursor, "{name}: cursor");
            assert_eq!(
                e.registers.get_kind('a'),
                Some((named.into(), named_kind)),
                "{name}: named"
            );
            assert_eq!(
                e.registers.get_kind('"'),
                Some((unnamed.into(), unnamed_kind)),
                "{name}: unnamed"
            );
            if after != "abcde\nABCDE\nxyz" {
                e.key("u");
                assert_eq!(e.contents(), "abcde\nABCDE\nxyz", "{name}: one undo");
            }
        }
    }
    #[test]
    fn rectangles_match_clean_neovim_reference_for_register_text_type_cursor_and_edits() {
        for (name, before, keys, after, register, width, cursor) in [
            (
                "yank",
                "abcde\nABCDE\nxyz",
                "l<C-v>jly",
                "abcde\nABCDE\nxyz",
                "bc\nBC",
                2,
                (0, 1),
            ),
            (
                "delete",
                "abcde\nABCDE\nxyz",
                "l<C-v>jld",
                "ade\nADE\nxyz",
                "bc\nBC",
                2,
                (0, 1),
            ),
            (
                "case",
                "abcde\nABCDE\nxyz",
                "l<C-v>jl~",
                "aBCde\nAbcDE\nxyz",
                "",
                0,
                (0, 1),
            ),
            (
                "replace",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlrx",
                "axxde\nAxxDE\nxyz",
                "",
                0,
                (0, 1),
            ),
            (
                "insert",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlI_<Esc>",
                "a_bcde\nA_BCDE\nxyz",
                "",
                0,
                (0, 1),
            ),
            (
                "append",
                "abcde\nA\nxyz",
                "ll<C-v>jA_<Esc>",
                "abc_de\nA  _\nxyz",
                "",
                0,
                (0, 2),
            ),
            (
                "change",
                "abcde\nA\nxyz",
                "ll<C-v>jcQ<Esc>",
                "abQde\nA Q\nxyz",
                "c\n ",
                1,
                (0, 2),
            ),
            (
                "put",
                "abcde\nABCDE\nxyz",
                "l<C-v>jlyj0p",
                "abcde\nAbcBCDE\nxBCyz",
                "bc\nBC",
                2,
                (1, 1),
            ),
            (
                "tab-yank",
                "a\tbc\nABCD\nxy",
                "l<C-v>jly",
                "a\tbc\nABCD\nxy",
                "\tb\nBC",
                2,
                (0, 1),
            ),
            (
                "wide-yank",
                "a界bc\nABCDE\nxy",
                "l<C-v>jly",
                "a界bc\nABCDE\nxy",
                "界\nBC",
                2,
                (0, 1),
            ),
            (
                "wide-delete",
                "a界bc\nABCDE\nxy",
                "l<C-v>jld",
                "abc\nADE\nxy",
                "界\nBC",
                2,
                (0, 1),
            ),
            (
                "dollar-append",
                "abcde\nA\nxy",
                "ll<C-v>j$A_<Esc>",
                "abcde_\nA_\nxy",
                "",
                0,
                (0, 1),
            ),
            (
                "eol-yank",
                "abcde\nA\nxy",
                "ll<C-v>j$y",
                "abcde\nA\nxy",
                "bcde \n     ",
                4,
                (0, 1),
            ),
            (
                "eol-delete",
                "abcde\nA\nxy",
                "ll<C-v>j$d",
                "a\nA\nxy",
                "bcde \n     ",
                4,
                (0, 1),
            ),
        ] {
            let mut e = Editor::new(before.into());
            reference_keys(&mut e, keys);
            assert_eq!(e.contents(), after, "{name}: text");
            assert_eq!(e.position(), cursor, "{name}: cursor");
            let actual = e.registers.get_kind('"');
            assert_eq!(
                actual.as_ref().map_or("", |v| v.0.as_str()),
                register,
                "{name}: register"
            );
            if width > 0 {
                assert_eq!(
                    actual.unwrap().1,
                    registers::Kind::Block(width),
                    "{name}: kind"
                );
            }
            if before != after {
                e.key("u");
                assert_eq!(e.contents(), before, "{name}: one undo");
            }
        }
    }
    #[test]
    fn typed_clipboard_round_trip_pastes_rectangular_rows_instead_of_newline_text() {
        let mut e = Editor::new("a\nx".into()).with_clipboard();
        assert_eq!(e.key("p"), Effect::ClipboardRead);
        e.provide_clipboard_typed(Ok(("AB\nCD".into(), false, Some(2))));
        assert_eq!(e.contents(), "aAB\nxCD");
        assert_eq!(
            e.registers.get_kind('+').unwrap().1,
            registers::Kind::Block(2)
        );
        e.clipboard_write = Some(("AB\nCD".into(), false, Some(2)));
        assert_eq!(
            e.take_clipboard_write_typed(),
            Some(("AB\nCD".into(), false, Some(2)))
        );
        e.key("u");
        assert_eq!(e.contents(), "a\nx");
    }
    #[test]
    fn block_put_retains_shape_between_buffers_and_pads_short_lines() {
        let bank = RegisterBank::default();
        bank.capture_kind('a', 'y', "bc\nBC".into(), registers::Kind::Block(2))
            .unwrap();
        let mut e = Editor::with_registers("ABCDE\nxyz".into(), bank);
        for key in ["\"", "a", "p"] {
            e.key(key);
        }
        assert_eq!(e.contents(), "AbcBCDE\nxBCyz");
        assert_eq!(e.position(), (0, 1));
        e.key("u");
        assert_eq!(e.contents(), "ABCDE\nxyz");
        let mut e = Editor::new("a\nx\n".into());
        e.registers
            .import_kind("AB\nCD\nEF".into(), registers::Kind::Block(2))
            .unwrap();
        e.chosen_register = Some('+');
        e.key("p");
        assert_eq!(e.contents(), "aAB\nxCD\n EF");
    }
    #[test]
    fn display_cell_split_preserves_unicode_and_splits_only_boundary_tabs() {
        assert_eq!(
            split_cells("a\tbc", 4, 2, 3, false),
            ("a ".into(), " ".into(), " bc".into())
        );
        assert_eq!(
            split_cells("a界e\u{301}z", 2, 1, 3, false),
            ("a".into(), "界".into(), "e\u{301}z".into())
        );
        assert_eq!(
            split_cells("a界b", 2, 2, 3, false),
            ("a ".into(), " ".into(), "b".into())
        );
    }
    #[test]
    fn invalid_block_clipboard_and_put_budget_leave_buffer_and_registers_unchanged() {
        let mut e = Editor::new("keep".into()).with_clipboard();
        assert_eq!(e.key("p"), Effect::ClipboardRead);
        e.provide_clipboard_typed(Ok(("X".into(), false, Some(0))));
        assert_eq!(e.contents(), "keep");
        assert!(e.registers.get_kind('+').is_none());
        e.registers
            .capture_kind('a', 'y', "X\nY".into(), registers::Kind::Block(2_000_000))
            .unwrap();
        for key in ["\"", "a", "p"] {
            e.key(key);
        }
        assert_eq!(e.contents(), "keep");
        assert_eq!(
            e.registers.get_kind('a').unwrap().1,
            registers::Kind::Block(2_000_000)
        );
    }
}
