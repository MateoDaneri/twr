//! Unwrapped vertical window motions, using the measured native viewport.
use super::*;

/// Follow a cursor with sidescrolloff=8 and Neovim's half-window jump threshold.
/// Coordinates use the same display columns as the native editor renderer.
pub fn followed_column(left: usize, column: usize, columns: usize) -> usize {
    let columns = columns.max(1);
    let margin = 8.min(columns.saturating_sub(1) / 2);
    let followed = left.clamp(
        column.saturating_sub(columns.saturating_sub(1 + margin)),
        column.saturating_sub(margin),
    );
    if left.abs_diff(followed) >= columns / 2 {
        column.saturating_sub(columns / 2)
    } else {
        followed
    }
}

#[cfg(test)]
mod buffer_view_tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Deserialize)]
    struct Reference {
        line: Option<String>,
        cases: Vec<Case>,
    }
    type Visit = (String, Option<usize>, Option<usize>, Option<String>);
    #[derive(Deserialize)]
    struct Case {
        label: String,
        visits: Vec<Visit>,
        observations: Vec<Observation>,
    }
    #[derive(Deserialize)]
    struct Observation {
        buffer: String,
        row: usize,
        column: usize,
        top: usize,
        left: usize,
        entry_top: usize,
        entry_left: usize,
        entry_row: usize,
        entry_column: usize,
        height: usize,
        width: usize,
        virtual_column: Option<usize>,
    }

    #[test]
    fn buffer_entry_and_scroll_match_settled_neovim_ui_reference() {
        let mut visits = 0;
        for fixture in [
            include_str!("../../../Resources/Fixtures/nvim-buffer-views.json"),
            include_str!("../../../Resources/Fixtures/nvim-buffer-columns.json"),
        ] {
            let reference: Reference = serde_json::from_str(fixture).unwrap();
            let text = vec![reference.line.unwrap_or_else(|| "a".repeat(160)); 200].join("\n");
            for case in reference.cases {
                let mut editors: BTreeMap<_, _> = ["a", "b"]
                    .into_iter()
                    .map(|name| (name.to_owned(), Editor::new(text.clone())))
                    .collect();
                let mut active = None;
                let (mut top, mut left) = (0, 0);
                assert_eq!(case.visits.len(), case.observations.len());
                for ((name, row, column, scroll), expected) in
                    case.visits.into_iter().zip(case.observations)
                {
                    let editor = editors.get_mut(&name).unwrap();
                    assert_eq!(expected.buffer, name);
                    assert_eq!(
                        editor.position(),
                        (expected.entry_row, expected.entry_column)
                    );
                    let entry = editor.activated_viewport(
                        top,
                        left,
                        expected.height,
                        expected.width,
                        active.as_ref() != Some(&name),
                    );
                    assert_eq!(
                        entry,
                        (expected.entry_top, expected.entry_left),
                        "{} {name}",
                        case.label
                    );
                    (top, left) = entry;
                    active = Some(name);
                    if let (Some(row), Some(column)) = (row, column) {
                        editor.goto(row - 1, column);
                    }
                    editor.jump_viewport(top, left, expected.height, expected.width);
                    if let Some(scroll) = scroll {
                        assert_eq!(editor.key("z"), Effect::None);
                        let Effect::Viewport(next_top) = editor.key(&scroll[1..]) else {
                            panic!("Expected viewport effect: {}", case.label);
                        };
                        top = next_top;
                    }
                    top = editor.followed_top(top, expected.height);
                    left = followed_column(left, editor.display_cell_column(), expected.width);
                    if let Some(column) = expected.virtual_column
                        && expected.column != 0
                    {
                        // Explicit destinations are ASCII cells after mixed-width prefixes.
                        // Nvim virtcol('.') at the leading wide glyph reports its last cell.
                        assert_eq!(editor.display_cell_column(), column);
                    }
                    assert_eq!(editor.position(), (expected.row, expected.column));
                    assert_eq!((top, left), (expected.top, expected.left), "{}", case.label);
                    assert_eq!(editor.contents(), text);
                    assert!(!editor.dirty());
                    visits += 1;
                }
            }
        }
        assert_eq!(visits, 44);
    }

    #[test]
    fn narrow_and_unknown_viewports_keep_cursor_visible_without_overflow() {
        let mut editor = Editor::new(vec!["abcdefgh"; 100].join("\n"));
        editor.goto(49, 5);
        assert_eq!(editor.activated_viewport(0, 0, 0, 0, true), (49, 5));
        assert_eq!(editor.activated_viewport(0, 0, 22, 80, true), (39, 0));
        assert_eq!(editor.activated_viewport(0, 0, 23, 80, true), (38, 0));
        assert_eq!(editor.activated_viewport(45, 17, 22, 80, false), (45, 17));
        assert_eq!(
            editor.activated_viewport(0, 0, usize::MAX, usize::MAX, true),
            (0, 0)
        );
        for columns in 0..=20 {
            for column in 0..=160 {
                for left in [0, 1, 40, 80, usize::MAX] {
                    let next = followed_column(left, column, columns);
                    assert!(next <= column);
                    assert!(column - next < columns.max(1));
                }
            }
        }
    }
}

impl Editor {
    pub fn set_window_scroll(&mut self, value: Option<usize>) {
        self.scroll_lines = value;
    }

    pub fn window_scroll(&self) -> Option<usize> {
        self.scroll_lines
    }
    /// Horizontal viewport coordinates must use the renderer's cells, not scalar offsets.
    /// Rectangular Visual already supplies virtual cells; other modes use their displayed caret.
    pub fn display_cell_column(&self) -> usize {
        if let Some((_, column)) = self.block_display_position() {
            return column;
        }
        let cursor = self.display_cursor().min(self.text.len());
        let start = self.line_start(cursor);
        self.display_text_column(&self.text[start..cursor])
    }

    /// Enter another buffer using its saved cursor, without inventing a saved viewport.
    /// Neovim recenters vertically on buffer changes and inherits horizontal scroll.
    pub fn activated_viewport(
        &self,
        top: usize,
        left: usize,
        rows: usize,
        columns: usize,
        changed_buffer: bool,
    ) -> (usize, usize) {
        if !changed_buffer {
            return (top, left);
        }
        let rows = rows.max(1);
        let row = self.display_position().0;
        let column = self.display_cell_column();
        let last = self.visible_row(self.line_starts.len().saturating_sub(1));
        let top = self
            .visible_row(row)
            .saturating_sub(rows.saturating_sub(1) / 2)
            .min(last.saturating_sub(rows.saturating_sub(1)));
        (self.source_row(top), followed_column(left, column, columns))
    }

    /// Enter a buffer at a cursor saved by this window, without moving the
    /// editor's shared cursor while the destination is being staged.
    pub fn activated_viewport_at(
        &self,
        row: usize,
        column: usize,
        left: usize,
        rows: usize,
        columns: usize,
    ) -> (usize, usize, usize, usize) {
        let (row, column) = self.pane_normal_position(row, column);
        let start = self.line_starts[row];
        let cells = self.display_text_column(&self.text[start..start + column]);
        let rows = rows.max(1);
        let last = self.visible_row(self.line_starts.len().saturating_sub(1));
        let top = self
            .visible_row(row)
            .saturating_sub(rows.saturating_sub(1) / 2)
            .min(last.saturating_sub(rows.saturating_sub(1)));
        (
            row,
            column,
            self.source_row(top),
            followed_column(left, cells, columns),
        )
    }

    pub fn followed_top(&self, top: usize, rows: usize) -> usize {
        let rows = rows.max(1);
        let margin = 4.min(rows.saturating_sub(1) / 2);
        let row = self.visible_row(self.display_position().0);
        let last = self.visible_row(self.line_starts.len().saturating_sub(1));
        let top = self.visible_row(top).min(last);
        let available = rows.min(last.saturating_sub(top) + 1);
        let above = if top == 0 {
            0
        } else {
            margin.min(available - 1)
        };
        let below = if top.saturating_add(rows) > last {
            0
        } else {
            margin
        };
        self.source_row(top.clamp(
            row.saturating_sub(rows.saturating_sub(1 + below)),
            row.saturating_sub(above),
        ))
    }

    /// Compute the viewport and optional cursor row without touching the shared editor.
    fn mouse_scroll_target(
        &self,
        top: usize,
        rows: usize,
        delta: isize,
        row: usize,
    ) -> (usize, Option<usize>) {
        let rows = rows.max(1);
        let last = self.visible_row(self.line_starts.len().saturating_sub(1));
        let old_top = self.visible_row(top);
        let next_top = old_top.saturating_add_signed(delta).min(last);
        let visible = self.visible_row(row);
        let mut above_wanted = 4;
        let mut below_wanted = 4;
        if next_top == 0 {
            above_wanted = 0;
            below_wanted = below_wanted.min(rows / 2);
        }
        if next_top.saturating_add(rows) > last {
            below_wanted = 0;
            above_wanted = above_wanted.min(rows.saturating_sub(1) / 2);
        }
        let mut low = next_top;
        let mut high = next_top.saturating_add(rows - 1).min(last);
        let (mut above, mut below) = (0, 0);
        while (above < above_wanted || below < below_wanted) && low < high {
            if below < below_wanted && (below <= above || above >= above_wanted) {
                high -= 1;
                below += 1;
            }
            if above < above_wanted && (above < below || below >= below_wanted) && low < high {
                low += 1;
                above += 1;
            }
        }
        let next_row = visible.clamp(low, high);
        (
            self.source_row(next_top),
            (next_row != visible).then(|| self.source_row(next_row)),
        )
    }

    /// Normalize an inactive window's saved cursor without changing the editor
    /// used by another window, including when a fold hides the saved line.
    pub fn pane_normal_position(&self, row: usize, column: usize) -> (usize, usize) {
        let row = self.source_row(self.visible_row(row));
        let start = self.line_starts[row];
        let end = self.line_end(start);
        let mut cursor = start.saturating_add(column).min(end);
        if cursor > start && cursor == end {
            cursor = self
                .grapheme_move(cursor, 1, false, true)
                .unwrap_or(cursor - 1);
        }
        cursor = self.grapheme_floor(cursor).unwrap_or(cursor);
        (row, cursor.saturating_sub(start))
    }

    /// Scroll an inactive normal-mode view without moving the shared cursor.
    pub fn mouse_scroll_inactive(
        &self,
        top: usize,
        rows: usize,
        delta: isize,
        row: usize,
        column: usize,
    ) -> (usize, usize, usize) {
        let (row, column) = self.pane_normal_position(row, column);
        let (top, moved) = self.mouse_scroll_target(top, rows, delta, row);
        let (row, column) =
            moved.map_or((row, column), |row| self.pane_normal_position(row, column));
        (top, row, column)
    }

    /// Scroll the viewport in visible lines while keeping the cursor on screen.
    pub fn mouse_scroll(&mut self, top: usize, rows: usize, delta: isize) -> usize {
        let (row, column) = self.position();
        let (top, moved) = self.mouse_scroll_target(top, rows, delta, row);
        if let Some(row) = moved {
            self.goto(row, column);
        }
        top
    }

    pub(super) fn scroll_command(
        &mut self,
        key: &str,
        count: usize,
        explicit: bool,
    ) -> Option<Effect> {
        if !matches!(
            self.mode,
            Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        ) {
            return None;
        }
        let (top, _, rows, _) = self.jump_view;
        let rows = rows.max(1);
        let margin = 4.min(rows.saturating_sub(1) / 2);
        let last = self.line_starts.len().saturating_sub(1);
        let (row, column) = self.position();
        if self.pending.starts_with('z') {
            let pending = std::mem::take(&mut self.pending);
            let count = pending[1..].parse::<usize>().unwrap_or(1).min(9999);
            if self.fold_command(key, count) {
                return Some(Effect::None);
            }
            if !matches!(key, "t" | "z" | "b" | "<Enter>" | "." | "-") {
                return Some(Effect::None);
            }
            let row = pending[1..]
                .parse::<usize>()
                .ok()
                .map(|n| n.saturating_sub(1).min(last))
                .unwrap_or(row);
            self.goto(row, column);
            if matches!(key, "<Enter>" | "." | "-") {
                self.cursor = self.line_start(self.cursor);
                while self.cursor < self.line_end(self.cursor)
                    && self.text[self.cursor].is_whitespace()
                {
                    self.cursor += 1;
                }
            }
            if self.wrap_input && matches!(key, "z" | ".") {
                return Some(Effect::WrappedCenter);
            }
            if self.wrap_input && matches!(key, "t" | "<Enter>") {
                return Some(Effect::WrappedTop);
            }
            if self.wrap_input && matches!(key, "b" | "-") {
                return Some(Effect::WrappedBottom);
            }
            let offset = match key {
                "t" | "<Enter>" => margin,
                "z" | "." => rows.saturating_sub(1) / 2,
                _ => rows.saturating_sub(1 + margin),
            };
            return Some(Effect::Viewport(
                self.source_row(self.visible_row(row).saturating_sub(offset)),
            ));
        }
        if !self.pending.is_empty() {
            return None;
        }
        if key == "z" {
            self.pending = if explicit {
                format!("z{count}")
            } else {
                "z".into()
            };
            return Some(Effect::None);
        }
        let top = self.visible_row(top);
        let row = self.visible_row(row);
        let last = self.visible_row(last);
        let down = matches!(key, "<C-d>" | "<C-f>" | "<PageDown>" | "<C-e>");
        let half = matches!(key, "<C-d>" | "<C-u>");
        let page = matches!(key, "<C-f>" | "<C-b>" | "<PageDown>" | "<PageUp>");
        let line = matches!(key, "<C-e>" | "<C-y>");
        if !half && !page && !line {
            return None;
        }
        if self.wrap_input && line {
            return Some(Effect::WrappedScroll(down, count));
        }
        if half && explicit {
            // Neovim limits a count assigned to 'scroll' to the window height.
            self.scroll_lines = Some(count.min(rows));
        }
        let amount = if half {
            self.scroll_lines.unwrap_or((rows / 2).max(1)).min(rows)
        } else if page {
            rows.saturating_sub(2).max(1).saturating_mul(count)
        } else {
            count
        };
        if half && ((down && row == last) || (!down && row == 0)) {
            return Some(Effect::Viewport(self.source_row(top)));
        }
        let amount = if half {
            amount.min(if down { last - row } else { row })
        } else {
            amount
        };
        let next_top = if down {
            top.saturating_add(amount).min(last)
        } else {
            top.saturating_sub(amount)
        };
        let next_row = if half {
            if down { row + amount } else { row - amount }
        } else if page {
            if down {
                next_top.saturating_add(margin).min(last)
            } else {
                next_top
                    .saturating_add(rows.saturating_sub(1 + margin))
                    .min(last)
            }
        } else {
            row.clamp(
                next_top.saturating_add(margin).min(last),
                next_top
                    .saturating_add(rows.saturating_sub(1 + margin))
                    .min(last),
            )
        };
        self.goto(self.source_row(next_row), column);
        Some(Effect::Viewport(self.source_row(next_top)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mouse_scroll_keeps_visible_cursor_without_changing_text() {
        let text = (0..20)
            .map(|i| format!("line {i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        for rows in 1..=20 {
            for current in 0..20 {
                for top in 0..20 {
                    for delta in [-30, -3, 0, 3, 30] {
                        let mut editor = Editor::new(text.clone());
                        editor.goto(current, 2);
                        let new_top = editor.mouse_scroll(top, rows, delta);
                        let cursor = editor.position().0;
                        assert!(
                            cursor >= new_top && cursor < new_top.saturating_add(rows),
                            "rows={rows}, cursor={current}, top={top}, delta={delta}"
                        );
                        assert_eq!(
                            editor.followed_top(new_top, rows),
                            new_top,
                            "rows={rows}, cursor={current}, top={top}, delta={delta}"
                        );
                        assert_eq!(editor.contents(), text);
                    }
                }
            }
        }
    }
    #[test]
    fn mouse_scroll_matches_neovim_at_file_boundaries() {
        #[derive(serde::Deserialize)]
        struct View {
            topline: usize,
        }
        #[derive(serde::Deserialize)]
        struct Step {
            direction: String,
            cursor: [usize; 5],
            view: View,
        }
        #[derive(serde::Deserialize)]
        struct Reference {
            height: usize,
            lines: Vec<String>,
            initial: View,
            steps: Vec<Step>,
        }
        for raw in include_str!("../../../Resources/Fixtures/nvim-mouse-wheel.jsonl").lines() {
            let reference: Reference = serde_json::from_str(raw).unwrap();
            let mut editor = Editor::new(reference.lines.join("\n"));
            let mut top = reference.initial.topline - 1;
            for (index, step) in reference.steps.iter().enumerate() {
                let delta = if step.direction == "down" { 3 } else { -3 };
                top = editor.mouse_scroll(top, reference.height, delta);
                assert_eq!(top, step.view.topline - 1, "top at step {index}");
                assert_eq!(
                    editor.position(),
                    (step.cursor[1] - 1, step.cursor[2] - 1),
                    "cursor at step {index}"
                );
                assert_eq!(
                    editor.followed_top(top, reference.height),
                    top,
                    "render follow at step {index}"
                );
            }
        }
    }
    #[test]
    fn mouse_scroll_skips_closed_fold_like_neovim() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-mouse-wheel-fold.jsonl"
        ))
        .unwrap();
        assert_eq!(reference["closed_before"], "1");
        let lines: Vec<_> = reference["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect();
        let text = lines.join("\n");
        let mut editor = Editor::new(text.clone()).for_path(std::path::Path::new("synthetic.rs"));
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
        assert_eq!(editor.folded_line_count(0), Some(3));
        let rows = reference["height"].as_u64().unwrap() as usize;
        let mut top = reference["initial"]["topline"].as_u64().unwrap() as usize - 1;
        for step in reference["steps"].as_array().unwrap() {
            let delta = if step["direction"] == "down" { 3 } else { -3 };
            top = editor.mouse_scroll(top, rows, delta);
            assert_eq!(top, step["view"]["topline"].as_u64().unwrap() as usize - 1);
            assert_eq!(
                editor.position().0,
                step["cursor"][1].as_u64().unwrap() as usize - 1
            );
            assert_eq!(editor.followed_top(top, rows), top);
        }
    }
    #[test]
    fn mouse_scroll_moves_view_and_keeps_cursor_visible_without_editing() {
        let text = (0..100).map(|i| format!("línea {i}\n")).collect::<String>();
        let mut editor = Editor::new(text.clone());
        editor.goto(10, 2);
        let top = editor.mouse_scroll(0, 12, 8);
        assert_eq!(top, 8);
        assert_eq!(editor.position(), (12, 2));
        assert_eq!(editor.followed_top(top, 12), top);
        let top = editor.mouse_scroll(top, 12, -50);
        assert_eq!(top, 0);
        assert_eq!(editor.followed_top(top, 12), top);
        assert_eq!(editor.text.iter().collect::<String>(), text);
    }
    #[test]
    fn wrapped_line_scroll_routes_counts_and_preserves_selection() {
        for entry in [None, Some("v"), Some("V"), Some("<C-v>")] {
            let mut editor = Editor::new("a long synthetic line\nsecond line\n".into());
            editor.goto(0, 4);
            if let Some(key) = entry {
                editor.key_with_wrap(key);
            }
            let position = editor.position();
            let mode = editor.mode;
            let block_anchor = editor.block_anchor_for_test();
            assert_eq!(
                editor.key_with_wrap("<C-e>"),
                Effect::WrappedScroll(true, 1)
            );
            for key in ["1", "0", "1"] {
                assert_eq!(editor.key_with_wrap(key), Effect::None);
            }
            assert_eq!(
                editor.key_with_wrap("<C-y>"),
                Effect::WrappedScroll(false, 101)
            );
            assert_eq!(editor.position(), position);
            assert_eq!(editor.mode, mode);
            assert_eq!(editor.block_anchor_for_test(), block_anchor);
            assert!(!editor.dirty());
        }
        let mut editor = Editor::new("a\nb\nc\n".into());
        assert!(matches!(editor.key("<C-e>"), Effect::Viewport(_)));
    }

    #[test]
    fn wrapped_center_routes_to_visual_viewport_without_changing_buffer() {
        let mut editor = Editor::new("  first\nsecond\nthird".into());
        editor.goto(0, 3);
        editor.jump_viewport(0, 0, 10, 30);
        assert_eq!(editor.key_with_wrap("z"), Effect::None);
        assert_eq!(editor.key_with_wrap("z"), Effect::WrappedCenter);
        assert_eq!(editor.position(), (0, 3));
        assert_eq!(editor.key_with_wrap("z"), Effect::None);
        assert_eq!(editor.key_with_wrap("."), Effect::WrappedCenter);
        assert_eq!(editor.position(), (0, 2));
        editor.goto(0, 3);
        assert_eq!(editor.key_with_wrap("z"), Effect::None);
        assert_eq!(editor.key_with_wrap("t"), Effect::WrappedTop);
        assert_eq!(editor.position(), (0, 3));
        assert_eq!(editor.key_with_wrap("z"), Effect::None);
        assert_eq!(editor.key_with_wrap("<Enter>"), Effect::WrappedTop);
        assert_eq!(editor.position(), (0, 2));
        editor.goto(0, 3);
        assert_eq!(editor.key_with_wrap("z"), Effect::None);
        assert_eq!(editor.key_with_wrap("b"), Effect::WrappedBottom);
        assert_eq!(editor.position(), (0, 3));
        assert_eq!(editor.key_with_wrap("z"), Effect::None);
        assert_eq!(editor.key_with_wrap("-"), Effect::WrappedBottom);
        assert_eq!(editor.position(), (0, 2));
        assert!(!editor.dirty());
        assert_eq!(editor.key("z"), Effect::None);
        assert!(matches!(editor.key("z"), Effect::Viewport(_)));
    }

    #[test]
    fn measured_pages_recenter_and_sticky_half_page_count_preserve_text_and_unicode() {
        let text = (1..=120)
            .map(|i| format!("  ñ界 {i}\n"))
            .collect::<String>();
        // Expected views cross-checked against clean Neovim 0.12.1,
        // height23, scrolloff4, startofline=false; no personal config loaded.
        for (key, expected_row, expected_top) in
            [("<C-d>", 60, 50), ("<C-f>", 64, 60), ("<C-b>", 36, 18)]
        {
            let mut e = Editor::new(text.clone());
            e.goto(49, 2);
            e.jump_viewport(39, 0, 23, 100);
            assert_eq!(e.key(key), Effect::Viewport(expected_top));
            assert_eq!(e.position(), (expected_row, 2));
            assert!(!e.dirty());
        }
        let mut e = Editor::new(text.clone());
        e.goto(49, 2);
        e.jump_viewport(39, 0, 23, 100);
        e.key("3");
        assert_eq!(e.key("<C-d>"), Effect::Viewport(42));
        e.jump_viewport(42, 0, 23, 100);
        assert_eq!(e.key("<C-u>"), Effect::Viewport(39));
        assert_eq!(e.position(), (49, 2));
        e.jump_viewport(39, 0, 23, 100);
        e.key("9");
        e.key("9");
        assert_eq!(e.key("<C-d>"), Effect::Viewport(62));
        assert_eq!(e.position(), (72, 2));
        e.jump_viewport(62, 0, 23, 100);
        assert_eq!(e.key("<C-u>"), Effect::Viewport(39));
        assert_eq!(e.position(), (49, 2));
        for (suffix, top) in [("t", 45), ("z", 38), ("b", 31)] {
            e.key("z");
            assert_eq!(e.key(suffix), Effect::Viewport(top));
            assert_eq!(e.position(), (49, 2));
        }
        e.key("1");
        e.key("z");
        assert_eq!(e.key("t"), Effect::Viewport(0));
        assert_eq!(e.position(), (0, 2));
        assert_eq!(e.followed_top(0, 23), 0);
        e.goto(49, 2);
        assert_eq!(e.followed_top(0, 23), 31);
        assert_eq!(e.followed_top(100, 23), 45);
        assert_eq!(e.followed_top(0, 1), 49);
        e.goto(0, 2);
        e.key("v");
        e.key("<C-d>");
        assert_eq!(e.mode, Mode::Visual);
        assert!(e.selection().unwrap().len() > 1);
        assert_eq!(e.text.iter().collect::<String>(), text);
        e.key("<Esc>");
        e.goto(120, 0);
        e.jump_viewport(110, 0, 23, 100);
        assert_eq!(e.key("<C-d>"), Effect::Viewport(110));
        e.key("i");
        assert_eq!(e.key("<C-d>"), Effect::None);
        assert_eq!(e.mode, Mode::Insert);
    }
}
/// Partial trackpad movement belongs to one view and must not leak into a
/// different pane, workspace or buffer when the pointer moves between them.
pub struct WheelAccumulator<K> {
    target: Option<K>,
    remainder: f32,
}

impl<K> Default for WheelAccumulator<K> {
    fn default() -> Self {
        Self {
            target: None,
            remainder: 0.0,
        }
    }
}

impl<K: PartialEq> WheelAccumulator<K> {
    pub fn push(&mut self, target: K, delta: f32, line_height: f32) -> isize {
        if !delta.is_finite() || !line_height.is_finite() || line_height <= 0.0 {
            return 0;
        }
        if self.target.as_ref() != Some(&target) {
            self.target = Some(target);
            self.remainder = 0.0;
        }
        self.remainder += delta;
        if !self.remainder.is_finite() {
            self.remainder = 0.0;
            return 0;
        }
        let whole = (self.remainder.abs() / line_height).floor();
        if whole < 1.0 {
            return 0;
        }
        let direction = self.remainder.signum();
        self.remainder -= direction * whole * line_height;
        -(direction as isize) * whole.min(8.0) as isize
    }
}

#[cfg(test)]
mod wheel_accumulator_tests {
    use super::WheelAccumulator;

    #[test]
    fn partial_gestures_do_not_cross_pane_workspace_or_file() {
        let mut wheel = WheelAccumulator::default();
        assert_eq!(wheel.push((0, 1, "a"), 6.0, 10.0), 0);
        assert_eq!(wheel.push((0, 2, "a"), 6.0, 10.0), 0);
        assert_eq!(wheel.push((0, 2, "a"), 4.0, 10.0), -1);
        assert_eq!(wheel.push((1, 2, "a"), 6.0, 10.0), 0);
        assert_eq!(wheel.push((1, 2, "b"), 6.0, 10.0), 0);
        assert_eq!(wheel.push((1, 2, "b"), 4.0, 10.0), -1);
        assert_eq!(wheel.push((1, 2, "b"), -1000.0, 10.0), 8);
        assert_eq!(wheel.push((1, 2, "b"), f32::NAN, 10.0), 0);
    }

    #[test]
    fn terminal_reconnection_and_pane_switch_do_not_inherit_motion() {
        let mut wheel = WheelAccumulator::default();
        assert_eq!(wheel.push((0, Some(1), 100), 6.0, 10.0), 0);
        assert_eq!(wheel.push((0, Some(1), 101), 6.0, 10.0), 0);
        assert_eq!(wheel.push((0, Some(2), 101), 6.0, 10.0), 0);
        assert_eq!(wheel.push((0, Some(2), 101), 4.0, 10.0), -1);
    }
}
