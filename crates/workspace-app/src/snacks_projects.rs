//! Data-only snapshot of the user's declared Snacks sources. No Lua or history is run/read.
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use workspace_core::picker::Request;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub patterns: Vec<String>,
    pub dev: Vec<String>,
    pub projects: Vec<String>,
    pub max_depth: usize,
    pub recent: bool,
}
impl Config {
    pub fn configured() -> Result<Self, String> {
        Self::parse(include_str!(
            "../../../Resources/Contracts/snacks-projects-config.json"
        ))
    }
    /// Validation's virtual home is state/demo, never the process HOME.
    pub fn validation() -> Result<Self, String> {
        Self::parse(
            r#"{"patterns":["compose.yaml"],"dev":["~/snacks-projects"],"projects":["~/workspace"],"max_depth":3,"recent":false}"#,
        )
    }
    fn parse(source: &str) -> Result<Self, String> {
        let config: Self = serde_json::from_str(source).map_err(|e| e.to_string())?;
        if config.recent
            || config.max_depth > 8
            || config
                .patterns
                .iter()
                .any(|p| p.contains('/') || p.is_empty())
        {
            return Err("Configuración Snacks fuera del alcance admitido".into());
        }
        Ok(config)
    }
    fn expand(home: &Path, path: &str) -> Option<PathBuf> {
        let path = if path == "~" {
            home.to_owned()
        } else if let Some(tail) = path.strip_prefix("~/") {
            home.join(tail)
        } else {
            PathBuf::from(path)
        };
        (path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)))
        .then_some(path)
    }
    /// Enumerate directory names/marker names only, lazily when the picker opens.
    /// Bounds and no symlink traversal intentionally differ from fd --follow.
    pub fn discover(
        &self,
        home: &Path,
        remembered: &[PathBuf],
        request: &Request,
    ) -> Result<Vec<PathBuf>, String> {
        let mut seen = BTreeSet::new();
        let mut found = Vec::new();
        let mut add = |path: PathBuf| {
            if seen.insert(path.clone()) && found.len() < 1000 {
                found.push(path);
            }
        };
        // Retain app roots, including Home, without replacing the registry.
        for path in remembered {
            add(path.clone());
        }
        for path in &self.projects {
            if let Some(path) = Self::expand(home, path) {
                add(path);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut inspected = 0usize;
        let mut pending = self
            .dev
            .iter()
            .rev()
            .filter_map(|p| Self::expand(home, p))
            .map(|p| (p, 0))
            .collect::<Vec<_>>();
        let mut visited = BTreeMap::new();
        while let Some((path, depth)) = pending.pop() {
            if request.cancelled() {
                return Err("cancelled".into());
            }
            if inspected >= 10000 || Instant::now() >= deadline {
                break;
            }
            let remaining = self.max_depth.saturating_sub(depth);
            if visited
                .get(&path)
                .is_some_and(|previous| *previous >= remaining)
            {
                continue;
            }
            visited.insert(path.clone(), remaining);
            if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&path) else {
                continue;
            };
            let mut entries = entries
                .take(10000usize.saturating_sub(inspected))
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if request.cancelled() {
                    return Err("cancelled".into());
                }
                inspected += 1;
                if inspected > 10000 || Instant::now() >= deadline {
                    break;
                }
                let name = entry.file_name();
                // fd max-depth counts the marker itself, not its parent directory.
                if depth < self.max_depth && self.patterns.iter().any(|p| name == p.as_str()) {
                    add(path.clone());
                }
                if depth + 1 < self.max_depth
                    && !matches!(
                        name.to_str(),
                        Some(".git" | ".worktrees" | "node_modules" | "target")
                    )
                    && entry.file_type().is_ok_and(|t| t.is_dir())
                {
                    pending.push((entry.path(), depth + 1));
                }
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use workspace_core::picker::PickerRequests;
    #[test]
    fn declared_sources_expand_without_reading_home_or_recent_buffers() {
        let config = Config::configured().unwrap();
        assert_eq!(config.dev.len(), 8);
        assert_eq!(config.projects, ["~/.config/nvim", "~/.config/tmux"]);
        assert_eq!(config.max_depth, 3);
        assert!(!config.recent);
        assert_eq!(
            Config::expand(Path::new("/synthetic"), "~/Projects/"),
            Some(PathBuf::from("/synthetic/Projects"))
        );
        assert!(Config::expand(Path::new("/synthetic"), "relative").is_none());
        assert!(Config::expand(Path::new("/synthetic"), "~/../escape").is_none());
    }
    #[test]
    fn synthetic_markers_depth_duplicates_roots_and_cancellation() {
        let root = std::env::temp_dir().join(format!("tw-snacks-projects-{}", std::process::id()));
        std::fs::create_dir_all(root.join("dev/alpha/.git")).unwrap();
        std::fs::create_dir_all(root.join("dev/group/bravo")).unwrap();
        std::fs::write(root.join("dev/group/bravo/compose.yaml"), "synthetic").unwrap();
        std::fs::create_dir_all(root.join("dev/group/deeper/too-deep/.git")).unwrap();
        std::fs::create_dir_all(root.join("dev/.hidden/.codex")).unwrap();
        std::fs::create_dir_all(root.join("dev/node_modules/excluded/.git")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("dev/group"), root.join("dev/linked")).unwrap();
        let config = Config {
            patterns: vec![".git".into(), ".codex".into(), "compose.yaml".into()],
            dev: vec!["~/dev".into(), "~/dev/group".into(), "~/absent".into()],
            projects: vec!["~/explicit".into()],
            max_depth: 3,
            recent: false,
        };
        let requests = PickerRequests::default();
        let request = requests.begin();
        let remembered = vec![root.clone(), root.join("dev/alpha")];
        let paths = config.discover(&root, &remembered, &request).unwrap();
        assert!(paths.contains(&root));
        assert!(paths.contains(&root.join("explicit")));
        assert!(paths.contains(&root.join("dev/alpha")));
        assert!(paths.contains(&root.join("dev/group/bravo")));
        assert!(paths.contains(&root.join("dev/.hidden")));
        // The overlapping dev/group root permits a deeper result from that root.
        assert!(paths.contains(&root.join("dev/group/deeper/too-deep")));
        assert!(!paths.contains(&root.join("dev/node_modules/excluded")));
        assert!(!paths.contains(&root.join("dev/linked/bravo")));
        assert_eq!(
            paths
                .iter()
                .filter(|p| **p == root.join("dev/alpha"))
                .count(),
            1
        );
        let single_root = Config {
            dev: vec!["~/dev".into()],
            ..config
        };
        assert!(
            !single_root
                .discover(&root, &[], &request)
                .unwrap()
                .contains(&root.join("dev/group/deeper/too-deep"))
        );
        requests.begin();
        assert_eq!(
            single_root.discover(&root, &[], &request).unwrap_err(),
            "cancelled"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
