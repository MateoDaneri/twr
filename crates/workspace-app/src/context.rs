//! Existing exact-UUID dashboard helper, executed only after in-app opt-in.
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub fn collect(helper: &Path, dashboard: &Path, tasks: &Value) -> Result<Value, String> {
    let input = serde_json::to_vec(tasks).map_err(|e| e.to_string())?;
    if input.len() > 256000 {
        return Err("Lista de contexto excedida".into());
    }
    let mut child = Command::new("/usr/bin/python3")
        .arg(helper)
        .arg("--dashboard-root")
        .arg(dashboard)
        .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(4_000_001).read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Timeout del helper de contexto".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = writer.join();
    let bytes = reader.join().unwrap_or_default();
    if !status.success() || bytes.len() > 4_000_000 {
        return Err("No se pudo consultar contexto".into());
    }
    let data: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Respuesta de contexto inválida")?;
    if !data["tasks"].is_object() {
        return Err("Contexto sin tabla de tareas".into());
    }
    Ok(data)
}
pub fn review_urls(context: &Value) -> Vec<String> {
    let pattern = regex::Regex::new(
        r"^https://github\.com/([A-Za-z0-9_-]+)/([A-Za-z0-9_.-]+)/pull/([1-9][0-9]{0,9})/?$",
    )
    .unwrap();
    let mut urls = context["objectives"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o["review_url"].as_str())
        .filter(|url| {
            pattern.captures(url).is_some_and(|c| {
                &c[2] != "."
                    && &c[2] != ".."
                    && c[3].parse::<u32>().is_ok_and(|n| n <= 2_147_483_647)
            })
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    urls.sort();
    urls.dedup();
    urls
}
pub fn describe(context: &Value) -> String {
    fn clean(v: &Value) -> String {
        v.as_str()
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(12000)
            .collect()
    }
    let mut text = String::new();
    if let Some(checkout) = context.get("checkout") {
        text.push_str(&format!(
            "Checkout observado\n{}\nBranch: {}\n\n",
            clean(&checkout["root"]),
            clean(&checkout["branch"])
        ));
    }
    if let Some(objectives) = context["objectives"].as_array() {
        for o in objectives.iter().take(50) {
            text.push_str(&format!(
                "{} · {}\n{}\nWorktree: {}\nBranch registrada: {}\nSiguiente paso: {}\n\n",
                clean(&o["title"]),
                clean(&o["state"]),
                clean(&o["objective"]),
                clean(&o["worktree"]),
                clean(&o["branch"]),
                clean(&o["note"])
            ));
        }
    }
    if text.is_empty() {
        "No hay objetivos asociados al UUID exacto de este chat".into()
    } else {
        text
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn pr_links_are_exact_saved_github_urls_and_deduplicated() {
        let context = json!({"objectives":[{"review_url":"https://github.com/a/b/pull/12"},{"review_url":"https://evil.test/a/b/pull/12"},{"review_url":"https://github.com/a/b/pull/12?token=x"},{"review_url":"https://github.com/a/b/pull/12"}]});
        assert_eq!(
            review_urls(&context),
            vec!["https://github.com/a/b/pull/12"]
        );
    }
}
