//! Pinned LazyVim base options: expandtab, tabstop=2, shiftwidth=2, shiftround.
//! Language indentexpr, comments and EditorConfig are separate adapters.
use super::{Editor, JumpImpact, Mode};

#[derive(Clone, Copy)]
pub(super) struct Style {
    pub(super) width: usize,
    pub(super) tabstop: usize,
    pub(super) expand: bool,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            width: 2,
            tabstop: 2,
            expand: true,
        }
    }
}

impl Editor {
    pub(super) fn shift_to_position(&mut self, op: char, target: usize) {
        let last_real = self.source_line_at(self.text.len().saturating_sub(1));
        let first =
            self.folded_first_row(self.source_line_at(self.cursor.min(target)).min(last_real));
        let last =
            self.folded_last_row(self.source_line_at(self.cursor.max(target)).min(last_real));
        self.shift_lines(
            self.line_starts[first],
            self.line_starts[last],
            op == '>',
            1,
            true,
        );
    }
    /// Static defaults verified against the installed Neovim ftplugins. No file IO.
    pub fn for_path(mut self, path: &std::path::Path) -> Self {
        let language = crate::syntax::Language::for_path(path);
        if language != self.fold_language {
            self.fold_language = language;
            self.parser_fold_result = None;
            self.folds = None;
        }
        self.indent_style = match path.extension().and_then(|s| s.to_str()) {
            Some("rs") => Style {
                width: 4,
                ..Style::default()
            },
            Some("py" | "pyi") => Style {
                width: 4,
                tabstop: 4,
                expand: true,
            },
            Some("go" | "mk") => Style {
                expand: false,
                ..Style::default()
            },
            _ if matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some("Makefile" | "makefile" | "GNUmakefile")
            ) =>
            {
                Style {
                    expand: false,
                    ..Style::default()
                }
            }
            _ => Style::default(),
        };
        self
    }
    pub fn tab_padding(&self, column: usize) -> usize {
        self.indent_style.tabstop - column % self.indent_style.tabstop
    }
    pub(super) fn tab_column(&self, offset: usize) -> usize {
        let column = self.indent_width(self.line_start(offset), offset);
        if self.text.get(offset) == Some(&'\t') && self.mode != Mode::Insert {
            column + self.tab_padding(column) - 1
        } else {
            column
        }
    }
    pub(super) fn offset_at_tab_column(&self, start: usize, target: usize) -> usize {
        let end = self.line_end(start);
        let mut column = 0;
        for offset in start..end {
            let width = if self.text[offset] == '\t' {
                self.tab_padding(column)
            } else {
                1
            };
            if column.saturating_add(width) > target {
                return offset;
            }
            column += width;
        }
        end
    }
    fn indent_end(&self, start: usize) -> usize {
        start
            + self.text[start..]
                .iter()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .count()
    }
    fn indent_width(&self, start: usize, end: usize) -> usize {
        self.text[start..end].iter().fold(0, |column, c| {
            column
                + if *c == '\t' {
                    self.tab_padding(column)
                } else {
                    1
                }
        })
    }
    pub(super) fn indent_tab(&mut self) {
        if !self.indent_style.expand {
            self.insert_base("\t");
            return;
        }
        let column = self.indent_width(self.line_start(self.cursor), self.cursor);
        let width = self.indent_style.width;
        self.insert_base(&" ".repeat(width - column % width));
    }
    pub(super) fn indent_backspace(&mut self) -> bool {
        let start = self.line_start(self.cursor);
        if self.cursor == start
            || self.text[start..self.cursor]
                .iter()
                .any(|c| !matches!(c, ' ' | '\t'))
        {
            return false;
        }
        let column = self.indent_width(start, self.cursor);
        let width = self.indent_style.width;
        let target = column.saturating_sub(1) / width * width;
        let mut from = self.cursor;
        let mut remaining = column;
        while from > start && remaining > target {
            from -= 1;
            remaining = self.indent_width(start, from);
        }
        self.prepare_jump_change(JumpImpact::Lines {
            start: self.source_line_at(from),
            removed: 0,
            added: 0,
        });
        self.text.splice(
            from..self.cursor,
            std::iter::repeat_n(' ', target - remaining),
        );
        self.cursor = from + target - remaining;
        self.changed();
        true
    }
    pub(super) fn indented_newline(&mut self) {
        let start = self.line_start(self.cursor);
        let end = self.indent_end(start).min(self.cursor);
        let prefix: String = self.text[start..end].iter().collect();
        if self.text.len().saturating_add(prefix.chars().count() + 1) > 2_000_000 {
            self.message = "Límite de edición: 2 millones de caracteres".into();
            return;
        }
        self.clear_automatic_indent();
        self.insert_base(&format!("\n{prefix}"));
        self.automatic_indent = Some((
            self.version,
            self.cursor - prefix.chars().count()..self.cursor,
        ));
    }
    pub(super) fn clear_automatic_indent(&mut self) {
        let Some((version, range)) = self.automatic_indent.take() else {
            return;
        };
        if version == self.version
            && self.cursor == range.end
            && self.line_start(self.cursor) == range.start
            && self.line_end(self.cursor) == range.end
        {
            self.prepare_jump_change(JumpImpact::Lines {
                start: self.source_line_at(range.start),
                removed: 0,
                added: 0,
            });
            self.text.drain(range.clone());
            self.cursor = range.start;
            self.changed();
        }
    }
    pub(super) fn open_indented_line(&mut self, below: bool) {
        let start = self.line_start(self.cursor);
        let prefix: String = self.text[start..self.indent_end(start)].iter().collect();
        if self.text.len().saturating_add(prefix.chars().count() + 1) > 2_000_000 {
            self.message = "Límite de edición: 2 millones de caracteres".into();
            return;
        }
        self.checkpoint();
        self.cursor = if below {
            self.line_end(self.cursor)
        } else {
            start
        };
        self.mode = Mode::Insert;
        if below {
            self.insert_base(&format!("\n{prefix}"));
        } else {
            self.insert_base_tracked(&format!("{prefix}\n"), true, true);
            self.cursor -= 1;
        }
        self.automatic_indent = Some((
            self.version,
            self.cursor - prefix.chars().count()..self.cursor,
        ));
    }
    pub(super) fn shift_insert_indent(&mut self, right: bool) {
        let start = self.line_start(self.cursor);
        self.shift_lines(start, start, right, 1, false);
    }
    pub(super) fn shift_lines(
        &mut self,
        start: usize,
        end: usize,
        right: bool,
        count: usize,
        checkpoint: bool,
    ) {
        let mut edits = Vec::new();
        let line_count = self
            .line_starts
            .iter()
            .filter(|&&line| line >= start && line <= end)
            .count();
        let mut resulting_len = self.text.len();
        for &line in self
            .line_starts
            .iter()
            .filter(|&&line| line >= start && line <= end)
        {
            let indent_end = self.indent_end(line);
            // Neovim does not add indentation to empty lines with >>.
            if checkpoint && indent_end == line && self.line_end(line) == line {
                continue;
            }
            let width = self.indent_width(line, indent_end);
            let step = self.indent_style.width;
            let target = if right {
                (width / step + count) * step
            } else {
                width.div_ceil(step).saturating_sub(count) * step
            };
            let replacement = if self.indent_style.expand {
                " ".repeat(target)
            } else {
                format!(
                    "{}{}",
                    "\t".repeat(target / self.indent_style.tabstop),
                    " ".repeat(target % self.indent_style.tabstop)
                )
            };
            let target = replacement.chars().count();
            resulting_len = resulting_len
                .saturating_sub(indent_end - line)
                .saturating_add(target);
            if resulting_len > 2_000_000 {
                self.message = "Indentación fuera del límite de edición".into();
                return;
            }
            edits.push((line, indent_end, replacement, target));
        }
        let visual = self.selection().is_some();
        let old_mode = self.mode;
        let mut anchor = self.anchor;
        let folds = self.shift_fold_snapshot();
        let shifted_rows = self.source_line_at(start)..self.source_line_at(end) + 1;
        let manual_shift = checkpoint
            .then(|| self.manual_shift_fold_snapshot())
            .flatten()
            .filter(|definitions| {
                line_count == 1
                    || definitions.iter().any(|(rows, _, closed)| {
                        *closed
                            && ((rows.start > shifted_rows.start && rows.end <= shifted_rows.end)
                                || rows.end == shifted_rows.start)
                    })
            });
        if checkpoint {
            self.checkpoint();
        }
        for (line, indent_end, replacement, target) in edits.into_iter().rev() {
            if anchor >= indent_end {
                anchor = anchor - (indent_end - line) + target;
            } else if anchor >= line {
                anchor = line + target;
            }
            if self.cursor >= indent_end {
                self.cursor = self.cursor - (indent_end - line) + target;
            } else if self.cursor >= line {
                self.cursor = line + target;
            }
            self.text.splice(line..indent_end, replacement.chars());
        }
        let mapped_cursor = self.cursor;
        if checkpoint {
            self.mode = Mode::Normal;
            self.cursor = start;
        }
        self.changed();
        self.restore_shift_folds(folds);
        if let Some(previous) = manual_shift {
            self.restore_manual_shift(previous, shifted_rows);
        }
        if visual {
            // LazyVim maps Visual >/< to >gv/<gv. Each press remains one undo step.
            self.finish_change();
            if self.replay_depth == 0 {
                self.remember_change(vec![super::EditEvent::Shift {
                    lines: line_count,
                    count,
                    right,
                }]);
            }
            self.anchor = anchor;
            self.cursor = mapped_cursor;
            self.mode = old_mode;
            self.clamp();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(editor: &mut Editor, keys: &[&str]) {
        for key in keys {
            editor.key(key);
        }
    }
    #[test]
    fn base_indent_matches_clean_neovim_and_keeps_insert_undo() {
        let mut e = Editor::new("abc".into());
        keys(&mut e, &["i", "<Tab>", "<BS>"]);
        e.insert("X");
        e.key("<Esc>");
        assert_eq!(e.contents(), "Xabc");
        e.key("u");
        assert_eq!(e.contents(), "abc");
        for key in ["o", "O", "A"] {
            let mut e = Editor::new("  abc".into());
            e.key(key);
            if key == "A" {
                e.key("<Enter>");
            }
            e.insert("X");
            e.key("<Esc>");
            assert_eq!(
                e.contents(),
                if key == "O" {
                    "  X\n  abc"
                } else {
                    "  abc\n  X"
                }
            );
            e.key("u");
            assert_eq!(e.contents(), "  abc");
        }
    }
    #[test]
    fn rounded_shift_counts_visual_and_replay_are_one_change() {
        let mut e = Editor::new(" a\n   b\n\nc".into());
        keys(&mut e, &["3", ">", ">"]);
        assert_eq!(e.contents(), "  a\n    b\n\nc");
        e.key("u");
        assert_eq!(e.contents(), " a\n   b\n\nc");
        keys(&mut e, &["V", "j", ">"]);
        assert_eq!(e.contents(), "  a\n    b\n\nc");
        assert_eq!(e.mode, Mode::VisualLine);
        e.key(">");
        assert_eq!(e.contents(), "    a\n      b\n\nc");
        e.key("<Esc>");
        e.key("u");
        assert_eq!(e.contents(), "  a\n    b\n\nc");
        let mut e = Editor::new("   a\r\nb".into());
        keys(&mut e, &["<", "<"]);
        assert_eq!(e.contents(), "  a\r\nb");
        e.key(".");
        while e.replaying() {
            e.advance_replay();
        }
        assert_eq!(e.contents(), "a\r\nb");
    }
    #[test]
    fn shift_motion_at_file_boundary_matches_clean_neovim() {
        for sequence in ["G>j", "gg>k", "G2>>", "G2>j"] {
            let mut e = Editor::new("one\ntwo".into());
            for key in sequence.chars() {
                e.key(&key.to_string());
            }
            assert_eq!(e.contents(), "one\ntwo", "{sequence}");
            assert!(!e.dirty(), "{sequence}");
        }
        for sequence in ["gg2>j", "gg3>>", "gg2>>", "G2>k"] {
            let mut e = Editor::new("one\ntwo".into());
            for key in sequence.chars() {
                e.key(&key.to_string());
            }
            assert_eq!(e.contents(), "  one\n  two", "{sequence}");
        }
    }
    #[test]
    fn shift_gg_and_g_counts_match_clean_neovim() {
        for (sequence, expected, row) in [
            ("4G>gg", "  one\n  two\n  three\n  four\nfive", 0),
            ("4G2>gg", "one\n  two\n  three\n  four\nfive", 1),
            ("4G>2gg", "one\n  two\n  three\n  four\nfive", 1),
            ("4G2>2gg", "one\ntwo\nthree\n  four\nfive", 3),
            ("2G>G", "one\n  two\n  three\n  four\n  five", 1),
            ("2G>3G", "one\n  two\n  three\nfour\nfive", 1),
            ("2G2>G", "one\n  two\nthree\nfour\nfive", 1),
        ] {
            let mut e = Editor::new("one\ntwo\nthree\nfour\nfive".into());
            for key in sequence.chars() {
                e.key(&key.to_string());
            }
            assert_eq!(e.contents(), expected, "{sequence}");
            assert_eq!(e.position().0, row, "{sequence}");
            e.key("u");
            assert_eq!(e.contents(), "one\ntwo\nthree\nfour\nfive", "{sequence}");
        }
        let mut e = Editor::new("  one\n  two\n  three".into());
        keys(&mut e, &["G", "<", "g", "g"]);
        assert_eq!(e.contents(), "one\ntwo\nthree");
        e.key("u");
        keys(&mut e, &["<", "g", "<Esc>"]);
        assert_eq!(e.contents(), "  one\n  two\n  three");
        for (sequence, expected, row) in [
            (
                "4G>ggG.",
                "    one\n    two\n    three\n    four\n  five",
                0,
            ),
            ("4G>2ggG.", "one\n    two\n    three\n    four\n  five", 1),
            ("2G>3GG.", "one\n  two\n    three\n  four\n  five", 2),
        ] {
            let mut e = Editor::new("one\ntwo\nthree\nfour\nfive".into());
            for key in sequence.chars() {
                e.key(&key.to_string());
            }
            while e.replaying() {
                e.advance_replay();
            }
            assert_eq!(e.contents(), expected, "{sequence}");
            assert_eq!(e.position().0, row, "{sequence}");
        }
    }
    #[test]
    fn literal_paste_retains_tabs_and_newlines() {
        let mut e = Editor::new("  abc".into());
        e.key("A");
        e.paste("\n\tdata");
        e.key("<Esc>");
        assert_eq!(e.contents(), "  abc\n\tdata");
        e.key("u");
        assert_eq!(e.contents(), "  abc");
    }
    #[test]
    fn automatic_empty_indent_is_removed_but_typed_whitespace_survives() {
        let mut e = Editor::new("  abc".into());
        keys(&mut e, &["o", "<Esc>"]);
        assert_eq!(e.contents(), "  abc\n");
        e.key("u");
        keys(&mut e, &["o", "<Enter>"]);
        e.insert("X");
        e.key("<Esc>");
        assert_eq!(e.contents(), "  abc\n\n  X");
        e.key("u");
        e.key("o");
        e.insert(" ");
        e.key("<Esc>");
        assert_eq!(e.contents(), "  abc\n   ");
    }
    #[test]
    fn filetype_indent_and_insert_shift_preserve_make_tabs() {
        for (name, expected) in [
            ("main.rs", "    X"),
            ("main.py", "    X"),
            ("main.go", "\tX"),
            ("Makefile", "\tX"),
            ("main.json", "  X"),
        ] {
            let mut e = Editor::new(String::new()).for_path(std::path::Path::new(name));
            keys(&mut e, &["i", "<Tab>"]);
            e.insert("X");
            e.key("<Esc>");
            assert_eq!(e.contents(), expected, "{name}");
            e.key("u");
            assert_eq!(e.contents(), "");
        }
        let mut e = Editor::new(String::new()).for_path(std::path::Path::new("Makefile"));
        keys(&mut e, &["i", "<C-t>"]);
        e.insert("cmd");
        e.key("<Esc>");
        assert_eq!(e.contents(), "\tcmd");
        keys(&mut e, &[">", ">"]);
        assert_eq!(e.contents(), "\t\tcmd");
        keys(&mut e, &["<", "<"]);
        assert_eq!(e.contents(), "\tcmd");
    }
    #[test]
    fn dot_after_reselected_visual_shift_repeats_the_last_indent_only() {
        let mut e = Editor::new("a\nb\nc\nd".into());
        keys(&mut e, &["V", "j", ">", ">", "<Esc>", "j", "."]);
        while e.replaying() {
            e.advance_replay();
        }
        assert_eq!(e.contents(), "    a\n    b\n  c\n  d");
        e.key("u");
        assert_eq!(e.contents(), "    a\n    b\nc\nd");
    }
}
