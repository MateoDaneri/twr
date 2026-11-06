//! Explicit-file Git inspection. No hooks, external diff commands or mutations.
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub start: usize,
    pub count: usize,
    pub patch: String,
}
#[derive(Clone, Debug)]
pub struct Inspection {
    pub diff: String,
    pub hunks: Vec<Hunk>,
}
fn command(root: &Path) -> Command {
    let mut cmd = Command::new("/usr/bin/git");
    cmd.args([
        "--no-pager",
        "--no-optional-locks",
        "--no-lazy-fetch",
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-C",
    ])
    .arg(root);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_EXTERNAL_DIFF",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd
}
fn output(mut command: Command) -> Result<String, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    fn drain(
        mut stream: impl Read + Send + 'static,
        limit: usize,
    ) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let mut chunk = [0; 8192];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let left = (limit + 1).saturating_sub(kept.len());
                        kept.extend_from_slice(&chunk[..n.min(left)]);
                    }
                }
            }
            kept
        })
    }
    let stdout = drain(child.stdout.take().unwrap(), 4 * 1024 * 1024);
    let stderr = drain(child.stderr.take().unwrap(), 65536);
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Git excedió ocho segundos".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let bytes = stdout.join().unwrap_or_default();
    let error = stderr.join().unwrap_or_default();
    if !status.success() {
        return Err(String::from_utf8_lossy(&error).chars().take(800).collect());
    }
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Diff supera 4 MiB".into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}
fn relative(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let canonical = root.canonicalize().map_err(|e| e.to_string())?;
    if canonical != root || path.canonicalize().map_err(|e| e.to_string())? != path {
        return Err("Git requiere rutas reales, sin symlinks".into());
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "Archivo fuera del proyecto")?;
    // A worktree diff can run clean/process filters even with --no-ext-diff.
    // This initial read-only adapter declines such repositories before diffing.
    let mut config = command(root);
    config.args(["config", "--list", "--name-only"]);
    if output(config)?.lines().any(|name| {
        name.starts_with("filter.") && (name.ends_with(".clean") || name.ends_with(".process"))
    }) {
        return Err("Este repositorio tiene filtros Git externos; la inspección nativa todavía no los ejecuta".into());
    }
    let mut cmd = command(root);
    cmd.args(["rev-parse", "--show-toplevel"]);
    let top = PathBuf::from(output(cmd)?.trim_end_matches('\n'));
    if top.canonicalize().map_err(|e| e.to_string())? != root {
        return Err("Seleccioná la raíz del repositorio para inspeccionar Git".into());
    }
    Ok(relative.into())
}
/// Status indicators for an explicitly selected repository. No hooks,
/// external diff programs or optional Git locks are used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TreeStatus {
    Untracked,
    Added,
    Deleted,
    Modified,
    Conflict,
}

fn parse_tree_status(root: &Path, text: &str) -> Result<BTreeMap<PathBuf, TreeStatus>, String> {
    let mut statuses = BTreeMap::new();
    for item in text.split('\0').filter(|item| !item.is_empty()) {
        let bytes = item.as_bytes();
        if bytes.len() < 4 || bytes[2] != b' ' {
            return Err("Estado Git inválido".into());
        }
        let status = match (bytes[0], bytes[1]) {
            (b'?', b'?') => TreeStatus::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => TreeStatus::Conflict,
            (b'M', _) | (_, b'M') | (b'T', _) | (_, b'T') => TreeStatus::Modified,
            (b'D', _) | (_, b'D') => TreeStatus::Deleted,
            (b'A', _) | (_, b'A') => TreeStatus::Added,
            _ => continue,
        };
        let relative = Path::new(&item[3..]);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("Ruta Git fuera del repositorio".into());
        }
        let mut path = root.join(relative);
        while path != root && path.starts_with(root) {
            statuses
                .entry(path.clone())
                .and_modify(|old: &mut TreeStatus| *old = (*old).max(status))
                .or_insert(status);
            if !path.pop() {
                break;
            }
        }
    }
    Ok(statuses)
}
pub fn tree_status(root: &Path) -> Result<BTreeMap<PathBuf, TreeStatus>, String> {
    relative(root, root)?;
    let mut cmd = command(root);
    cmd.args([
        "status",
        "--porcelain=v1",
        "-z",
        "--no-renames",
        "--untracked-files=all",
    ]);
    parse_tree_status(root, &output(cmd)?)
}

/// Read-only project views, available without opening a file.
pub fn overview(root: &Path, action: &str) -> Result<String, String> {
    relative(root, root)?;
    let mut cmd = command(root);
    match action {
        "status" => {
            cmd.args(["status", "--short", "--branch", "--untracked-files=normal"]);
        }
        "log" => {
            cmd.args([
                "log",
                "--no-color",
                "--max-count=50",
                "--date=short",
                "--format=%h %ad %s",
                "--decorate=short",
            ]);
        }
        "project_diff" => {
            cmd.args([
                "diff",
                "HEAD",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--",
            ]);
        }
        _ => return Err("Vista Git desconocida".into()),
    }
    let text = output(cmd)?;
    let text = if action == "status" {
        text.lines()
            .map(|line| {
                if let Some(branch) = line.strip_prefix("## ") {
                    return format!("Rama: {branch}");
                }
                let bytes = line.as_bytes();
                if bytes.len() >= 3 && bytes[0].is_ascii() && bytes[1].is_ascii() {
                    let index = if bytes[0] == b' ' {
                        '-'
                    } else {
                        bytes[0] as char
                    };
                    let worktree = if bytes[1] == b' ' {
                        '-'
                    } else {
                        bytes[1] as char
                    };
                    return format!("Índice: {index} · Trabajo: {worktree} · {}", &line[3..]);
                }
                line.to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text
    };
    Ok(format!(
        "Git · {action}\nSolo lectura · Esc vuelve\n\n{}",
        if text.is_empty() {
            "Sin cambios"
        } else {
            &text
        }
    ))
}

pub fn inspect(root: &Path, path: &Path, expected: &str) -> Result<Inspection, String> {
    let relative = relative(root, path)?;
    let metadata = path.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
        return Err("Archivo Git fuera de límites".into());
    }
    let disk = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if disk != expected {
        return Err("El archivo cambió en disco; recargá antes de consultar hunks".into());
    }
    let mut cmd = command(root);
    cmd.args([
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--unified=0",
        "--",
    ])
    .arg(relative);
    let diff = output(cmd)?;
    let hunks = hunks(&diff)?;
    // Positions refer to the checked disk revision, not to a later rewrite.
    if std::fs::read_to_string(path).map_err(|e| e.to_string())? != expected {
        return Err("El archivo cambió durante la consulta Git".into());
    }
    Ok(Inspection { diff, hunks })
}
pub fn blame(root: &Path, path: &Path, line: usize) -> Result<String, String> {
    let relative = relative(root, path)?;
    let mut cmd = command(root);
    cmd.args(["blame", "--line-porcelain", "-L"])
        .arg(format!("{},{}", line.max(1), line.max(1)))
        .arg("--")
        .arg(relative);
    let data = output(cmd)?;
    let mut summary = Vec::new();
    for (i, line) in data.lines().enumerate() {
        if i == 0 {
            summary.push(format!(
                "Commit {}",
                line.split_whitespace().next().unwrap_or_default()
            ));
        } else if line.starts_with("author ")
            || line.starts_with("author-time ")
            || line.starts_with("summary ")
            || line.starts_with('\t')
        {
            summary.push(line.to_owned());
        }
    }
    Ok(summary.join("\n"))
}
pub fn hunks(diff: &str) -> Result<Vec<Hunk>, String> {
    let pattern = regex::Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
        .map_err(|e| e.to_string())?;
    let mut result: Vec<Hunk> = Vec::new();
    for line in diff.split_inclusive('\n') {
        if let Some(c) = pattern.captures(line) {
            if result.len() >= 1000 {
                return Err("Diff supera 1.000 hunks".into());
            }
            let number = |n| {
                c.get(n)
                    .map(|x| x.as_str().parse::<usize>())
                    .transpose()
                    .map_err(|e| e.to_string())
            };
            result.push(Hunk {
                old_start: number(1)?.unwrap_or(0),
                old_count: number(2)?.unwrap_or(1),
                start: number(3)?.unwrap_or(0),
                count: number(4)?.unwrap_or(1),
                patch: line.into(),
            });
        } else if let Some(hunk) = result.last_mut() {
            hunk.patch.push_str(line);
        }
    }
    Ok(result)
}
pub fn target(hunks: &[Hunk], row: usize, action: &str) -> Option<usize> {
    let starts: Vec<_> = hunks.iter().map(|h| h.start.saturating_sub(1)).collect();
    match action {
        "first" => starts.first().copied(),
        "last" => starts.last().copied(),
        "next" => starts
            .iter()
            .copied()
            .find(|r| *r > row)
            .or(starts.first().copied()),
        "prev" => starts
            .iter()
            .copied()
            .rev()
            .find(|r| *r < row)
            .or(starts.last().copied()),
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn git_tree_status_propagates_and_keeps_weird_filenames_literal() {
        let root = Path::new("/synthetic");
        let data = " M src/á b.tf\0?? src/new file.txt\0A  added.txt\0";
        let statuses = parse_tree_status(root, data).unwrap();
        assert_eq!(
            statuses.get(&root.join("src/á b.tf")),
            Some(&TreeStatus::Modified)
        );
        assert_eq!(
            statuses.get(&root.join("src/new file.txt")),
            Some(&TreeStatus::Untracked)
        );
        assert_eq!(statuses.get(&root.join("src")), Some(&TreeStatus::Modified));
        assert_eq!(
            statuses.get(&root.join("added.txt")),
            Some(&TreeStatus::Added)
        );
        assert!(parse_tree_status(root, "?? ../escape\0").is_err());
    }
    #[test]
    fn real_git_hunks_blame_unicode_and_literal_pathspecs() {
        let dir = std::env::temp_dir().join(format!("tw-git-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let run = |args: &[&str]| {
            let mut c = command(&dir);
            c.env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null");
            c.args([
                "-c",
                "user.name=Synthetic",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgSign=false",
            ]);
            c.args(args);
            output(c).unwrap()
        };
        run(&["init", "-q"]);
        let path = dir.join("[ñ].txt");
        std::fs::write(
            &path,
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
        )
        .unwrap();
        run(&["add", "--", "[ñ].txt"]);
        run(&["commit", "-qm", "Synthetic baseline"]);
        let changed = "one\nDOS 🦀\nthree\nfour\nfive\nsix\nseven\neight\nnine\nDIEZ\n";
        std::fs::write(&path, changed).unwrap();
        assert_eq!(
            tree_status(&dir).unwrap().get(&path),
            Some(&TreeStatus::Modified)
        );
        assert!(overview(&dir, "status").unwrap().contains("["));
        assert!(
            overview(&dir, "status")
                .unwrap()
                .contains("Índice: - · Trabajo: M")
        );
        assert!(
            overview(&dir, "log")
                .unwrap()
                .contains("Synthetic baseline")
        );
        assert!(overview(&dir, "project_diff").unwrap().contains("+DOS 🦀"));
        let inspection = inspect(&dir, &path, changed).unwrap();
        assert_eq!(inspection.hunks.len(), 2);
        assert_eq!(target(&inspection.hunks, 0, "next"), Some(1));
        assert_eq!(target(&inspection.hunks, 1, "next"), Some(9));
        assert_eq!(target(&inspection.hunks, 9, "next"), Some(1));
        assert!(
            blame(&dir, &path, 1)
                .unwrap()
                .contains("Synthetic baseline")
        );
        assert!(inspect(&dir, &path, "stale").is_err());
        assert!(inspect(&dir, &dir.join("../escape"), changed).is_err());
        let deleted = hunks("@@ -1,2 +0,0 @@\n-a\n-b\n").unwrap();
        assert_eq!(target(&deleted, 0, "first"), Some(0));
        let marker = dir.join("filter-was-executed");
        run(&[
            "config",
            "filter.blocked.clean",
            &format!("touch {}", marker.display()),
        ]);
        std::fs::write(dir.join(".gitattributes"), "*.txt filter=blocked\n").unwrap();
        assert!(
            inspect(&dir, &path, changed)
                .unwrap_err()
                .contains("filtros Git externos")
        );
        assert!(!marker.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
