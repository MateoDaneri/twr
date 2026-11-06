//! Pinned nvim-treesitter fold queries, evaluated only on supplied text in the syntax worker.
//! Row levels follow Neovim 0.12.1 runtime/lua/vim/treesitter/_fold.lua.
use crate::syntax::{Language, MAX_BYTES, MAX_CHARS};
use std::{
    collections::HashMap,
    ops::{ControlFlow, Range},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tree_sitter::{
    ParseOptions, Parser, Query, QueryCursor, QueryCursorOptions, StreamingIterator,
};
pub const MAX_FOLDS: usize = 4096;
const MAX_DEPTH: usize = 20;
const TIMEOUT: &str = "Pliegues sintácticos: tiempo agotado";
const CANCELLED: &str = "Pliegues sintácticos cancelados";

fn stop_reason(cancel: &AtomicUsize, deadline: Instant) -> Option<&'static str> {
    if cancel.load(Ordering::Relaxed) != 0 {
        Some(CANCELLED)
    } else if Instant::now() >= deadline {
        Some(TIMEOUT)
    } else {
        None
    }
}

// Initialization can consume the first deadline. Retry that deadline once,
// using the now-cached parser and query; hard limits and cancellation never retry.
fn retry_timeout<T>(
    cancel: &AtomicUsize,
    mut operation: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let first = operation();
    if matches!(&first, Err(error) if error == TIMEOUT) && cancel.load(Ordering::Acquire) == 0 {
        operation()
    } else {
        first
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub rows: Range<usize>,
    pub level: usize,
}
struct Configuration {
    parser: Parser,
    query: Query,
}
#[derive(Default)]
pub struct Engine {
    configurations: HashMap<Language, Configuration>,
}
// nvim-treesitter 4916d6592ede8c07973490d9322f187e07dfefac runtime/queries/rust/folds.scm
const RUST: &str = r#"[
  (mod_item)
  (foreign_mod_item)
  (function_item)
  (struct_item)
  (trait_item)
  (enum_item)
  (impl_item)
  (type_item)
  (union_item)
  (const_item)
  (let_declaration)
  (loop_expression)
  (for_expression)
  (while_expression)
  (if_expression)
  (match_expression)
  (call_expression)
  (array_expression)
  (macro_definition)
  (macro_invocation)
  (attribute_item)
  (block)
  (use_declaration)+
] @fold
"#;
// nvim-treesitter 4916d6592ede8c07973490d9322f187e07dfefac runtime/queries/c/folds.scm
const C: &str = r#"[
  (for_statement)
  (if_statement)
  (while_statement)
  (do_statement)
  (switch_statement)
  (case_statement)
  (function_definition)
  (struct_specifier)
  (enum_specifier)
  (comment)
  (preproc_if)
  (preproc_elif)
  (preproc_else)
  (preproc_ifdef)
  (preproc_function_def)
  (initializer_list)
  (gnu_asm_expression)
  (preproc_include)+
] @fold

(compound_statement
  (compound_statement) @fold)
"#;
// nvim-treesitter 4916d6592ede8c07973490d9322f187e07dfefac runtime/queries/lua/folds.scm
const LUA: &str = r#"[
  (do_statement)
  (while_statement)
  (repeat_statement)
  (if_statement)
  (for_statement)
  (function_declaration)
  (function_definition)
  (parameters)
  (arguments)
  (table_constructor)
] @fold
"#;
// nvim-treesitter 4916d6592ede8c07973490d9322f187e07dfefac runtime/queries/json/folds.scm
const JSON: &str = r#"[
  (pair)
  (object)
  (array)
] @fold
"#;

impl Engine {
    pub fn compute(
        &mut self,
        language: Language,
        source: &str,
        cancel: &AtomicUsize,
    ) -> Result<Vec<Definition>, String> {
        retry_timeout(cancel, || self.compute_once(language, source, cancel))
    }

    fn compute_once(
        &mut self,
        language: Language,
        source: &str,
        cancel: &AtomicUsize,
    ) -> Result<Vec<Definition>, String> {
        if source.len() > MAX_BYTES || source.chars().count() > MAX_CHARS {
            return Err("Pliegues sintácticos: archivo sobre el presupuesto".into());
        }
        let deadline = Instant::now() + Duration::from_millis(150);
        let stopped = || stop_reason(cancel, deadline).is_some();
        if let Some(reason) = stop_reason(cancel, deadline) {
            return Err(reason.into());
        }
        if let std::collections::hash_map::Entry::Vacant(entry) =
            self.configurations.entry(language)
        {
            let (grammar, query) = match language {
                Language::Rust => (tree_sitter_rust::LANGUAGE.into(), RUST),
                Language::C => (tree_sitter_c::LANGUAGE.into(), C),
                Language::Lua => (tree_sitter_lua::LANGUAGE.into(), LUA),
                Language::Json => (tree_sitter_json::LANGUAGE.into(), JSON),
                Language::Hcl => (
                    tree_sitter_hcl::LANGUAGE.into(),
                    "[(block) (object) (tuple)] @fold",
                ),
                Language::Python => (
                    tree_sitter_python::LANGUAGE.into(),
                    "[(function_definition) (class_definition) (block)] @fold",
                ),
            };
            let mut parser = Parser::new();
            parser.set_language(&grammar).map_err(|e| e.to_string())?;
            entry.insert(Configuration {
                parser,
                query: Query::new(&grammar, query).map_err(|e| e.to_string())?,
            });
        }
        let configuration = self.configurations.get_mut(&language).unwrap();
        // A cancelled parser can retain an unfinished parse. Every request is a full new snapshot.
        configuration.parser.reset();
        let mut progress = |_: &tree_sitter::ParseState| {
            if stopped() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let bytes = source.as_bytes();
        let tree = configuration
            .parser
            .parse_with_options(
                &mut |offset, _| bytes.get(offset..).unwrap_or_default(),
                None,
                Some(ParseOptions::new().progress_callback(&mut progress)),
            )
            .ok_or_else(|| {
                stop_reason(cancel, deadline).unwrap_or("Pliegues sintácticos: análisis cancelado")
            })?;
        let lines = source.bytes().filter(|b| *b == b'\n').count() + 1;
        let mut enters = vec![0usize; lines];
        let mut leaves = vec![0usize; lines];
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(8192);
        let mut progress = |_: &tree_sitter::QueryCursorState| {
            if stopped() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut matches = cursor.matches_with_options(
            &configuration.query,
            tree.root_node(),
            bytes,
            QueryCursorOptions::new().progress_callback(&mut progress),
        );
        let mut previous = None;
        let mut ranges = 0;
        let mut inspected = 0;
        while let Some(found) = matches.next() {
            inspected += 1;
            if let Some(reason) = stop_reason(cancel, deadline) {
                return Err(reason.into());
            }
            if inspected > 32768 {
                return Err("Pliegues sintácticos: máximo de 32.768 coincidencias".into());
            }
            let Some(first) = found.captures().first() else {
                continue;
            };
            let last = found.captures().last().unwrap();
            let start = first.node.start_position().row;
            let end = last.node.end_position();
            let stop = end.row.saturating_sub(usize::from(end.column == 0));
            if stop <= start || previous == Some((start, stop)) {
                continue;
            }
            if stop >= lines || ranges == MAX_FOLDS {
                return Err("Pliegues sintácticos: máximo de 4.096 rangos".into());
            }
            enters[start] += 1;
            leaves[stop] += 1;
            ranges += 1;
            previous = Some((start, stop));
        }
        drop(matches);
        if let Some(reason) = stop_reason(cancel, deadline) {
            return Err(reason.into());
        }
        if cursor.did_exceed_match_limit() {
            return Err("Pliegues sintácticos: límite de coincidencias".into());
        }
        let mut definitions = Vec::new();
        let mut opened = Vec::<usize>::new();
        let (mut previous_level, mut previous_leave) = (0usize, 0usize);
        for row in 0..=lines {
            if row % 256 == 0
                && let Some(reason) = stop_reason(cancel, deadline)
            {
                return Err(reason.into());
            }
            let enter = enters.get(row).copied().unwrap_or(0);
            let mut leave = leaves.get(row).copied().unwrap_or(0);
            let mut level = previous_level
                .saturating_sub(previous_leave)
                .saturating_add(enter);
            if enter > 0 && leave > 0 {
                level = level.saturating_sub(leave);
                leave = 0;
            }
            let restart = enter > 0 && level <= MAX_DEPTH;
            let target = if row == lines {
                0
            } else {
                level.min(MAX_DEPTH)
            };
            while opened.len() > target || (restart && target > 0 && opened.len() == target) {
                let start = opened.pop().unwrap();
                if row > start + 1 {
                    if definitions.len() == MAX_FOLDS {
                        return Err("Pliegues sintácticos: máximo de 4.096 definiciones".into());
                    }
                    definitions.push(Definition {
                        rows: start..row,
                        level: opened.len() + 1,
                    });
                }
            }
            while opened.len() < target {
                opened.push(row);
            }
            previous_level = level;
            previous_leave = leave;
        }
        definitions.sort_by_key(|f| (f.rows.start, std::cmp::Reverse(f.rows.end), f.level));
        Ok(definitions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_one_expired_deadline_only_while_request_is_live() {
        let cancel = AtomicUsize::new(0);
        let mut calls = 0;
        let result = retry_timeout(&cancel, || {
            calls += 1;
            if calls == 1 {
                Err(TIMEOUT.into())
            } else {
                Ok(17)
            }
        });
        assert_eq!(result, Ok(17));
        assert_eq!(calls, 2);

        calls = 0;
        let result: Result<(), String> = retry_timeout(&cancel, || {
            calls += 1;
            Err(TIMEOUT.into())
        });
        assert_eq!(result, Err(TIMEOUT.into()));
        assert_eq!(calls, 2);

        calls = 0;
        let result: Result<(), String> = retry_timeout(&cancel, || {
            calls += 1;
            Err("hard limit".into())
        });
        assert_eq!(result, Err("hard limit".into()));
        assert_eq!(calls, 1);

        calls = 0;
        let result: Result<(), String> = retry_timeout(&cancel, || {
            calls += 1;
            cancel.store(1, Ordering::Release);
            Err(TIMEOUT.into())
        });
        assert_eq!(result, Err(TIMEOUT.into()));
        assert_eq!(calls, 1);
        assert_eq!(stop_reason(&cancel, Instant::now()), Some(CANCELLED));
    }
    #[test]
    fn unindented_grammars_match_clean_neovim_fold_levels() {
        let mut engine = Engine::default();
        let cancel = AtomicUsize::new(0);
        {
            let text =
                "fn first() {\nif true {\nprintln!(\"ñ\");\n}\n}\nfn second() {\nlet x = 1;\n}";
            let definitions = engine.compute(Language::Rust, text, &cancel).unwrap();
            assert_eq!(definitions.len(), 3);
            let levels: Vec<_> = (0..text.lines().count())
                .map(|row| {
                    definitions
                        .iter()
                        .filter(|f| f.rows.contains(&row))
                        .map(|f| f.level)
                        .max()
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(levels, vec![1, 2, 2, 2, 1, 1, 1, 1]);
        }
        {
            let text = "int first(void) {\nif (1) {\nreturn 2;\n}\nreturn 0;\n}\nint second(void) {\nreturn 3;\n}";
            let definitions = engine.compute(Language::C, text, &cancel).unwrap();
            assert_eq!(definitions.len(), 3);
            let levels: Vec<_> = (0..text.lines().count())
                .map(|row| {
                    definitions
                        .iter()
                        .filter(|f| f.rows.contains(&row))
                        .map(|f| f.level)
                        .max()
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(levels, vec![1, 2, 2, 2, 1, 1, 1, 1, 1]);
        }
        {
            let text = "function first()\nwhile true do\nprint(\"ñ\")\nend\nend\nfunction second()\nreturn 3\nend";
            let definitions = engine.compute(Language::Lua, text, &cancel).unwrap();
            assert_eq!(definitions.len(), 3);
            let levels: Vec<_> = (0..text.lines().count())
                .map(|row| {
                    definitions
                        .iter()
                        .filter(|f| f.rows.contains(&row))
                        .map(|f| f.level)
                        .max()
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(levels, vec![1, 2, 2, 2, 1, 1, 1, 1]);
        }
        {
            let text = "{\n\"a\": [\n{\n\"ñ\": true\n}\n]\n}";
            let definitions = engine.compute(Language::Json, text, &cancel).unwrap();
            assert_eq!(definitions.len(), 3);
            let levels: Vec<_> = (0..text.lines().count())
                .map(|row| {
                    definitions
                        .iter()
                        .filter(|f| f.rows.contains(&row))
                        .map(|f| f.level)
                        .max()
                        .unwrap_or(0)
                })
                .collect();
            assert_eq!(levels, vec![1, 2, 3, 3, 3, 2, 1]);
        }
    }
    #[test]
    fn cancellation_and_size_range_caps_do_not_publish_partial_definitions() {
        let mut engine = Engine::default();
        let cancel = AtomicUsize::new(1);
        assert!(
            engine
                .compute(Language::Rust, "fn f() {}", &cancel)
                .is_err()
        );
        cancel.store(0, Ordering::Release);
        assert!(
            engine
                .compute(Language::Json, &" ".repeat(MAX_BYTES + 1), &cancel)
                .is_err()
        );
        let text = format!("[\n{}]", "{\n\"x\": 1\n},\n".repeat(MAX_FOLDS + 1));
        assert!(text.len() < MAX_CHARS);
        assert!(engine.compute(Language::Json, &text, &cancel).is_err());
        // Cancellation must not poison the next independently supplied snapshot.
        assert_eq!(
            engine
                .compute(Language::Json, "{\n\"x\": 1\n}", &cancel)
                .unwrap()
                .len(),
            1
        );
    }
}
