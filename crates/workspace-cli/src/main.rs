use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;
use std::{
    borrow::Cow,
    hint::black_box,
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};
use workspace_core::{
    agents::{Agent, Location, Overview, TaskTree},
    input::{Binding, Context, KeyRouter, Route},
    modules::{ModuleSpec, Modules},
    picker::{PickerRequests, rank},
};

const KEYMAPS: &str = include_str!("../../../Resources/Contracts/keymaps.json");
const AGENTS: &str = include_str!("../../../Resources/Contracts/agents-demo.json");

fn main() {
    let result = match std::env::args().nth(1).as_deref() {
        Some("demo") => demo(),
        Some("pty-smoke") => pty_smoke(),
        Some("bench") => bench(),
        Some("edit-bench") => edit_bench(),
        _ => Err("usage: workspace-rust demo | pty-smoke | bench | edit-bench".into()),
    };
    if let Err(error) = result {
        eprintln!("ERROR: {error}");
        std::process::exit(1);
    }
}
fn bindings() -> Result<Vec<Binding>, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(KEYMAPS)?)
}

fn demo() -> Result<(), Box<dyn std::error::Error>> {
    let mut router = KeyRouter::compile(&bindings()?)?;
    router.set_context(Context::Workspace);
    let routes: Vec<_> = ["<Space>", "a", "a"]
        .map(|key| format!("{:?}", router.route(key, key.as_bytes())))
        .into();
    let mut modules = Modules::new(vec![
        ModuleSpec {
            id: "agent_model".into(),
            dependencies: vec![],
        },
        ModuleSpec {
            id: "agents_ui".into(),
            dependencies: vec!["agent_model".into()],
        },
        ModuleSpec {
            id: "tree".into(),
            dependencies: vec![],
        },
        ModuleSpec {
            id: "previews".into(),
            dependencies: vec![],
        },
    ])?;
    let requested = modules.request("agents_ui")?;
    let mut overview = Overview::default();
    overview.replace(serde_json::from_str::<Vec<Agent>>(AGENTS)?, 1000)?;
    let before: Vec<_> = overview
        .agents()
        .iter()
        .map(|a| json!({"id":a.id,"state":a.status_kind(1000),"target":a.focus_target()}))
        .collect();
    overview.toggle_backlog("codex-thread:demo-ready")?;
    let backlog_before_hide = overview.visible(Location::Later).len();
    let changes = overview.dismiss("codex-thread:demo-ready", 1000)?;
    let journal = workspace_core::agents::ViewState::journal_record(changes)?;
    let tree = TaskTree::build(&overview.visible(Location::Main));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "mode":"headless Rust contract demo", "data":"synthetic only", "gui":"separate workspace-app binary; this command is headless",
            "input_sequence":routes,"lazy_load_order":requested,"tree_state":modules.state("tree"),
            "previews_state":modules.state("previews"),"agents":before,"task_tree":tree,
            "backlog_before_hide":backlog_before_hide,"backlog_after_hide":overview.visible(Location::Later).len(),
            "hide_journal":journal,"messages_sent":0
        }))?
    );
    Ok(())
}

fn pty_smoke() -> Result<(), Box<dyn std::error::Error>> {
    let system = native_pty_system();
    let pair = system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut command = CommandBuilder::new("/bin/sh");
    command.arg("-c");
    command.arg(r#"printf '\033[2J\033[HREADY\n'; IFS= read -r value; /bin/stty size; printf 'echo:%s\n' "$value""#);
    command.env_clear();
    command.env("PATH", "/usr/bin:/bin");
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader.take(65536).read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    let result = (|| -> Result<_, Box<dyn std::error::Error>> {
        pair.master.resize(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        writer.write_all(b"rust-pty\n")?;
        writer.flush()?;
        let bytes = rx.recv_timeout(Duration::from_secs(5))??;
        let text = String::from_utf8_lossy(&bytes);
        if !text.contains("40 120")
            || !text.contains("echo:rust-pty")
            || !bytes.starts_with(b"rust-pty") && !text.contains("READY")
        {
            return Err(
                "PTY byte/resize probe did not return the expected synthetic result".into(),
            );
        }
        let status = child.wait()?;
        if !status.success() {
            return Err("PTY probe process failed".into());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"passed":true,"transport":"native PTY","input_echo":true,"resize":{"rows":40,"columns":120},"vt_bytes_seen":bytes.windows(4).any(|b|b==b"\x1b[2J"),"child_exit_success":true,"renderer_tested":false,"user_config_loaded":false})
            )?
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn bench() -> Result<(), Box<dyn std::error::Error>> {
    let mut router = KeyRouter::compile(&bindings()?)?;
    let input = b"ordinary terminal bytes";
    let iterations = 1_000_000u64;
    let started = Instant::now();
    for _ in 0..iterations {
        if !matches!(
            black_box(router.route("text", black_box(input))),
            Route::Pty(Cow::Borrowed(_))
        ) {
            return Err("terminal fast path allocated or intercepted input".into());
        }
    }
    let mean_ns = started.elapsed().as_nanos() as f64 / iterations as f64;
    let items: Vec<_> = (0..10_000)
        .map(|i| format!("workspace/project-{i:05}/src/module.rs"))
        .collect();
    let requests = PickerRequests::default();
    let mut times = Vec::new();
    let mut matches = 0;
    for _ in 0..30 {
        let request = requests.begin();
        let started = Instant::now();
        matches = black_box(rank(&request, "projsrc", &items, 50)?).len();
        times.push(started.elapsed().as_micros());
    }
    times.sort_unstable();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"scope":"headless routing and synthetic fuzzy ranking; no GPU or input-to-pixel measurement","profile":"release","terminal_routes":iterations,"route_mean_ns":mean_ns,"picker_candidates":items.len(),"picker_samples":times.len(),"picker_results":matches,"picker_median_us":times[times.len()/2],"picker_p95_us":times[times.len()*95/100]})
        )?
    );
    Ok(())
}

fn edit_bench() -> Result<(), Box<dyn std::error::Error>> {
    let mut cases = Vec::new();
    for size in [10_000usize, 100_000, 1_000_000] {
        let text = "let answer = 42; // synthetic line\n".repeat(size / 35);
        let initial_chars = text.chars().count();
        let created = Instant::now();
        let mut editor = workspace_app::editor::Editor::new(text);
        let editor_create_us = created.elapsed().as_micros();
        editor.goto(size / 70, 0);
        editor.key("i");
        let mut samples = Vec::new();
        for _ in 0..50 {
            let start = Instant::now();
            editor.insert(black_box("x"));
            samples.push(start.elapsed().as_micros());
        }
        let first_insert_us = samples[0];
        samples.sort_unstable();
        let insert_max_us = samples[samples.len() - 1];
        let start = Instant::now();
        for _ in 0..1000 {
            black_box(editor.visible_lines(100, 60));
        }
        cases.push(json!({"requested_chars_approx":size,"initial_chars":initial_chars,"editor_create_us":editor_create_us,"first_insert_us":first_insert_us,"insert_max_us":insert_max_us,"insert_samples":samples.len(),"insert_median_us":samples[samples.len()/2],"insert_p95_us":samples[samples.len()*95/100],"visible_60_lines_mean_ns":start.elapsed().as_nanos()/1000}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"scope":"synthetic editor mutation and visible-line index only; no GUI, GPU, IME or physical input-to-pixel measurement","profile":"release","cases":cases})
        )?
    );
    Ok(())
}
