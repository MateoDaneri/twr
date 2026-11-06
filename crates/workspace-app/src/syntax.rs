//! Lazy Tree-sitter highlighting. Operates only on bounded supplied text, never files.
use std::{
    collections::HashMap,
    ops::Range,
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};
pub const MAX_CHARS: usize = 128 * 1024;
pub const MAX_BYTES: usize = 512 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    C,
    Json,
    Lua,
    Hcl,
    Python,
}
impl Language {
    pub fn for_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "rs" => Some(Self::Rust),
            "c" | "h" => Some(Self::C),
            "json" => Some(Self::Json),
            "lua" => Some(Self::Lua),
            "tf" | "tfvars" | "hcl" => Some(Self::Hcl),
            "py" | "pyi" => Some(Self::Python),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Comment,
    Keyword,
    Type,
    String,
    Number,
    Function,
    Operator,
    Property,
}
#[derive(Clone, Debug)]
pub struct Span {
    pub range: Range<usize>,
    pub kind: Kind,
}
pub fn kind_at(spans: &[Span], position: usize) -> Option<Kind> {
    let index = spans
        .partition_point(|span| span.range.start <= position)
        .checked_sub(1)?;
    spans[index]
        .range
        .contains(&position)
        .then_some(spans[index].kind)
}
const NAMES: &[&str] = &[
    "comment",
    "keyword",
    "type",
    "string",
    "number",
    "boolean",
    "constant",
    "function",
    "constructor",
    "operator",
    "punctuation",
    "property",
    "tag",
    "attribute",
    "variable",
];
const KINDS: &[Option<Kind>] = &[
    Some(Kind::Comment),
    Some(Kind::Keyword),
    Some(Kind::Type),
    Some(Kind::String),
    Some(Kind::Number),
    Some(Kind::Number),
    Some(Kind::Number),
    Some(Kind::Function),
    Some(Kind::Function),
    Some(Kind::Operator),
    Some(Kind::Operator),
    Some(Kind::Property),
    Some(Kind::Type),
    Some(Kind::Property),
    None,
];
fn configuration(language: Language) -> Result<HighlightConfiguration, String> {
    let (grammar, name, query) = match language {
        Language::Rust => (
            tree_sitter_rust::LANGUAGE.into(),
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ),
        Language::C => (
            tree_sitter_c::LANGUAGE.into(),
            "c",
            tree_sitter_c::HIGHLIGHT_QUERY,
        ),
        Language::Lua => (
            tree_sitter_lua::LANGUAGE.into(),
            "lua",
            tree_sitter_lua::HIGHLIGHTS_QUERY,
        ),
        Language::Hcl => (
            tree_sitter_hcl::LANGUAGE.into(),
            "hcl",
            r#"(comment) @comment
(numeric_lit) @number
(bool_lit) @boolean
(null_lit) @constant
(string_lit) @string
(template_literal) @string
(function_call (identifier) @function)
(attribute (identifier) @property)
"#,
        ),
        Language::Python => (
            tree_sitter_python::LANGUAGE.into(),
            "python",
            tree_sitter_python::HIGHLIGHTS_QUERY,
        ),
        Language::Json => (
            tree_sitter_json::LANGUAGE.into(),
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
        ),
    };
    // No injected languages or project-provided queries are loaded.
    let mut configuration =
        HighlightConfiguration::new(grammar, name, query, "", "").map_err(|e| e.to_string())?;
    configuration.configure(NAMES);
    Ok(configuration)
}
#[derive(Default)]
struct Engine {
    folds: crate::syntax_folds::Engine,
    highlighter: Highlighter,
    configurations: HashMap<Language, HighlightConfiguration>,
}
impl Engine {
    fn highlight(
        &mut self,
        language: Language,
        source: &str,
        cancel: &AtomicUsize,
    ) -> Result<Vec<Span>, String> {
        if source.len() > MAX_BYTES || source.chars().count() > MAX_CHARS {
            return Err("Syntax: archivo sobre el presupuesto".into());
        }
        if cancel.load(Ordering::Acquire) != 0 {
            return Err("Syntax cancelado".into());
        }
        if let std::collections::hash_map::Entry::Vacant(entry) =
            self.configurations.entry(language)
        {
            entry.insert(configuration(language)?);
        }
        let configuration = &self.configurations[&language];
        let events = self
            .highlighter
            .highlight(configuration, source.as_bytes(), None, Some(cancel), |_| {
                None
            })
            .map_err(|e| e.to_string())?;
        let mut stack = Vec::new();
        let mut spans: Vec<Span> = Vec::new();
        let (mut byte, mut character) = (0, 0);
        for (count, event) in events.enumerate() {
            if count >= 60000 || spans.len() >= 20000 || cancel.load(Ordering::Relaxed) != 0 {
                return Err("Syntax: presupuesto o cancelación".into());
            }
            match event.map_err(|e| e.to_string())? {
                HighlightEvent::HighlightStart(index) => {
                    if stack.len() >= 128 {
                        return Err("Syntax: anidación excesiva".into());
                    }
                    stack.push(KINDS.get(index.0).copied().flatten());
                }
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    if start < byte
                        || end < start
                        || end > source.len()
                        || !source.is_char_boundary(start)
                        || !source.is_char_boundary(end)
                    {
                        return Err("Syntax: rango UTF-8 inválido".into());
                    }
                    character += source[byte..start].chars().count();
                    let finish = character + source[start..end].chars().count();
                    if let Some(kind) = stack.last().copied().flatten()
                        && character < finish
                    {
                        if let Some(previous) = spans.last_mut()
                            && previous.kind == kind
                            && previous.range.end == character
                        {
                            previous.range.end = finish;
                        } else {
                            spans.push(Span {
                                range: character..finish,
                                kind,
                            });
                        }
                    }
                    character = finish;
                    byte = end;
                }
            }
        }
        Ok(spans)
    }
}
pub struct Output {
    pub generation: u64,
    pub result: Result<Vec<Span>, String>,
    pub folds: Result<Vec<crate::syntax_folds::Definition>, String>,
}
struct Request {
    generation: u64,
    language: Language,
    text: String,
    cancel: Arc<AtomicUsize>,
}
struct Shared {
    stop: AtomicBool,
    pending: Mutex<Option<Request>>,
    ready: Condvar,
    output: Mutex<Option<Output>>,
    cancel: Mutex<Option<Arc<AtomicUsize>>>,
}
pub struct Worker {
    shared: Arc<Shared>,
}
impl Worker {
    pub fn new(wake: Option<async_channel::Sender<()>>) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            pending: Mutex::new(None),
            ready: Condvar::new(),
            output: Mutex::new(None),
            cancel: Mutex::new(None),
        });
        let state = shared.clone();
        std::thread::Builder::new()
            .name("syntax".into())
            .spawn(move || {
                let mut engine = Engine::default();
                loop {
                    let request = {
                        let Ok(mut pending) = state.pending.lock() else {
                            break;
                        };
                        while pending.is_none() && !state.stop.load(Ordering::Acquire) {
                            pending = match state.ready.wait(pending) {
                                Ok(p) => p,
                                Err(_) => return,
                            };
                        }
                        if state.stop.load(Ordering::Acquire) {
                            break;
                        }
                        pending.take().unwrap()
                    };
                    let folds =
                        engine
                            .folds
                            .compute(request.language, &request.text, &request.cancel);
                    let result = engine.highlight(request.language, &request.text, &request.cancel);
                    if request.cancel.load(Ordering::Acquire) == 0
                        && !state.stop.load(Ordering::Acquire)
                    {
                        if let Ok(mut output) = state.output.lock() {
                            *output = Some(Output {
                                generation: request.generation,
                                result,
                                folds,
                            });
                        }
                        if let Some(wake) = &wake {
                            let _ = wake.try_send(());
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { shared })
    }
    pub fn submit(
        &self,
        generation: u64,
        language: Language,
        text: String,
        cancel: Arc<AtomicUsize>,
    ) -> Result<(), String> {
        if text.len() > MAX_BYTES {
            return Err("Syntax: archivo sobre el presupuesto".into());
        }
        if let Some(old) = self
            .shared
            .cancel
            .lock()
            .map_err(|_| "Syntax cancel lock")?
            .replace(cancel.clone())
        {
            old.store(1, Ordering::Release);
        }
        self.shared
            .output
            .lock()
            .map_err(|_| "Syntax output lock")?
            .take();
        *self
            .shared
            .pending
            .lock()
            .map_err(|_| "Syntax queue lock")? = Some(Request {
            generation,
            language,
            text,
            cancel,
        });
        self.shared.ready.notify_one();
        Ok(())
    }
    pub fn take(&self) -> Option<Output> {
        self.shared.output.lock().ok()?.take()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Ok(flag) = self.shared.cancel.lock()
            && let Some(flag) = flag.as_ref()
        {
            flag.store(1, Ordering::Release);
        }
        // Synchronize with the wait predicate to avoid a lost stop notification.
        if let Ok(_pending) = self.shared.pending.lock() {
            self.shared.ready.notify_one();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grammars_produce_ordered_character_spans_for_unicode_and_broken_code() {
        let mut engine = Engine::default();
        let cancel = AtomicUsize::new(0);
        for (language, text) in [
            (
                Language::Rust,
                "fn main() { let s = \"ñ界\"; // comentario\n}",
            ),
            (Language::C, "int main(void) { return 42; }"),
            (Language::Lua, "local s = \"ñ界\" -- comentario\nreturn 42"),
            (
                Language::Hcl,
                "resource \"demo\" \"ñ\" {\n  count = 42\n  enabled = true # nota\n}",
            ),
            (
                Language::Python,
                "def saludar():\n    return \"ñ界\" # comentario\n",
            ),
            (Language::Json, "{\"ñ界\": true, \"n\": 42}"),
        ] {
            let spans = engine.highlight(language, text, &cancel).unwrap();
            assert!(!spans.is_empty());
            let chars: Vec<_> = text.chars().collect();
            let mut end = 0;
            for span in &spans {
                assert!(span.range.start >= end && span.range.end <= chars.len());
                end = span.range.end;
            }
            if matches!(language, Language::Rust | Language::Lua) {
                assert_eq!(kind_at(&spans, 0), Some(Kind::Keyword));
                let quoted: Vec<_> = spans
                    .iter()
                    .filter(|s| s.kind == Kind::String)
                    .map(|s| chars[s.range.clone()].iter().collect::<String>())
                    .collect();
                assert!(quoted.join("").contains("ñ界"));
            }
        }
        assert!(
            engine
                .highlight(Language::Rust, "fn { \"unterminated", &cancel)
                .is_ok()
        );
        assert!(
            engine
                .highlight(Language::Rust, &"x".repeat(MAX_BYTES + 1), &cancel)
                .is_err()
        );
        cancel.store(1, Ordering::Release);
        assert!(
            engine
                .highlight(Language::Rust, "fn f() {}", &cancel)
                .is_err()
        );
    }
    #[test]
    fn terraform_and_python_recognize_paths_colors_and_folds() {
        let cancel = AtomicUsize::new(0);
        let mut syntax = Engine::default();
        let mut folds = crate::syntax_folds::Engine::default();
        for (name, language, text, token, kind) in [
            (
                "main.tf",
                Language::Hcl,
                "resource \"test\" \"uno\" {\n  count = 42\n}\n",
                "count",
                Kind::Property,
            ),
            (
                "sample.py",
                Language::Python,
                "def run():\n    return 42\n",
                "def",
                Kind::Keyword,
            ),
        ] {
            assert_eq!(Language::for_path(Path::new(name)), Some(language));
            let spans = syntax.highlight(language, text, &cancel).unwrap();
            let position = text[..text.find(token).unwrap()].chars().count();
            assert_eq!(kind_at(&spans, position), Some(kind));
            assert!(
                !folds.compute(language, text, &cancel).unwrap().is_empty(),
                "{name}"
            );
        }
        for name in ["vars.tfvars", "config.hcl"] {
            assert_eq!(Language::for_path(Path::new(name)), Some(Language::Hcl));
        }
        assert_eq!(
            Language::for_path(Path::new("api.pyi")),
            Some(Language::Python)
        );
        for (name, language) in [
            ("MAIN.TF", Language::Hcl),
            ("script.PY", Language::Python),
            ("LIB.RS", Language::Rust),
            ("data.JSON", Language::Json),
        ] {
            assert_eq!(Language::for_path(Path::new(name)), Some(language));
        }
    }
    #[test]
    fn worker_coalesces_versions_and_cancels_replaced_requests() {
        let worker = Worker::new(None).unwrap();
        let old = Arc::new(AtomicUsize::new(0));
        worker
            .submit(1, Language::Rust, "fn old() {}".into(), old.clone())
            .unwrap();
        worker
            .submit(
                2,
                Language::Json,
                "{\n\"ñ\":42\n}".into(),
                Arc::new(AtomicUsize::new(0)),
            )
            .unwrap();
        assert_eq!(old.load(Ordering::Acquire), 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(output) = worker.take()
                && output.generation == 2
            {
                assert!(!output.result.unwrap().is_empty());
                assert_eq!(output.folds.unwrap().len(), 1);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "syntax worker deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
