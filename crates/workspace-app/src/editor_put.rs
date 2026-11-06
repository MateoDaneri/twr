//! Bounded character/linewise put, including Visual replacement.
use super::*;
impl Editor {
    pub(super) fn put_register(&mut self, key: &str, count: usize) -> Effect {
        let name = self.effective_register();
        if matches!(name, '+' | '*') && self.clipboard_enabled && !self.clipboard_ready {
            self.clipboard_pending = Some((
                key.into(),
                count,
                name,
                self.version,
                self.cursor,
                self.mode,
            ));
            return Effect::ClipboardRead;
        }
        self.clipboard_ready = false;
        self.chosen_register = None;
        let Some((value, kind)) = self.registers.get_kind(name) else {
            return Effect::None;
        };
        if self.mode == Mode::VisualBlock {
            return self.put_over_block(&value, kind, count, key == "p");
        }
        if let registers::Kind::Block(width) = kind {
            return self.put_block_register(&value, width, count, key == "p");
        }
        let line = kind == registers::Kind::Line;
        if value.is_empty() {
            return Effect::None;
        }
        let selected = self.selection();
        if self.mode == Mode::Visual && selected.is_none() {
            self.message = "Selección Unicode fuera del límite; texto conservado".into();
            return Effect::None;
        }
        let selected_line = self.mode == Mode::VisualLine;
        let removed = selected.as_ref().map_or(0, |r| r.len());
        let count = count.clamp(1, 9999);
        if value.len().saturating_mul(count) > 8 * 1024 * 1024
            || self
                .text
                .len()
                .saturating_sub(removed)
                .saturating_add(value.chars().count().saturating_mul(count))
                .saturating_add(2)
                > 2_000_000
        {
            self.message = "Pegado excede el límite de edición".into();
            return Effect::None;
        }
        let mut text = value.repeat(count);
        let mut cursor_offset = 0;
        let range = if let Some(mut range) = selected.clone() {
            if line && !selected_line {
                if range.start > self.line_start(range.start) {
                    text.insert(0, '\n');
                    cursor_offset = 1;
                }
                if self.text.get(range.end) == Some(&'\n') {
                    range.end += 1;
                }
            } else if selected_line && !text.ends_with('\n') {
                text.push('\n');
            }
            range
        } else {
            let start = if line {
                if key == "p" {
                    let end = self.line_end(self.cursor);
                    if end == self.text.len() {
                        text = format!("\n{}", text.trim_end_matches('\n'));
                        cursor_offset = 1;
                        end
                    } else {
                        end + 1
                    }
                } else {
                    self.line_start(self.cursor)
                }
            } else if key == "p" {
                let Some(after) = self.grapheme_move(self.cursor, 1, true, true) else {
                    self.message = "Pegado Unicode fuera del límite; texto conservado".into();
                    return Effect::None;
                };
                after
            } else {
                self.cursor
            };
            start..start
        };
        if selected.is_some() && key == "p" {
            self.register = self.text[range.clone()].iter().collect();
            self.line_register = selected_line || (line && self.register.ends_with('\n'));
            self.capture_register('d');
        }
        self.checkpoint();
        let length = text.chars().count();
        self.track_visual_edit(
            range.clone(),
            text.chars().filter(|c| *c == '\n').count(),
            selected.is_none() && line && key == "P",
        );
        self.text.splice(range.clone(), text.chars());
        self.mode = Mode::Normal;
        self.cursor = range.start
            + if line || selected_line {
                cursor_offset
            } else {
                length.saturating_sub(1)
            };
        self.changed();
        if !line && !selected_line {
            self.cursor = self.grapheme_floor(self.cursor).unwrap_or(self.cursor);
        }
        if selected.is_some() {
            self.remember_visual_put(
                if selected_line {
                    Mode::VisualLine
                } else {
                    Mode::Visual
                },
                range.start + cursor_offset,
                range.start + length.saturating_sub(1 + usize::from(text.ends_with('\n'))),
            );
        }
        if line || selected_line {
            while self.cursor < self.line_end(self.cursor) && self.text[self.cursor].is_whitespace()
            {
                self.cursor += 1;
            }
            self.clamp();
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counted_put_visual_replace_linewise_and_clipboard_ack_are_single_undo_changes() {
        let mut e = Editor::new("ab".into()).with_clipboard();
        e.key("3");
        assert_eq!(e.key("p"), Effect::ClipboardRead);
        e.provide_clipboard(Ok(("ñ".into(), false)));
        assert_eq!(e.contents(), "añññb");
        assert_eq!(e.position(), (0, 3));
        e.key("u");
        assert_eq!(e.contents(), "ab");
        for (line, selection, key, expected, removed) in [
            (false, "vll", "p", "abXfghij\nnext", "cde"),
            (false, "vll", "P", "abXfghij\nnext", "OLD"),
            (true, "vll", "p", "ab\none\nfghij\nnext", "cde"),
            (true, "v$", "p", "ab\none\nnext", "cdefghij\n"),
            (false, "V", "p", "X\nnext", "abcdefghij\n"),
        ] {
            let mut e = Editor::new("abcdefghij\nnext".into());
            e.registers
                .capture('a', 'y', if line { "one\n" } else { "X" }.into(), line)
                .unwrap();
            e.registers.capture('"', 'y', "OLD".into(), false).unwrap();
            e.goto(0, 2);
            for c in selection.chars() {
                e.key(&c.to_string());
            }
            e.key("\"");
            e.key("a");
            e.key(key);
            assert_eq!(e.contents(), expected);
            assert_eq!(e.mode, Mode::Normal);
            assert_eq!(e.registers.get('"').unwrap().0, removed);
            e.key("u");
            assert_eq!(e.contents(), "abcdefghij\nnext");
        }
    }
}
