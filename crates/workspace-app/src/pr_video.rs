//! PR attachment metadata and explicit, cancellable helper requests.
//! Eligibility is not a redirect policy; the downloader must validate that too.
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::{collections::HashSet, sync::LazyLock};
use std::{
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_BODIES: usize = 256;
const MAX_URLS: usize = 32;
static URLS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"https://[^\s<>)\]\"']+"#).unwrap());
static PR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^https://github\.com/([A-Za-z0-9_-]+)/([A-Za-z0-9_.-]+)/pull/([1-9][0-9]{0,9})/?$",
    )
    .unwrap()
});

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub repository: String,
    pub number: u32,
}
impl PullRequest {
    pub fn from_url(url: &str) -> Option<Self> {
        if url.len() > 4096 {
            return None;
        }
        let fields = PR.captures(url)?;
        if matches!(&fields[2], "." | "..") {
            return None;
        }
        let number = fields[3]
            .parse::<u32>()
            .ok()
            .filter(|n| *n <= 2_147_483_647)?;
        Some(Self {
            repository: format!("{}/{}", &fields[1], &fields[2]),
            number,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Attachment {
    pub url: String,
    pub label: String,
}
impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Signed private attachment queries must not leak through Debug logs.
        f.debug_struct("Attachment")
            .field("url", &"[redacted]")
            .finish()
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Attachments {
    pub items: Vec<Attachment>,
    pub limited: bool,
}

/// One cancellable request. Bodies and signed attachment URLs remain in memory.
pub struct Request {
    result: mpsc::Receiver<Result<Attachments, String>>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Request {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Request {
    pub fn start(
        helper: PathBuf,
        pr: PullRequest,
        validation: bool,
        wake: async_channel::Sender<()>,
    ) -> Self {
        let (tx, result) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            let response = if validation {
                if pr.repository == "terminal-workspace/synthetic" && pr.number == 1 {
                    attachments([
                        "Validación sintética\nhttps://github.com/user-attachments/assets/synthetic-first",
                        "https://github.com/user-attachments/assets/synthetic-second",
                    ])
                } else {
                    Err("Validación: usá https://github.com/terminal-workspace/synthetic/pull/1; red real desactivada".into())
                }
            } else {
                collect(&helper, &pr, &worker_cancel)
            };
            if !worker_cancel.load(Ordering::Acquire) {
                let _ = tx.try_send(response);
                let _ = wake.try_send(());
            }
        });
        Self { result, cancel }
    }
    pub fn poll(&self) -> Option<Result<Attachments, String>> {
        match self.result.try_recv() {
            Ok(value) => Some(value),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("Video: consulta interrumpida".into()))
            }
        }
    }
}

/// Construct only after explicit normal-profile PR access. Validation never
/// calls this function and cannot inherit the user's gh configuration/tokens.
pub(crate) fn authenticated_helper(helper: &Path) -> Command {
    let mut command = Command::new("/usr/bin/python3");
    command
        .args(["-I", "-S"])
        .arg(helper)
        .env_clear()
        .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "GH_CONFIG_DIR",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}
fn collect(helper: &Path, pr: &PullRequest, cancel: &AtomicBool) -> Result<Attachments, String> {
    let input =
        serde_json::to_vec(&serde_json::json!({"repository":pr.repository,"number":pr.number}))
            .map_err(|_| "Video: PR inválido")?;
    let mut command = authenticated_helper(helper);
    command.arg("metadata");
    let bytes = run_metadata(command, &input, cancel, Duration::from_secs(75))?;
    #[derive(serde::Deserialize)]
    struct Bodies {
        bodies: Vec<String>,
    }
    let data: Bodies = serde_json::from_slice(&bytes).map_err(|_| "Video: metadata inválida")?;
    attachments(data.bodies.iter().map(String::as_str))
}
/// Explicit read-only PR view. Validation never starts gh or reads auth.
#[cfg(test)]
#[test]
fn review_validation_never_launches_helper_or_real_pr() {
    let helper = Path::new("/nonexistent/helper.py");
    let pr =
        PullRequest::from_url("https://github.com/terminal-workspace/synthetic/pull/1").unwrap();
    assert!(
        review(helper, &pr, true)
            .unwrap()
            .contains("+let answer = 43;")
    );
    let real = PullRequest::from_url("https://github.com/other/repo/pull/2").unwrap();
    assert!(review(helper, &real, true).is_err());
}
pub fn review(helper: &Path, pr: &PullRequest, validation: bool) -> Result<String, String> {
    if validation {
        return if pr.repository == "terminal-workspace/synthetic" && pr.number == 1 {
            Ok("Git · PR terminal-workspace/synthetic#1\nFixture sintético, sin red\nEstado: open · draft: false · merged: false\nBase: main 1111111\nHead: feature 2222222\n\nsample.rs · modified · +1 / -1\n@@ -1 +1 @@\n-let answer = 42;\n+let answer = 43;".into())
        } else {
            Err("Validación: PR real desactivado".into())
        };
    }
    let input =
        serde_json::to_vec(&serde_json::json!({"repository":pr.repository,"number":pr.number}))
            .map_err(|_| "PR inválido")?;
    let mut command = authenticated_helper(helper);
    command.arg("review");
    let bytes = run_metadata(
        command,
        &input,
        &AtomicBool::new(false),
        Duration::from_secs(75),
    )?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "PR: metadata inválida")?;
    value["text"]
        .as_str()
        .filter(|t| t.len() <= 4 * 1024 * 1024)
        .map(str::to_owned)
        .ok_or("PR: vista inválida o excedida".into())
}

fn run_metadata(
    mut command: Command,
    input: &[u8],
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| "Video: helper PR no disponible")?;
    let written = child.stdin.take().unwrap().write_all(input);
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        let result = stdout.take(MAX_OUTPUT + 1).read_to_end(&mut data);
        result.map(|_| data)
    });
    let started = Instant::now();
    let (status, running) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.success() && written.is_ok(), false),
            Err(_) => break (false, true),
            _ => {}
        }
        if cancel.load(Ordering::Acquire) || started.elapsed() > timeout {
            break (false, true);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Kill the group while its ID still belongs to this just-created request.
    if running {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.wait();
    let data = reader
        .join()
        .ok()
        .and_then(Result::ok)
        .ok_or("Video: respuesta PR no disponible")?;
    if !status || data.len() as u64 > MAX_OUTPUT || cancel.load(Ordering::Acquire) {
        return Err("Video: consulta PR cancelada, excedida o sin acceso".into());
    }
    Ok(data)
}

pub fn eligible_url(url: &str) -> bool {
    if url.len() > 4096
        || !url.is_ascii()
        || url
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || url.contains("..")
    {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%' {
            let Some(pair) = bytes.get(index + 1..index + 3) else {
                return false;
            };
            let Some(high) = (pair[0] as char).to_digit(16) else {
                return false;
            };
            let Some(low) = (pair[1] as char).to_digit(16) else {
                return false;
            };
            let decoded = (high * 16 + low) as u8;
            if decoded <= 32 || decoded == 127 || matches!(decoded, b'.' | b'/' | b'\\') {
                return false;
            }
        }
    }
    let (tail, kind) =
        if let Some(tail) = url.strip_prefix("https://github.com/user-attachments/assets/") {
            (tail, 0)
        } else if let Some(tail) = url.strip_prefix("https://github.com/user-attachments/files/") {
            (tail, 1)
        } else if let Some(tail) = url.strip_prefix("https://user-images.githubusercontent.com/") {
            (tail, 1)
        } else if let Some(tail) =
            url.strip_prefix("https://private-user-images.githubusercontent.com/")
        {
            (tail, 2)
        } else {
            return false;
        };
    !tail.is_empty()
        && tail.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || b == b'-'
                || (kind >= 1 && matches!(b, b'_' | b'.' | b'/'))
                || (kind == 2 && matches!(b, b'?' | b'=' | b'&' | b'%'))
        })
}

fn display_source(body: &str) -> Result<String, String> {
    let mut source = String::new();
    let mut code = false;
    for (index, event) in Parser::new(body).enumerate() {
        if index >= 20_000 {
            return Err("Adjuntos: límite de estructura Markdown".into());
        }
        match event {
            Event::Start(Tag::CodeBlock(_)) => code = true,
            Event::End(TagEnd::CodeBlock) => code = false,
            Event::Start(Tag::Link { dest_url, .. }) if !code => {
                source.push_str(&dest_url);
                source.push('\n');
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) if !code => {
                source.push_str(&text);
                source.push('\n');
            }
            _ => {}
        }
    }
    Ok(source)
}
pub fn attachments<'a>(bodies: impl IntoIterator<Item = &'a str>) -> Result<Attachments, String> {
    let mut result = Attachments {
        items: Vec::new(),
        limited: false,
    };
    let mut seen = HashSet::new();
    let mut bytes = 0usize;
    for (index, body) in bodies.into_iter().enumerate() {
        bytes = bytes.saturating_add(body.len());
        if index >= MAX_BODIES || bytes > MAX_BODY_BYTES {
            return Err("Adjuntos: máximo 256 textos y 1 MiB total".into());
        }
        let source = display_source(body)?;
        let selected = source
            .find("<!-- codex-pr-video:start -->")
            .and_then(|start| {
                let after = &source[start + "<!-- codex-pr-video:start -->".len()..];
                after
                    .find("<!-- codex-pr-video:end -->")
                    .map(|end| &after[..end])
            })
            .unwrap_or(&source);
        for found in URLS.find_iter(selected) {
            let url = found.as_str();
            let path = url.split('?').next().unwrap_or(url);
            let lower_path = path.to_ascii_lowercase();
            if !eligible_url(url)
                || [".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp"]
                    .iter()
                    .any(|extension| lower_path.ends_with(extension))
                || !seen.insert(url.to_owned())
            {
                continue;
            }
            if result.items.len() == MAX_URLS {
                result.limited = true;
                continue;
            }
            result.items.push(Attachment {
                url: url.to_owned(),
                label: path.rsplit('/').next().unwrap_or("Video").to_owned(),
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "https://github.com/user-attachments/assets/abc-123";
    const B: &str = "https://user-images.githubusercontent.com/123/clip.mp4";
    #[test]
    fn validation_request_uses_only_synthetic_metadata_without_starting_a_helper() {
        let (wake, _) = async_channel::bounded(1);
        let request = Request::start(
            "/must-not-run.py".into(),
            PullRequest {
                repository: "terminal-workspace/synthetic".into(),
                number: 1,
            },
            true,
            wake,
        );
        let start = Instant::now();
        let response = loop {
            if let Some(result) = request.poll() {
                break result.unwrap();
            }
            assert!(start.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(response.items.len(), 2);
        assert_eq!(response.items[0].label, "synthetic-first");
    }
    #[test]
    fn metadata_supervisor_caps_output_and_cancels_a_sleeping_process() {
        fn command(code: &str) -> Command {
            let mut c = Command::new("/usr/bin/python3");
            c.args(["-I", "-S", "-c", code]).env_clear();
            c
        }
        let cancel = AtomicBool::new(false);
        let valid = run_metadata(
            command("print('{\"bodies\":[\"synthetic\"]}')"),
            b"{}",
            &cancel,
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(String::from_utf8(valid).unwrap().contains("synthetic"));
        assert!(
            run_metadata(
                command("import sys; sys.stdout.write('x'*9000000)"),
                b"{}",
                &cancel,
                Duration::from_secs(2)
            )
            .is_err()
        );
        cancel.store(true, Ordering::Release);
        let start = Instant::now();
        assert!(
            run_metadata(
                command("import time; time.sleep(10)"),
                b"{}",
                &cancel,
                Duration::from_secs(2)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn exact_pr_targets_reject_issue_links_credentials_queries_and_invalid_numbers() {
        assert_eq!(
            PullRequest::from_url("https://github.com/a/b/pull/12/").unwrap(),
            PullRequest {
                repository: "a/b".into(),
                number: 12
            }
        );
        for url in [
            "https://evil.test/a/b/pull/12",
            "https://github.com/a/b/issues/12",
            "https://github.com/a/b/pull/0",
            "https://github.com/a/b/pull/12?token=x",
            "https://github.com/a/../pull/12",
            "https://github.com/a/b/pull/2147483648",
            "https://github.com@evil.test/a/b/pull/12",
        ] {
            assert!(PullRequest::from_url(url).is_none(), "{url}");
        }
    }
    #[test]
    fn attachment_eligibility_rejects_other_hosts_userinfo_traversal_and_encoded_controls() {
        for url in [
            A,
            B,
            "https://github.com/user-attachments/files/123/clip.mov",
            "https://private-user-images.githubusercontent.com/123/a.mp4?jwt=abc%3D&v=1",
        ] {
            assert!(eligible_url(url));
        }
        for url in [
            "http://github.com/user-attachments/assets/id",
            "https://127.0.0.1/a.mp4",
            "https://github.com.evil.test/user-attachments/assets/id",
            "https://github.com@evil.test/user-attachments/assets/id",
            "https://github.com/user-attachments/assets/",
            "https://user-images.githubusercontent.com/a/../b.mp4",
            "https://private-user-images.githubusercontent.com/a.mp4?x=%2fetc",
            "https://private-user-images.githubusercontent.com/a.mp4?x=%0Dabc",
            "https://private-user-images.githubusercontent.com/a.mp4?x=%zz",
            "https://user-images.githubusercontent.com/a.mp4#x",
            "https://github.com/user-attachments/assets/--x\n",
        ] {
            assert!(!eligible_url(url), "{url}");
        }
    }
    #[test]
    fn markdown_code_is_ignored_declared_block_has_priority_and_comments_deduplicate() {
        let body = format!(
            "{B}\n```text\n{A}\n```\n`{B}`\n<!-- codex-pr-video:start -->\n[Video]({A})\n<!-- codex-pr-video:end -->"
        );
        let result = attachments([body.as_str(), A, B]).unwrap();
        assert_eq!(
            result
                .items
                .iter()
                .map(|i| i.url.as_str())
                .collect::<Vec<_>>(),
            vec![A, B]
        );
        assert!(!result.limited);
        assert!(
            attachments([format!("```\n{A}").as_str()])
                .unwrap()
                .items
                .is_empty()
        );
        assert!(
            attachments([format!("`{A}`").as_str()])
                .unwrap()
                .items
                .is_empty()
        );
    }
    #[test]
    fn static_images_are_excluded_and_signed_queries_are_not_in_debug_or_labels() {
        let signed = "https://private-user-images.githubusercontent.com/123/Clip.MP4?jwt=SECRET";
        let body = format!(
            "{signed}\nhttps://user-images.githubusercontent.com/123/still.PNG\nhttps://private-user-images.githubusercontent.com/123/still.png?jwt=x"
        );
        let result = attachments([body.as_str()]).unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].label, "Clip.MP4");
        assert!(!format!("{result:?}").contains("SECRET"));
    }
    #[test]
    fn input_and_result_budgets_report_limits() {
        assert!(attachments(["x".repeat(MAX_BODY_BYTES + 1).as_str()]).is_err());
        assert!(attachments(std::iter::repeat_n("", MAX_BODIES + 1)).is_err());
        let body = (0..40)
            .map(|n| format!("https://github.com/user-attachments/assets/id-{n}\n"))
            .collect::<String>();
        let result = attachments([body.as_str()]).unwrap();
        assert!(result.limited);
        assert_eq!(result.items.len(), 32);
    }
}
