//! Insert-mode placeholder editing. Replay stores edits and relative cursor moves.
use super::*;
pub(super) struct Active {
    start: usize,
    session: crate::snippets::Session,
}
impl Editor {
    pub fn snippet_active(&self) -> bool {
        self.snippet.is_some()
    }
    pub fn apply_lsp_snippet(
        &mut self,
        range: &serde_json::Value,
        source: &str,
        version: u64,
    ) -> Result<(), String> {
        if self.version != version || self.mode != Mode::Insert {
            return Err("Snippet obsoleto o fuera de INSERT".into());
        }
        let start = crate::lsp::offset(&self.text, &range["start"])
            .ok_or("Posición de snippet inválida")?;
        let end =
            crate::lsp::offset(&self.text, &range["end"]).ok_or("Posición de snippet inválida")?;
        if start > self.cursor || end < self.cursor || start > end {
            return Err("Snippet fuera del cursor".into());
        }
        let session = crate::snippets::Template::parse(source)?.session()?;
        let previous = self.snippet.take();
        if let Err(error) = self.replace_input(
            self.cursor - start,
            end - self.cursor,
            &session.text,
            session.cursor(),
        ) {
            self.snippet = previous;
            return Err(error);
        }
        if session.range().is_some() {
            self.snippet = Some(Active { start, session });
        }
        self.message = "Snippet · Tab siguiente · Shift+Tab anterior · Esc sale".into();
        Ok(())
    }
    pub fn snippet_selection(&self) -> Option<Range<usize>> {
        let active = self.snippet.as_ref()?;
        if self.mode != Mode::Insert || !active.session.selected {
            return None;
        }
        let range = active.session.range()?;
        Some(active.start + range.start..active.start + range.end)
    }
    pub(super) fn snippet_input(
        &mut self,
        left: usize,
        right: usize,
        text: &str,
        caret: usize,
    ) -> Option<Result<(), String>> {
        let mut active = self.snippet.take()?;
        let end = active.start + active.session.text.chars().count();
        if self.mode != Mode::Insert
            || self.cursor < active.start
            || end > self.text.len()
            || !self.text[active.start..end]
                .iter()
                .copied()
                .eq(active.session.text.chars())
        {
            return None;
        }
        let offset = self.cursor - active.start;
        let range = active.session.range()?;
        if offset < range.start
            || offset > range.end
            || left > offset - range.start
            || right > range.end - offset
        {
            return None; // Explicit edits outside the field end the session.
        }
        let (next, cursor) = match active.session.replace(offset, left, right, text, caret) {
            Ok(result) => result,
            Err(error) => {
                self.snippet = Some(active);
                return Some(Err(error));
            }
        };
        let result = self.replace_input(
            self.cursor - active.start,
            end - self.cursor,
            &next.text,
            cursor,
        );
        if result.is_ok() {
            active.session = next;
        }
        self.snippet = Some(active);
        Some(result)
    }
    pub(super) fn snippet_key(&mut self, key: &str) -> bool {
        if self.snippet.is_none() {
            return false;
        }
        if self.mode != Mode::Insert
            || !matches!(key, "<Tab>" | "<S-Tab>" | "<BS>" | "<Delete>" | "<Enter>")
        {
            self.snippet = None;
            return false;
        }
        if matches!(key, "<Tab>" | "<S-Tab>") {
            let mut active = self.snippet.take().unwrap();
            let field = active.session.jump(key == "<S-Tab>");
            let cursor = active.start
                + field
                    .as_ref()
                    .map_or_else(|| active.session.final_cursor(), |r| r.start);
            self.record(EditEvent::Cursor(cursor as isize - self.cursor as isize));
            self.cursor = cursor;
            self.count = 0;
            if field.is_some() {
                self.snippet = Some(active);
            }
            return true;
        }
        let selected = self.snippet.as_ref().unwrap().session.selected;
        let (left, right, text) = match key {
            "<BS>" => (usize::from(!selected), 0, ""),
            "<Delete>" => (0, usize::from(!selected), ""),
            _ => (0, 0, "\n"),
        };
        match self.snippet_input(left, right, text, text.chars().count()) {
            Some(Ok(())) => true,
            Some(Err(error)) => {
                self.message = error;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expand(e: &mut Editor, source: &str) {
        let range =
            serde_json::json!({"start":e.lsp_position(e.cursor),"end":e.lsp_position(e.cursor)});
        e.apply_lsp_snippet(&range, source, e.version).unwrap();
    }
    #[test]
    fn insert_fields_mirror_unicode_and_jump_without_buffer_version_changes() {
        let mut e = Editor::new(String::new());
        e.key("i");
        expand(&mut e, "fn ${1:name}(${2:arg}, $2) { $0 }");
        assert_eq!(e.snippet_selection(), Some(3..7));
        e.insert("sum");
        e.key("<Tab>");
        assert_eq!(e.snippet_selection(), Some(7..10));
        let version = e.version;
        e.key("<S-Tab>");
        assert_eq!(e.version, version);
        e.key("<Tab>");
        e.paste("ñ😀");
        assert_eq!(e.contents(), "fn sum(ñ😀, ñ😀) {  }");
        e.key("<BS>");
        assert_eq!(e.contents(), "fn sum(ñ, ñ) {  }");
        e.key("<Tab>");
        assert!(e.snippet.is_none());
        e.insert("result");
        e.key("<Esc>");
        assert_eq!(e.contents(), "fn sum(ñ, ñ) { result }");
        e.key("u");
        assert_eq!(e.contents(), "");
        e.key("<C-r>");
        assert_eq!(e.contents(), "fn sum(ñ, ñ) { result }");
    }
    #[test]
    fn invalid_or_stale_snippets_preserve_input_and_composition_commits_mirrors_once() {
        let mut e = Editor::new("start".into());
        e.key("A");
        let range =
            serde_json::json!({"start":e.lsp_position(e.cursor),"end":e.lsp_position(e.cursor)});
        assert!(e.apply_lsp_snippet(&range, "${BAD}", e.version).is_err());
        assert_eq!(e.contents(), "start");
        assert!(e.apply_lsp_snippet(&range, "$1", e.version + 1).is_err());
        expand(&mut e, "${1:default}-$1$0");
        let version = e.version;
        e.mark_input(None, "é", Some(1..1)).unwrap();
        assert_eq!(e.version, version);
        assert_eq!(e.contents(), "startdefault-default");
        e.commit_input(None, "é").unwrap();
        assert_eq!(e.contents(), "starté-é");
        assert_eq!(e.version, version + 1);
        e.key("<Left>");
        assert!(e.snippet.is_none());
        e.key("<Esc>");
        e.key("u");
        assert_eq!(e.contents(), "start");
    }
    #[test]
    fn placeholder_delete_bounds_and_external_edits_end_sessions_safely() {
        let mut e = Editor::new(String::new());
        e.key("i");
        expand(&mut e, "${1:default}-$1-${2:second}$0");
        e.key("<Delete>");
        assert_eq!(e.contents(), "--second");
        let version = e.version;
        e.paste(&"x".repeat(65537));
        assert_eq!(e.contents(), "--second");
        assert_eq!(e.version, version);
        assert!(e.snippet_active());
        e.key("<Tab>");
        assert_eq!(e.snippet_selection(), Some(2..8));
        e.paste("row\nñ");
        assert_eq!(e.contents(), "--row\nñ");
        e.replace_formatted("other".into());
        assert!(!e.snippet_active());
        let mut e = Editor::new(String::new());
        e.key("i");
        expand(&mut e, "${1:x}-$1$0");
        e.paste("Z");
        e.key("<Right>");
        assert!(!e.snippet_active());
        e.paste("raw");
        assert_eq!(e.contents(), "Z-rawZ");
    }
    #[test]
    fn snippet_edits_and_cursor_jumps_replay_in_macro_without_active_session() {
        let mut e = Editor::new(String::new());
        e.key("q");
        e.key("a");
        e.key("i");
        expand(&mut e, "${1:one}-${2:two}-$1$0");
        e.paste("ñ");
        e.key("<Tab>");
        e.insert("X");
        e.key("<Tab>");
        e.key("<Esc>");
        e.key("q");
        let expected = e.contents();
        assert_eq!(expected, "ñ-X-ñ");
        e.key("u");
        assert_eq!(e.contents(), "");
        e.key("@");
        e.key("a");
        assert_eq!(e.contents(), expected);
        e.key("u");
        assert_eq!(e.contents(), "");
    }
}
