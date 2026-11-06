//! Lazy lexical completion over a bounded slice of the already-open buffer.
//! No disk, project discovery, server startup or full-buffer index.
use std::{
    collections::BTreeSet,
    ops::Range,
    path::{Component, Path, PathBuf},
};

pub struct PathContext {
    pub replace: Range<usize>,
    directory: PathBuf,
    prefix: String,
    query: String,
}
fn path_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(
            c,
            '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '=' | ',' | ';'
        )
}
/// Only lexical paths inside an explicit project. No environment or filesystem IO.
pub fn path_context(root: &Path, file: &Path, text: &[char], cursor: usize) -> Option<PathContext> {
    if cursor > text.len() || !file.starts_with(root) || !root.is_absolute() {
        return None;
    }
    let mut start = cursor;
    while start > 0 && cursor - start <= 1024 && path_char(text[start - 1]) {
        start -= 1;
    }
    if cursor - start > 1024 {
        return None;
    }
    let typed: String = text[start..cursor].iter().collect();
    if typed.len() > 4096 || typed.contains([':', '\\', '$', '~']) {
        return None;
    }
    let slash = typed.rfind('/');
    if slash.is_none() && (start == 0 || !matches!(text[start - 1], '\'' | '"' | '`')) {
        return None;
    }
    let (prefix, query) = slash.map_or(("", typed.as_str()), |slash| typed.split_at(slash + 1));
    let base = file.parent()?;
    let mut directory = PathBuf::new();
    for part in base.join(prefix).components() {
        match part {
            Component::RootDir => directory.push("/"),
            Component::Normal(part) => directory.push(part),
            Component::CurDir => {}
            Component::ParentDir if directory.pop() => {}
            _ => return None,
        }
    }
    if !directory.starts_with(root) {
        return None;
    }
    let mut end = cursor;
    while end < text.len() && end - start <= 1024 && path_char(text[end]) {
        end += 1;
    }
    if end - start > 1024 {
        return None;
    }
    Some(PathContext {
        replace: start..end,
        directory,
        prefix: prefix.into(),
        query: query.into(),
    })
}
/// Reuse the bounded tree index, never scan disk from completion or UI handlers.
pub fn paths(context: &PathContext, entries: &[crate::workspace::Entry]) -> Vec<(String, String)> {
    let mut matches = BTreeSet::new();
    for entry in entries.iter().take(10_000) {
        if entry.path.parent() != Some(&context.directory) {
            continue;
        }
        let Some(name) = entry.path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.len() > 255
            || name.chars().any(char::is_control)
            || !name.starts_with(&context.query)
            || (name.starts_with('.') && !context.query.starts_with('.'))
        {
            continue;
        }
        let label = format!("{name}{}", if entry.directory { "/" } else { "" });
        matches.insert((!entry.directory, label));
        if matches.len() > 100 {
            matches.pop_last();
        }
    }
    matches
        .into_iter()
        .take(100)
        .map(|(_, label)| {
            let insert = format!("{}{label}", context.prefix);
            (label, insert)
        })
        .collect()
}

pub struct Suggestions {
    pub replace: Range<usize>,
    pub words: Vec<String>,
}
fn keyword(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
pub fn buffer(text: &[char], cursor: usize) -> Suggestions {
    let cursor = cursor.min(text.len());
    let mut start = cursor;
    while start > 0 && keyword(text[start - 1]) && cursor - start <= 128 {
        start -= 1;
    }
    let mut end = cursor;
    while end < text.len() && keyword(text[end]) && end - start <= 128 {
        end += 1;
    }
    let mut result = Suggestions {
        replace: start..end,
        words: Vec::new(),
    };
    if start == cursor || cursor - start > 128 || end - start > 128 {
        return result;
    }
    let query: String = text[start..cursor].iter().collect();
    let sensitive = query.chars().any(char::is_uppercase);
    let folded = query.to_lowercase();
    let current: String = text[start..end].iter().collect();
    let lo = cursor.saturating_sub(8192);
    let hi = cursor.saturating_add(8192).min(text.len());
    let mut at = lo;
    // Do not offer a suffix of a token clipped by the bounded window.
    if lo > 0 && keyword(text[lo - 1]) {
        while at < hi && keyword(text[at]) {
            at += 1;
        }
    }
    let mut words = BTreeSet::new();
    while at < hi && words.len() < 50 {
        if !keyword(text[at]) {
            at += 1;
            continue;
        }
        let a = at;
        while at < hi && keyword(text[at]) {
            at += 1;
        }
        if at == hi && hi < text.len() && keyword(text[hi]) {
            break;
        }
        if at - a < 2 || at - a > 128 || (a <= cursor && cursor <= at) {
            continue;
        }
        let word: String = text[a..at].iter().collect();
        let matched = if sensitive {
            word.starts_with(&query)
        } else {
            word.to_lowercase().starts_with(&folded)
        };
        if matched && word != current {
            words.insert(word);
        }
    }
    result.words = words.into_iter().collect();
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entries() -> Vec<crate::workspace::Entry> {
        [
            ("src/sample.lua", false),
            ("src/sample.tex", false),
            ("src/samples", true),
            ("src/.secret", false),
            ("docs/test.txt", false),
        ]
        .into_iter()
        .map(|(name, directory)| crate::workspace::Entry {
            path: Path::new("/project").join(name),
            label: name.into(),
            depth: 1,
            directory,
        })
        .collect()
    }
    #[test]
    fn project_paths_replace_complete_token_and_use_utf16_edit_contract() {
        let original = "😀 \"../src/saBAD\"";
        let text: Vec<_> = original.chars().collect();
        let cursor = text.iter().position(|c| *c == 'B').unwrap();
        let context = path_context(
            Path::new("/project"),
            Path::new("/project/docs/test.txt"),
            &text,
            cursor,
        )
        .unwrap();
        assert_eq!(
            text[context.replace.clone()].iter().collect::<String>(),
            "../src/saBAD"
        );
        assert_eq!(
            paths(&context, &entries()),
            vec![
                ("samples/".into(), "../src/samples/".into()),
                ("sample.lua".into(), "../src/sample.lua".into()),
                ("sample.tex".into(), "../src/sample.tex".into()),
            ]
        );
        let mut editor = crate::editor::Editor::new(original.into());
        editor.goto(0, cursor);
        editor.key("i");
        let range = serde_json::json!({"start":editor.lsp_position(context.replace.start),"end":editor.lsp_position(context.replace.end)});
        editor
            .apply_lsp_edit(&range, "../src/sample.lua", editor.version)
            .unwrap();
        assert_eq!(editor.contents(), "😀 \"../src/sample.lua\"");
        editor.key("<Esc>");
        editor.key("u");
        assert_eq!(editor.contents(), original);
    }
    #[test]
    fn path_context_never_expands_home_variables_urls_or_outside_project() {
        let root = Path::new("/project");
        let file = root.join("docs/test.txt");
        for input in [
            "word",
            "../../outside/",
            "/etc/",
            "~/",
            "$HOME/",
            "https://example/",
            "x\\y/",
        ] {
            let text: Vec<_> = input.chars().collect();
            assert!(
                path_context(root, &file, &text, text.len()).is_none(),
                "{input}"
            );
        }
        let text: Vec<_> = "../src/.".chars().collect();
        let context = path_context(root, &file, &text, text.len()).unwrap();
        assert_eq!(
            paths(&context, &entries()),
            vec![(".secret".into(), "../src/.secret".into())]
        );
        let text: Vec<_> = "../src/".chars().collect();
        let context = path_context(root, &file, &text, text.len()).unwrap();
        assert!(
            !paths(&context, &entries())
                .iter()
                .any(|(name, _)| name.starts_with('.'))
        );
        let text: Vec<_> = "\"te".chars().collect();
        let context = path_context(root, &file, &text, text.len()).unwrap();
        assert_eq!(
            paths(&context, &entries()),
            vec![("test.txt".into(), "test.txt".into())]
        );
        let text: Vec<_> = format!("./{}", "x".repeat(1025)).chars().collect();
        assert!(path_context(root, &file, &text, text.len()).is_none());
    }
    #[test]
    fn path_completion_uses_only_bounded_cached_entries_and_results() {
        let text: Vec<_> = "./s".chars().collect();
        let context = path_context(
            Path::new("/project"),
            Path::new("/project/test.txt"),
            &text,
            text.len(),
        )
        .unwrap();
        let mut entries: Vec<_> = (0..10_001)
            .map(|n| crate::workspace::Entry {
                path: Path::new("/project").join(format!("sample{n:05}")),
                label: String::new(),
                directory: false,
                depth: 0,
            })
            .collect();
        let result = paths(&context, &entries);
        assert_eq!(result.len(), 100);
        assert_eq!(result[0].0, "sample00000");
        entries[..10_000]
            .iter_mut()
            .for_each(|e| e.path = "/elsewhere/s".into());
        assert!(paths(&context, &entries).is_empty());
    }
    #[test]
    fn unicode_smartcase_dedup_suffix_and_window_are_bounded() {
        let text: Vec<char> = "árbol árbol ÁrbolGrande árXYZ".chars().collect();
        let cursor = text.len() - 3;
        let result = buffer(&text, cursor);
        assert_eq!(result.words, vec!["ÁrbolGrande", "árbol"]);
        assert_eq!(text[result.replace].iter().collect::<String>(), "árXYZ");
        let upper: Vec<char> = "árbol ÁrbolGrande Ár".chars().collect();
        assert_eq!(buffer(&upper, upper.len()).words, vec!["ÁrbolGrande"]);
        assert!(buffer(&text, 0).words.is_empty());
        let huge: Vec<char> = format!("outsideWord {}out", " ".repeat(17000))
            .chars()
            .collect();
        assert!(buffer(&huge, huge.len()).words.is_empty());
        let long: Vec<char> = "a".repeat(20000).chars().collect();
        assert!(buffer(&long, long.len()).words.is_empty());
        let many: Vec<char> = format!(
            "{} item",
            (0..200).map(|n| format!("item{n} ")).collect::<String>()
        )
        .chars()
        .collect();
        assert_eq!(buffer(&many, many.len()).words.len(), 50);
    }
}
