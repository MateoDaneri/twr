use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::BTreeMap};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Context {
    #[default]
    TerminalInput,
    TerminalNormal,
    Workspace,
    Tree,
    PickerInput,
    PickerNormal,
    Agents,
    Backlog,
    Details,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub context: Context,
    pub sequence: Vec<String>,
    pub action: String,
    pub source: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Route<'a> {
    /// Ordinary terminal input is borrowed; the renderer must not re-encode it.
    Pty(Cow<'a, [u8]>),
    Action(String),
    Pending,
    Unhandled,
}

#[derive(Default, Debug)]
struct Node {
    action: Option<String>,
    children: BTreeMap<String, Node>,
}

#[derive(Default, Debug)]
pub struct KeyRouter {
    roots: BTreeMap<Context, Node>,
    context: Context,
    pending: Vec<String>,
    pending_bytes: Vec<u8>,
}

impl KeyRouter {
    pub fn compile(bindings: &[Binding]) -> Result<Self, String> {
        if bindings.len() > 4096 {
            return Err("keymap exceeds 4096 bindings".into());
        }
        let mut effective = BTreeMap::new();
        for global in bindings
            .iter()
            .filter(|binding| binding.context == Context::Workspace)
        {
            for context in [
                Context::Tree,
                Context::TerminalNormal,
                Context::Agents,
                Context::Backlog,
                Context::Details,
                Context::PickerNormal,
            ] {
                let mut inherited = global.clone();
                inherited.context = context;
                effective.insert((context, inherited.sequence.clone()), inherited);
            }
        }
        let mut explicit = std::collections::BTreeSet::new();
        for binding in bindings {
            let key = (binding.context, binding.sequence.clone());
            if !explicit.insert(key.clone()) {
                return Err("duplicate explicit key binding".into());
            }
            effective.insert(key, binding.clone());
        }
        let mut router = Self::default();
        for binding in effective.values() {
            if binding.sequence.is_empty()
                || binding.sequence.len() > 8
                || binding
                    .sequence
                    .iter()
                    .any(|key| key.is_empty() || key.len() > 48)
                || binding.action.is_empty()
                || binding.action.len() > 96
                || !binding
                    .action
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err("invalid key binding".into());
            }
            if binding.context == Context::TerminalInput {
                let keys: Vec<_> = binding.sequence.iter().map(String::as_str).collect();
                let native_navigation = matches!(
                    keys.as_slice(),
                    ["<C-h>"]
                        | ["<C-j>"]
                        | ["<C-k>"]
                        | ["<C-l>"]
                        | ["<C-/>"]
                        | ["<Esc>", "<Esc>"]
                        | ["<C-\\>", "<C-n>"]
                );
                if !native_navigation {
                    return Err(
                        "terminal override is not in the verified navigation contract".into(),
                    );
                }
            }
            let mut node = router.roots.entry(binding.context).or_default();
            for key in &binding.sequence {
                if node.action.is_some() {
                    return Err("ambiguous key prefix requires an explicit timeout policy".into());
                }
                node = node.children.entry(key.clone()).or_default();
            }
            if node.action.is_some() || !node.children.is_empty() {
                return Err("duplicate or ambiguous key binding".into());
            }
            node.action = Some(binding.action.clone());
        }
        Ok(router)
    }

    /// Flush to the OLD terminal target before changing focus. No implicit globals.
    pub fn set_context(&mut self, context: Context) -> Option<Vec<u8>> {
        if context == self.context {
            return None;
        }
        let flushed = (self.context == Context::TerminalInput && !self.pending_bytes.is_empty())
            .then(|| std::mem::take(&mut self.pending_bytes));
        self.pending.clear();
        self.pending_bytes.clear();
        self.context = context;
        flushed
    }

    pub fn context(&self) -> Context {
        self.context
    }

    /// Available next keys for the active prefix. Never exposes another input context.
    pub fn hints(&self) -> Vec<(String, Option<String>)> {
        if self.pending.is_empty() || self.context == Context::TerminalInput {
            return Vec::new();
        }
        let mut node = self.roots.get(&self.context);
        for key in &self.pending {
            node = node.and_then(|n| n.children.get(key));
        }
        node.map(|n| {
            n.children
                .iter()
                .map(|(k, v)| (k.clone(), v.action.clone()))
                .collect()
        })
        .unwrap_or_default()
    }
    /// Call when the configured mapping timeout expires. Single Escape is preserved.
    pub fn expire_prefix(&mut self) -> Option<Vec<u8>> {
        self.pending.clear();
        if self.pending_bytes.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.pending_bytes))
    }

    pub fn route<'a>(&mut self, chord: &str, bytes: &'a [u8]) -> Route<'a> {
        let terminal = self.context == Context::TerminalInput;
        let mut node = self.roots.get(&self.context);
        for key in &self.pending {
            node = node.and_then(|n| n.children.get(key));
        }
        let next = node.and_then(|n| n.children.get(chord));
        if let Some(next) = next {
            if let Some(action) = &next.action {
                let action = action.clone();
                self.pending.clear();
                self.pending_bytes.clear();
                return Route::Action(action);
            }
            self.pending.push(chord.into());
            if terminal {
                self.pending_bytes.extend_from_slice(bytes);
            }
            return Route::Pending;
        }
        self.pending.clear();
        if terminal {
            if self.pending_bytes.is_empty() {
                return Route::Pty(Cow::Borrowed(bytes));
            }
            let mut buffered = std::mem::take(&mut self.pending_bytes);
            buffered.extend_from_slice(bytes);
            return Route::Pty(Cow::Owned(buffered));
        }
        Route::Unhandled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(context: Context, keys: &[&str], action: &str) -> Binding {
        Binding {
            context,
            sequence: keys.iter().map(|s| (*s).into()).collect(),
            action: action.into(),
            source: "fixture".into(),
        }
    }
    #[test]
    fn prefix_hints_respect_context_and_disappear_after_action() {
        let mut r = KeyRouter::compile(&[
            binding(Context::Workspace, &["<Space>", "f", "f"], "files"),
            binding(Context::Agents, &["<Space>", "a"], "agent"),
        ])
        .unwrap();
        assert!(r.hints().is_empty());
        r.set_context(Context::Tree);
        r.route("<Space>", b" ");
        assert_eq!(r.hints(), vec![("f".into(), None)]);
        r.route("f", b"f");
        assert_eq!(r.hints(), vec![("f".into(), Some("files".into()))]);
        r.route("f", b"f");
        assert!(r.hints().is_empty());
        r.set_context(Context::TerminalInput);
        r.route("<Space>", b" ");
        assert!(r.hints().is_empty());
    }
    #[test]
    fn printable_and_control_bytes_reach_terminal_without_allocation() {
        let mut router =
            KeyRouter::compile(&[binding(Context::Tree, &["j"], "tree.next")]).unwrap();
        for value in [
            b"hjkl :q\r".as_slice(),
            b"\x03",
            b"\x00\xff",
            "ñ🙂".as_bytes(),
            b"\x1b[200~multi\nline\x1b[201~",
        ] {
            assert!(
                matches!(router.route("text", value), Route::Pty(Cow::Borrowed(v)) if v == value)
            );
        }
        router.set_context(Context::Tree);
        assert_eq!(router.route("j", b"j"), Route::Action("tree.next".into()));
    }
    #[test]
    fn terminal_escape_failure_and_timeout_do_not_drop_input() {
        let mut router = KeyRouter::compile(&[binding(
            Context::TerminalInput,
            &["<Esc>", "<Esc>"],
            "terminal.normal",
        )])
        .unwrap();
        assert_eq!(router.route("<Esc>", b"\x1b"), Route::Pending);
        assert_eq!(
            router.route("x", b"x"),
            Route::Pty(Cow::Owned(b"\x1bx".to_vec()))
        );
        assert_eq!(router.route("<Esc>", b"\x1b"), Route::Pending);
        assert_eq!(router.expire_prefix(), Some(vec![27]));
        assert_eq!(router.route("<Esc>", b"\x1b"), Route::Pending);
        assert_eq!(
            router.route("<Esc>", b"\x1b"),
            Route::Action("terminal.normal".into())
        );
    }
    #[test]
    fn focus_change_flushes_prefix_to_original_target() {
        let mut router = KeyRouter::compile(&[binding(
            Context::TerminalInput,
            &["<Esc>", "<Esc>"],
            "terminal.normal",
        )])
        .unwrap();
        router.route("<Esc>", b"\x1b");
        assert_eq!(router.set_context(Context::Agents), Some(vec![27]));
        assert_eq!(router.route("j", b"j"), Route::Unhandled);
    }
    #[test]
    fn unsafe_terminal_and_ambiguous_context_maps_are_rejected() {
        assert!(
            KeyRouter::compile(&[binding(Context::TerminalInput, &["j"], "tree.next")]).is_err()
        );
        assert!(
            KeyRouter::compile(&[
                binding(Context::Workspace, &["g"], "open"),
                binding(Context::Workspace, &["g", "g"], "top")
            ])
            .is_err()
        );
    }
    #[test]
    fn normal_global_maps_inherit_without_capturing_picker_or_terminal_text() {
        let mut router = KeyRouter::compile(&[
            binding(Context::Workspace, &["<Space>", "a", "a"], "agents.toggle"),
            binding(Context::Workspace, &["<C-j>"], "focus.down"),
            binding(Context::Agents, &["<C-j>"], "backlog.focus"),
        ])
        .unwrap();
        router.set_context(Context::Agents);
        assert_eq!(
            router.route("<C-j>", b"\x0a"),
            Route::Action("backlog.focus".into())
        );
        assert_eq!(router.route("<Space>", b" "), Route::Pending);
        assert_eq!(router.set_context(Context::Agents), None);
        assert_eq!(router.route("a", b"a"), Route::Pending);
        assert_eq!(
            router.route("a", b"a"),
            Route::Action("agents.toggle".into())
        );
        router.set_context(Context::PickerInput);
        assert_eq!(router.route("<Space>", b" "), Route::Unhandled);
        router.set_context(Context::TerminalInput);
        assert_eq!(
            router.route("<Space>", b" "),
            Route::Pty(Cow::Borrowed(b" "))
        );
    }
}
