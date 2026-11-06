//! Modal command grammar shared by physical keys, text commits and bounded replay.
use super::*;
#[derive(Clone, Debug)]
pub(super) enum EditEvent {
    Key(String),
    Input(char),
    Text(String),
    Literal(String),
    Completion {
        left: usize,
        right: usize,
        text: String,
        caret: usize,
    },
    Shift {
        lines: usize,
        count: usize,
        right: bool,
    },
    Cursor(isize),
}
pub(super) struct ReplayFrame {
    events: std::sync::Arc<Vec<EditEvent>>,
    index: usize,
    remaining: usize,
}
/// Opaque continuation moved between already-open buffers of one workspace.
pub struct ReplayContinuation {
    frames: Vec<ReplayFrame>,
    budget: usize,
    registers: RegisterBank,
}
#[derive(Clone, Copy)]
pub(super) enum Await {
    Register,
    Macro,
    Play(usize),
    Replace(usize),
    Find(char, usize, Option<char>),
    Mark,
    Jump(bool),
    Object(char, usize, bool),
    CaseG(char, usize),
    ShiftG(char, usize),
}
impl Editor {
    pub(super) fn record(&mut self, event: EditEvent) {
        if self.internal || self.replay_depth > 0 {
            return;
        }
        if let Err(error) = self.registers.replay().record(&event) {
            self.message = error;
        }
        if self.change_events.is_empty() {
            self.change_version = self.version;
        }
        if self.change_events.len() < 4096
            && self
                .change_events
                .iter()
                .map(EditEvent::retained_bytes)
                .sum::<usize>()
                .saturating_add(event.retained_bytes())
                <= 8 * 1024 * 1024
        {
            self.change_events.push(event);
        } else {
            self.change_failed = true;
        }
    }
    pub(super) fn remember_change(&mut self, events: Vec<EditEvent>) {
        if let Err(error) = self.registers.replay().set_change(events) {
            self.message = error;
        }
    }
    pub(super) fn finish_change(&mut self) {
        if self.replay_depth == 0
            && self.mode == Mode::Normal
            && self.pending.is_empty()
            && self.awaiting.is_none()
            && self.jump.is_none()
            && self.count == 0
            && self.clipboard_pending.is_none()
        {
            if self.version != self.change_version {
                let events = std::mem::take(&mut self.change_events);
                if self.change_failed {
                    self.remember_change(Vec::new());
                    self.message =
                        "Cambio fuera de límites de repetición; punto desactivado".into();
                } else {
                    self.remember_change(events);
                }
            } else {
                self.change_events.clear();
            }
            self.change_failed = false;
        }
    }
    pub(super) fn replace_completion(
        &mut self,
        left: usize,
        right: usize,
        text: &str,
    ) -> Result<(), String> {
        self.replace_input(left, right, text, text.chars().count())
    }
    pub(super) fn replace_input(
        &mut self,
        left: usize,
        right: usize,
        text: &str,
        caret: usize,
    ) -> Result<(), String> {
        if let Some(result) = self.snippet_input(left, right, text, caret) {
            return result;
        }
        if self.mode != Mode::Insert
            || left > self.cursor
            || right > self.text.len().saturating_sub(self.cursor)
        {
            return Err("Completion fuera del cursor de INSERT".into());
        }
        let count = text.chars().count();
        if caret > count
            || text.len() > 2 * 1024 * 1024
            || self
                .text
                .len()
                .saturating_sub(left + right)
                .saturating_add(count)
                > 2_000_000
        {
            return Err("Completion fuera de límites".into());
        }
        self.record(EditEvent::Completion {
            left,
            right,
            text: text.into(),
            caret,
        });
        let start = self.cursor - left;
        self.track_visual_edit(
            start..self.cursor + right,
            text.chars().filter(|c| *c == '\n').count(),
            false,
        );
        self.text.splice(start..self.cursor + right, text.chars());
        self.cursor = start + caret;
        self.changed();
        Ok(())
    }
    pub fn insert(&mut self, text: &str) {
        self.commit_text(text, false);
    }
    pub fn paste(&mut self, text: &str) {
        self.commit_text(text, true);
    }
    fn commit_text(&mut self, text: &str, literal: bool) {
        self.cancel_input();
        let normalized = text.replace("\r\n", "\n");
        if let Some(result) = self.snippet_input(0, 0, &normalized, normalized.chars().count()) {
            if let Err(error) = result {
                self.message = error;
            }
            return;
        }
        if text.len() > 2 * 1024 * 1024
            || (self.mode == Mode::Insert
                && self.text.len().saturating_add(text.chars().count()) > 2_000_000)
        {
            self.message = "Texto demasiado grande".into();
            return;
        }
        if matches!(self.mode, Mode::Insert | Mode::Command | Mode::Search) {
            self.record(if literal {
                EditEvent::Literal(text.into())
            } else {
                EditEvent::Text(text.into())
            });
        }
        if !literal && self.mode == Mode::Insert && text.chars().count() == 1 {
            let ch = text.chars().next().unwrap();
            if matches!(ch, ')' | ']' | '}' | '\'' | '"' | '`')
                && self.text.get(self.cursor) == Some(&ch)
            {
                self.cursor += 1;
                return;
            }
            let close = match ch {
                '(' => Some(')'),
                '[' => Some(']'),
                '{' => Some('}'),
                '\'' | '"' | '`' => Some(ch),
                _ => None,
            };
            if let Some(close) = close {
                let start = self.line_start(self.cursor);
                let skip_next = self.text.get(self.cursor).is_some_and(|c| {
                    c.is_alphanumeric()
                        || matches!(c, '_' | '%' | '\'' | '[' | '"' | '.' | '`' | '$')
                });
                let mut quote = None;
                let mut escaped = false;
                if self.cursor - start <= 4096 {
                    for c in &self.text[start..self.cursor] {
                        if escaped {
                            escaped = false;
                            continue;
                        }
                        if *c == '\\' {
                            escaped = true;
                            continue;
                        }
                        if quote == Some(*c) {
                            quote = None;
                        } else if quote.is_none() && matches!(c, '\'' | '"' | '`') {
                            quote = Some(*c);
                        }
                    }
                } else {
                    quote = Some('"');
                }
                let word_quote = matches!(ch, '\'' | '"' | '`')
                    && self.cursor > 0
                    && self.text[self.cursor - 1].is_alphanumeric();
                if !skip_next && quote.is_none() && !word_quote && self.text.len() + 2 <= 2_000_000
                {
                    self.insert_base(&format!("{ch}{close}"));
                    self.cursor = self.cursor.saturating_sub(1);
                    return;
                }
            }
        }
        // Arbitrary multiline IME/paste input may use different mark gravity.
        self.insert_base_tracked(text, false, false);
    }
    pub fn recording_register(&self) -> Option<char> {
        self.registers.replay().recording()
    }
    pub fn replaying(&self) -> bool {
        !self.replay_stack.is_empty()
    }
    pub fn cancel_replay(&mut self) {
        if self.replay_stack.is_empty() && self.replay_depth == 0 {
            return;
        }
        self.replay_stack.clear();
        self.replay_depth = 0;
        self.change_events.clear();
    }
    pub fn take_replay(&mut self) -> Option<ReplayContinuation> {
        if self.replay_stack.is_empty() {
            return None;
        }
        self.replay_depth = 0;
        self.change_events.clear();
        Some(ReplayContinuation {
            frames: std::mem::take(&mut self.replay_stack),
            budget: self.replay_budget,
            registers: self.registers.clone(),
        })
    }
    pub fn resume_replay(&mut self, continuation: ReplayContinuation) -> Result<(), String> {
        if self.mode != Mode::Normal || !self.registers.shares_session(&continuation.registers) {
            return Err("Replay cancelado: destino fuera de NORMAL o de la misma sesión".into());
        }
        self.cancel_replay();
        self.pending.clear();
        self.awaiting = None;
        self.count = 0;
        self.chosen_register = None;
        self.replay_stack = continuation.frames;
        self.replay_budget = continuation.budget;
        Ok(())
    }
    fn play(&mut self, events: impl Into<std::sync::Arc<Vec<EditEvent>>>, n: usize) -> Effect {
        let events = events.into();
        if events.is_empty() {
            return Effect::None;
        }
        if self.replay_stack.len() >= 16 {
            self.cancel_replay();
            self.message = "Macro recursiva detenida".into();
            return Effect::None;
        }
        if self.replay_stack.is_empty() {
            self.replay_budget = 10000;
        }
        self.replay_stack.push(ReplayFrame {
            events,
            index: 0,
            remaining: n.min(9999),
        });
        if self.replay_depth > 0 {
            return Effect::None;
        }
        self.advance_replay()
    }
    pub fn advance_replay(&mut self) -> Effect {
        if self.saving
            || self.clipboard_pending.is_some()
            || self.search_pending.is_some()
            || self.replay_stack.is_empty()
        {
            return Effect::None;
        }
        self.replay_depth = 1;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(4);
        while std::time::Instant::now() < deadline {
            let Some(frame) = self.replay_stack.last_mut() else {
                self.replay_depth = 0;
                return Effect::None;
            };
            if frame.index >= frame.events.len() {
                if frame.remaining > 1 {
                    frame.remaining -= 1;
                    frame.index = 0;
                } else {
                    self.replay_stack.pop();
                }
                continue;
            }
            if self.replay_budget == 0 {
                self.cancel_replay();
                self.message = "Límite de 10.000 eventos de replay alcanzado".into();
                return Effect::None;
            }
            self.replay_budget -= 1;
            let event = frame.events[frame.index].clone();
            frame.index += 1;
            let effect = match event {
                EditEvent::Key(k) => self.key(&k),
                EditEvent::Input(ch) => {
                    let input = ch.to_string();
                    if matches!(self.mode, Mode::Insert | Mode::Command | Mode::Search) {
                        self.insert(&input);
                        Effect::None
                    } else {
                        self.key(&input)
                    }
                }
                EditEvent::Text(s) => {
                    self.insert(&s);
                    Effect::None
                }
                EditEvent::Literal(s) => {
                    self.paste(&s);
                    Effect::None
                }
                EditEvent::Completion {
                    left,
                    right,
                    text,
                    caret,
                } => {
                    if let Err(error) = self.replace_input(left, right, &text, caret) {
                        self.cancel_replay();
                        self.message = error;
                    }
                    Effect::None
                }
                EditEvent::Shift {
                    lines,
                    count,
                    right,
                } => {
                    let start = self.line_start(self.cursor);
                    let end = self.line_number(self.position().0.saturating_add(lines));
                    self.shift_lines(start, end, right, count, true);
                    Effect::None
                }
                EditEvent::Cursor(delta) => {
                    if self.mode == Mode::Insert
                        && let Some(cursor) = self
                            .cursor
                            .checked_add_signed(delta)
                            .filter(|p| *p <= self.text.len())
                    {
                        self.cursor = cursor;
                    } else {
                        self.cancel_replay();
                        self.message =
                            "Movimiento de snippet fuera del buffer; replay detenido".into();
                    }
                    Effect::None
                }
            };
            // The owner must apply ordered jump/edit events and refresh the
            // pane viewport before replay can produce another event.
            if effect != Effect::None || self.jump_changes.as_ref().is_some_and(|j| j.has_changes())
            {
                return effect;
            }
        }
        Effect::None
    }
    /// Resume replay using the focused window's wrap setting for this slice.
    pub fn advance_replay_with_wrap(&mut self) -> Effect {
        let previous = self.wrap_input;
        self.wrap_input = true;
        let effect = self.advance_replay();
        self.wrap_input = previous;
        effect
    }
    pub(super) fn capture_register(&mut self, op: char) {
        let name = self.effective_register();
        self.chosen_register = None;
        if let Err(error) = self.registers.capture(
            name,
            op,
            std::mem::take(&mut self.register),
            self.line_register,
        ) {
            self.message = error;
        } else if self.clipboard_enabled && matches!(name, '+' | '*') {
            self.clipboard_write = self
                .registers
                .get(name)
                .map(|(text, line)| (text, line, None));
        }
    }
    pub fn key(&mut self, key: &str) -> Effect {
        if self.search_navigation_busy()
            && self.mode == Mode::Normal
            && self.pending.is_empty()
            && self.awaiting.is_none()
            && self.count == 0
            && matches!(key, "n" | "N")
        {
            if self.search_followups.len() < 128 {
                self.record(EditEvent::Key(key.into()));
                self.search_followups.push_back(key == "N");
            } else {
                self.cancel_search();
                self.cancel_replay();
                self.message = "Repetición de búsqueda limitada a 128 teclas pendientes".into();
            }
            return Effect::None;
        }
        // Enter can promote the in-flight preview instead of losing a just
        // typed query or Ctrl-G/T navigation before its worker result arrives.
        if !(self.mode == Mode::Search
            && (self.prompt_register_pending && key == "<Esc>"
                || matches!(
                    key,
                    "<Enter>"
                        | "<C-g>"
                        | "<C-t>"
                        | "<C-l>"
                        | "<C-r>"
                        | "<C-w>"
                        | "<Left>"
                        | "<Right>"
                        | "<Home>"
                        | "<End>"
                        | "<C-b>"
                        | "<C-e>"
                        | "<S-Left>"
                        | "<S-Right>"
                        | "<C-Left>"
                        | "<C-Right>"
                )))
        {
            self.cancel_search();
        }
        if self.mode == Mode::Normal
            && self.pending.is_empty()
            && matches!(
                key,
                "i" | "a" | "I" | "A" | "o" | "O" | "h" | "l" | "<Left>" | "<Right>"
            )
        {
            self.open_cursor_folds();
        }
        let previous = self.visual_area();
        let effect = self.key_inner(key);
        self.sync_block_cursor();
        if let Some(previous) = previous
            && !matches!(
                self.mode,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            )
            && !matches!(key, "p" | "P")
        {
            self.remember_visual(previous);
        }
        effect
    }
    /// Route an eligible uncounted j/k to a window-local wrapped viewport.
    pub fn key_with_wrap(&mut self, key: &str) -> Effect {
        let previous = self.wrap_input;
        self.wrap_input = true;
        let effect = self.key(key);
        self.wrap_input = previous;
        effect
    }
    fn key_inner(&mut self, key: &str) -> Effect {
        self.cancel_input();
        if self.clipboard_pending.is_some() {
            return Effect::None;
        }
        if self.prompt_clipboard_pending.is_some() {
            if matches!(key, "<Esc>" | "<C-c>") {
                self.prompt_clipboard_pending = None;
            } else {
                return Effect::None;
            }
        }
        if self.snippet_key(key) {
            return Effect::None;
        }
        if key == "d"
            && self.mode == Mode::Normal
            && self.pending.is_empty()
            && self.awaiting.is_none()
        {
            self.operator_goal_column = Some(
                self.desired_column
                    .filter(|(expected, _)| *expected == self.cursor)
                    .map_or_else(
                        || self.grapheme_column(self.cursor).unwrap_or(0),
                        |(_, column)| column,
                    ),
            );
        }
        if !matches!(key, "j" | "k" | "<Down>" | "<Up>" | "g" | "G")
            && !(key.len() == 1 && key.as_bytes()[0].is_ascii_digit())
        {
            self.desired_column = None;
        }
        if self.jump.is_some() {
            self.record(EditEvent::Key(key.into()));
            self.jump_key(key);
            self.finish_change();
            return Effect::None;
        }
        if key == "s"
            && matches!(self.mode, Mode::Normal | Mode::Visual)
            && self.awaiting.is_none()
            && (self.pending.is_empty() || self.pending.starts_with(['d', 'c', 'y']))
        {
            self.record(EditEvent::Key(key.into()));
            self.begin_jump();
            return Effect::None;
        }
        if key == "q"
            && self.mode == Mode::Normal
            && self.recording_register().is_some()
            && self.replay_depth == 0
            && self.awaiting.is_none()
            && self.pending.is_empty()
        {
            self.message = match self.registers.finish_macro() {
                Ok(name) => format!("Macro @{name} grabada"),
                Err(error) => error,
            };
            self.change_events.clear();
            return Effect::None;
        }
        if self.mode == Mode::Normal
            && self.pending.is_empty()
            && self.awaiting.is_none()
            && matches!(key, "u" | "<C-r>")
        {
            self.record(EditEvent::Key(key.into()));
            let effect = self.key_base(key);
            self.change_events.clear();
            return effect;
        }
        self.record(EditEvent::Key(key.into()));
        if matches!(self.mode, Mode::Normal | Mode::Visual)
            && self.pending.is_empty()
            && self.awaiting.is_none()
            && let Some(pos) = self.csv.as_ref().and_then(|t| t.jump(self.cursor, key))
        {
            self.cursor = pos;
            self.clamp();
            self.finish_change();
            return Effect::None;
        }
        if self.mode == Mode::Normal && self.awaiting.is_none() {
            if self.pending.is_empty() && matches!(key, "<C-o>" | "<C-i>" | "<Tab>") {
                self.change_events.clear();
                let count = std::mem::take(&mut self.count).max(1);
                return Effect::JumpHistory(key == "<C-o>", count);
            }
            if self.pending.is_empty() && matches!(key, "<C-6>" | "<C-^>") {
                self.change_events.clear();
                let count = std::mem::take(&mut self.count);
                if count != 0 {
                    return Effect::Buffer(format!("buffers.number.{count}"));
                }
                return Effect::Buffer("buffers.alternate".into());
            }
            if self.pending.is_empty() && matches!(key, "H" | "L") {
                self.count = 0;
                self.change_events.clear();
                return Effect::Buffer(
                    if key == "H" {
                        "buffers.line.prev"
                    } else {
                        "buffers.line.next"
                    }
                    .into(),
                );
            }
            if self.pending.starts_with('g') && matches!(key, "d" | "r") {
                self.pending.clear();
                self.change_events.clear();
                return Effect::Lsp(
                    if key == "d" {
                        "definition"
                    } else {
                        "references"
                    }
                    .into(),
                );
            }
            if matches!(self.pending.as_str(), "[" | "]") {
                let forward = self.pending == "]";
                self.pending.clear();
                if key == "b" {
                    self.count = 0;
                    self.change_events.clear();
                    return Effect::Buffer(
                        if forward {
                            "buffers.line.next"
                        } else {
                            "buffers.line.prev"
                        }
                        .into(),
                    );
                }
                if key == "B" {
                    self.count = 0;
                    self.change_events.clear();
                    return Effect::Buffer(
                        if forward {
                            "buffers.move.next"
                        } else {
                            "buffers.move.prev"
                        }
                        .into(),
                    );
                }
                if key == "d" {
                    return Effect::Lsp(if forward { "next" } else { "prev" }.into());
                }
                if matches!(key, "h" | "H") {
                    return Effect::Git(
                        match (forward, key) {
                            (true, "H") => "last",
                            (false, "H") => "first",
                            (true, _) => "next",
                            (false, _) => "prev",
                        }
                        .into(),
                    );
                }
            }
            if self.pending.is_empty() && matches!(key, "[" | "]") {
                self.pending = key.into();
                return Effect::None;
            }
            if self.pending.is_empty() && key == "K" {
                self.change_events.clear();
                return Effect::Lsp("hover".into());
            }
        }
        if key == "<Esc>" {
            self.awaiting = None;
        } else if let Some(wait) = self.awaiting.take() {
            let ch = key.chars().next().filter(|_| key.chars().count() == 1);
            if let Some(ch) = ch {
                match wait {
                    Await::Register => {
                        if ch.is_ascii_alphanumeric() || matches!(ch, '"' | '_' | '+' | '*') {
                            self.chosen_register = Some(ch);
                        }
                    }
                    Await::Macro => {
                        if ch.is_ascii_alphanumeric() {
                            self.message = match self.registers.start_macro(ch) {
                                Ok(()) => format!("En grabación @{ch}"),
                                Err(error) => error,
                            };
                        }
                        self.change_events.clear();
                    }
                    Await::Play(n) => {
                        let name = if ch == '@' {
                            self.registers.replay().last_macro().unwrap_or('@')
                        } else {
                            ch
                        };
                        match self.registers.macro_program(name) {
                            Ok(Some(events)) => {
                                self.registers.replay().played(name);
                                self.change_events.clear();
                                return self.play(events, n);
                            }
                            Err(error) => self.message = error,
                            Ok(None) => self.message = format!("Registro @{name} vacío"),
                        }
                    }
                    Await::Replace(n) => {
                        if self.mode == Mode::VisualBlock {
                            self.block_replace(ch);
                            self.finish_change();
                            return Effect::None;
                        }
                        if let Some(range) = self.selection() {
                            self.replace_visual(range, ch);
                            self.finish_change();
                            return Effect::None;
                        }
                        if let Some(end) = self.grapheme_exact_forward(self.cursor, n)
                            && end > self.cursor
                        {
                            self.checkpoint();
                            self.text
                                .splice(self.cursor..end, std::iter::repeat_n(ch, n));
                            self.cursor += n - 1;
                            self.changed();
                        }
                    }
                    Await::CaseG(op, n) => {
                        if ch == 'g' {
                            self.case_to_line(op, self.line_number(n));
                        } else if ch == op {
                            // gUgU, gugu and g~g~ operate linewise, like
                            // gUU, guu and g~~, including multiplied counts.
                            self.case_current_lines(op, n);
                        } else {
                            self.message = "Movimiento g de cambio de caja pendiente".into();
                        }
                    }
                    Await::ShiftG(op, n) => {
                        if ch == 'g' {
                            self.shift_to_position(op, self.line_number(n));
                        } else {
                            self.message = "Movimiento g de indentación pendiente".into();
                        }
                    }
                    Await::Find(kind, n, op) => {
                        if let Some(end) = self.find_char(kind, ch, n) {
                            self.last_find = Some((kind, ch));
                            if let Some(op) = op {
                                // Neovim's backward T operator can end immediately
                                // after the target scalar, including inside a grapheme.
                                let target = if kind == 'T' && end < self.cursor {
                                    end
                                } else if let Some(target) = self.grapheme_floor(end) {
                                    target
                                } else {
                                    self.chosen_register = None;
                                    return Effect::None;
                                };
                                let start = self.cursor.min(target);
                                let end = if target >= self.cursor {
                                    let Some(end) = self.grapheme_move(target, 1, true, true)
                                    else {
                                        self.chosen_register = None;
                                        return Effect::None;
                                    };
                                    end
                                } else {
                                    self.cursor
                                };
                                self.operate(op, start..end, false);
                            } else {
                                self.cursor = end;
                                self.clamp();
                            }
                        }
                    }
                    Await::Mark => {
                        self.marks.insert(ch, self.cursor);
                    }
                    Await::Jump(line) => {
                        if let Some(&p) = self.marks.get(&ch) {
                            self.record_history_jump();
                            self.cursor = p.min(self.text.len());
                            if line {
                                self.cursor = self.line_start(self.cursor);
                            }
                            self.clamp();
                        }
                    }
                    Await::Object(op, n, around) => {
                        let fresh_visual =
                            op == 'v' && self.mode == Mode::Visual && self.anchor == self.cursor;
                        let eligible = matches!(op, 'd' | 'c' | 'y') || fresh_visual;
                        let visual_whitespace = if fresh_visual && around {
                            self.visual_around_trailing_whitespace(ch, n)
                        } else {
                            None
                        };
                        let simple_multiline = if eligible {
                            self.two_line_word_object(ch, n)
                        } else {
                            None
                        };
                        let blank_around = if around && eligible {
                            self.word_object_around_empty_line(ch, n)
                        } else {
                            None
                        };
                        let around_multiline = if around && eligible {
                            self.two_line_around_word_object(ch, n)
                                .or_else(|| blank_around.clone())
                        } else {
                            None
                        };
                        let whole_around = around_multiline.as_ref().is_some_and(|range| {
                            self.text[self.line_start(range.start)..range.start]
                                .iter()
                                .all(|ch| matches!(ch, ' ' | '\t'))
                                && (range.end == self.line_end(range.end)
                                    || (blank_around.is_some()
                                        && self.text[range.end..self.line_end(range.end)]
                                            .iter()
                                            .all(|ch| matches!(ch, ' ' | '\t'))))
                        });
                        let empty_line_multiline = if !around && eligible {
                            self.word_object_with_empty_next_line(ch, n)
                        } else {
                            None
                        };
                        let is_empty_line_object = empty_line_multiline.is_some();
                        let linewise = is_empty_line_object
                            || (op == 'd' && (simple_multiline.is_some() || whole_around));
                        let inner_multiline = if !around && eligible {
                            self.two_line_inner_word_object(ch, n)
                        } else {
                            None
                        };
                        if let Some(mut range) = visual_whitespace
                            .or(empty_line_multiline)
                            .or(simple_multiline)
                            .or(around_multiline)
                            .or(inner_multiline)
                            .or_else(|| self.text_object(ch, n, around))
                        {
                            if linewise {
                                range.start = self.line_start(range.start);
                                if op == 'd' && blank_around.is_some() && whole_around {
                                    range.end = self.line_end(range.end);
                                }
                                if !is_empty_line_object && self.text.get(range.end) == Some(&'\n')
                                {
                                    range.end += 1;
                                }
                            }
                            if is_empty_line_object && op == 'y' {
                                self.cursor = range.start;
                            }
                            if op == 'v' {
                                if is_empty_line_object {
                                    let Some(end) = self.grapheme_move(range.end, 1, true, true)
                                    else {
                                        self.message = "Textobject Unicode fuera de límites".into();
                                        return Effect::None;
                                    };
                                    range.end = end;
                                }
                                self.anchor = range.start;
                                let last = range.end.saturating_sub(1);
                                self.cursor = self.grapheme_floor(last).unwrap_or(last);
                                self.mode = Mode::Visual;
                            } else {
                                self.operate(op, range, linewise);
                                if linewise && op == 'd' {
                                    let mut start = self.line_start(self.cursor);
                                    if start == self.text.len() && start > 0 {
                                        start = self.line_start(start - 1);
                                    }
                                    if let Some(position) = self.grapheme_offset_at_column(
                                        start,
                                        if is_empty_line_object {
                                            0
                                        } else {
                                            self.operator_goal_column.unwrap_or(0)
                                        },
                                    ) {
                                        self.cursor = position;
                                        self.clamp();
                                    }
                                }
                            }
                        } else {
                            // Neovim leaves the cursor at the final reachable character
                            // when a word-object count cannot be completed.
                            if op == 'v'
                                && around
                                && matches!(ch, 'w' | 'W')
                                && self.text[self.cursor..self.line_end(self.cursor)]
                                    .iter()
                                    .all(|ch| ch.is_whitespace())
                            {
                                self.cursor = self.line_end(self.cursor);
                            } else if matches!(ch, 'w' | 'W')
                                && self.word_class_at(self.cursor, ch == 'W').is_some()
                                && !self.text.is_empty()
                                && self.text.iter().filter(|ch| **ch == '\n').count() <= 1
                                && (self.text.last() == Some(&'\n') || !self.text.contains(&'\n'))
                            {
                                let end = self.text.len()
                                    - 1
                                    - usize::from(
                                        self.text.last() == Some(&'\n') && self.text.len() > 1,
                                    );
                                if let Some(position) = self.grapheme_floor(end) {
                                    self.cursor = position;
                                }
                            }
                            self.message = "Textobject no encontrado".into();
                        }
                    }
                }
            }
            self.finish_change();
            return Effect::None;
        }
        if matches!(
            self.mode,
            Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        ) && self.pending.is_empty()
        {
            let n = self.count.max(1);
            let wait = match key {
                "\"" => Some(Await::Register),
                "q" => Some(Await::Macro),
                "@" => Some(Await::Play(n)),
                "r" => Some(Await::Replace(n)),
                "f" | "F" | "t" | "T" => Some(Await::Find(key.chars().next().unwrap(), n, None)),
                "m" => Some(Await::Mark),
                "'" | "`" => Some(Await::Jump(key == "'")),
                "i" | "a" if self.mode != Mode::Normal => Some(Await::Object('v', n, key == "a")),
                _ => None,
            };
            if let Some(wait) = wait {
                self.count = 0;
                self.awaiting = Some(wait);
                return Effect::None;
            }
            if key == "." {
                self.count = 0;
                self.change_events.clear();
                return self.play(self.registers.replay().change(), n);
            }
            if matches!(key, ";" | ",") {
                if let Some(p) = self.repeat_find_position(key, n) {
                    self.cursor = p;
                    self.clamp();
                }
                self.count = 0;
                self.finish_change();
                return Effect::None;
            }
        }
        if self.mode == Mode::Visual
            && self.selection().is_none()
            && matches!(
                key,
                "d" | "c"
                    | "y"
                    | "x"
                    | "D"
                    | "C"
                    | "X"
                    | "Y"
                    | "S"
                    | "R"
                    | "u"
                    | "U"
                    | "~"
                    | ">"
                    | "<"
                    | "p"
                    | "P"
            )
        {
            self.pending.clear();
            self.chosen_register = None;
            self.message = "Selección Unicode fuera del límite; texto conservado".into();
            return Effect::None;
        }
        if self.visual_key(key) {
            self.finish_change();
            return Effect::None;
        }
        if self.mode == Mode::Normal
            && self.awaiting.is_none()
            && key == "g"
            && matches!(self.pending.chars().next(), Some('d' | 'c' | 'y'))
            && !self.pending.ends_with('g')
        {
            let op = self.pending.chars().next().unwrap();
            let previous = self.pending[1..].parse::<usize>().unwrap_or(1);
            let count = previous
                .saturating_mul(std::mem::take(&mut self.count).max(1))
                .min(9999);
            self.pending = format!("{op}{count}g");
            return Effect::None;
        }
        if self.mode == Mode::Normal
            && self.awaiting.is_none()
            && key == "g"
            && self.pending.ends_with('g')
            && matches!(self.pending.chars().next(), Some('d' | 'c' | 'y'))
        {
            let pending = std::mem::take(&mut self.pending);
            let after_prefix = std::mem::take(&mut self.count);
            if after_prefix == 0 {
                let op = pending.chars().next().unwrap();
                let count = pending[1..pending.len() - 1]
                    .parse::<usize>()
                    .unwrap_or(1)
                    .clamp(1, 9999);
                let last = self.line_start(self.text.len().saturating_sub(1));
                self.operate_line_motion(op, self.line_number(count).min(last));
            } else {
                self.chosen_register = None;
            }
            self.finish_change();
            return Effect::None;
        }
        if !self.wrap_input
            && self.mode == Mode::Normal
            && self.awaiting.is_none()
            && matches!(key, "j" | "k")
            && self.pending.ends_with('g')
            && matches!(self.pending.chars().next(), Some('d' | 'c' | 'y'))
        {
            let pending = std::mem::take(&mut self.pending);
            let after_prefix = std::mem::take(&mut self.count);
            let op = pending.chars().next().unwrap();
            let count = pending[1..pending.len() - 1]
                .parse::<usize>()
                .unwrap_or(1)
                .clamp(1, 9999);
            if after_prefix == 0
                && let Some(target) = self.motion(key, count)
                && self.source_line_at(target) != self.source_line_at(self.cursor)
            {
                self.operate_line_motion(op, target);
            } else {
                self.chosen_register = None;
            }
            self.finish_change();
            return Effect::None;
        }
        if self.wrap_input
            && self.awaiting.is_none()
            && matches!(
                self.mode,
                Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            )
            && matches!(key, "j" | "k" | "<Down>" | "<Up>")
        {
            if self.mode == Mode::Normal
                && self.pending.ends_with('g')
                && matches!(self.pending.chars().next(), Some('d' | 'c' | 'y'))
                && matches!(key, "j" | "k")
            {
                let pending = std::mem::take(&mut self.pending);
                let after_prefix = std::mem::take(&mut self.count);
                if after_prefix > 0 {
                    self.finish_change();
                    return Effect::None;
                }
                let op = pending.chars().next().unwrap();
                let count = pending[1..pending.len() - 1]
                    .parse::<usize>()
                    .unwrap_or(1)
                    .clamp(1, 9999);
                return Effect::WrappedOperator(op, key == "j", count);
            }
            if self.pending.starts_with('g') && matches!(key, "j" | "k") {
                let pending = std::mem::take(&mut self.pending);
                let after_prefix = std::mem::take(&mut self.count);
                self.finish_change();
                if after_prefix > 0 {
                    return Effect::None;
                }
                let count = pending[1..].parse::<usize>().unwrap_or(1).clamp(1, 9999);
                return Effect::WrappedMove(matches!(key, "j" | "<Down>"), count);
            }
            if self.pending.is_empty() && self.count == 0 {
                self.finish_change();
                return Effect::WrappedMove(matches!(key, "j" | "<Down>"), 1);
            }
        }
        let internal = self.internal;
        self.internal = true;
        let result = self.key_base(key);
        self.internal = internal;
        self.finish_change();
        result
    }
    pub(super) fn repeat_find_position(&self, key: &str, count: usize) -> Option<usize> {
        let (mut kind, ch) = self.last_find?;
        if key == "," {
            kind = match kind {
                'f' => 'F',
                'F' => 'f',
                't' => 'T',
                _ => 't',
            };
        }
        // An uncounted repeated till-motion skips an adjacent match, while
        // a count includes it in the clean Neovim 0.12.1 reference.
        let skip_adjacent = count == 1
            && matches!(kind, 't' | 'T')
            && self
                .find_char(kind, ch, 1)
                .and_then(|p| self.grapheme_floor(p))
                == Some(self.cursor);
        self.grapheme_floor(self.find_char(kind, ch, count + usize::from(skip_adjacent))?)
    }
    fn find_char(&self, kind: char, target: char, n: usize) -> Option<usize> {
        let backward = matches!(kind, 'F' | 'T');
        let found = if backward {
            (self.line_start(self.cursor)..self.cursor)
                .rev()
                .filter(|p| self.text[*p] == target)
                .nth(n - 1)
        } else {
            (self.cursor.saturating_add(1)..self.line_end(self.cursor))
                .filter(|p| self.text[*p] == target)
                .nth(n - 1)
        }?;
        match kind {
            't' => self.grapheme_move(found, 1, false, true),
            'T' => Some((found + 1).min(self.text.len())),
            _ => self.grapheme_floor(found),
        }
    }
    // Only the fully bounded, two-word case is enabled. Other multiline
    // objects remain rejected until their whitespace/empty-line behavior is
    // modeled against Neovim; destructive operations must not guess a range.
    // For two inner objects, Neovim treats the next line's initial run as
    // the second object. Whole-line spans remain with the linewise handler.
    fn two_line_inner_word_object(&self, object: char, n: usize) -> Option<Range<usize>> {
        if !matches!(object, 'w' | 'W') || n != 2 {
            return None;
        }
        let big = object == 'W';
        let cursor = self.cursor;
        let first_start = self.line_start(cursor);
        let first_end = self.line_end(cursor);
        if cursor >= first_end || self.text.get(first_end) != Some(&'\n') {
            return None;
        }
        let class = self.word_class_at(cursor, big)?;
        let mut start = cursor;
        while start > first_start && self.word_class_at(start - 1, big)? == class {
            start -= 1;
        }
        let mut end = cursor + 1;
        while end < first_end && self.word_class_at(end, big)? == class {
            end += 1;
        }
        if end != first_end {
            return None;
        }
        let second_start = first_end + 1;
        let second_end = self.line_end(second_start);
        if second_start == second_end {
            return None;
        }
        let next_class = self.word_class_at(second_start, big)?;
        let mut finish = second_start + 1;
        while finish < second_end && self.word_class_at(finish, big)? == next_class {
            finish += 1;
        }
        if start == first_start && finish == second_end {
            return None;
        }
        Some(start..finish)
    }
    // A bounded two-word around-object spanning a newline. Empty lines,
    // non-ASCII whitespace and larger counts use the safe fallback.
    fn two_line_around_word_object(&self, object: char, n: usize) -> Option<Range<usize>> {
        if !matches!(object, 'w' | 'W') || n != 2 {
            return None;
        }
        let big = object == 'W';
        let cursor = self.cursor;
        let first_start = self.line_start(cursor);
        let first_end = self.line_end(cursor);
        if cursor >= first_end || self.text.get(first_end) != Some(&'\n') {
            return None;
        }
        let class = self.word_class_at(cursor, big)?;
        if class == 0 {
            return None;
        }
        let mut start = cursor;
        while start > first_start && self.word_class_at(start - 1, big)? == class {
            start -= 1;
        }
        if self.text[first_start..start]
            .iter()
            .any(|c| c.is_whitespace() && !matches!(c, ' ' | '\t'))
        {
            return None;
        }
        let mut end = cursor + 1;
        while end < first_end && self.word_class_at(end, big)? == class {
            end += 1;
        }
        while end < first_end && self.word_class_at(end, big)? == 0 {
            end += 1;
        }
        if end != first_end {
            return None;
        }
        let second_start = first_end + 1;
        let second_end = self.line_end(second_start);
        let mut finish = second_start;
        while finish < second_end && self.word_class_at(finish, big)? == 0 {
            finish += 1;
        }
        if finish == second_end {
            return None;
        }
        let has_indent = finish > second_start;
        let next_class = self.word_class_at(finish, big)?;
        while finish < second_end && self.word_class_at(finish, big)? == next_class {
            finish += 1;
        }
        let word_end = finish;
        if !has_indent {
            while finish < second_end && self.word_class_at(finish, big)? == 0 {
                finish += 1;
            }
        }
        if finish == word_end
            && self.text[first_start..start]
                .iter()
                .any(|c| !c.is_whitespace())
        {
            while start > first_start && self.word_class_at(start - 1, big)? == 0 {
                start -= 1;
            }
        }
        Some(start..finish)
    }
    // For iw across a nonempty one-word line and a following empty line,
    // Neovim uses a linewise register even for yank. Require a following
    // nonempty line so an end-of-file newline cannot be mistaken for a row.
    // A counted around-object includes the next word past one blank line.
    // Bound the first line to a single class, and include indentation and the
    // first word on the third line. Further text remains outside the object.
    fn word_object_around_empty_line(&self, object: char, n: usize) -> Option<Range<usize>> {
        if !matches!(object, 'w' | 'W') || n != 2 {
            return None;
        }
        let big = object == 'W';
        let start = self.line_start(self.cursor);
        let first_end = self.line_end(start);
        if first_end == start
            || self.text.get(first_end) != Some(&'\n')
            || self.text.get(first_end + 1) != Some(&'\n')
        {
            return None;
        }
        let third_start = first_end + 2;
        let third_end = self.line_end(third_start);
        if third_start == third_end {
            return None;
        }
        let class = self.word_class_at(start, big)?;
        if class == 0 || (start..first_end).any(|p| self.word_class_at(p, big) != Some(class)) {
            return None;
        }
        let mut finish = third_start;
        while finish < third_end && self.word_class_at(finish, big)? == 0 {
            finish += 1;
        }
        let class = self.word_class_at(finish, big)?;
        if class == 0 {
            return None;
        }
        while finish < third_end && self.word_class_at(finish, big)? == class {
            finish += 1;
        }
        Some(start..finish)
    }
    fn word_object_with_empty_next_line(&self, object: char, n: usize) -> Option<Range<usize>> {
        if !matches!(object, 'w' | 'W') || n != 2 {
            return None;
        }
        let big = object == 'W';
        let start = self.line_start(self.cursor);
        let end = self.line_end(start);
        if end == start
            || self.text.get(end) != Some(&'\n')
            || self.text.get(end + 1) != Some(&'\n')
        {
            return None;
        }
        let following = end + 2;
        if self.text.get(following).is_none_or(|ch| *ch == '\n') {
            return None;
        }
        let class = self.word_class_at(start, big)?;
        if class == 0
            || (start..end).any(|position| self.word_class_at(position, big) != Some(class))
        {
            return None;
        }
        Some(start..following)
    }
    // Visual 2aw from trailing whitespace may cross the newline and select
    // two whitespace-separated classes on the next line. Other shapes need
    // their own Neovim reference before widening this bounded case.
    fn visual_around_trailing_whitespace(&self, object: char, n: usize) -> Option<Range<usize>> {
        if !matches!(object, 'w' | 'W') || n != 2 {
            return None;
        }
        let big = object == 'W';
        let first_start = self.line_start(self.cursor);
        let first_end = self.line_end(self.cursor);
        if self.cursor >= first_end
            || self.text[first_start..self.cursor]
                .iter()
                .all(|c| c.is_whitespace())
            || self.text[self.cursor..first_end]
                .iter()
                .any(|c| !matches!(c, ' ' | '\t'))
            || self.text.get(first_end) != Some(&'\n')
        {
            return None;
        }
        let mut start = self.cursor;
        while start > first_start && matches!(self.text[start - 1], ' ' | '\t') {
            start -= 1;
        }
        let second_start = first_end + 1;
        let second_end = self.line_end(second_start);
        let mut finish = second_start;
        while finish < second_end && self.word_class_at(finish, big)? == 0 {
            finish += 1;
        }
        if finish == second_end {
            return None;
        }
        let first_class = self.word_class_at(finish, big)?;
        while finish < second_end && self.word_class_at(finish, big)? == first_class {
            finish += 1;
        }
        let first_word_end = finish;
        while finish < second_end && self.word_class_at(finish, big)? == 0 {
            finish += 1;
        }
        if finish == first_word_end || finish == second_end {
            return None;
        }
        let second_class = self.word_class_at(finish, big)?;
        while finish < second_end && self.word_class_at(finish, big)? == second_class {
            finish += 1;
        }
        Some(start..finish)
    }
    fn two_line_word_object(&self, object: char, n: usize) -> Option<Range<usize>> {
        if !matches!(object, 'w' | 'W') || n != 2 {
            return None;
        }
        let big = object == 'W';
        let first_start = self.line_start(self.cursor);
        let first_end = self.line_end(first_start);
        if first_end == first_start || self.text.get(first_end) != Some(&'\n') {
            return None;
        }
        let second_start = first_end + 1;
        let second_end = self.line_end(second_start);
        if second_end == second_start {
            return None;
        }
        for mut range in [first_start..first_end, second_start..second_end] {
            let class = self.word_class_at(range.start, big)?;
            if class == 0 || range.any(|position| self.word_class_at(position, big) != Some(class))
            {
                return None;
            }
        }
        Some(first_start..second_end)
    }
    fn text_object(&self, object: char, n: usize, around: bool) -> Option<Range<usize>> {
        if object == 'g' {
            let mut start = 0;
            let mut end = self.text.len();
            if !around {
                while start < end
                    && self.text[self.line_start(start)..self.line_end(start)]
                        .iter()
                        .all(|c| c.is_whitespace())
                {
                    let next = (self.line_end(start) + 1).min(end);
                    if next == start {
                        break;
                    }
                    start = next;
                }
                while end > start {
                    let prev = self.line_start(end.saturating_sub(1));
                    if self.text[prev..end].iter().all(|c| c.is_whitespace()) {
                        end = prev;
                    } else {
                        end = self.line_end(prev);
                        break;
                    }
                }
            }
            return Some(start..end);
        }
        if object == 'd' {
            let mut start = self.cursor.min(self.text.len());
            let mut end = start;
            while start > 0 && self.text[start - 1].is_ascii_digit() {
                start -= 1;
            }
            while end < self.text.len() && self.text[end].is_ascii_digit() {
                end += 1;
            }
            return (start < end).then_some(start..end);
        }
        if object == 'f' && self.csv_enabled {
            return self.csv.as_ref()?.object(self.cursor, n, around);
        }
        let len = self.text.len();
        if len == 0 {
            return None;
        }
        let p = self.cursor.min(len - 1);
        if matches!(object, 'w' | 'W') {
            let mut start = p;
            let mut end = p + 1;
            let line_start = self.line_start(p);
            let line_end = self.line_end(p);
            if p >= line_end {
                return None;
            }
            let c = self.word_class_at(p, object == 'W')?;
            while start > line_start && self.word_class_at(start - 1, object == 'W')? == c {
                start -= 1;
            }
            while end < line_end && self.word_class_at(end, object == 'W')? == c {
                end += 1;
            }
            if around && c == 0 && end == line_end {
                return None;
            }
            if around && c == 0 && end < line_end {
                let next_class = self.word_class_at(end, object == 'W')?;
                while end < line_end && self.word_class_at(end, object == 'W')? == next_class {
                    end += 1;
                }
            }
            for _ in 1..n {
                if around {
                    while end < line_end && self.text[end].is_whitespace() {
                        end += 1;
                    }
                }
                if end >= line_end {
                    return None;
                }
                let c = self.word_class_at(end, object == 'W')?;
                while end < line_end && self.word_class_at(end, object == 'W')? == c {
                    end += 1;
                }
            }
            if around && c != 0 {
                let old = end;
                while end < line_end && self.text[end].is_whitespace() {
                    end += 1;
                }
                if old == end
                    && self.text[line_start..start]
                        .iter()
                        .any(|ch| !ch.is_whitespace())
                {
                    while start > 0
                        && self.text[start - 1] != '\n'
                        && self.text[start - 1].is_whitespace()
                    {
                        start -= 1;
                    }
                }
            }
            return Some(start..end);
        }
        if object == 'p' {
            let mut start = self.line_start(p);
            let mut end = self.line_end(p);
            while start > 0 {
                let prev = self.line_start(start - 1);
                if self.text[prev..start - 1].iter().all(|c| c.is_whitespace()) {
                    break;
                }
                start = prev;
            }
            while end < len {
                let next = (end + 1).min(len);
                let next_end = self.line_end(next);
                if self.text[next..next_end].iter().all(|c| c.is_whitespace()) {
                    break;
                }
                end = next_end;
            }
            if around {
                while end < len && self.text[end].is_whitespace() {
                    end += 1;
                }
            }
            return Some(start..end);
        }
        let (open, close) = match object {
            '(' | ')' | 'b' => ('(', ')'),
            '[' | ']' => ('[', ']'),
            '{' | '}' | 'B' => ('{', '}'),
            '<' | '>' => ('<', '>'),
            '\'' | '"' | '`' => (object, object),
            _ => return None,
        };
        if open == close {
            let start_line = self.line_start(p);
            let end_line = self.line_end(p);
            let quotes = (start_line..end_line)
                .filter(|i| self.text[*i] == open && (*i == 0 || self.text[*i - 1] != '\\'))
                .collect::<Vec<_>>();
            let pair = quotes
                .chunks(2)
                .find(|pair| pair.len() == 2 && pair[0] <= p && p <= pair[1])?;
            return Some((pair[0] + usize::from(!around))..(pair[1] + usize::from(around)));
        }
        let mut nesting = 0;
        let mut candidates = 0;
        for start in (0..=p).rev() {
            if self.text[start] == close && start != p {
                nesting += 1;
            }
            if self.text[start] == open {
                if nesting > 0 {
                    nesting -= 1;
                    continue;
                }
                let mut depth = 1;
                for end in start + 1..len {
                    if self.text[end] == open {
                        depth += 1;
                    }
                    if self.text[end] == close {
                        depth -= 1;
                        if depth == 0 {
                            if end >= p {
                                candidates += 1;
                                if candidates == n {
                                    return Some(
                                        (start + usize::from(!around))..(end + usize::from(around)),
                                    );
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(e: &mut Editor, keys: &[&str]) {
        for key in keys {
            e.key(key);
        }
    }
    #[test]
    fn jump_history_keys_route_only_from_normal_with_count() {
        let mut editor = Editor::new("alpha\nbeta".into());
        assert_eq!(editor.key("<C-o>"), Effect::JumpHistory(true, 1));
        editor.key("3");
        assert_eq!(editor.key("<C-i>"), Effect::JumpHistory(false, 3));
        assert_eq!(editor.key("<Tab>"), Effect::JumpHistory(false, 1));
        editor.key("v");
        assert!(!matches!(editor.key("<C-o>"), Effect::JumpHistory(..)));
        editor.key("<Esc>");
        editor.key("i");
        assert!(!matches!(editor.key("<Tab>"), Effect::JumpHistory(..)));
    }
    #[test]
    fn typed_pairs_literal_paste_and_buffer_object_preserve_modes() {
        let mut e = Editor::new(String::new());
        e.key("i");
        e.insert("(");
        assert_eq!(e.contents(), "()");
        assert_eq!(e.cursor, 1);
        e.insert(")");
        assert_eq!(e.contents(), "()");
        assert_eq!(e.cursor, 2);
        e.key("<BS>");
        e.key("<BS>");
        e.paste("(");
        assert_eq!(e.contents(), "(");
        let mut e = Editor::new(String::new());
        e.key("i");
        e.insert("[");
        e.key("<BS>");
        assert_eq!(e.contents(), "");
        let mut e = Editor::new("\n  word\n\n".into());
        e.key("d");
        e.key("i");
        e.key("g");
        assert_eq!(e.contents(), "\n\n\n");
    }
    #[test]
    fn macro_yields_at_save_and_resumes_after_ack() {
        let mut e = Editor::new("abc".into());
        e.registers
            .replay()
            .set_macro(
                'a',
                vec![
                    EditEvent::Key("i".into()),
                    EditEvent::Text("X".into()),
                    EditEvent::Key("<Esc>".into()),
                    EditEvent::Key(":".into()),
                    EditEvent::Text("w".into()),
                    EditEvent::Key("<Enter>".into()),
                    EditEvent::Key("i".into()),
                    EditEvent::Text("Y".into()),
                    EditEvent::Key("<Esc>".into()),
                ],
            )
            .unwrap();
        e.key("@");
        assert_eq!(e.key("a"), Effect::Save);
        assert_eq!(e.contents(), "Xabc");
        e.saving = true;
        e.advance_replay();
        assert_eq!(e.contents(), "Xabc");
        e.saving = false;
        for _ in 0..100 {
            e.advance_replay();
            if !e.replaying() {
                break;
            }
        }
        assert_eq!(e.contents(), "YXabc");
        assert!(!e.replaying());
        e.registers
            .replay()
            .set_macro(
                'b',
                vec![EditEvent::Key("@".into()), EditEvent::Key("b".into())],
            )
            .unwrap();
        e.key("@");
        e.key("b");
        for _ in 0..100 {
            e.advance_replay();
            if !e.replaying() {
                break;
            }
        }
        assert!(!e.replaying());
    }
    #[test]
    fn wrapped_macro_yields_before_edit_and_resumes_in_focused_window() {
        let mut e = Editor::new("abcdefghij".into());
        e.registers
            .replay()
            .set_macro(
                'a',
                vec![
                    EditEvent::Key("3".into()),
                    EditEvent::Key("g".into()),
                    EditEvent::Key("j".into()),
                    EditEvent::Key("i".into()),
                    EditEvent::Text("X".into()),
                    EditEvent::Key("<Esc>".into()),
                ],
            )
            .unwrap();
        e.key_with_wrap("@");
        assert_eq!(e.key_with_wrap("a"), Effect::WrappedMove(true, 3));
        assert_eq!(e.contents(), "abcdefghij");
        assert!(e.replaying());
        assert!(e.commit_wrapped_motion(6));
        while e.replaying() {
            assert_eq!(e.advance_replay_with_wrap(), Effect::None);
        }
        assert_eq!(e.contents(), "abcdefXghij");
    }
    #[test]
    fn wrapped_replay_can_be_cancelled_before_later_edits() {
        let mut e = Editor::new("abcdefghij".into());
        e.registers
            .replay()
            .set_macro(
                'a',
                vec![
                    EditEvent::Key("j".into()),
                    EditEvent::Key("i".into()),
                    EditEvent::Text("X".into()),
                ],
            )
            .unwrap();
        e.key_with_wrap("@");
        assert_eq!(e.key_with_wrap("a"), Effect::WrappedMove(true, 1));
        e.cancel_replay();
        assert_eq!(e.advance_replay_with_wrap(), Effect::None);
        assert_eq!(e.contents(), "abcdefghij");
    }
    #[test]
    fn nested_wrapped_macro_waits_for_movement_before_inserting() {
        let mut e = Editor::new("abcdefghijabcdefghijtail".into());
        e.registers
            .replay()
            .set_macro(
                'b',
                vec![EditEvent::Key("g".into()), EditEvent::Key("j".into())],
            )
            .unwrap();
        e.registers
            .replay()
            .set_macro(
                'a',
                vec![
                    EditEvent::Key("@".into()),
                    EditEvent::Key("b".into()),
                    EditEvent::Key("i".into()),
                    EditEvent::Text("X".into()),
                    EditEvent::Key("<Esc>".into()),
                ],
            )
            .unwrap();
        e.key_with_wrap("@");
        assert_eq!(e.key_with_wrap("a"), Effect::WrappedMove(true, 1));
        assert_eq!(e.contents(), "abcdefghijabcdefghijtail");
        assert!(e.commit_wrapped_motion(20));
        while e.replaying() {
            assert_eq!(e.advance_replay_with_wrap(), Effect::None);
        }
        assert_eq!(e.contents(), "abcdefghijabcdefghijXtail");
    }
    #[test]
    fn repeat_survives_undo_and_csv_textobject_is_character_safe() {
        let mut e = Editor::new("alpha beta".into());
        e.key("c");
        e.key("i");
        e.key("w");
        e.insert("x");
        e.key("<Esc>");
        e.key("u");
        e.key("w");
        e.key(".");
        assert_eq!(e.contents(), "alpha x");
        let mut e = Editor::new("name,note\nñ,\"a,b\"\n".into());
        e.toggle_csv(Path::new("a.csv"));
        e.goto(1, 3);
        e.key("c");
        e.key("i");
        e.key("f");
        e.insert("z");
        e.key("<Esc>");
        assert_eq!(e.contents(), "name,note\nñ,\"z\"\n");
    }
    #[test]
    fn repeat_insert_operator_and_register() {
        let mut e = Editor::new("uno dos tres".into());
        keys(&mut e, &["c", "w"]);
        e.insert("X");
        keys(&mut e, &["<Esc>", "w", "."]);
        assert_eq!(e.contents(), "X X tres");
        keys(&mut e, &["0", "\"", "a", "y", "w", "$", "\"", "a", "p"]);
        assert!(e.contents().ends_with("X "));
    }
    #[test]
    fn macros_record_text_and_replay_without_recursive_hang() {
        let mut e = Editor::new("a\nb\nc".into());
        keys(&mut e, &["q", "a", "I"]);
        e.insert("# ");
        keys(&mut e, &["<Esc>", "j", "q", "2", "@", "a"]);
        assert_eq!(e.contents(), "# a\n# b\n# c");
    }
    #[test]
    fn nested_textobjects_and_character_find() {
        let mut e = Editor::new("call(one, (two)) tail".into());
        e.cursor = 12;
        keys(&mut e, &["d", "i", "("]);
        assert_eq!(e.contents(), "call(one, ()) tail");
        keys(&mut e, &["0", "f", ",", "r", ";"]);
        assert_eq!(e.contents(), "call(one; ()) tail");
    }
}
