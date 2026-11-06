//! Host clipboard handshake. No operating-system access from the modal core.
use super::*;
impl Editor {
    pub fn with_clipboard(mut self) -> Self {
        self.clipboard_enabled = true;
        self
    }
    pub(super) fn effective_register(&self) -> char {
        match self.chosen_register {
            None | Some('"') if self.clipboard_enabled => '+',
            name => name.unwrap_or('"'),
        }
    }
    pub fn needs_clipboard(&self) -> bool {
        self.clipboard_pending.is_some() || self.prompt_clipboard_pending.is_some()
    }
    pub fn cancel_prompt_clipboard(&mut self) -> bool {
        if self.prompt_clipboard_pending.take().is_some() {
            self.message = "Clipboard cancelado: cambió el panel".into();
            true
        } else {
            false
        }
    }
    pub fn take_clipboard_write(&mut self) -> Option<(String, bool)> {
        self.clipboard_write
            .take()
            .map(|(text, line, _)| (text, line))
    }
    pub fn take_clipboard_write_typed(&mut self) -> Option<(String, bool, Option<usize>)> {
        self.clipboard_write.take()
    }
    pub fn provide_clipboard(&mut self, content: Result<(String, bool), String>) -> Effect {
        self.provide_clipboard_typed(content.map(|(text, line)| (text, line, None)))
    }
    pub fn provide_clipboard_typed(
        &mut self,
        content: Result<(String, bool, Option<usize>), String>,
    ) -> Effect {
        if let Some((command, prompt_cursor, mode, version, cursor)) =
            self.prompt_clipboard_pending.take()
        {
            if self.version != version
                || self.cursor != cursor
                || self.mode != mode
                || self.command != command
                || self.prompt_cursor != prompt_cursor
            {
                self.message = "Clipboard obsoleto: cambió la línea de comandos".into();
                return Effect::None;
            }
            return match content {
                Ok((text, line, _)) => {
                    let value = if line {
                        text.strip_suffix('\n').unwrap_or(&text)
                    } else {
                        &text
                    };
                    self.insert_prompt_register_value(value)
                }
                Err(error) => {
                    self.message = error;
                    Effect::None
                }
            };
        }
        let Some((key, count, name, version, cursor, mode)) = self.clipboard_pending.take() else {
            return Effect::None;
        };
        let content = if self.version != version || self.cursor != cursor || self.mode != mode {
            Err("Clipboard obsoleto: cambió el destino del pegado".into())
        } else {
            content
        };
        let imported = content.and_then(|(text, line, width)| {
            if text.len() > 2 * 1024 * 1024 || text.contains('\0') {
                return Err("Clipboard fuera de límites".into());
            }
            let text = text.replace("\r\n", "\n");
            if let Some(width) = width {
                self.registers
                    .import_kind(text, super::registers::Kind::Block(width))
            } else {
                self.registers.import_clipboard(text, line)
            }
        });
        if let Err(error) = imported {
            self.chosen_register = None;
            self.count = 0;
            self.change_events.clear();
            self.message = error;
            self.cancel_replay();
            return Effect::None;
        }
        self.chosen_register = Some(name);
        self.count = count;
        self.clipboard_ready = true;
        let effect = self.key_base(&key);
        self.finish_change();
        effect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_clipboard_registers_read_current_clipboard_at_the_caret() {
        for name in ["+", "*"] {
            let mut editor = Editor::new("fixture".into()).with_clipboard();
            editor.key(":");
            editor.insert("ab");
            editor.key("<Left>");
            editor.key("<C-r>");
            editor.insert(name);
            assert!(editor.needs_clipboard(), "{name}");
            assert_eq!(editor.command, "ab");
            editor.provide_clipboard(Ok(("fresh".into(), false)));
            assert!(!editor.needs_clipboard());
            assert_eq!(editor.command, "afreshb", "{name}");
            assert_eq!(editor.prompt_cursor(), 6);
        }
    }
    #[test]
    fn prompt_unnamed_register_uses_memory_even_with_clipboard_enabled() {
        let bank = RegisterBank::default();
        bank.capture('"', 'y', "remembered\n".into(), true).unwrap();
        let mut editor = Editor::with_registers("fixture".into(), bank).with_clipboard();
        editor.key(":");
        editor.key("<C-r>");
        editor.insert("\"");
        assert!(!editor.needs_clipboard());
        assert_eq!(editor.command, "remembered");
    }
    #[test]
    fn prompt_clipboard_rejects_stale_or_multiline_without_modifying_text() {
        let mut editor = Editor::new("fixture".into()).with_clipboard();
        editor.key(":");
        editor.insert("abc");
        editor.key("<C-r>");
        editor.insert("+");
        editor.command.push('x');
        editor.provide_clipboard(Ok(("old".into(), false)));
        assert_eq!(editor.command, "abcx");
        assert!(editor.message.contains("obsoleto"));
        editor.key("<Esc>");
        editor.key(":");
        editor.insert("abc");
        editor.key("<C-r>");
        editor.insert("+");
        editor.provide_clipboard(Ok(("one\ntwo".into(), true)));
        assert_eq!(editor.command, "abc");
        assert!(editor.message.contains("multilínea"));
        editor.key("<C-r>");
        editor.insert("+");
        assert!(editor.cancel_prompt_clipboard());
        assert!(!editor.needs_clipboard());
        editor.provide_clipboard(Ok(("late".into(), false)));
        assert_eq!(editor.command, "abc");
        editor.key("<C-r>");
        editor.insert("+");
        editor.key("<Esc>");
        assert!(!editor.needs_clipboard());
        assert_eq!(editor.mode, Mode::Normal);
    }
    #[test]
    fn search_clipboard_register_refreshes_the_search_preview() {
        let mut editor = Editor::new("alpha beta alpha".into()).with_clipboard();
        editor.key("/");
        editor.key("<C-r>");
        editor.insert("+");
        assert!(editor.needs_clipboard());
        assert_eq!(
            editor.provide_clipboard(Ok(("alpha".into(), false))),
            Effect::Search
        );
        assert_eq!(editor.command, "alpha");
        assert!(editor.take_search_job().is_some());
    }
    #[test]
    fn wrapped_yank_respects_clipboard_named_and_blackhole_registers() {
        for (prefix, clipboard, named) in [
            (None, true, false),
            (Some('a'), false, true),
            (Some('_'), false, false),
        ] {
            let mut editor = Editor::new("abcdefghijabcdefghijtail".into()).with_clipboard();
            if let Some(register) = prefix {
                editor.key_with_wrap("\"");
                editor.key_with_wrap(&register.to_string());
            }
            editor.key_with_wrap("y");
            editor.key_with_wrap("g");
            assert_eq!(
                editor.key_with_wrap("j"),
                Effect::WrappedOperator('y', true, 1)
            );
            assert!(editor.commit_wrapped_operator('y', 0, editor.version, 20));
            assert_eq!(editor.contents(), "abcdefghijabcdefghijtail");
            assert_eq!(
                editor.take_clipboard_write(),
                clipboard.then(|| ("abcdefghijabcdefghij".into(), false))
            );
            assert_eq!(
                editor.registers.get('a').map(|item| item.0),
                named.then(|| "abcdefghijabcdefghij".into())
            );
        }
    }
    #[test]
    fn stale_clipboard_response_cannot_edit_a_moved_or_changed_destination() {
        let mut e = Editor::new("one two".into()).with_clipboard();
        assert_eq!(e.key("p"), Effect::ClipboardRead);
        e.goto(0, 4);
        e.provide_clipboard(Ok(("OLD".into(), false)));
        assert_eq!(e.contents(), "one two");
        assert!(!e.dirty());
        assert_eq!(e.key("p"), Effect::ClipboardRead);
        e.version += 1;
        e.provide_clipboard(Ok(("OLD".into(), false)));
        assert_eq!(e.contents(), "one two");
        assert!(!e.needs_clipboard());
    }
    #[test]
    fn clipboard_is_explicit_effect_preserves_registers_and_pauses_replay_until_ack() {
        let mut e = Editor::new("alpha beta".into()).with_clipboard();
        e.key("y");
        e.key("e");
        assert_eq!(e.take_clipboard_write(), Some(("alpha".into(), false)));
        e.key("\"");
        e.key("_");
        e.key("d");
        e.key("e");
        assert!(e.take_clipboard_write().is_none());
        assert_eq!(e.key("P"), Effect::ClipboardRead);
        assert!(e.needs_clipboard());
        let old = e.text.clone();
        e.key("x");
        assert_eq!(e.text, old);
        e.provide_clipboard(Ok(("ñ界".into(), false)));
        assert!(!e.needs_clipboard());
        assert!(e.contents().starts_with("ñ界"));
        assert_eq!(e.registers.get('0').unwrap().0, "alpha");
        e.key("u");
        assert_eq!(e.text, old);
        assert_eq!(e.key("."), Effect::ClipboardRead);
        e.provide_clipboard(Ok(("second".into(), false)));
        while e.replaying() {
            e.advance_replay();
        }
        assert!(e.contents().starts_with("second"));
        e.key("q");
        e.key("a");
        assert_eq!(e.key("p"), Effect::ClipboardRead);
        e.provide_clipboard(Ok(("macro".into(), false)));
        e.key("q");
        e.key("@");
        assert_eq!(e.key("a"), Effect::ClipboardRead);
        let before = e.text.clone();
        assert_eq!(e.advance_replay(), Effect::None);
        assert_eq!(before, e.text);
        e.provide_clipboard(Err("Sin texto en clipboard".into()));
        assert!(!e.replaying());
        assert_eq!(before, e.text);
        e.key("\"");
        e.key("a");
        e.key("y");
        e.key("e");
        assert!(e.take_clipboard_write().is_none());
        assert_eq!(e.key("p"), Effect::ClipboardRead);
        e.provide_clipboard(Ok(("x".repeat(2 * 1024 * 1024 + 1), false)));
        assert_eq!(before, e.text);
    }
}
