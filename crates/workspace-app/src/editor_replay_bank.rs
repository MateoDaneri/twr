//! Session-wide repeat and macro state. Programs are immutable during replay.
use super::commands::EditEvent;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

const PROGRAM_BUDGET: usize = 8 * 1024 * 1024;
const RECORDING_BUDGET: usize = 1024 * 1024;
const EVENT_LIMIT: usize = 4096;

impl EditEvent {
    fn payload_bytes(&self) -> usize {
        match self {
            Self::Key(s) | Self::Text(s) | Self::Literal(s) => s.capacity(),
            Self::Completion { text, .. } => text.capacity(),
            Self::Shift { .. } | Self::Input(_) | Self::Cursor(_) => 0,
        }
    }
    pub(super) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.payload_bytes()
    }
}
fn bytes(events: &Vec<EditEvent>) -> usize {
    events.capacity() * std::mem::size_of::<EditEvent>()
        + events.iter().map(EditEvent::payload_bytes).sum::<usize>()
}
struct Recording {
    name: char,
    events: Vec<EditEvent>,
    size: usize,
    failed: bool,
}
#[derive(Default)]
struct Data {
    macros: BTreeMap<char, Arc<Vec<EditEvent>>>,
    last_macro: Option<char>,
    change: Arc<Vec<EditEvent>>,
    recording: Option<Recording>,
}
#[derive(Clone, Default)]
pub(super) struct ReplayBank(Arc<Mutex<Data>>);
impl ReplayBank {
    pub(super) fn recording(&self) -> Option<char> {
        self.0.lock().ok()?.recording.as_ref().map(|r| r.name)
    }
    pub(super) fn start(&self, name: char) -> Result<(), String> {
        let mut data = self.0.lock().map_err(|_| "Macros no disponibles")?;
        let events = if name.is_ascii_uppercase() {
            data.macros
                .get(&name.to_ascii_lowercase())
                .map(|p| p.as_ref().clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let size = bytes(&events);
        if size > RECORDING_BUDGET || events.len() > EVENT_LIMIT {
            return Err("Macro demasiado grande para continuar grabando".into());
        }
        data.recording = Some(Recording {
            name: name.to_ascii_lowercase(),
            events,
            size,
            failed: false,
        });
        Ok(())
    }
    pub(super) fn record(&self, event: &EditEvent) -> Result<(), String> {
        let mut data = self.0.lock().map_err(|_| "Macros no disponibles")?;
        if let Some(r) = &mut data.recording {
            if r.failed {
                return Ok(());
            }
            if r.events.len() == EVENT_LIMIT
                || r.size.saturating_add(event.retained_bytes()) > RECORDING_BUDGET
            {
                r.failed = true;
                return Err("Límite de macro; se conservará la grabación anterior".into());
            }
            r.size += event.retained_bytes();
            r.events.push(event.clone());
        }
        Ok(())
    }
    pub(super) fn stop(&self) -> Result<char, String> {
        let recording = self
            .0
            .lock()
            .map_err(|_| "Macros no disponibles")?
            .recording
            .take()
            .ok_or("No hay macro en grabación")?;
        if recording.failed {
            return Err("Macro fuera de límites; grabación anterior conservada".into());
        }
        self.set_macro(recording.name, recording.events)?;
        Ok(recording.name)
    }
    pub(super) fn set_macro(&self, name: char, events: Vec<EditEvent>) -> Result<(), String> {
        let name = name.to_ascii_lowercase();
        let size = bytes(&events);
        let mut data = self.0.lock().map_err(|_| "Macros no disponibles")?;
        let retained = data
            .macros
            .iter()
            .filter(|(key, _)| **key != name)
            .map(|(_, p)| bytes(p))
            .sum::<usize>()
            + bytes(&data.change);
        if events.len() > EVENT_LIMIT
            || size > RECORDING_BUDGET
            || retained.saturating_add(size) > PROGRAM_BUDGET
        {
            return Err(
                "Macros fuera del presupuesto de memoria; registro anterior conservado".into(),
            );
        }
        data.macros.insert(name, Arc::new(events));
        Ok(())
    }
    pub(super) fn text_program(text: &str) -> Result<Arc<Vec<EditEvent>>, String> {
        if text.len() > RECORDING_BUDGET {
            return Err("Registro demasiado grande para ejecutar como macro".into());
        }
        let mut events = Vec::new();
        for ch in text.chars() {
            if events.len() == EVENT_LIMIT {
                return Err("Macro de texto supera4096 eventos; no se ejecutó".into());
            }
            let key = match ch {
                '\u{1b}' => Some("<Esc>"),
                '\r' | '\n' => Some("<Enter>"),
                '\t' => Some("<Tab>"),
                '\u{8}' | '\u{7f}' => Some("<BS>"),
                '\u{12}' => Some("<C-r>"),
                '\u{14}' => Some("<C-t>"),
                '\u{4}' => Some("<C-d>"),
                c if c.is_control() => {
                    return Err("Control no soportado en macro de texto; no se ejecutó".into());
                }
                _ => None,
            };
            events.push(if let Some(key) = key {
                EditEvent::Key(key.into())
            } else {
                EditEvent::Input(ch)
            });
        }
        if bytes(&events) > RECORDING_BUDGET {
            return Err("Macro de texto fuera del presupuesto; no se ejecutó".into());
        }
        Ok(Arc::new(events))
    }
    pub(super) fn macro_program(&self, name: char) -> Option<Arc<Vec<EditEvent>>> {
        self.0
            .lock()
            .ok()?
            .macros
            .get(&name.to_ascii_lowercase())
            .cloned()
    }
    /// Only expose a byte stream when every event has a lossless representation.
    /// Completion, semantic indentation and special Vim key encodings stay native.
    pub(super) fn macro_text(&self, name: char) -> Option<String> {
        let events = self.macro_program(name)?;
        let mut text = String::new();
        for event in events.iter() {
            match event {
                EditEvent::Input(ch) => text.push(*ch),
                EditEvent::Text(value) => text.push_str(value),
                EditEvent::Key(value) => text.push(match value.as_str() {
                    "<Esc>" => '\u{1b}',
                    "<Enter>" => '\r',
                    "<Tab>" => '\t',
                    "<BS>" => '\u{8}',
                    "<C-r>" => '\u{12}',
                    "<C-t>" => '\u{14}',
                    "<C-d>" => '\u{4}',
                    _ if value.chars().count() == 1 => value.chars().next()?,
                    _ => return None,
                }),
                _ => return None,
            }
            if text.len() > RECORDING_BUDGET {
                return None;
            }
        }
        Some(text)
    }
    pub(super) fn invalidate_macro(&self, name: char) {
        if let Ok(mut data) = self.0.lock() {
            data.macros.remove(&name.to_ascii_lowercase());
        }
    }
    pub(super) fn last_macro(&self) -> Option<char> {
        self.0.lock().ok()?.last_macro
    }
    pub(super) fn played(&self, name: char) {
        if let Ok(mut data) = self.0.lock() {
            data.last_macro = Some(name.to_ascii_lowercase());
        }
    }
    pub(super) fn set_change(&self, events: Vec<EditEvent>) -> Result<(), String> {
        let mut data = self.0.lock().map_err(|_| "Repetición no disponible")?;
        let retained = data.macros.values().map(|p| bytes(p)).sum::<usize>();
        if events.len() > EVENT_LIMIT || retained.saturating_add(bytes(&events)) > PROGRAM_BUDGET {
            data.change = Arc::default();
            return Err("Cambio fuera del presupuesto de repetición; punto desactivado".into());
        }
        data.change = Arc::new(events);
        Ok(())
    }
    pub(super) fn change(&self) -> Arc<Vec<EditEvent>> {
        self.0
            .lock()
            .map(|data| data.change.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Editor, RegisterBank};
    use super::*;
    fn keys(e: &mut Editor, input: &[&str]) {
        for key in input {
            e.key(key);
        }
        while e.replaying() {
            e.advance_replay();
        }
    }
    #[test]
    fn dot_macros_and_last_macro_follow_the_session_across_buffers() {
        // Cross-checked against nvim --clean with two synthetic buffers.
        let bank = RegisterBank::default();
        let mut a = Editor::with_registers("abc".into(), bank.clone());
        let mut b = Editor::with_registers("def".into(), bank);
        a.key("i");
        a.insert("X");
        a.key("<Esc>");
        keys(&mut b, &["."]);
        assert_eq!(b.contents(), "Xdef");
        keys(&mut a, &["q", "a", "A"]);
        a.insert("!");
        keys(&mut a, &["<Esc>", "q"]);
        keys(&mut b, &["@", "a"]);
        assert_eq!(b.contents(), "Xdef!");
        keys(&mut a, &["@", "@"]);
        assert_eq!(a.contents(), "Xabc!!");
        b.key("u");
        assert_eq!(b.contents(), "Xdef");
        assert_eq!(a.contents(), "Xabc!!");
    }
    #[test]
    fn recording_is_visible_and_can_finish_in_another_buffer() {
        let bank = RegisterBank::default();
        let mut a = Editor::with_registers("abc".into(), bank.clone());
        let mut b = Editor::with_registers("def".into(), bank.clone());
        keys(&mut a, &["q", "a", "i"]);
        a.insert("X");
        a.key("<Esc>");
        assert_eq!(b.recording_register(), Some('a'));
        b.key("i");
        b.insert("Y");
        keys(&mut b, &["<Esc>", "q"]);
        assert_eq!(a.recording_register(), None);
        let mut c = Editor::with_registers("z".into(), bank);
        keys(&mut c, &["@", "a"]);
        assert_eq!(c.contents(), "YXz");
    }
    #[test]
    fn append_recording_and_register_replacement_do_not_execute_stale_programs() {
        let bank = RegisterBank::default();
        let mut a = Editor::with_registers("a".into(), bank.clone());
        keys(&mut a, &["q", "a", "A"]);
        a.insert("!");
        keys(&mut a, &["<Esc>", "q"]);
        keys(&mut a, &["q", "A", "A"]);
        a.insert("?");
        keys(&mut a, &["<Esc>", "q"]);
        let mut b = Editor::with_registers("b".into(), bank.clone());
        keys(&mut b, &["@", "a"]);
        assert_eq!(b.contents(), "b!?");
        bank.capture('a', 'y', "x".into(), false).unwrap();
        let mut c = Editor::with_registers("c".into(), bank);
        keys(&mut c, &["@", "a"]);
        assert_eq!(c.contents(), "");
    }
    #[test]
    fn replay_moves_between_open_buffers_without_resetting_limits_or_undo() {
        use super::super::{Effect, Mode};
        let bank = RegisterBank::default();
        bank.capture('a', 'y', "A1\u{1b}LA2\u{1b}HA3\u{1b}".into(), false)
            .unwrap();
        let mut a = Editor::with_registers("a".into(), bank.clone());
        let mut b = Editor::with_registers("b".into(), bank.clone());
        let until_effect = |e: &mut Editor, mut effect: Effect| {
            for _ in 0..100 {
                if effect != Effect::None || !e.replaying() {
                    return effect;
                }
                effect = e.advance_replay();
            }
            panic!("replay did not yield")
        };
        a.key("@");
        let first = a.key("a");
        assert_eq!(
            until_effect(&mut a, first),
            Effect::Buffer("buffers.next".into())
        );
        b.resume_replay(a.take_replay().unwrap()).unwrap();
        assert!(!a.replaying());
        assert_eq!(a.contents(), "a1");
        assert_eq!(
            until_effect(&mut b, Effect::None),
            Effect::Buffer("buffers.prev".into())
        );
        a.resume_replay(b.take_replay().unwrap()).unwrap();
        assert_eq!(until_effect(&mut a, Effect::None), Effect::None);
        assert_eq!(a.contents(), "a13");
        assert_eq!(b.contents(), "b2");
        keys(&mut a, &["u"]);
        keys(&mut b, &["u"]);
        assert_eq!(a.contents(), "a1");
        assert_eq!(b.contents(), "b");
        bank.capture('a', 'y', "L@a".into(), false).unwrap();
        a.key("@");
        let mut effect = a.key("a");
        let mut on_a = true;
        for _ in 0..100 {
            let current = if on_a { &mut a } else { &mut b };
            effect = until_effect(current, effect);
            if matches!(effect, Effect::Buffer(_)) {
                let continuation = current.take_replay().unwrap();
                let target = if on_a { &mut b } else { &mut a };
                target.resume_replay(continuation).unwrap();
                on_a = !on_a;
                effect = Effect::None;
            } else {
                break;
            }
        }
        assert!(!a.replaying() && !b.replaying());
        assert!(a.message.contains("recursiva") || b.message.contains("recursiva"));
        bank.capture('a', 'y', "Lx".into(), false).unwrap();
        a.key("@");
        let first = a.key("a");
        until_effect(&mut a, first);
        let continuation = a.take_replay().unwrap();
        b.mode = Mode::Insert;
        assert!(b.resume_replay(continuation).is_err());
        assert!(!a.replaying() && !b.replaying());
        assert_eq!(b.contents(), "b");
    }
    #[test]
    fn macros_expose_lossless_text_and_append_across_text_and_recorded_programs() {
        let bank = RegisterBank::default();
        let mut a = Editor::with_registers("z".into(), bank.clone());
        keys(&mut a, &["q", "a", "A"]);
        a.insert("!");
        keys(&mut a, &["<Esc>", "q"]);
        assert_eq!(bank.get('a'), Some(("A!\u{1b}".into(), false)));
        bank.capture('A', 'y', "A?\u{1b}".into(), false).unwrap();
        assert_eq!(bank.get('a').unwrap().0, "A!\u{1b}A?\u{1b}");
        let mut b = Editor::with_registers("z".into(), bank.clone());
        keys(&mut b, &["@", "a"]);
        assert_eq!(b.contents(), "z!?");
        bank.capture('a', 'y', "iX\u{1b}".into(), false).unwrap();
        let mut c = Editor::with_registers("z".into(), bank.clone());
        keys(&mut c, &["q", "A", "A"]);
        c.insert("!");
        keys(&mut c, &["<Esc>", "q"]);
        assert_eq!(bank.get('a').unwrap().0, "iX\u{1b}A!\u{1b}");
        let mut d = Editor::with_registers("z".into(), bank.clone());
        keys(&mut d, &["@", "a"]);
        assert_eq!(d.contents(), "Xz!");
        bank.replay()
            .set_macro(
                'b',
                vec![EditEvent::Shift {
                    lines: 1,
                    count: 1,
                    right: true,
                }],
            )
            .unwrap();
        assert!(bank.get('b').is_none());
        let before = bank.replay().macro_program('b').unwrap();
        assert!(bank.capture('B', 'y', "x".into(), false).is_err());
        assert!(Arc::ptr_eq(
            &before,
            &bank.replay().macro_program('b').unwrap()
        ));
        bank.capture('c', 'y', "x\0".into(), false).unwrap();
        assert!(bank.start_macro('C').is_err());
        assert_eq!(bank.get('c').unwrap().0, "x\0");
        assert_eq!(bank.replay().recording(), None);
    }
    #[test]
    fn text_register_macros_are_unicode_counted_and_parse_controls_before_execution() {
        let bank = RegisterBank::default();
        bank.capture('a', 'y', "x".into(), false).unwrap();
        let mut e = Editor::with_registers("abc".into(), bank.clone());
        keys(&mut e, &["2", "@", "a"]);
        assert_eq!(e.contents(), "c");
        keys(&mut e, &["@", "@"]);
        assert_eq!(e.contents(), "");
        for (program, expected) in [("iñ界\u{1b}", "ñ界z"), ("A<Esc>\u{1b}", "z<Esc>")] {
            bank.capture('a', 'y', program.into(), false).unwrap();
            let mut e = Editor::with_registers("z".into(), bank.clone());
            keys(&mut e, &["@", "a"]);
            assert_eq!(e.contents(), expected);
            assert_eq!(e.mode, super::super::Mode::Normal);
        }
        for program in [
            "x\0".into(),
            "x".repeat(EVENT_LIMIT + 1),
            "x".repeat(RECORDING_BUDGET + 1),
        ] {
            bank.capture('a', 'y', program, false).unwrap();
            let mut e = Editor::with_registers("preserved".into(), bank.clone());
            keys(&mut e, &["@", "a"]);
            assert_eq!(e.contents(), "preserved");
            assert!(!e.replaying());
        }
        bank.capture('a', 'y', "@a".into(), false).unwrap();
        let mut e = Editor::with_registers("preserved".into(), bank);
        keys(&mut e, &["@", "a"]);
        assert_eq!(e.contents(), "preserved");
        assert!(e.message.contains("recursiva"));
    }
    #[test]
    fn rejected_recording_preserves_previous_macro_and_oversized_change_disables_dot() {
        let bank = ReplayBank::default();
        bank.set_macro('a', vec![EditEvent::Key("x".into())])
            .unwrap();
        let before = bank.macro_program('a').unwrap();
        bank.start('a').unwrap();
        assert!(
            bank.record(&EditEvent::Literal("x".repeat(RECORDING_BUDGET + 1)))
                .is_err()
        );
        assert!(bank.stop().is_err());
        assert!(Arc::ptr_eq(&before, &bank.macro_program('a').unwrap()));
        bank.set_change(vec![EditEvent::Key("x".into())]).unwrap();
        assert!(
            bank.set_change(vec![EditEvent::Literal("x".repeat(PROGRAM_BUDGET + 1))])
                .is_err()
        );
        assert!(bank.change().is_empty());
    }
}
