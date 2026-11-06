//! * / # and g* / g#: supplied buffer text only, using the shared search worker.
use super::*;
fn keyword(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}
fn escape_literal(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n")
}
impl Editor {
    fn word_search_error(&mut self, message: &str) -> Effect {
        self.cancel_replay();
        self.message = message.into();
        Effect::None
    }
    pub(super) fn search_word(&mut self, reverse: bool, count: usize, whole: bool) -> Effect {
        let visual = matches!(self.mode, Mode::Visual | Mode::VisualLine);
        let (query, origin, count) = if visual {
            if !whole {
                return self.word_search_error("g*/g# disponibles en NORMAL");
            }
            let Some(range) = self.selection() else {
                return Effect::None;
            };
            if range.len() > 4096 {
                return self.word_search_error("Selección de búsqueda sobre el presupuesto");
            }
            let mut text: String = self.text[range].iter().collect();
            if self.mode == Mode::VisualLine && text.ends_with('\n') {
                text.pop();
            }
            if text.is_empty() {
                self.mode = Mode::Normal;
                return self.word_search_error("Sin texto seleccionado para buscar");
            }
            let (row, column) = self.position();
            let anchor_row = self
                .line_starts
                .partition_point(|s| *s <= self.anchor)
                .saturating_sub(1);
            let extra = reverse
                && (row > anchor_row
                    || self.mode == Mode::Visual && self.cursor > self.anchor
                    || self.mode == Mode::VisualLine && column != 0);
            (
                format!("\\V{}", escape_literal(&text)),
                self.cursor,
                count.saturating_add(usize::from(extra)),
            )
        } else {
            let end = self.line_end(self.cursor);
            let cursor = self.cursor.min(end);
            let selected = self
                .text
                .get(cursor)
                .filter(|c| keyword(**c))
                .map(|_| cursor)
                .or_else(|| (cursor..end).find(|p| keyword(self.text[*p])))
                .or_else(|| {
                    self.text
                        .get(cursor)
                        .filter(|c| !c.is_whitespace() && **c != '\n')
                        .map(|_| cursor)
                })
                .or_else(|| (cursor..end).find(|p| !self.text[*p].is_whitespace()));
            let Some(selected) = selected else {
                return self.word_search_error("Sin palabra en esta línea");
            };
            let is_keyword = keyword(self.text[selected]);
            let same = |ch: char| !ch.is_whitespace() && keyword(ch) == is_keyword;
            let mut start = selected;
            while start > self.line_start(selected) && same(self.text[start - 1]) {
                start -= 1;
            }
            let mut finish = selected + 1;
            while finish < end && same(self.text[finish]) {
                finish += 1;
            }
            if finish - start > 4096 {
                return self.word_search_error("Palabra de búsqueda sobre el presupuesto");
            }
            let text: String = self.text[start..finish].iter().collect();
            let bounded = whole && is_keyword;
            let query = format!(
                "\\c{}\\V{}\\m{}",
                if bounded { "\\<" } else { "" },
                escape_literal(&text),
                if bounded { "\\>" } else { "" }
            );
            (query, start, count)
        };
        if query.len() > 4096 {
            return self.word_search_error("Patrón de búsqueda sobre el presupuesto");
        }
        self.cursor = origin;
        self.mode = Mode::Normal;
        self.search_reverse = reverse;
        self.search_count = count;
        self.submit_search(query)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sequence(e: &mut Editor, input: &str) {
        for ch in input.chars() {
            e.key(&ch.to_string());
            e.finish_search_test();
        }
    }
    #[test]
    fn nearest_keyword_counts_and_substrings_match_clean_neovim_0121() {
        for (text, input, cursor) in [
            ("Foo foo foobar FOO", "*", 4),
            ("Foo foo foobar FOO", "#", 15),
            ("Foo foo foobar FOO", "2*", 15),
            ("Foo foo foobar FOO", "l#", 15),
            ("Foo foo foobar FOO", "g*g*", 8),
            ("Foo foo foobar FOO", "2g*", 8),
            ("Foo foo foobar FOO", "*n", 15),
            ("Foo foo foobar FOO", "g#", 15),
            ("++ foo ++ foo", "*", 10),
            ("  ++ ++", "*", 5),
            ("++ ++", "*", 3),
            ("foo!! foo!!", "3l*", 0),
            ("ñ ññ ñ", "*", 5),
            ("foo_bar foo foobar foo_bar", "*", 19),
        ] {
            let mut e = Editor::new(text.into());
            sequence(&mut e, input);
            assert_eq!(e.cursor, cursor, "{text}: {input}");
            assert!(!e.dirty());
        }
        let mut e = Editor::new("   ".into());
        sequence(&mut e, "*");
        assert_eq!(e.cursor, 0);
        assert!(e.message.contains("Sin palabra"));
        sequence(&mut e, "qa*xq");
        e.key("u");
        sequence(&mut e, "0@a");
        assert_eq!(e.contents(), "   ");
        assert!(!e.replaying());
        assert!(!e.dirty());
    }
    #[test]
    fn visual_search_uses_literal_selection_and_smartcase_with_reverse_count() {
        for (text, input, cursor) in [
            ("Foo foo foobar FOO", "vll*", 0),
            ("Foo foo foobar FOO", "vll#", 0),
            ("foo foobar foo", "vll*", 4),
            ("foo foo foo", "vll#", 8),
        ] {
            let mut e = Editor::new(text.into());
            sequence(&mut e, input);
            assert_eq!(e.cursor, cursor, "{input}");
            assert_eq!(e.mode, Mode::Normal);
            assert!(!e.dirty());
            sequence(&mut e, "gv");
            assert_eq!(e.selection(), Some(0..3));
        }
        let mut e = Editor::new("a\\c a\\c".into());
        sequence(&mut e, "vll*");
        assert_eq!(e.cursor, 4);
    }
}
