//! Paragraph navigation, using Neovim's default paragraph/section macros.
use super::*;

pub(super) struct ParagraphMotion {
    pub offset: usize,
    pub inclusive: bool,
}

const PARAGRAPHS: &str = "IPLPPPQPP TPHPLIPpLpItpplpipbp";
const SECTIONS: &str = "SHNHH HUnhsh";

impl Editor {
    fn paragraph_boundary(&self, row: usize) -> bool {
        let start = self.line_starts[row];
        let end = self.line_end(start);
        let line = &self.text[start..end];
        if line.is_empty() || line.first() == Some(&'\x0c') {
            return true;
        }
        if line.first() != Some(&'.') {
            return false;
        }
        let a = line.get(1).copied();
        let b = line.get(2).copied();
        [PARAGRAPHS, SECTIONS].into_iter().any(|option| {
            option.as_bytes().as_chunks::<2>().0.iter().any(|pair| {
                let first = pair[0] as char;
                let second = pair[1] as char;
                (a == Some(first) || (first == ' ' && (a.is_none() || a == Some(' '))))
                    && (b == Some(second)
                        || (second == ' ' && (a.is_none() || b.is_none() || b == Some(' '))))
            })
        })
    }

    /// An unsuccessful count leaves the cursor unchanged, matching findpar.
    pub(super) fn paragraph_motion(&self, forward: bool, count: usize) -> Option<ParagraphMotion> {
        let last = self.source_line_at(self.text.len().saturating_sub(1));
        let mut row = self.source_line_at(self.cursor).min(last) as isize;
        let direction = if forward { 1 } else { -1 };
        for remaining in (0..count.max(1)).rev() {
            let mut did_skip = false;
            let mut first = true;
            loop {
                let current = row as usize;
                let start = self.line_starts[current];
                if self.line_end(start) != start {
                    did_skip = true;
                }
                let mut fold_skipped = false;
                if first && let Some(fold) = self.closed_fold_rows(current) {
                    row = if forward {
                        fold.end as isize
                    } else {
                        fold.start as isize - 1
                    };
                    fold_skipped = true;
                }
                if !first && did_skip && self.paragraph_boundary(row as usize) {
                    break;
                }
                if fold_skipped {
                    row -= direction;
                }
                row += direction;
                if row < 0 || row > last as isize {
                    if remaining != 0 {
                        return None;
                    }
                    row -= direction;
                    break;
                }
                first = false;
            }
        }
        let row = row as usize;
        let start = self.line_starts[row];
        let end = self.line_end(start);
        if forward && row == last && end > start {
            Some(ParagraphMotion {
                offset: self.grapheme_move(end, 1, false, true)?,
                inclusive: true,
            })
        } else {
            Some(ParagraphMotion {
                offset: start,
                inclusive: false,
            })
        }
    }

    /// Apply the exclusive-motion line adjustment used by Neovim's d/y
    /// operators, including its multiline delete promotion at end of line.
    pub(super) fn operate_paragraph(&mut self, op: char, forward: bool, count: usize) {
        let original = self.cursor;
        let Some(motion) = self.paragraph_motion(forward, count) else {
            self.chosen_register = None;
            return;
        };
        self.record_history_jump();
        let mut start = original.min(motion.offset);
        let mut end = original.max(motion.offset);
        if motion.inclusive {
            let Some(next) = self.grapheme_move(end, 1, true, true) else {
                self.chosen_register = None;
                return;
            };
            end = next;
        }
        if start == end {
            if op == 'y' {
                self.operate(op, start..end, false);
            } else if op == 'c' {
                self.chosen_register = None;
                self.mode = Mode::Insert;
            } else {
                self.chosen_register = None;
            }
            return;
        }
        let first_row = self.source_line_at(start);
        let start_of_line = self.line_starts[first_row];
        let in_indent = self.text[start_of_line..start]
            .iter()
            .all(|c| matches!(c, ' ' | '\t'));
        let mut linewise = false;
        if !motion.inclusive && end == self.line_start(end) && self.source_line_at(end) > first_row
        {
            if in_indent {
                linewise = true;
                start = start_of_line;
            } else {
                // The end moves to the character before the preceding newline.
                end = end.saturating_sub(1);
            }
        }
        if op == 'd' && !linewise && end > start {
            let last_row = self.source_line_at(end.saturating_sub(1));
            let last_end = self.line_end(self.line_starts[last_row]);
            if last_row > first_row
                && in_indent
                && end <= last_end
                && self.text[end..last_end]
                    .iter()
                    .all(|c| matches!(c, ' ' | '\t'))
            {
                linewise = true;
                start = start_of_line;
                end = (last_end + 1).min(self.text.len());
            }
        }
        if end <= start {
            self.chosen_register = None;
            return;
        }
        if linewise && op == 'y' && motion.offset < original {
            self.cursor = start;
        }
        let previous_fold = (op == 'd' && linewise && end == self.text.len() && first_row > 0)
            .then(|| self.closed_fold_rows(first_row - 1))
            .flatten()
            .map(|fold| self.line_starts[fold.start]);
        // For linewise change, undo restores the first inserted source line
        // after a multiline replacement, or the changed line for one line.
        let undo_cursor = (op == 'c' && linewise).then(|| {
            let last_row = self.source_line_at(end.saturating_sub(1));
            let row = first_row + usize::from(last_row > first_row);
            (self.line_starts[row], row)
        });
        self.operate(op, start..end, linewise);
        if let Some((cursor, row)) = undo_cursor
            && let Some(revision) = self.undo.last_mut()
        {
            revision.cursor = cursor;
            revision.redo_cursor_line = Some(row);
        }
        // A linewise delete can expose a closed fold. Neovim places the
        // cursor on its summary row at the first nonblank character.
        let current_fold = self
            .closed_fold_rows(self.source_line_at(self.cursor))
            .map(|fold| self.line_starts[fold.start]);
        if op == 'd'
            && linewise
            && let Some(start) = current_fold.or(previous_fold)
        {
            let indent = if current_fold.is_some() {
                self.text[start..self.line_end(start)]
                    .iter()
                    .take_while(|c| matches!(c, ' ' | '\t'))
                    .count()
            } else {
                0
            };
            self.cursor = start + indent;
            self.clamp();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_repeat_undo_and_paste_match_clean_neovim() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-paragraph-repeat.json"
        ))
        .unwrap();
        let lines = reference["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let mut failures = Vec::new();
        for (index, scenario) in reference["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let mut editor = Editor::new(lines.clone());
            editor
                .registers
                .capture('"', 'y', "__SENTINEL__".into(), false)
                .unwrap();
            for state in scenario.as_array().unwrap() {
                let keys = state["key"].as_str().unwrap();
                for key in keys
                    .replace("<Esc>", "\u{1b}")
                    .replace("<CR>", "\n")
                    .replace("<C-r>", "\u{12}")
                    .chars()
                {
                    if key == '\u{1b}' {
                        editor.key("<Esc>");
                    } else if key == '\n' {
                        editor.key("<Enter>");
                    } else if key == '\u{12}' {
                        editor.key("<C-r>");
                    } else if editor.mode == Mode::Insert {
                        editor.insert(&key.to_string());
                    } else {
                        editor.key(&key.to_string());
                    }
                    while editor.replaying() {
                        editor.advance_replay();
                    }
                }
                let expected = state["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("\n");
                let pos = state["pos"].as_array().unwrap();
                let expected_pos = (
                    pos[0].as_u64().unwrap() as usize - 1,
                    pos[1].as_u64().unwrap() as usize,
                );
                let reg = (
                    state["reg"].as_str().unwrap().to_owned(),
                    state["regtype"] == "V",
                );
                if editor.contents() != expected
                    || editor.position() != expected_pos
                    || editor.registers.get('"') != Some(reg)
                {
                    failures.push(format!("scenario {index} {keys}: text {:?}/{expected:?}, cursor {:?}/{expected_pos:?}, reg {:?}/{:?}",
                        editor.contents(), editor.position(), editor.registers.get('"'), state["reg"]));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn visual_and_unicode_paragraphs_match_clean_neovim() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-paragraph-visual-unicode.json"
        ))
        .unwrap();
        assert_eq!(reference["version"], "0.12.1");
        assert_eq!(reference["virtualedit"], "block");
        let lines: Vec<_> = reference["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let cases = reference["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 224);
        let to_position = |value: &serde_json::Value| {
            let pair = value.as_array().unwrap();
            let row = pair[0].as_u64().unwrap() as usize - 1;
            let byte = pair[1].as_u64().unwrap() as usize;
            (row, lines[row][..byte].chars().count())
        };
        let mut failures = Vec::new();
        for case in cases {
            let mut editor = Editor::new(lines.join("\n"));
            editor
                .registers
                .capture('"', 'y', "__SENTINEL__".into(), false)
                .unwrap();
            editor.goto(case["row"].as_u64().unwrap() as usize - 1, 0);
            if case["origin"] == "l" {
                editor.key("l");
            }
            let mode = case["mode"].as_str().unwrap();
            match mode {
                "visual" => {
                    editor.key("v");
                }
                "line" => {
                    editor.key("V");
                }
                "block" => {
                    editor.key("<C-v>");
                }
                _ => (),
            }
            for key in case["motion"].as_str().unwrap().chars() {
                editor.key(&key.to_string());
            }
            let expected = to_position(&case["finish"]);
            let before = editor.position();
            let anchor_row = editor.source_line_at(editor.anchor);
            let anchor = (anchor_row, editor.anchor - editor.line_starts[anchor_row]);
            let expected_anchor = to_position(&case["anchor"]);
            let expected_mode = match mode {
                "visual" => Mode::Visual,
                "line" => Mode::VisualLine,
                "block" => Mode::VisualBlock,
                _ => Mode::Normal,
            };
            let actual_mode = editor.mode;
            if mode != "normal" {
                editor.key("y");
            }
            let yank_position = to_position(&case["yank_position"]);
            let kind = match case["regtype"].as_str().unwrap() {
                "v" => registers::Kind::Character,
                "V" => registers::Kind::Line,
                other => registers::Kind::Block(other[1..].parse().unwrap()),
            };
            let actual_reg = editor.registers.get_kind('"').unwrap();
            if before != expected
                || actual_mode != expected_mode
                || (mode != "normal" && anchor != expected_anchor)
                || editor.position() != yank_position
                || actual_reg != (case["reg"].as_str().unwrap().to_owned(), kind)
            {
                failures.push(format!("row {} origin {} {} {}: cursor {before:?}/{expected:?}, mode {actual_mode:?}/{expected_mode:?}, anchor {anchor:?}/{expected_anchor:?}, yank {:?}/{yank_position:?}, reg {actual_reg:?}/{:?} {kind:?}",
                    case["row"], case["origin"], mode, case["motion"], editor.position(), case["reg"]));
            }
        }
        assert!(
            failures.is_empty(),
            "{} mismatches:\n{}",
            failures.len(),
            failures
                .iter()
                .take(25)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn paragraphs_with_closed_folds_match_clean_neovim() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-paragraph-folds.json"
        ))
        .unwrap();
        let text = reference["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let mut mismatches = Vec::new();
        for case in reference["cases"].as_array().unwrap() {
            let mut editor = Editor::new(text.clone());
            editor
                .registers
                .capture('"', 'y', "__SENTINEL__".into(), false)
                .unwrap();
            let keys = case["keys"].as_str().unwrap();
            for key in keys.chars() {
                editor.key(&key.to_string());
            }
            let expected = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let finish = case["finish"].as_array().unwrap();
            let position = (
                finish[0].as_u64().unwrap() as usize - 1,
                finish[1].as_u64().unwrap() as usize,
            );
            let register = (
                case["reg"].as_str().unwrap().to_owned(),
                case["regtype"].as_str().unwrap() == "V",
            );
            if editor.contents() != expected
                || editor.position() != position
                || editor.registers.get('"') != Some(register)
            {
                mismatches.push(format!("{keys}: position {:?}/{position:?}, text {:?}/{expected:?}, register {:?}/{:?}",
                    editor.position(), editor.contents(), editor.registers.get('"'), case["reg"]));
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn normal_paragraph_motion_matches_clean_neovim_reference() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-paragraph-reference.json"
        ))
        .unwrap();
        assert_eq!(reference["version"], "0.12.1");
        assert_eq!(reference["paragraphs"], PARAGRAPHS);
        assert_eq!(reference["sections"], SECTIONS);
        let text = [
            "alpha one",
            "alpha two",
            "",
            "",
            "  ",
            "beta one",
            "",
            "gamma",
            "",
            ".IP term",
            "after",
            ".SH section",
            "tail",
            "\x0cpage",
            "end",
        ]
        .join("\n");
        let cases = reference["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 90);
        for case in cases {
            let start = case["start"].as_array().unwrap();
            let finish = case["finish"].as_array().unwrap();
            let mut editor = Editor::new(text.clone());
            editor.goto(
                start[0].as_u64().unwrap() as usize - 1,
                start[1].as_u64().unwrap() as usize,
            );
            let keys = case["key"].as_str().unwrap();
            for key in keys.chars() {
                editor.key(&key.to_string());
            }
            assert_eq!(
                editor.position(),
                (
                    finish[0].as_u64().unwrap() as usize - 1,
                    finish[1].as_u64().unwrap() as usize
                ),
                "start={start:?} keys={keys}"
            );
            assert_eq!(editor.contents(), text);
        }
    }

    #[test]
    fn paragraph_yank_delete_match_clean_neovim_reference() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-paragraph-operators.json"
        ))
        .unwrap();
        assert_eq!(reference["version"], "0.12.1");
        assert_eq!(reference["autoindent"], true);
        let lines = reference["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>();
        let cases = reference["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 112);
        let mut mismatches = Vec::new();
        for case in cases {
            let start = case["start"].as_array().unwrap();
            let finish = case["finish"].as_array().unwrap();
            let mut editor = Editor::new(lines.join("\n"));
            editor
                .registers
                .capture('"', 'y', "__SENTINEL__".into(), false)
                .unwrap();
            editor.goto(
                start[0].as_u64().unwrap() as usize - 1,
                start[1].as_u64().unwrap() as usize,
            );
            let keys = case["keys"].as_str().unwrap();
            for key in keys.chars() {
                editor.key(&key.to_string());
            }
            let expected_lines = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let position = (
                finish[0].as_u64().unwrap() as usize - 1,
                finish[1].as_u64().unwrap() as usize,
            );
            let linewise = case["regtype"].as_str().unwrap() == "V";
            let (register, register_linewise) = editor.registers.get('"').unwrap();
            if editor.contents() != expected_lines
                || editor.position() != position
                || register != case["reg"].as_str().unwrap()
                || register_linewise != linewise
            {
                mismatches.push(format!("start={start:?} keys={keys} pos={:?}/{position:?} reg={:?}/{:?} line={}/{linewise} text={:?}/{expected_lines:?}", editor.position(), register, case["reg"], register_linewise, editor.contents()));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} mismatches; examples: {:#?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(8)]
        );
    }

    #[test]
    fn paragraph_change_and_insert_match_clean_neovim_reference() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-paragraph-operators.json"
        ))
        .unwrap();
        let lines = reference["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>();
        let cases = reference["changes"].as_array().unwrap();
        assert_eq!(cases.len(), 56);
        let mut mismatches = Vec::new();
        for case in cases {
            let start = case["start"].as_array().unwrap();
            let finish = case["finish"].as_array().unwrap();
            let mut editor = Editor::new(lines.join("\n"));
            editor
                .registers
                .capture('"', 'y', "__SENTINEL__".into(), false)
                .unwrap();
            editor.goto(
                start[0].as_u64().unwrap() as usize - 1,
                start[1].as_u64().unwrap() as usize,
            );
            let keys = case["keys"].as_str().unwrap();
            for key in keys.chars() {
                editor.key(&key.to_string());
            }
            // :normal aborts the remaining keys when the motion fails.
            if editor.mode == Mode::Insert {
                editor.insert("X");
                editor.key("<Esc>");
            }
            let expected_lines = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let position = (
                finish[0].as_u64().unwrap() as usize - 1,
                finish[1].as_u64().unwrap() as usize,
            );
            let (register, register_linewise) = editor.registers.get('"').unwrap();
            let linewise = case["regtype"].as_str().unwrap() == "V";
            if editor.contents() != expected_lines
                || editor.position() != position
                || register != case["reg"].as_str().unwrap()
                || register_linewise != linewise
            {
                mismatches.push(format!("start={start:?} keys={keys} pos={:?}/{position:?} reg={:?}/{:?} line={}/{linewise} text={:?}/{expected_lines:?}", editor.position(), register, case["reg"], register_linewise, editor.contents()));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} mismatches; examples: {:#?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(8)]
        );
    }
}
