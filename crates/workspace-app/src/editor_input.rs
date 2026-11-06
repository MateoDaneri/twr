//! Bounded line-local UTF-16 context for platform text input. Preedit is transient.
use super::*;

const CONTEXT_LIMIT: usize = 16_384;

#[derive(Clone)]
pub(super) struct Composition {
    version: u64,
    cursor: usize,
    source: Range<usize>,
    text: Vec<char>,
    selected: Range<usize>,
    marked: Option<Range<usize>>,
}

pub struct InputDisplay {
    pub source: Range<usize>,
    pub text: Vec<char>,
    pub selected: Range<usize>,
    pub marked: Option<Range<usize>>,
}

fn utf16_offset(text: &[char], offset: usize) -> usize {
    text[..offset].iter().map(|c| c.len_utf16()).sum()
}

fn char_offset(text: &[char], offset: usize) -> Option<usize> {
    let mut units = 0;
    for (i, ch) in text.iter().enumerate() {
        if units == offset {
            return Some(i);
        }
        units += ch.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(text.len())
}

fn from_utf16(text: &[char], range: Range<usize>) -> Result<Range<usize>, String> {
    if range.start > range.end {
        return Err("Rango de entrada invertido".into());
    }
    Ok(
        char_offset(text, range.start).ok_or("Inicio UTF-16 inválido")?
            ..char_offset(text, range.end).ok_or("Final UTF-16 inválido")?,
    )
}

impl Composition {
    fn replace(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        mark: bool,
    ) -> Result<(), String> {
        let range = if let Some(range) = range {
            from_utf16(&self.text, range)?
        } else {
            self.marked.clone().unwrap_or(self.selected.clone())
        };
        // Validate before mutating so a malformed platform request preserves preedit.
        if text.len() > CONTEXT_LIMIT * 4 || text.contains(['\r', '\n', '\0']) {
            return Err("Composición fuera del contexto de una línea".into());
        }
        let replacement = text.chars().collect::<Vec<_>>();
        if self.text.len() - range.len() + replacement.len() > CONTEXT_LIMIT {
            return Err("Composición supera16.384 caracteres".into());
        }
        let selected = selected
            .map(|range| from_utf16(&replacement, range))
            .transpose()?
            .unwrap_or(replacement.len()..replacement.len());
        self.selected = range.start + selected.start..range.start + selected.end;
        self.marked = (mark && !replacement.is_empty())
            .then_some(range.start..range.start + replacement.len());
        self.text.splice(range, replacement);
        Ok(())
    }
}

impl Editor {
    fn input_context(&self) -> Result<Composition, String> {
        if self.mode != Mode::Insert {
            return Err("Entrada de composición fuera de INSERT".into());
        }
        if let Some(input) = &self.input {
            if input.version != self.version || input.cursor != self.cursor {
                return Err("Composición obsoleta; buffer o cursor cambiado".into());
            }
            return Ok(input.clone());
        }
        let source = self.line_start(self.cursor)..self.line_end(self.cursor);
        if source.len() > CONTEXT_LIMIT {
            return Err("Línea demasiado grande para el contexto IME".into());
        }
        let caret = self.cursor - source.start;
        Ok(Composition {
            version: self.version,
            cursor: self.cursor,
            text: self.text[source.clone()].to_vec(),
            source,
            selected: caret..caret,
            marked: None,
        })
    }

    pub fn composing(&self) -> bool {
        self.input.as_ref().is_some_and(|input| {
            self.mode == Mode::Insert
                && input.version == self.version
                && input.cursor == self.cursor
        })
    }

    pub fn cancel_input(&mut self) {
        self.input = None;
    }

    pub fn input_selection(&self) -> Option<Range<usize>> {
        let input = self.input_context().ok()?;
        Some(
            utf16_offset(&input.text, input.selected.start)
                ..utf16_offset(&input.text, input.selected.end),
        )
    }

    pub fn input_marked(&self) -> Option<Range<usize>> {
        let input = self.input_context().ok()?;
        let marked = input.marked?;
        Some(utf16_offset(&input.text, marked.start)..utf16_offset(&input.text, marked.end))
    }

    pub fn input_text(&self, range: Range<usize>) -> Option<(String, Range<usize>)> {
        let input = self.input_context().ok()?;
        let total = utf16_offset(&input.text, input.text.len());
        if range.start > range.end || range.start > total {
            return None;
        }
        // Queries may cross a surrogate boundary; expand to complete Unicode scalars.
        let mut start = range.start;
        let mut end = range.end.min(total);
        while char_offset(&input.text, start).is_none() {
            start = start.checked_sub(1)?;
        }
        while char_offset(&input.text, end).is_none() {
            end += 1;
        }
        let chars = from_utf16(&input.text, start..end).ok()?;
        Some((input.text[chars].iter().collect(), start..end))
    }

    pub fn input_geometry(&self, range: Range<usize>) -> Option<(Vec<char>, Range<usize>)> {
        let input = self.input_context().ok()?;
        let chars = from_utf16(&input.text, range).ok()?;
        Some((input.text, chars))
    }

    pub fn mark_input(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) -> Result<(), String> {
        let mut input = self.input_context()?;
        input.replace(range, text, selected, true)?;
        self.cancel_replay();
        self.input = Some(input);
        Ok(())
    }

    fn apply_input(&mut self, input: Composition) -> Result<(), String> {
        let original = &self.text[input.source.clone()];
        if original == input.text {
            self.cancel_input();
            self.cursor = input.source.start + input.selected.end;
            return Ok(());
        }
        // Keep unchanged surrounding context out of macros and dot repeat. Expand
        // the edit only enough to include both the old and committed caret.
        let prefix = original
            .iter()
            .zip(&input.text)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = original[prefix..]
            .iter()
            .rev()
            .zip(input.text[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_caret = self.cursor - input.source.start;
        let start = prefix.min(old_caret).min(input.selected.end);
        let mut old_end = (original.len() - suffix).max(old_caret);
        let mut new_end = input.text.len() - (original.len() - old_end);
        if input.selected.end > new_end {
            old_end += input.selected.end - new_end;
            new_end = input.selected.end;
        }
        let text = input.text[start..new_end].iter().collect::<String>();
        self.replace_input(
            old_caret - start,
            old_end - old_caret,
            &text,
            input.selected.end - start,
        )
    }

    pub fn commit_input(&mut self, range: Option<Range<usize>>, text: &str) -> Result<(), String> {
        if self.input.is_none() && range.is_none() {
            self.insert(text);
            return Ok(());
        }
        let mut input = self.input_context()?;
        input.replace(range, text, None, false)?;
        self.cancel_replay();
        self.apply_input(input)
    }

    pub fn unmark_input(&mut self) -> Result<(), String> {
        if self.input.is_some() {
            self.apply_input(self.input_context()?)?;
        }
        Ok(())
    }

    pub fn input_display(&self) -> Option<InputDisplay> {
        if !self.composing() {
            return None;
        }
        let input = self.input_context().ok()?;
        Some(InputDisplay {
            source: input.source,
            text: input.text,
            selected: input.selected,
            marked: input.marked,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf16_composition_replaces_preedit_and_commits_once() {
        let mut e = Editor::new("header\na😀z\nend".into());
        e.goto(1, 2);
        e.key("i");
        assert_eq!(e.input_selection(), Some(3..3));
        assert_eq!(e.input_text(2..2), Some(("😀".into(), 1..3)));
        let version = e.version;
        e.mark_input(Some(1..3), "に", Some(1..1)).unwrap();
        assert_eq!(e.input_marked(), Some(1..2));
        assert_eq!(e.input_selection(), Some(2..2));
        e.mark_input(None, "日本😀", Some(2..4)).unwrap();
        assert_eq!(e.input_selection(), Some(3..5));
        assert_eq!(e.input_marked(), Some(1..5));
        assert_eq!(e.version, version);
        assert!(!e.dirty());
        assert_eq!(e.contents(), "header\na😀z\nend");
        e.commit_input(None, "日本語").unwrap();
        assert_eq!(e.contents(), "header\na日本語z\nend");
        assert_eq!(e.cursor, 11);
        assert_eq!(e.version, version + 1);
        assert!(!e.composing());
        e.key("<Esc>");
        e.key("u");
        assert_eq!(e.contents(), "header\na😀z\nend");
        e.key("<C-r>");
        assert_eq!(e.contents(), "header\na日本語z\nend");
    }

    #[test]
    fn cancelled_invalid_and_stale_composition_preserves_original() {
        let mut e = Editor::new("😀word".into());
        e.key("i");
        assert!(e.mark_input(Some(1..2), "x", None).is_err());
        e.mark_input(None, "候補", None).unwrap();
        assert!(e.mark_input(None, "😀", Some(1..1)).is_err());
        assert_eq!(e.input_text(0..2).unwrap().0, "候補");
        assert!(
            e.mark_input(None, &"x".repeat(CONTEXT_LIMIT + 1), None)
                .is_err()
        );
        assert!(e.mark_input(None, "x\ny", None).is_err());
        e.cancel_input();
        assert_eq!(e.contents(), "😀word");
        assert!(!e.dirty());
        e.mark_input(None, "x", None).unwrap();
        e.cursor = 1;
        assert!(e.commit_input(None, "wrong").is_err());
        assert_eq!(e.contents(), "😀word");
        e.key("<Esc>");
        assert!(!e.composing());
    }

    #[test]
    fn unmark_commits_and_replay_records_only_final_text_with_caret() {
        let mut e = Editor::new("z".into());
        e.key("q");
        e.key("a");
        e.key("i");
        e.mark_input(None, "仮", None).unwrap();
        e.mark_input(None, "日本", Some(1..1)).unwrap();
        e.unmark_input().unwrap();
        assert_eq!(e.contents(), "日本z");
        assert_eq!(e.cursor, 1);
        e.key("<Esc>");
        e.key("q");
        e.key("u");
        e.key("@");
        e.key("a");
        for _ in 0..10 {
            e.advance_replay();
        }
        assert_eq!(e.contents(), "日本z");
        assert!(!e.contents().contains('仮'));
        assert_eq!(e.cursor, 0);
        let mut other = Editor::with_registers("different".into(), e.registers.clone());
        other.key("@");
        other.key("a");
        for _ in 0..10 {
            other.advance_replay();
        }
        assert_eq!(other.contents(), "日本different");
        assert_eq!(other.cursor, 0);
        let mut huge = Editor::new("x".repeat(CONTEXT_LIMIT + 1));
        huge.key("i");
        assert_eq!(huge.input_selection(), None);
        huge.commit_input(None, "a").unwrap();
        assert!(huge.contents().starts_with('a'));
    }
}
