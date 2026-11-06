//! Character/line Visual operations. These preserve newlines and registers.
use super::*;

fn upper(c: char) -> char {
    // Vim uses simple case mappings, not expanding Unicode strings. The sharp
    // s has a simple uppercase mapping absent from Rust's full mapping.
    if c == 'ß' {
        return 'ẞ';
    }
    let mut mapped = c.to_uppercase();
    let first = mapped.next().unwrap_or(c);
    if mapped.next().is_none() { first } else { c }
}
fn lower(c: char) -> char {
    // Simple lowercase of dotted capital I is i, without a combining mark.
    if c == 'İ' {
        return 'i';
    }
    let mut mapped = c.to_lowercase();
    let first = mapped.next().unwrap_or(c);
    if mapped.next().is_none() { first } else { c }
}
pub(super) fn convert(c: char, op: char) -> char {
    match op {
        'u' => lower(c),
        'U' => upper(c),
        _ if c.is_uppercase() => lower(c),
        _ => upper(c),
    }
}
impl Editor {
    /// Apply a counted linewise case operator, including its end-of-file rule.
    pub(super) fn case_current_lines(&mut self, op: char, count: usize) {
        let row = self.source_line_at(self.cursor);
        let first_visible = self.visible_row(row);
        let last_visible = self.visible_row(self.line_starts.len().saturating_sub(1));
        if count > 1 && first_visible == last_visible {
            return;
        }
        let start = self.line_starts[self.folded_first_row(row)];
        let mut end = start;
        for _ in 0..count {
            let row = self
                .line_starts
                .partition_point(|start| *start <= end)
                .saturating_sub(1);
            let last = self.folded_last_row(row);
            end = (self.line_end(self.line_starts[last]) + 1).min(self.text.len());
        }
        self.operate(op, start..end, true);
    }
    pub(super) fn case_to_line(&mut self, op: char, target: usize) {
        let column = self.cursor - self.line_start(self.cursor);
        let start = self.line_start(self.cursor.min(target));
        let end = (self.line_end(self.cursor.max(target)) + 1).min(self.text.len());
        let cursor = start + column.min(self.line_end(start) - start);
        self.case_range(start..end, op, cursor);
    }
    pub(super) fn case_range(&mut self, range: Range<usize>, op: char, cursor: usize) {
        if !self.text[range.clone()]
            .iter()
            .any(|c| convert(*c, op) != *c)
        {
            self.finish_visual(cursor);
            return;
        }
        self.checkpoint();
        for value in &mut self.text[range] {
            *value = convert(*value, op);
        }
        self.finish_visual(cursor);
        self.changed();
    }
    pub(super) fn replace_visual(&mut self, range: Range<usize>, ch: char) {
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthChar;
        let selected: String = self.text[range.clone()].iter().collect();
        let mut replacement = String::new();
        let mut previous_tab = false;
        for grapheme in selected.graphemes(true) {
            if previous_tab && grapheme.chars().all(|c| c.width() == Some(0)) {
                replacement.push_str(grapheme);
            } else {
                replacement.push(if grapheme == "\n" { '\n' } else { ch });
            }
            previous_tab = grapheme == "\t";
        }
        self.checkpoint();
        self.track_visual_edit(range.clone(), 0, false);
        self.text.splice(range.clone(), replacement.chars());
        self.changed();
        self.finish_visual(range.start);
    }
    fn finish_visual(&mut self, cursor: usize) {
        self.mode = Mode::Normal;
        self.cursor = cursor;
        self.count = 0;
        self.pending.clear();
        self.chosen_register = None;
        self.clamp();
    }
    pub(super) fn visual_key(&mut self, key: &str) -> bool {
        if self.pending.starts_with('z') {
            return false;
        }
        let Some(range) = self.selection() else {
            return false;
        };
        match key {
            "o" | "O" => {
                std::mem::swap(&mut self.cursor, &mut self.anchor);
                self.count = 0;
            }
            "u" | "U" | "~" => {
                self.case_range(range.clone(), key.chars().next().unwrap(), range.start);
            }
            "x" => self.operate('d', range, self.mode == Mode::VisualLine),
            "D" | "X" | "C" | "S" | "R" | "Y" => {
                let column = self.cursor - self.line_start(self.cursor);
                let start = self.line_start(range.start);
                let end = (self.line_end(range.end.saturating_sub(1)) + 1).min(self.text.len());
                let op = match key {
                    "D" | "X" => 'd',
                    "Y" => 'y',
                    _ => 'c',
                };
                let deletes_tail =
                    end == self.text.len() && start > 0 && self.text.last() != Some(&'\n');
                self.operate(op, start..end, true);
                if op == 'd' && !deletes_tail {
                    let start = self.line_start(self.cursor);
                    self.cursor = start + column.min(self.line_end(start) - start);
                    self.clamp();
                }
            }
            _ => return false,
        }
        true
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
    #[test]
    fn normal_case_operators_counts_objects_and_tilde_match_neovim_probe() {
        for (text, sequence, expected, cursor) in [
            ("ab cd ef", "gUw", "AB cd ef", (0, 0)),
            ("AB CD EF", "gu2w", "ab cd EF", (0, 0)),
            ("  ab cd\n next", "gUU", "  AB CD\n next", (0, 0)),
            ("  ab cd\n next", "3lgUU", "  AB CD\n next", (0, 2)),
            ("  ab\ncd\nef", "2gUU", "  AB\nCD\nef", (0, 0)),
            ("  ab\ncd\nef", "lgUj", "  AB\nCD\nef", (0, 1)),
            ("(ab cd) tail", "lgUi)", "(AB CD) tail", (0, 1)),
            ("aBñÉß", "3~", "AbÑÉß", (0, 3)),
            ("ab\ncd\nef", "jlgUgg", "AB\nCD\nef", (0, 1)),
            ("ab\ncd\nef", "gU3G", "AB\nCD\nEF", (0, 0)),
            ("  ab\ncd", "3lgUj", "  AB\nCD", (0, 3)),
        ] {
            let mut e = Editor::new(text.into());
            e.registers.capture('a', 'y', "KEEP".into(), false).unwrap();
            keys(&mut e, sequence);
            assert_eq!(e.contents(), expected, "{sequence}");
            assert_eq!(e.position(), cursor, "{sequence}");
            assert_eq!(e.registers.get('"').unwrap().0, "KEEP");
            e.key("u");
            assert_eq!(e.contents(), text, "{sequence}");
        }
        let mut e = Editor::new("ab cd".into());
        keys(&mut e, "gUww.");
        assert_eq!(e.contents(), "AB CD");
        e.key("u");
        assert_eq!(e.contents(), "AB cd");
        let mut e = Editor::new("ab".into());
        e.key("x");
        let version = e.version;
        keys(&mut e, "guw");
        assert_eq!(e.version, version);
        e.key("u");
        assert_eq!(e.contents(), "ab");
    }
    #[test]
    fn g_case_goto_counts_match_clean_neovim() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-case-line-goto.json"
        ))
        .unwrap();
        let original = fixture["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for case in fixture["cases"].as_array().unwrap() {
            let sequence = case["keys"].as_str().unwrap();
            let expected = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let mut editor = Editor::new(original.clone());
            editor
                .registers
                .capture('a', 'y', "KEEP".into(), false)
                .unwrap();
            keys(&mut editor, sequence);
            assert_eq!(editor.contents(), expected, "{sequence}");
            assert_eq!(
                editor.position(),
                (
                    case["cursor"][0].as_u64().unwrap() as usize - 1,
                    case["cursor"][1].as_u64().unwrap() as usize,
                ),
                "{sequence}"
            );
            assert_eq!(editor.registers.get('"').unwrap().0, "KEEP");
            if expected != original {
                editor.key("u");
                assert_eq!(editor.contents(), original, "undo {sequence}");
            }
        }
    }
    #[test]
    fn repeated_g_case_line_operators_match_neovim() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-case-line-operator.json"
        ))
        .unwrap();
        let original = fixture["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for case in fixture["cases"].as_array().unwrap() {
            let sequence = case["keys"].as_str().unwrap();
            let expected = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let mut editor = Editor::new(original.clone());
            editor
                .registers
                .capture('a', 'y', "KEEP".into(), false)
                .unwrap();
            keys(&mut editor, sequence);
            assert_eq!(editor.contents(), expected, "{sequence}");
            assert_eq!(
                editor.position(),
                (
                    case["cursor"][0].as_u64().unwrap() as usize - 1,
                    case["cursor"][1].as_u64().unwrap() as usize,
                ),
                "{sequence}"
            );
            assert_eq!(editor.registers.get('"').unwrap().0, "KEEP");
            editor.key("u");
            assert_eq!(editor.contents(), original, "undo {sequence}");
        }
    }
    #[test]
    fn repeated_g_case_operators_count_closed_folds_like_neovim() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-case-line-fold.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            assert_eq!(case["closed"], 2);
            let original = case["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let expected = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let mut editor =
                Editor::new(original.clone()).for_path(std::path::Path::new("synthetic.rs"));
            let mut engine = crate::syntax_folds::Engine::default();
            let folds = engine
                .compute(
                    crate::syntax::Language::Rust,
                    &original,
                    &std::sync::atomic::AtomicUsize::new(0),
                )
                .unwrap();
            assert!(editor.apply_parser_folds(editor.version, Ok(folds)));
            keys(&mut editor, "zM");
            assert_eq!(editor.folded_line_count(editor.line_starts[1]), Some(3));
            let sequence = case["keys"].as_str().unwrap();
            keys(&mut editor, sequence);
            assert_eq!(editor.contents(), expected, "{sequence}");
            assert_eq!(
                editor.position(),
                (
                    case["cursor"][0].as_u64().unwrap() as usize - 1,
                    case["cursor"][1].as_u64().unwrap() as usize
                ),
                "{sequence}"
            );
            editor.key("u");
            assert_eq!(editor.contents(), original, "undo {sequence}");
        }
    }
    #[test]
    fn repeated_g_case_operators_preserve_dot_repeat() {
        for raw in [
            include_str!("../../../Resources/Fixtures/nvim-case-line-repeat.json"),
            include_str!("../../../Resources/Fixtures/nvim-case-line-after-failed.json"),
        ] {
            let fixture: serde_json::Value = serde_json::from_str(raw).unwrap();
            let original = fixture["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|line| line.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            for case in fixture["cases"].as_array().unwrap() {
                let sequence = case["keys"].as_str().unwrap();
                let expected = case["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|line| line.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut editor = Editor::new(original.clone());
                editor
                    .registers
                    .capture('a', 'y', "KEEP".into(), false)
                    .unwrap();
                keys(&mut editor, sequence);
                assert_eq!(editor.contents(), expected, "{sequence}");
                assert_eq!(
                    editor.position(),
                    (
                        case["cursor"][0].as_u64().unwrap() as usize - 1,
                        case["cursor"][1].as_u64().unwrap() as usize,
                    ),
                    "{sequence}"
                );
                assert_eq!(editor.registers.get('"').unwrap().0, "KEEP");
            }
        }
    }
    #[test]
    fn visual_replace_case_and_cursor_match_clean_neovim_probe() {
        for (text, sequence, expected, cursor) in [
            ("abcde\nNEXT", "lvllrñ", "añññe\nNEXT", (0, 1)),
            ("abcde\nNEXT", "3lvhhrX", "aXXXe\nNEXT", (0, 1)),
            ("ab\ncde\nFG", "lvjlrX", "aX\nXXX\nFG", (0, 1)),
            ("  AbC\nDÉf", "Vju", "  abc\ndéf", (0, 0)),
            ("ñßéıﬀ", "v$U", "ÑẞÉIﬀ", (0, 0)),
            ("aBñÉß", "v$~", "AbÑéẞ", (0, 0)),
        ] {
            let mut e = Editor::new(text.into());
            e.registers.capture('a', 'y', "KEEP".into(), false).unwrap();
            keys(&mut e, sequence);
            assert_eq!(e.contents(), expected, "{sequence}");
            assert_eq!(e.position(), cursor, "{sequence}");
            assert_eq!(e.mode, Mode::Normal);
            assert_eq!(e.registers.get('"').unwrap().0, "KEEP");
            e.key("u");
            assert_eq!(e.contents(), text);
            e.key("<C-r>");
            assert_eq!(e.contents(), expected);
        }
    }
    #[test]
    fn endpoint_exchange_preserves_selection_and_delete_uses_register() {
        for (sequence, mode, selection) in [
            ("lvjlo", Mode::Visual, 1..7),
            ("Vjo", Mode::VisualLine, 0..7),
        ] {
            let mut e = Editor::new("abc\ndef".into());
            keys(&mut e, sequence);
            assert_eq!(e.cursor, if mode == Mode::Visual { 1 } else { 0 });
            assert_eq!(e.mode, mode);
            assert_eq!(e.selection(), Some(selection.clone()));
            e.key("o");
            assert_eq!(e.selection(), Some(selection));
            assert!(!e.dirty());
        }
        let mut e = Editor::new("abc\ndef".into());
        keys(&mut e, "vlx");
        assert_eq!(e.contents(), "c\ndef");
        assert_eq!(e.registers.get('"').unwrap().0, "ab");
        e.key("u");
        assert_eq!(e.contents(), "abc\ndef");
    }
    #[test]
    fn visual_capital_aliases_use_whole_lines_and_keep_registers_linewise() {
        for key in ["D", "X", "C", "S", "R", "Y"] {
            let mut e = Editor::new("abcde\nNEXT\nlast".into());
            keys(&mut e, "lvjl");
            e.key(key);
            let change = matches!(key, "C" | "S" | "R");
            if change {
                assert_eq!(e.mode, Mode::Insert);
                e.insert("Z");
                e.key("<Esc>");
            }
            assert_eq!(e.registers.get('"'), Some(("abcde\nNEXT\n".into(), true)));
            assert_eq!(
                e.contents(),
                match key {
                    "D" | "X" => "last",
                    "Y" => "abcde\nNEXT\nlast",
                    _ => "Z\nlast",
                },
                "{key}"
            );
            assert_eq!(
                e.position(),
                if matches!(key, "D" | "X") {
                    (0, 2)
                } else {
                    (0, 0)
                }
            );
            assert_eq!(e.mode, Mode::Normal);
            if key != "Y" {
                e.key("u");
                assert_eq!(e.contents(), "abcde\nNEXT\nlast");
            } else {
                assert!(!e.dirty());
            }
        }
        let mut e = Editor::new("abcde\nNEXT\nlast".into());
        keys(&mut e, "qavjDq");
        assert_eq!(e.contents(), "last");
        e.key("u");
        e.goto(0, 0);
        keys(&mut e, "@a");
        assert_eq!(e.contents(), "last");
    }
    #[test]
    fn line_aliases_keep_indent_last_line_and_reverse_selection() {
        for (text, sequence, expected, position, register) in [
            (
                "  abcde\n NEXT\nlast",
                "lvjlC",
                "  Z\nlast",
                (0, 2),
                "  abcde\n NEXT\n",
            ),
            ("first\nlast", "jlv$C", "first\nZ", (1, 0), "last\n"),
            ("first\nlast", "jlv$D", "first", (0, 4), "last\n"),
            ("first\nlast", "jlv$Y", "first\nlast", (1, 0), "last\n"),
            (
                "abcde\nNEXT\nlast",
                "jllvkhD",
                "last",
                (0, 1),
                "abcde\nNEXT\n",
            ),
            (
                "abcde\nNEXT\nlast",
                "llVjD",
                "last",
                (0, 2),
                "abcde\nNEXT\n",
            ),
        ] {
            let mut e = Editor::new(text.into());
            keys(&mut e, sequence);
            if e.mode == Mode::Insert {
                e.insert("Z");
                e.key("<Esc>");
            }
            assert_eq!(e.contents(), expected, "{sequence}");
            assert_eq!(e.position(), position, "{sequence}");
            assert_eq!(e.registers.get('"'), Some((register.into(), true)));
            if e.dirty() {
                e.key("u");
                assert_eq!(e.contents(), text, "{sequence}");
            }
        }
        let mut e = Editor::new("  abc\nend".into());
        keys(&mut e, "cc");
        assert_eq!(e.contents(), "  \nend");
        e.key("<Esc>");
        assert_eq!(e.contents(), "\nend");
        e.key("u");
        assert_eq!(e.contents(), "  abc\nend");
        let mut e = Editor::new("first\nlast".into());
        keys(&mut e, "jdd");
        assert_eq!(e.contents(), "first");
        assert_eq!(e.position(), (0, 0));
        e.key("u");
        assert_eq!(e.contents(), "first\nlast");
        for (text, sequence, expected, column) in [
            ("  first\nlast", "jlldd", "  first", 2),
            ("first\n  next", "lldd", "  next", 2),
            ("first\nlast", "llyy", "first\nlast", 2),
        ] {
            let mut e = Editor::new(text.into());
            keys(&mut e, sequence);
            assert_eq!(e.contents(), expected);
            assert_eq!(e.position(), (0, column));
        }
    }
    #[test]
    fn visual_replace_escape_and_macro_keep_normal_undo_contract() {
        let mut e = Editor::new("abc\ndef".into());
        keys(&mut e, "vlr");
        e.key("<Esc>");
        assert_eq!(e.contents(), "abc\ndef");
        assert_eq!(e.mode, Mode::Normal);
        e.goto(0, 0);
        keys(&mut e, "qavlrXq");
        assert_eq!(e.contents(), "XXc\ndef");
        e.key("u");
        e.goto(0, 0);
        keys(&mut e, "@a");
        assert_eq!(e.contents(), "XXc\ndef");
        e.key("u");
        assert_eq!(e.contents(), "abc\ndef");
    }
}
