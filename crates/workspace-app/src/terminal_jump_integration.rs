//! Synthetic end-to-end test of the tmux backend and native pane models.
use crate::{
    buffer_numbers::BufferTracking,
    buffer_registry::{Buffer, BufferRegistry},
    editor::{Editor, LiveJumpTracking},
    editor_jump_navigation::{JumpApplied, JumpRequest},
    editor_panes::{
        Content, OpenedTerminalFileGuard, Panes, PendingTerminalOriginLoad,
        record_terminal_origin_after_open, stage_listed_buffer_switch,
        stage_terminal_origin_file_jump,
    },
    terminal_jump_gate::{Completion, Context, DeferredJump, Gate, PendingFileCapture},
    terminal_jump_marks::MarkBank,
    tmux::{Action, Backend, CopyMark, Event, RestoreCopyMark, Session},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};
use workspace_core::editor_jumplist::{Location, View};

fn captured(backend: &Backend, token: u64) -> CopyMark {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < deadline, "synthetic capture timed out");
        match backend.events.recv_timeout(Duration::from_millis(500)) {
            Ok(Event::CopyMarkCaptured(id, Ok(mark))) if id == token => return mark,
            Ok(Event::CopyMarkCaptured(id, Err(error))) if id == token => panic!("{error}"),
            Ok(Event::Error(error)) => panic!("{error}"),
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("{error}"),
        }
    }
}

#[test]
fn synthetic_terminal_file_terminal_file_round_trip_preserves_panel_and_marks() {
    let root = std::env::temp_dir().canonicalize().unwrap();
    let dir = root.join(format!(
        "tw-jump-roundtrip-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let backend = Backend::start(root, dir.clone(), false);
    let session = loop {
        match backend.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Ready(session, _) => break session,
            Event::Error(error) => panic!("{error}"),
            _ => {}
        }
    };
    struct Cleanup(Session, PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.run(&["kill-server"]);
            let _ = std::fs::remove_dir_all(&self.1);
        }
    }
    let _cleanup = Cleanup(session.clone(), dir);
    session
        .run(&[
            "send-keys",
            "-t",
            &format!("{}:", session.name),
            "printf 'SYNTHETIC-JUMP-LINE\\n'",
            "Enter",
        ])
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = session
            .run(&["capture-pane", "-p", "-t", &format!("{}:", session.name)])
            .unwrap();
        if output.lines().any(|line| line == "SYNTHETIC-JUMP-LINE") {
            break;
        }
        assert!(Instant::now() < deadline, "synthetic output not ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    backend.send(Action::Normal(true)).unwrap();
    backend.send(Action::Motion("gg".into(), 1)).unwrap();
    backend.send(Action::CaptureCopyMark(11)).unwrap();
    let mark = captured(&backend, 11);

    let file_path = PathBuf::from("/synthetic/unsaved");
    // Real backend mark retained across either order of the synthetic file
    // read and capture; the GUI still has its own context checks.
    let mut mark_first = PendingFileCapture::new(file_path.clone());
    assert!(mark_first.receive(mark.clone()).is_ok());
    assert!(
        mark_first
            .take(PathBuf::from("/synthetic/other").as_path())
            .is_none()
    );
    assert!(mark_first.take(&file_path).is_some());
    let mut file_first = PendingFileCapture::new(file_path.clone());
    assert!(file_first.take(&file_path).is_none());
    assert!(file_first.receive(mark.clone()).is_ok());
    assert!(file_first.take(&file_path).is_some());
    let terminal = Buffer::Terminal(77);
    let file = Buffer::File(file_path.clone());
    let mut numbers = BufferTracking::default();
    numbers.register(1, terminal.clone());
    numbers.register(1, file.clone());
    let mut registry = BufferRegistry::default();
    registry.register(terminal.clone());
    registry.register(file.clone());
    // A listed buffer may have been unloaded while keeping its stable ID.
    assert!(registry.mark_unloaded(&file_path));
    assert!(registry.is_unloaded(&file_path));
    registry.mark_loaded(&file_path);
    assert!(!registry.is_unloaded(&file_path));
    let mut panes = Panes::terminal(77);
    let pane = panes.layout.active();
    let terminal_location = mark
        .jump_location(numbers.number_for(1, &terminal).unwrap())
        .unwrap();
    let mut marks = MarkBank::default();
    let anchor = marks.insert(1, terminal_location, mark.clone()).unwrap();
    let guard = panes.layout.focus_guard();
    let mut switched = stage_listed_buffer_switch(&panes, &numbers, 1, file.clone()).unwrap();
    registry.replace(pane, Some(&terminal), file.clone());
    let opened = OpenedTerminalFileGuard::capture(1, 8, &file_path, &switched).unwrap();
    assert!(opened.is_current(1, 8, &file_path, &switched));
    assert!(!opened.is_current(1, 9, &file_path, &switched));
    let mut stale = switched.clone();
    stale
        .layout
        .split(crate::editor_panes::Axis::Row, true)
        .unwrap();
    stale.layout.focus(pane).unwrap();
    assert!(!record_terminal_origin_after_open(
        &mut stale,
        &numbers,
        &registry,
        1,
        77,
        &guard,
        &file_path,
        terminal_location,
        anchor
    ));
    assert!(stale.layout.jump_list(pane).unwrap().entries().is_empty());
    assert!(record_terminal_origin_after_open(
        &mut switched,
        &numbers,
        &registry,
        1,
        77,
        &guard,
        &file_path,
        terminal_location,
        anchor
    ));
    panes = switched;
    let mut editors = HashMap::from([(file_path.clone(), Editor::new("file contents".into()))]);
    let file_id = numbers.number_for(1, &file).unwrap();
    let mut tracking = LiveJumpTracking::default();
    tracking.sync(
        file_id,
        editors.get_mut(&file_path).unwrap(),
        None,
        &mut [&mut panes.layout],
    );
    let file_location = Location {
        buffer: file_id,
        row: 0,
        column: 0,
        view: View::default(),
    };
    // The user requested Ctrl+o before the queued tmux mark was applied.
    // Once the guarded origin has been recorded, replay it only at the
    // original file position and in Normal mode.
    let deferred = DeferredJump::new(file_location, true, 1);
    let (older, count) = deferred.take_if_current(Some(file_location), true).unwrap();
    let terminals = HashSet::from([77]);
    let request = JumpRequest::prepare(
        &panes,
        &numbers,
        &tracking,
        &registry,
        &editors,
        &terminals,
        1,
        7,
        file_location,
        older,
        count,
    )
    .unwrap()
    .unwrap();
    assert_eq!(request.target(), Some(&terminal));
    assert_eq!(request.target_anchor(), Some(anchor));
    let mark_for_restore = marks
        .get(anchor, 1, request.target_location().unwrap())
        .unwrap()
        .clone();
    let context = Context {
        workspace: 1,
        pane: pane.0,
        attachment: 77,
    };
    let mut gate = Gate::default();
    let token = gate
        .try_dispatch(context, request, |token| {
            backend
                .send(Action::RestoreCopyMark(token.get(), mark_for_restore))
                .map_err(|_| ())
        })
        .unwrap_or_else(|_| panic!("queue refused"));
    let deadline = Instant::now() + Duration::from_secs(15);
    let result = loop {
        assert!(Instant::now() < deadline, "synthetic restore timed out");
        match backend.events.recv_timeout(Duration::from_millis(500)) {
            Ok(Event::CopyMarkRestored(id, result)) if id == token.get() => break result,
            Ok(Event::Error(error)) => panic!("{error}"),
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("{error}"),
        }
    };
    assert_eq!(result, RestoreCopyMark::Restored);
    let Some(Completion::Ready(request)) = gate.finish(token, context, result, true) else {
        panic!("missing ack")
    };
    assert!(matches!(
        request.apply(
            &mut panes,
            &numbers,
            &tracking,
            &mut registry,
            &editors,
            &terminals,
            1,
            7,
            file_location
        ),
        Ok(JumpApplied::Move {
            target: Buffer::Terminal(77),
            ..
        })
    ));
    assert_eq!(panes.content(pane), Some(&Content::Terminal(77)));

    backend.send(Action::CaptureCopyMark(12)).unwrap();
    let current_mark = captured(&backend, 12);
    let current = current_mark
        .jump_location(numbers.number_for(1, &terminal).unwrap())
        .unwrap();
    let current_anchor = marks.insert(1, current, current_mark).unwrap();
    let focus = panes.layout.focus_guard();
    let staged = stage_terminal_origin_file_jump(
        &panes,
        &numbers,
        &registry,
        1,
        77,
        &focus,
        current,
        current_anchor,
        false,
        1,
        |path| editors.contains_key(path),
    )
    .unwrap()
    .unwrap();
    assert_eq!(staged.destination, Some((file_path.clone(), file_location)));
    assert_eq!(
        staged.panes.content(pane),
        Some(&Content::File(file_path.clone()))
    );
    assert_eq!(
        staged
            .panes
            .layout
            .prepare_jump(file_location, true, 1)
            .unwrap()
            .target_anchor(),
        Some(anchor)
    );

    // The same forward jump also survives unloading the destination. The
    // backend mark is real; the file load and typed text are synthetic.
    tracking.suspend(
        file_id,
        editors.get_mut(&file_path),
        &mut [&mut panes.layout],
    );
    editors.remove(&file_path);
    assert!(registry.mark_unloaded(&file_path));
    let pending = PendingTerminalOriginLoad::prepare(
        &panes,
        &numbers,
        &registry,
        1,
        8,
        77,
        &focus,
        current,
        current_anchor,
        false,
        1,
        |number| tracking.is_suspended(number),
    )
    .unwrap()
    .unwrap();
    assert_eq!(pending.path(), file_path);
    let input_context = crate::open_input::Context {
        workspace: 1,
        pane: pane.0,
        attachment: 77,
        generation: 8,
        path: file_path.clone(),
    };
    let mut input = crate::open_input::Queue::new(input_context.clone(), 8, 128);
    assert!(input.push(crate::open_input::Event::Key("i"), 1).is_ok());
    assert!(
        input
            .push(
                crate::open_input::Event::Commit {
                    range: None,
                    text: "ñ".into()
                },
                0
            )
            .is_ok()
    );
    let mut restored = Editor::new("file contents".into());
    assert!(tracking.resume(file_id, &mut restored));
    let staged = pending
        .finish(&panes, &numbers, &registry, 1, 8, 77, |path| {
            path == file_path && tracking.is_current(file_id, &restored)
        })
        .unwrap();
    assert_eq!(staged.destination, Some((file_path.clone(), file_location)));
    assert_eq!(
        staged.panes.content(pane),
        Some(&Content::File(file_path.clone()))
    );
    assert_eq!(panes.content(pane), Some(&Content::Terminal(77)));
    assert!(input.ready(&input_context, true));
    while let Some((id, event)) = input
        .begin(&input_context, true)
        .map(|(id, event)| (id, event.clone()))
    {
        match event {
            crate::open_input::Event::Key(key) => {
                restored.key(key);
            }
            crate::open_input::Event::Commit { range, text } => {
                restored.commit_input(range, &text).unwrap()
            }
            _ => panic!("unexpected synthetic input"),
        }
        assert!(input.acknowledge(id));
    }
    assert_eq!(restored.contents(), "ñfile contents");
}
