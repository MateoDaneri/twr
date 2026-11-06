//! Cancellable content search over an explicit, already indexed project only.
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use workspace_core::picker::Request;
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub preview: String,
}
pub fn grep(
    root: &Path,
    paths: &[PathBuf],
    query: &str,
    request: &Request,
) -> Result<Vec<Hit>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let regex = regex::RegexBuilder::new(query)
        .case_insensitive(!query.chars().any(char::is_uppercase))
        .size_limit(1_000_000)
        .build()
        .map_err(|e| e.to_string())?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut hits = Vec::new();
    let mut bytes_left: usize = 32 * 1024 * 1024;
    let deadline = Instant::now() + Duration::from_secs(3);
    for path in paths.iter().take(10000) {
        if request.cancelled() {
            return Err("cancelled".into());
        }
        if Instant::now() > deadline || bytes_left == 0 || hits.len() >= 200 {
            break;
        }
        let Ok(m) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if !m.is_file() || m.len() > 2 * 1024 * 1024 {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if canonical != *path || !canonical.starts_with(&root) {
            continue;
        }
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let mut bytes = Vec::new();
        let limit = bytes_left.min(2 * 1024 * 1024 + 1);
        if file.take(limit as u64).read_to_end(&mut bytes).is_err() {
            continue;
        }
        bytes_left = bytes_left.saturating_sub(bytes.len());
        if bytes.len() > 2 * 1024 * 1024 || bytes.contains(&0) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        for (line, text) in text.lines().enumerate() {
            if request.cancelled() {
                return Err("cancelled".into());
            }
            if hits.len() >= 200 || Instant::now() > deadline {
                break;
            }
            if let Some(found) = regex.find(text) {
                hits.push(Hit {
                    path: path.clone(),
                    line: line + 1,
                    column: text[..found.start()].chars().count() + 1,
                    preview: text.chars().take(240).collect(),
                });
            }
        }
    }
    Ok(hits)
}
const MARKERS: &[&str] = &[
    ".git",
    ".claude",
    ".codex",
    "docker-compose.yml",
    "compose.yml",
    "compose.yaml",
    "docker-compose.yaml",
];
/// Caller chooses a root. Never reads paths from shell history or scans home by default.
pub fn projects(root: &Path, request: &Request) -> Result<Vec<PathBuf>, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    let mut pending = vec![(root, 0)];
    let mut visited = 0;
    while let Some((path, depth)) = pending.pop() {
        if request.cancelled() {
            return Err("cancelled".into());
        }
        visited += 1;
        if visited > 10000 || found.len() >= 1000 {
            break;
        }
        let entries = match std::fs::read_dir(&path) {
            Ok(e) => e.filter_map(Result::ok).take(10000).collect::<Vec<_>>(),
            Err(_) => continue,
        };
        if entries
            .iter()
            .any(|e| MARKERS.iter().any(|marker| e.file_name() == *marker))
        {
            found.push(path.clone());
        }
        if depth < 3 {
            for entry in entries {
                if entry.file_type().is_ok_and(|t| t.is_dir())
                    && !matches!(
                        entry.file_name().to_str(),
                        Some(".git" | ".worktrees" | "node_modules" | "target")
                    )
                    && pending.len() + visited < 10000
                {
                    pending.push((entry.path(), depth + 1));
                }
            }
        }
    }
    found.sort();
    Ok(found)
}
#[cfg(test)]
mod tests {
    use super::*;
    use workspace_core::picker::PickerRequests;
    #[test]
    fn hidden_ignored_unicode_regex_positions_and_cancellation() {
        let root = std::env::temp_dir().join(format!("tw-search-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join(".hidden");
        std::fs::write(&path, "á gato\nGATO\n").unwrap();
        let requests = PickerRequests::default();
        let r = requests.begin();
        let hits = grep(&root, std::slice::from_ref(&path), "gat.", &r).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].line, hits[0].column), (1, 3));
        requests.begin();
        assert_eq!(grep(&root, &[path], "gato", &r).unwrap_err(), "cancelled");
        std::fs::remove_dir_all(root).unwrap();
    }
}
