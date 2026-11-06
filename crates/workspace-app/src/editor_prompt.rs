//! Editing at the command/search prompt's current end-of-line caret.
use super::{Editor, Effect, Mode};
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct HistoryNavigation {
    original: String,
    entries: Vec<String>,
    index: usize,
}

impl Editor {
    /// Handle the pending register name before other prompt key bindings.
    pub(super) fn prompt_register_key(&mut self, key: &str) -> Option<Effect> {
        if !matches!(self.mode, Mode::Command | Mode::Search) {
            return None;
        }
        if self.prompt_register_pending {
            return match key {
                "<Esc>" => {
                    self.prompt_register_pending = false;
                    Some(Effect::None)
                }
                "<C-c>" => {
                    self.prompt_register_pending = false;
                    None
                }
                "<C-r>" => Some(Effect::None),
                "<C-w>" if self.mode == Mode::Search => {
                    self.prompt_register_pending = false;
                    Some(self.expand_search_object(true))
                }
                text if !text.starts_with('<') => {
                    self.consume_prompt_register_text(text);
                    Some(Effect::None)
                }
                _ => {
                    self.prompt_register_pending = false;
                    Some(Effect::None)
                }
            };
        }
        if key == "<C-r>" {
            self.prompt_register_pending = true;
            return Some(Effect::None);
        }
        None
    }

    /// The input method may deliver the register name as text rather than a key.
    pub(super) fn consume_prompt_register_text(&mut self, text: &str) -> bool {
        if !self.prompt_register_pending || !matches!(self.mode, Mode::Command | Mode::Search) {
            return false;
        }
        self.prompt_register_pending = false;
        let mut chars = text.chars();
        let Some(name) = chars.next() else {
            return true;
        };
        if chars.next().is_some()
            || !(name.is_ascii_alphanumeric() || matches!(name, '"' | '+' | '*'))
        {
            self.message = "Registros: nombre no disponible en esta línea".into();
            return true;
        }
        if matches!(name, '+' | '*') {
            if !self.clipboard_enabled {
                self.message = "Registros del clipboard no disponibles".into();
                return true;
            }
            self.prompt_clipboard_pending = Some((
                self.command.clone(),
                self.prompt_cursor,
                self.mode,
                self.version,
                self.cursor,
            ));
            return true;
        }
        let Some((value, kind)) = self.registers.get_kind(name) else {
            self.message = "Registros vacíos o no disponibles".into();
            return true;
        };
        let value = if kind == super::registers::Kind::Line {
            value.strip_suffix('\n').unwrap_or(&value)
        } else {
            &value
        };
        self.insert_prompt_register_value(value);
        true
    }

    pub(super) fn insert_prompt_register_value(&mut self, value: &str) -> Effect {
        if value.len() > 4096
            || value.chars().any(char::is_control)
            || self
                .command
                .chars()
                .count()
                .saturating_add(value.chars().count())
                >= 1024
        {
            self.message =
                "Registros multilínea o demasiado largos no disponibles en esta línea".into();
            return Effect::None;
        }
        self.prompt_history = None;
        self.insert_prompt_text(value);
        if self.mode == Mode::Search {
            self.refresh_search_preview(None)
        } else {
            Effect::None
        }
    }

    pub(super) fn prompt_history_key(&mut self, key: &str) -> Option<Effect> {
        if !matches!(self.mode, Mode::Command | Mode::Search) {
            return None;
        }
        let (older, filtered) = match key {
            "<Up>" => (true, true),
            "<Down>" => (false, true),
            "<S-Up>" | "<PageUp>" => (true, false),
            "<S-Down>" | "<PageDown>" => (false, false),
            _ => return None,
        };
        let navigation = self.prompt_history.get_or_insert_with(|| {
            let entries = self.registers.prompt_history(self.mode == Mode::Search);
            let index = entries.len();
            HistoryNavigation {
                original: self.command[..self.prompt_cursor].to_owned(),
                entries,
                index,
            }
        });
        let index = if older {
            (0..navigation.index)
                .rev()
                .find(|&i| !filtered || navigation.entries[i].starts_with(&navigation.original))
        } else {
            (navigation.index.saturating_add(1)..navigation.entries.len())
                .find(|&i| !filtered || navigation.entries[i].starts_with(&navigation.original))
                .or_else(|| {
                    (navigation.index < navigation.entries.len())
                        .then_some(navigation.entries.len())
                })
        };
        if let Some(index) = index {
            navigation.index = index;
            self.command = navigation
                .entries
                .get(index)
                .cloned()
                .unwrap_or_else(|| navigation.original.clone());
            self.prompt_cursor = self.command.len();
            if self.mode == Mode::Search {
                return Some(self.refresh_search_preview(None));
            }
        }
        Some(Effect::None)
    }

    /// Neovim Insert Ctrl-W: delete trailing whitespace then one small word.
    /// At the start of a line, delete only the preceding line break.
    pub(super) fn delete_insert_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.cursor;
        let line_start = self.line_start(end);
        let start = if end == line_start {
            end - 1
        } else {
            let prefix: String = self.text[line_start..end].iter().collect();
            let mut items = prefix.grapheme_indices(true).rev().peekable();
            let mut start = prefix.len();
            while let Some(&(offset, item)) = items.peek() {
                if !item.chars().all(char::is_whitespace) {
                    break;
                }
                start = offset;
                items.next();
            }
            if let Some((offset, item)) = items.next() {
                start = offset;
                let keyword = |s: &str| {
                    s.chars()
                        .next()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
                };
                let class = keyword(item);
                for (offset, item) in items {
                    if item.chars().all(char::is_whitespace) || keyword(item) != class {
                        break;
                    }
                    start = offset;
                }
            }
            line_start + prefix[..start].chars().count()
        };
        self.automatic_indent = None;
        self.track_visual_edit(start..end, 0, false);
        self.text.drain(start..end);
        self.cursor = start;
        self.changed();
    }
    /// Transfer only a standalone Normal/Visual count to a workspace window command.
    pub fn take_window_count(&mut self) -> usize {
        if self.mode.window_navigation()
            && self.pending.is_empty()
            && self.awaiting.is_none()
            && self.jump.is_none()
        {
            std::mem::take(&mut self.count)
        } else {
            0
        }
    }
    /// A Visual selection survives a failed window movement, but ends on a
    /// successful change to another buffer. Keep its area for gv.
    pub fn finish_window_navigation(&mut self, changed: bool) {
        if changed
            && matches!(
                self.mode,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            )
        {
            self.key("<Esc>");
        }
    }
    /// Translate modified arrow events only while a command/search prompt owns input.
    pub fn prompt_word_arrow(key: &str, control: bool, shift: bool) -> Option<&'static str> {
        match (key, control, shift) {
            ("left", true, false) => Some("<C-Left>"),
            ("right", true, false) => Some("<C-Right>"),
            ("left", false, true) => Some("<S-Left>"),
            ("right", false, true) => Some("<S-Right>"),
            _ => None,
        }
    }

    /// Byte offset of the prompt caret, always on a UTF-8 boundary.
    pub fn prompt_cursor(&self) -> usize {
        self.prompt_cursor
    }

    /// Text on either side of the caret and the grapheme drawn under it.
    pub fn prompt_parts(&self) -> (&str, &str, &str) {
        let (before, rest) = self.command.split_at(self.prompt_cursor);
        let len = rest.graphemes(true).next().map_or(0, str::len);
        (before, &rest[..len], &rest[len..])
    }

    pub(super) fn insert_prompt_text(&mut self, text: &str) {
        self.command.insert_str(self.prompt_cursor, text);
        self.prompt_cursor += text.len();
    }

    pub(super) fn prompt_edit_key(&mut self, key: &str) -> Option<Effect> {
        if !matches!(self.mode, Mode::Command | Mode::Search) {
            return None;
        }
        let before = &self.command[..self.prompt_cursor];
        let after = &self.command[self.prompt_cursor..];
        let mut changed = false;
        match key {
            "<Left>" => {
                self.prompt_cursor = before
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i);
            }
            "<Right>" => {
                self.prompt_cursor += after.graphemes(true).next().map_or(0, str::len);
            }
            "<S-Left>" | "<C-Left>" => {
                if let Some((mut next, _)) = before.grapheme_indices(true).next_back() {
                    while let Some((index, grapheme)) =
                        self.command[..next].grapheme_indices(true).next_back()
                    {
                        if matches!(grapheme, " " | "\t") {
                            break;
                        }
                        next = index;
                    }
                    self.prompt_cursor = next;
                }
            }
            "<S-Right>" | "<C-Right>" => {
                if let Some(grapheme) = after.graphemes(true).next() {
                    let mut next = self.prompt_cursor + grapheme.len();
                    while let Some(grapheme) = self.command[next..].graphemes(true).next() {
                        if matches!(grapheme, " " | "\t") {
                            break;
                        }
                        next += grapheme.len();
                    }
                    self.prompt_cursor = next;
                }
            }
            "<Home>" | "<C-b>" => self.prompt_cursor = 0,
            "<End>" | "<C-e>" => self.prompt_cursor = self.command.len(),
            "<BS>" | "<C-h>" => {
                if self.command.is_empty() {
                    return Some(self.key_base("<Esc>"));
                }
                if self.prompt_cursor > 0 {
                    let start = before.grapheme_indices(true).next_back().unwrap().0;
                    self.command.drain(start..self.prompt_cursor);
                    self.prompt_cursor = start;
                    changed = true;
                }
            }
            "<Delete>" => {
                if let Some(grapheme) = after.graphemes(true).next() {
                    let end = self.prompt_cursor + grapheme.len();
                    self.command.drain(self.prompt_cursor..end);
                    changed = true;
                }
            }
            "<C-w>" => {
                let mut items = before.grapheme_indices(true).rev().peekable();
                let mut start = self.prompt_cursor;
                while let Some(&(offset, item)) = items.peek() {
                    if !item.chars().all(char::is_whitespace) {
                        break;
                    }
                    start = offset;
                    items.next();
                }
                if let Some((offset, item)) = items.next() {
                    start = offset;
                    let keyword = |s: &str| {
                        s.chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    };
                    let class = keyword(item);
                    for (offset, item) in items {
                        if item.chars().all(char::is_whitespace) || keyword(item) != class {
                            break;
                        }
                        start = offset;
                    }
                }
                if start < self.prompt_cursor {
                    self.command.drain(start..self.prompt_cursor);
                    self.prompt_cursor = start;
                    changed = true;
                }
            }
            "<C-u>" => {
                if self.prompt_cursor > 0 {
                    self.command.drain(..self.prompt_cursor);
                    self.prompt_cursor = 0;
                    changed = true;
                }
            }
            _ => return None,
        }
        if changed {
            self.prompt_history = None;
        }
        Some(if changed && self.mode == Mode::Search {
            self.refresh_search_preview(None)
        } else {
            Effect::None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(e: &mut Editor, chord: &str) {
        e.key(chord);
        e.finish_search_test();
    }
    #[test]
    fn prompt_register_sequences_match_isolated_neovim_reference() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-prompt-register.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let bank = super::super::RegisterBank::default();
            bank.capture('a', 'y', "alpha beta".into(), false).unwrap();
            bank.capture('c', 'y', "only\n".into(), true).unwrap();
            bank.capture('"', 'y', "unnamed".into(), false).unwrap();
            let mut e = Editor::with_registers("alpha beta".into(), bank).with_clipboard();
            let prefix = case["prefix"].as_str().unwrap();
            key(&mut e, &prefix[..1]);
            e.insert(&prefix[1..]);
            e.finish_search_test();
            let expected = case["observations"].as_array().unwrap();
            let check = |e: &Editor, index: usize| {
                let mode = if matches!(e.mode, Mode::Command | Mode::Search) {
                    "c"
                } else {
                    "n"
                };
                let pos = if mode == "c" { e.prompt_cursor + 1 } else { 0 };
                assert_eq!(
                    e.command,
                    expected[index]["line"].as_str().unwrap(),
                    "{} step {}",
                    case["label"],
                    index
                );
                assert_eq!(
                    mode,
                    expected[index]["mode"].as_str().unwrap(),
                    "{} step {}",
                    case["label"],
                    index
                );
                assert_eq!(
                    pos,
                    expected[index]["pos"].as_u64().unwrap() as usize,
                    "{} step {}",
                    case["label"],
                    index
                );
            };
            check(&e, 0);
            for (index, operation) in case["operations"].as_array().unwrap().iter().enumerate() {
                for chord in operation.as_array().unwrap() {
                    key(&mut e, chord.as_str().unwrap());
                    if e.needs_clipboard() {
                        e.provide_clipboard(Ok(("synthetic-clipboard".into(), false)));
                        e.finish_search_test();
                    }
                }
                check(&e, index + 1);
            }
        }
    }
    #[test]
    fn command_register_insertion_uses_caret_and_keeps_prompt_on_escape() {
        let bank = super::super::RegisterBank::default();
        bank.capture('a', 'y', "alpha beta".into(), false).unwrap();
        bank.capture('"', 'y', "unnamed".into(), false).unwrap();
        let mut e = Editor::with_registers(String::new(), bank);
        key(&mut e, ":");
        e.insert("ab");
        key(&mut e, "<Left>");
        key(&mut e, "<C-r>");
        e.insert("a");
        assert_eq!(e.command, "aalpha betab");
        assert_eq!(e.prompt_cursor, 11);
        key(&mut e, "<C-r>");
        key(&mut e, "<Esc>");
        assert_eq!(e.mode, Mode::Command);
        assert_eq!(e.command, "aalpha betab");
        key(&mut e, "<Esc>");
        key(&mut e, ":");
        key(&mut e, "<C-r>");
        key(&mut e, "\"");
        assert_eq!(e.command, "unnamed");
    }
    #[test]
    fn search_register_insertion_and_unsupported_values_are_safe() {
        let bank = super::super::RegisterBank::default();
        bank.capture('a', 'y', "alpha".into(), false).unwrap();
        bank.capture('b', 'y', "first\nsecond\n".into(), true)
            .unwrap();
        let mut e = Editor::with_registers("alpha beta".into(), bank);
        key(&mut e, "/");
        key(&mut e, "<C-r>");
        key(&mut e, "a");
        assert_eq!(e.command, "alpha");
        key(&mut e, "<C-r>");
        e.insert("b");
        assert_eq!(e.command, "alpha");
        assert!(e.message.contains("multilínea"));
        key(&mut e, "<C-r>");
        e.insert("+");
        assert_eq!(e.command, "alpha");
        assert!(e.message.contains("Registros"));
        key(&mut e, "<C-r>");
        key(&mut e, "<C-c>");
        assert_eq!(e.mode, Mode::Normal);
    }
    #[test]
    fn history_sequences_match_isolated_neovim_reference() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-prompt-history.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let bank = super::super::RegisterBank::default();
            for entry in fixture["setup"].as_array().unwrap() {
                bank.record_prompt_history(
                    entry[0].as_str().unwrap() == "search",
                    entry[1].as_str().unwrap(),
                );
            }
            let mut e = Editor::with_registers("alpha beta".into(), bank);
            let prefix = case["prefix"].as_str().unwrap();
            key(&mut e, &prefix[..1]);
            e.insert(&prefix[1..]);
            e.finish_search_test();
            let expected = case["observations"].as_array().unwrap();
            let check = |e: &Editor, index: usize| {
                assert_eq!(
                    e.command,
                    expected[index]["line"].as_str().unwrap(),
                    "{} step {}",
                    case["label"],
                    index
                );
                assert_eq!(
                    e.prompt_cursor + 1,
                    expected[index]["pos"].as_u64().unwrap() as usize,
                    "{} step {}",
                    case["label"],
                    index
                );
                assert!(matches!(e.mode, Mode::Command | Mode::Search));
            };
            check(&e, 0);
            for (index, chord) in case["keys"].as_array().unwrap().iter().enumerate() {
                key(&mut e, chord.as_str().unwrap());
                check(&e, index + 1);
            }
        }
    }
    #[test]
    fn command_history_matches_neovim_prefix_and_unfiltered_arrows_across_editors() {
        let bank = super::super::RegisterBank::default();
        let mut a = Editor::with_registers(String::new(), bank.clone());
        for entry in ["echo one", "edit alpha", "echo two"] {
            key(&mut a, ":");
            a.insert(entry);
            key(&mut a, "<Esc>");
        }
        let mut b = Editor::with_registers(String::new(), bank);
        key(&mut b, ":");
        b.insert("echo");
        for (chord, expected) in [
            ("<Up>", "echo two"),
            ("<Up>", "echo one"),
            ("<Up>", "echo one"),
            ("<Down>", "echo two"),
            ("<Down>", "echo"),
        ] {
            key(&mut b, chord);
            assert_eq!(b.command, expected);
            assert_eq!(b.prompt_cursor, expected.len());
        }
        key(&mut b, "<S-Up>");
        assert_eq!(b.command, "echo two");
        key(&mut b, "<S-Up>");
        assert_eq!(b.command, "edit alpha");
    }
    #[test]
    fn command_and_search_histories_are_separate_and_share_search_directions() {
        let bank = super::super::RegisterBank::default();
        let mut e = Editor::with_registers("alpha beta".into(), bank.clone());
        for (prefix, value) in [(":", "echo one"), ("/", "alpha"), ("/", "beta")] {
            key(&mut e, prefix);
            e.insert(value);
            key(&mut e, "<Esc>");
        }
        let mut other = Editor::with_registers("alpha beta".into(), bank);
        key(&mut other, "?");
        key(&mut other, "<Up>");
        assert_eq!(other.command, "beta");
        key(&mut other, "<Up>");
        assert_eq!(other.command, "alpha");
        key(&mut other, "<Esc>");
        key(&mut other, ":");
        key(&mut other, "<Up>");
        assert_eq!(other.command, "echo one");
    }
    #[test]
    fn control_c_discards_prompt_without_recording_it() {
        let mut editor = Editor::new(String::new());
        key(&mut editor, ":");
        editor.insert("discarded");
        key(&mut editor, "<C-c>");
        assert_eq!(editor.mode, Mode::Normal);
        key(&mut editor, ":");
        key(&mut editor, "<Up>");
        assert_eq!(editor.command, "");
    }
    #[test]
    fn prompt_caret_sequences_match_isolated_neovim_reference() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-prompt-caret.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let mut e = Editor::new("synthetic buffer".into());
            key(&mut e, case["prefix"].as_str().unwrap());
            e.insert(case["query"].as_str().unwrap());
            e.finish_search_test();
            let observations = case["observations"].as_array().unwrap();
            let check = |editor: &Editor, index: usize| {
                let expected = &observations[index];
                let mode = if matches!(editor.mode, Mode::Command | Mode::Search) {
                    "c"
                } else {
                    "n"
                };
                let position = if mode == "c" {
                    editor.prompt_cursor() + 1
                } else {
                    0
                };
                assert_eq!(
                    editor.command,
                    expected["line"].as_str().unwrap(),
                    "{} step {index}",
                    case["label"]
                );
                assert_eq!(
                    position,
                    expected["pos"].as_u64().unwrap() as usize,
                    "{} step {index}",
                    case["label"]
                );
                assert_eq!(
                    mode,
                    expected["mode"].as_str().unwrap(),
                    "{} step {index}",
                    case["label"]
                );
                assert_eq!(editor.version, 0);
                assert!(!editor.dirty());
            };
            check(&e, 0);
            for (index, step) in case["keys"].as_array().unwrap().iter().enumerate() {
                let step = step.as_str().unwrap();
                if step.starts_with('<') {
                    // Neovim calls this <Del>; the platform adapter emits <Delete>.
                    key(&mut e, if step == "<Del>" { "<Delete>" } else { step });
                } else {
                    e.insert(step);
                    e.finish_search_test();
                }
                check(&e, index + 1);
            }
        }
    }

    #[test]
    fn prompt_caret_edits_match_clean_neovim_ascii_and_unicode() {
        for prefix in [":", "/", "?"] {
            let mut e = Editor::new("synthetic text".into());
            key(&mut e, prefix);
            e.insert("á界z");
            e.finish_search_test();
            assert_eq!(e.prompt_cursor(), 6);
            key(&mut e, "<Left>");
            assert_eq!(e.prompt_cursor(), 5);
            key(&mut e, "<Left>");
            assert_eq!(e.prompt_cursor(), 2);
            e.insert("X");
            e.finish_search_test();
            assert_eq!((e.command.as_str(), e.prompt_cursor()), ("áX界z", 3));
            assert_eq!(e.prompt_parts(), ("áX", "界", "z"));
            key(&mut e, "<BS>");
            assert_eq!((e.command.as_str(), e.prompt_cursor()), ("á界z", 2));
            key(&mut e, "<Home>");
            key(&mut e, "<BS>");
            assert_eq!((e.command.as_str(), e.prompt_cursor()), ("á界z", 0));
            key(&mut e, "<Right>");
            key(&mut e, "<C-u>");
            assert_eq!((e.command.as_str(), e.prompt_cursor()), ("界z", 0));
            key(&mut e, "<End>");
            assert_eq!(e.prompt_parts(), ("界z", "", ""));
            key(&mut e, "<C-b>");
            assert_eq!(e.prompt_cursor(), 0);
            key(&mut e, "<C-e>");
            assert_eq!(e.prompt_cursor(), 4);
            assert_eq!(e.version, 0);
            assert!(!e.dirty());
        }
    }

    #[test]
    fn modified_prompt_arrows_keep_key_ownership_explicit() {
        assert_eq!(
            Editor::prompt_word_arrow("left", true, false),
            Some("<C-Left>")
        );
        assert_eq!(
            Editor::prompt_word_arrow("right", false, true),
            Some("<S-Right>")
        );
        assert_eq!(Editor::prompt_word_arrow("left", true, true), None);
        assert_eq!(Editor::prompt_word_arrow("up", true, false), None);
        assert_eq!(Editor::prompt_word_arrow("left", false, false), None);
    }

    #[test]
    fn word_motion_keeps_pending_search_preview() {
        for chord in ["<S-Left>", "<C-Left>", "<S-Right>", "<C-Right>"] {
            let mut e = Editor::new("first needle next needle".into());
            e.key("/");
            e.insert("needle");
            let job = e.take_search_job().unwrap();
            e.key(chord);
            assert!(e.apply_search(job.run()), "{chord}");
            assert_eq!(e.search_match(), Some(6..12));
        }
    }

    #[test]
    fn moving_search_prompt_caret_keeps_pending_preview() {
        let mut e = Editor::new("first needle next needle".into());
        e.key("/");
        e.insert("needle");
        let job = e.take_search_job().unwrap();
        e.key("<Left>");
        assert_eq!(e.prompt_cursor(), 5);
        assert!(e.apply_search(job.run()));
        assert_eq!(e.search_match(), Some(6..12));
        assert_eq!(e.command, "needle");
    }

    #[test]
    fn prompt_midline_delete_word_and_combining_are_grapheme_safe() {
        let mut e = Editor::new("unchanged".into());
        key(&mut e, ":");
        e.insert("one two");
        key(&mut e, "<Left>");
        key(&mut e, "<Left>");
        key(&mut e, "<C-w>");
        assert_eq!((e.command.as_str(), e.prompt_cursor()), ("one wo", 4));
        key(&mut e, "<Esc>");
        key(&mut e, ":");
        e.insert("ae\u{301}z");
        key(&mut e, "<Left>");
        key(&mut e, "<Left>");
        assert_eq!(e.prompt_parts(), ("a", "e\u{301}", "z"));
        key(&mut e, "<Right>");
        key(&mut e, "<Delete>");
        assert_eq!((e.command.as_str(), e.prompt_cursor()), ("ae\u{301}", 4));
    }

    #[test]
    fn prompt_deletions_match_clean_neovim_reference() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-prompt-editing.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let text = case["text"].as_str().unwrap();
            let mut e = Editor::new(text.into());
            key(&mut e, case["prefix"].as_str().unwrap());
            e.insert(case["query"].as_str().unwrap());
            e.finish_search_test();
            for chord in case["keys"].as_array().unwrap() {
                key(&mut e, chord.as_str().unwrap());
            }
            assert_eq!(
                e.command,
                case["expected_command"].as_str().unwrap(),
                "{}",
                case["label"]
            );
            let mode = match e.mode {
                Mode::Normal => "n",
                Mode::Command | Mode::Search => "c",
                _ => "unexpected",
            };
            assert_eq!(
                mode,
                case["expected_mode"].as_str().unwrap(),
                "{}",
                case["label"]
            );
            assert_eq!(e.text.iter().collect::<String>(), text);
            assert_eq!(e.cursor, 0);
            assert_eq!(e.version, 0);
            assert!(!e.dirty());
        }
    }
    #[test]
    fn prompt_deletion_rejects_obsolete_search_results_and_restores_cursor() {
        let mut e = Editor::new("start foobar foobaz".into());
        e.cursor = 1;
        key(&mut e, "/");
        e.insert("foo");
        let old = e.take_search_job().unwrap();
        e.key("<C-h>");
        assert_eq!(e.command, "fo");
        assert!(!e.apply_search(old.run()));
        e.finish_search_test();
        key(&mut e, "<C-u>");
        assert!(e.command.is_empty());
        assert_eq!(e.mode, Mode::Search);
        key(&mut e, "<BS>");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.cursor, 1);
        assert!(e.registers.search().is_none());
        assert!(e.take_search_job().is_none());
    }
    #[test]
    fn search_object_prefix_still_expands_before_word_deletion() {
        let mut e = Editor::new("start foobar".into());
        key(&mut e, "/");
        e.insert("fo");
        e.finish_search_test();
        key(&mut e, "<C-r>");
        key(&mut e, "<C-w>");
        assert_eq!(e.command, "foobar");
        key(&mut e, "<C-w>");
        assert!(e.command.is_empty());
        assert_eq!(e.version, 0);
    }
    #[test]
    fn ctrl_h_ownership_depends_on_editor_mode() {
        for mode in [
            Mode::Search,
            Mode::Command,
            Mode::Insert,
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
        ] {
            for chord in ["<C-h>", "<C-j>", "<C-k>", "<C-l>", "<C-/>"] {
                assert!(!mode.workspace_chord(chord));
                assert!(Mode::Normal.workspace_chord(chord));
            }
        }
    }
}

#[cfg(test)]
mod window_insert_tests {
    use super::*;
    #[test]
    fn insert_word_deletion_matches_clean_neovim() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-visual-insert.json"
        ))
        .unwrap();
        for case in fixture["insert"].as_array().unwrap() {
            let mut e = Editor::new(case["text"].as_str().unwrap().into());
            e.key("i");
            e.cursor = case["cursor"].as_u64().unwrap() as usize;
            e.key("<C-w>");
            let want = case["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(e.text.iter().collect::<String>(), want, "{}", case["label"]);
            assert_eq!(e.mode, Mode::Insert);
            let before: String = e.text[..e.cursor].iter().collect();
            let row = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().unwrap().len() + 1;
            assert_eq!(
                (row, col),
                (
                    case["position"][0].as_u64().unwrap() as usize,
                    case["position"][1].as_u64().unwrap() as usize
                )
            );
        }
    }
    #[test]
    fn visual_window_change_keeps_last_area_and_no_move_keeps_selection() {
        for prefix in ["v", "V", "<C-v>"] {
            let mut e = Editor::new("abc\ndef".into());
            e.key(prefix);
            e.key("l");
            let mode = e.mode;
            assert!(mode.workspace_chord("<C-w>"));
            e.finish_window_navigation(false);
            assert_eq!(e.mode, mode);
            let selection = e.selection();
            e.finish_window_navigation(true);
            assert_eq!(e.mode, Mode::Normal);
            e.key("g");
            e.key("v");
            assert_eq!(e.mode, mode);
            assert_eq!(e.selection(), selection);
        }
        for mode in [Mode::Insert, Mode::Command, Mode::Search] {
            assert!(!mode.workspace_chord("<C-w>"));
        }
    }
    #[test]
    fn insert_word_deletion_shares_insert_undo() {
        let mut e = Editor::new("original".into());
        e.key("A");
        e.insert(" two words");
        e.key("<C-w>");
        e.key("<Esc>");
        e.key("u");
        assert_eq!(e.text.iter().collect::<String>(), "original");
    }
}

#[cfg(test)]
mod window_count_tests {
    use super::*;
    #[test]
    fn transfer_count_leaves_mode_text_and_subsequent_motion_intact() {
        for visual in [false, true] {
            let mut e = Editor::new("one\ntwo\nthree\nfour".into());
            if visual {
                e.key("v");
            }
            e.key("3");
            let mode = e.mode;
            assert_eq!(e.take_window_count(), 3);
            assert_eq!(e.mode, mode);
            assert!(!e.command_pending());
            assert_eq!(e.take_window_count(), 0);
            e.key("j");
            assert_eq!(e.cursor, 4);
            assert_eq!(e.contents(), "one\ntwo\nthree\nfour");
        }
        let mut e = Editor::new("one\ntwo".into());
        e.key("2");
        e.key("d");
        assert_eq!(
            e.take_window_count(),
            0,
            "an operator is not a window count"
        );
        assert!(e.command_pending());
        e.key("<Esc>");
        e.key("i");
        assert_eq!(e.take_window_count(), 0);
    }
}
