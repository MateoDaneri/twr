//! Registers shared by open buffers, kept in RAM only. Aliases share their text.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Character,
    Line,
    Block(usize),
}
type Value = (Arc<str>, Kind);
struct Data {
    values: BTreeMap<char, Value>,
    limit: usize,
    search: Option<(Arc<super::search::Pattern>, bool)>,
    search_highlight: bool,
    command_history: Vec<String>,
    search_history: Vec<String>,
}
#[derive(Clone)]
pub struct RegisterBank(Arc<Mutex<Data>>, super::replay_bank::ReplayBank);
impl Default for RegisterBank {
    fn default() -> Self {
        Self::with_limit(8 * 1024 * 1024)
    }
}
impl RegisterBank {
    /// Record already-validated linewise text from a terminal copy operation.
    /// The caller must separately validate its origin and perform any host
    /// clipboard write; this method does not interact with the operating system.
    pub fn record_terminal_lines(
        &self,
        text: String,
        clipboard_register: bool,
    ) -> Result<(), String> {
        if !text.ends_with('\n') || text.chars().any(|ch| ch.is_control() && ch != '\n') {
            return Err("Yank de terminal no representable de forma segura".into());
        }
        self.capture(if clipboard_register { '+' } else { '"' }, 'y', text, true)
    }

    fn with_limit(limit: usize) -> Self {
        Self(
            Arc::new(Mutex::new(Data {
                values: BTreeMap::new(),
                limit,
                search: None,
                search_highlight: true,
                command_history: Vec::new(),
                search_history: Vec::new(),
            })),
            Default::default(),
        )
    }
    pub(super) fn shares_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(super) fn record_prompt_history(&self, search: bool, value: &str) {
        if value.is_empty() || value.len() > 4096 {
            return;
        }
        if let Ok(mut data) = self.0.lock() {
            let items = if search {
                &mut data.search_history
            } else {
                &mut data.command_history
            };
            items.retain(|item| item != value);
            items.push(value.to_owned());
            if items.len() > 200 {
                items.remove(0);
            }
        }
    }
    pub(super) fn prompt_history(&self, search: bool) -> Vec<String> {
        self.0
            .lock()
            .map(|data| {
                if search {
                    data.search_history.clone()
                } else {
                    data.command_history.clone()
                }
            })
            .unwrap_or_default()
    }
    pub(super) fn search(&self) -> Option<(Arc<super::search::Pattern>, bool)> {
        self.0.lock().ok()?.search.clone()
    }
    pub(super) fn set_search(&self, pattern: Arc<super::search::Pattern>, reverse: bool) {
        if let Ok(mut data) = self.0.lock() {
            data.search = Some((pattern, reverse));
            data.search_highlight = true;
        }
    }
    pub(super) fn search_highlight(&self) -> bool {
        self.0.lock().is_ok_and(|data| data.search_highlight)
    }
    pub(super) fn set_search_highlight(&self, enabled: bool) {
        if let Ok(mut data) = self.0.lock() {
            data.search_highlight = enabled;
        }
    }
    pub(super) fn replay(&self) -> &super::replay_bank::ReplayBank {
        &self.1
    }
    pub(super) fn finish_macro(&self) -> Result<char, String> {
        let name = self.1.stop()?;
        // Keep the semantic program authoritative. Text reads are projected lazily
        // only when all events have a lossless byte-stream representation.
        self.0
            .lock()
            .map_err(|_| "Registros no disponibles")?
            .values
            .remove(&name);
        Ok(name)
    }
    pub(super) fn start_macro(&self, name: char) -> Result<(), String> {
        if name.is_ascii_uppercase()
            && self.1.macro_program(name).is_none()
            && let Some((text, _)) = self.get(name)
        {
            let program = super::replay_bank::ReplayBank::text_program(&text)?;
            self.1.set_macro(name, program.as_ref().clone())?;
        }
        self.1.start(name)
    }
    pub(super) fn macro_program(
        &self,
        name: char,
    ) -> Result<Option<Arc<Vec<super::EditEvent>>>, String> {
        if let Some(program) = self.1.macro_program(name) {
            return Ok(Some(program));
        }
        let value = self
            .0
            .lock()
            .map_err(|_| "Registros no disponibles")?
            .values
            .get(&name.to_ascii_lowercase())
            .map(|(text, _)| text.clone());
        value
            .map(|text| super::replay_bank::ReplayBank::text_program(&text))
            .transpose()
    }
    pub(super) fn get(&self, name: char) -> Option<(String, bool)> {
        self.get_kind(name)
            .map(|(text, kind)| (text, kind == Kind::Line))
    }
    pub(super) fn get_kind(&self, name: char) -> Option<(String, Kind)> {
        let value = self
            .0
            .lock()
            .ok()?
            .values
            .get(&name.to_ascii_lowercase())
            .map(|(s, kind)| (s.to_string(), *kind));
        value.or_else(|| self.1.macro_text(name).map(|text| (text, Kind::Character)))
    }
    pub(super) fn import_clipboard(&self, text: String, line: bool) -> Result<(), String> {
        self.import_kind(text, if line { Kind::Line } else { Kind::Character })
    }
    pub(super) fn import_kind(&self, text: String, kind: Kind) -> Result<(), String> {
        if matches!(kind, Kind::Block(0 | 2_000_001..)) {
            return Err("Ancho de registro rectangular fuera de límites".into());
        }
        let mut data = self.0.lock().map_err(|_| "Registros no disponibles")?;
        if text.len() > data.limit {
            return Err("Clipboard fuera de límites".into());
        }
        let mut next = data.values.clone();
        let value = (Arc::<str>::from(text), kind);
        next.insert('+', value.clone());
        next.insert('*', value);
        let mut seen = BTreeSet::new();
        let retained = next
            .values()
            .filter_map(|(s, _)| {
                seen.insert(Arc::as_ptr(s) as *const () as usize)
                    .then_some(s.len())
            })
            .sum::<usize>();
        if retained > data.limit {
            return Err("Clipboard excede el presupuesto de registros".into());
        }
        data.values = next;
        Ok(())
    }
    pub(super) fn capture(
        &self,
        name: char,
        op: char,
        text: String,
        line: bool,
    ) -> Result<(), String> {
        self.capture_kind(
            name,
            op,
            text,
            if line { Kind::Line } else { Kind::Character },
        )
    }
    pub(super) fn capture_kind(
        &self,
        name: char,
        op: char,
        text: String,
        kind: Kind,
    ) -> Result<(), String> {
        if matches!(kind, Kind::Block(0 | 2_000_001..)) {
            return Err("Ancho de registro rectangular fuera de límites".into());
        }
        if name == '_' {
            return Ok(());
        }
        let previous = if name.is_ascii_uppercase() {
            self.get_kind(name)
        } else {
            None
        };
        if name.is_ascii_uppercase() && previous.is_none() && self.1.macro_program(name).is_some() {
            return Err("Macro semántica sin representación de texto; registro conservado".into());
        }
        let mut data = self.0.lock().map_err(|_| "Registros no disponibles")?;
        let mut next = data.values.clone();
        let value = (Arc::<str>::from(text), kind);
        let mut unnamed = value.clone();
        if name.is_ascii_uppercase() {
            if let Some((previous, previous_kind)) = previous {
                if previous.len().saturating_add(value.0.len()) > data.limit {
                    return Err(
                        "Registros: límite de 8 MiB; se conservó el contenido anterior".into(),
                    );
                }
                let mut joined = previous.to_string();
                if matches!(previous_kind, Kind::Block(_)) && matches!(kind, Kind::Block(_)) {
                    joined.push('\n');
                }
                joined.push_str(&value.0);
                let joined_kind = if kind == Kind::Line || previous_kind == Kind::Line {
                    Kind::Line
                } else if let (Kind::Block(left), Kind::Block(right)) = (previous_kind, kind) {
                    Kind::Block(left.max(right))
                } else {
                    previous_kind
                };
                unnamed = (Arc::from(joined), joined_kind);
                next.insert(name.to_ascii_lowercase(), unnamed.clone());
            } else {
                next.insert(name.to_ascii_lowercase(), value.clone());
            }
        } else {
            next.insert(name, value.clone());
        }
        next.insert('"', unnamed);
        next.insert(if op == 'y' { '0' } else { '1' }, value);
        let mut identities = BTreeSet::new();
        let retained = next
            .values()
            .filter_map(|(s, _)| {
                // Count one allocation once even when unnamed/numbered registers alias it.
                identities
                    .insert(Arc::as_ptr(s) as *const () as usize)
                    .then_some(s.len())
            })
            .sum::<usize>();
        if retained > data.limit {
            return Err("Registros: límite de 8 MiB; se conservó el contenido anterior".into());
        }
        data.values = next;
        drop(data);
        self.1.invalidate_macro(name);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_share_budget_and_rejected_append_preserves_previous_text() {
        let bank = RegisterBank::with_limit(8);
        bank.capture('A', 'y', "12345678".into(), false).unwrap();
        assert!(bank.capture('A', 'y', "x".into(), false).is_err());
        assert_eq!(bank.get('a').unwrap().0, "12345678");
        assert_eq!(bank.get('"').unwrap().0, "12345678");
        bank.capture('_', 'd', "discarded".into(), true).unwrap();
        assert_eq!(bank.get('"').unwrap().0, "12345678");
    }
}
