#[cfg(all(test, feature = "native-test"))]
mod synthetic_window_probe {
    use super::{EditMode, Editor, Surface, WorkspaceView};
    use gpui::TestAppContext;

    #[gpui::test]
    fn restart_restores_only_listed_verified_terminals_without_duplicate_clients(
        cx: &mut TestAppContext,
    ) {
        use std::{
            fs,
            time::{Duration, Instant},
        };
        use workspace_app::{
            recovery,
            tmux::{Backend, Event, Session},
        };
        let root = workspace_app::unnamed::id(&std::env::temp_dir());
        workspace_app::storage::private_directory(&root).unwrap();
        let state = root.join("state");
        workspace_app::storage::private_directory(&state).unwrap();
        let demo_source = root.join("demo-source");
        fs::create_dir(&demo_source).unwrap();
        let project = state.join("demo/workspace");
        fs::create_dir_all(&project).unwrap();
        let runtime = root.join("tmux");
        let primary = Session::prepare(&project, &runtime, false).unwrap();
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let _cleanup = Cleanup(primary.clone());
        fn make_terminal(primary: &Session) -> (Session, workspace_app::terminal::Terminal) {
            let name = primary.additional_terminal_name().unwrap();
            let backend = Backend::start_additional_notifying(primary.clone(), name, false, None);
            loop {
                match backend
                    .events
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap()
                {
                    Event::ReadyAdditional(session, _, terminal) => return (session, terminal),
                    Event::Error(error) => panic!("synthetic creation: {error}"),
                    _ => {}
                }
            }
        }
        let (listed, listed_client) = make_terminal(&primary);
        let (unlisted, unlisted_client) = make_terminal(&primary);
        let (held, held_client) = make_terminal(&primary);
        let held_pid = held_client.process_id().unwrap();
        drop(listed_client);
        drop(unlisted_client);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !listed.client_pids().unwrap().is_empty()
            || !unlisted.client_pids().unwrap().is_empty()
        {
            assert!(
                Instant::now() < deadline,
                "synthetic clients did not detach"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let listed_record = recovery::Terminal {
            root: project.clone(),
            name: listed.name.clone(),
        };
        let held_record = recovery::Terminal {
            root: project.clone(),
            name: held.name.clone(),
        };
        let store = recovery::Store::start(state.clone());
        assert!(matches!(
            store.events.recv_timeout(Duration::from_secs(5)).unwrap(),
            recovery::Event::Loaded(_)
        ));
        store
            .schedule(recovery::State {
                terminals: vec![listed_record.clone(), held_record.clone()],
                ..Default::default()
            })
            .unwrap();
        assert!(matches!(
            store.events.recv_timeout(Duration::from_secs(5)).unwrap(),
            recovery::Event::Written
        ));
        drop(store);
        let layout = workspace_app::storage::Layout { state, runtime };
        let (view, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceView::new(demo_source, true, layout, true, window, cx)
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.recovery_loaded
                        && v.windows
                            .iter()
                            .any(|window| window.session.name == listed.name)
                        && v.terminal_recovery_attempted.contains(&held_record)
                        && v.pending_additional_terminals.is_empty()
                })
            });
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "listed recovery timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Exercise the command through the GPUI key dispatcher while another
        // client owns the held fixture. It must retry without attaching twice.
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                let primary_id = v
                    .windows
                    .iter()
                    .find(|w| w.session.name == primary.name)
                    .unwrap()
                    .attachment;
                v.show_terminal_buffer(primary_id);
                window.focus(&v.focus);
                assert_eq!(v.model.surface, Surface::Normal);
                assert!(v.terminal_recovery_attempted.contains(&held_record));
                cx.notify();
            })
        });
        cx.simulate_keystrokes(": shift-t e r m i n a l shift-r e c o v e r enter");
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert!(
                    !v.terminal_recovery_attempted.contains(&held_record),
                    "{}",
                    v.model.message
                );
            })
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let done = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.terminal_recovery_attempted.contains(&held_record)
                        && v.pending_additional_terminals.is_empty()
                })
            });
            if done {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal command recovery timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(held.client_pids().unwrap(), vec![held_pid]);
        let (recovered_id, original_pid, primary_id) = cx.update(|_, app| {
            view.update(app, |v, cx| {
                assert!(
                    !v.windows
                        .iter()
                        .any(|window| window.session.name == unlisted.name)
                );
                assert!(
                    !v.windows
                        .iter()
                        .any(|window| window.session.name == held.name)
                );
                assert!(v.terminal_recovery_pending.contains(&held_record));
                assert!(!v.terminal_recovery_pending.contains(&listed_record));
                assert_eq!(v.windows[v.active].session.name, primary.name);
                let state = v.recovery_state();
                assert!(state.terminals.contains(&listed_record));
                assert!(state.terminals.contains(&held_record));
                assert!(
                    !state
                        .terminals
                        .iter()
                        .any(|record| record.name == unlisted.name)
                );
                let id = v
                    .windows
                    .iter()
                    .find(|window| window.session.name == listed.name)
                    .unwrap()
                    .attachment;
                let pid = v
                    .windows
                    .iter()
                    .find(|window| window.attachment == id)
                    .unwrap()
                    .terminal
                    .process_id()
                    .unwrap();
                let primary_id = v
                    .windows
                    .iter()
                    .find(|window| window.session.name == primary.name)
                    .unwrap()
                    .attachment;
                v.show_terminal_buffer(id);
                v.reconnect_terminal();
                assert!(v.pending_additional_reconnects.contains_key(&(0, id)));
                assert!(!v.terminal_quit_ready());
                v.quit_intent.cancel();
                assert!(
                    v.additional_backends
                        .get(&(0, id))
                        .unwrap()
                        .backend
                        .send(workspace_app::tmux::Action::New)
                        .is_err()
                );
                v.pending_additional_reconnects
                    .get_mut(&(0, id))
                    .unwrap()
                    .started = Instant::now() - Duration::from_secs(16);
                v.poll(cx);
                assert!(!v.pending_additional_reconnects.contains_key(&(0, id)));
                v.reconnect_terminal();
                assert!(v.pending_additional_reconnects.contains_key(&(0, id)));
                v.vim_delete_terminal_buffer(id);
                assert!(v.windows.iter().any(|window| window.attachment == id));
                v.show_terminal_buffer(primary_id);
                (id, pid, primary_id)
            })
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let done = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.pending_additional_reconnects
                        .contains_key(&(0, recovered_id))
                })
            });
            if done {
                break;
            }
            assert!(Instant::now() < deadline, "additional reconnect timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert_eq!(v.windows[v.active].attachment, primary_id);
                let terminal = v
                    .windows
                    .iter()
                    .find(|window| window.attachment == recovered_id)
                    .unwrap();
                assert_eq!(terminal.terminal.process_id(), Some(original_pid));
                assert!(
                    v.additional_backends
                        .get(&(0, recovered_id))
                        .is_some_and(|backend| backend.attachment == Some(recovered_id))
                );
                assert!(
                    v.model.message.contains("reconectada"),
                    "{}",
                    v.model.message
                );
            })
        });
        // Replace only the fixture's metadata worker; the tmux client stays alive.
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.additional_backends
                    .get_mut(&(0, recovered_id))
                    .unwrap()
                    .backend = workspace_app::tmux::Backend::disconnected_for_test();
                v.show_terminal_buffer(recovered_id);
                v.reconnect_terminal();
                assert!(
                    v.pending_additional_reconnects
                        .get(&(0, recovered_id))
                        .is_some_and(|pending| pending.disconnected)
                );
                v.show_terminal_buffer(primary_id);
            })
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let done = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.pending_additional_reconnects
                        .contains_key(&(0, recovered_id))
                })
            });
            if done {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "disconnected backend reconnect timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert_eq!(v.windows[v.active].attachment, primary_id);
                assert_eq!(
                    v.windows
                        .iter()
                        .find(|w| w.attachment == recovered_id)
                        .unwrap()
                        .terminal
                        .process_id(),
                    Some(original_pid)
                );
                assert!(
                    v.model.message.contains("backend anterior falló"),
                    "{}",
                    v.model.message
                );
            })
        });
        let pane_pid = listed
            .run(&[
                "display-message",
                "-p",
                "-t",
                &format!("{}:", listed.name),
                "#{pane_pid}",
            ])
            .unwrap();
        assert!(listed.client_pids().unwrap().contains(&original_pid));
        // SAFETY: this PID belongs to the synthetic tmux client created above.
        assert_eq!(unsafe { libc::kill(original_pid as i32, libc::SIGTERM) }, 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let exited = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.windows
                        .iter()
                        .find(|window| window.attachment == recovered_id)
                        .unwrap()
                        .snapshot
                        .exited
                })
            });
            if exited {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic terminal client did not exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.additional_backends
                    .get_mut(&(0, recovered_id))
                    .unwrap()
                    .backend = workspace_app::tmux::Backend::disconnected_for_test();
                v.show_terminal_buffer(recovered_id);
                v.reconnect_terminal();
                assert!(
                    v.pending_additional_reconnects
                        .get(&(0, recovered_id))
                        .is_some_and(|pending| pending.reattach)
                );
                v.effect(super::Effect::Pty(b"SYNTHETIC_UNSENT".to_vec()));
                let guard = v.terminal_resume.retained_guard().unwrap();
                assert_eq!(
                    v.terminal_resume.text_if_guarded(&guard),
                    Some("SYNTHETIC_UNSENT")
                );
            })
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let done = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.pending_additional_reconnects
                        .contains_key(&(0, recovered_id))
                })
            });
            if done {
                break;
            }
            assert!(Instant::now() < deadline, "input guard did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        let pane = listed
            .run(&["capture-pane", "-p", "-t", &format!("{}:", listed.name)])
            .unwrap();
        assert!(!pane.contains("SYNTHETIC_UNSENT"));
        cx.update(|_, app| {
            view.update(app, |v, _| {
                let guard = v.terminal_resume.retained_guard().unwrap();
                assert_eq!(
                    v.terminal_resume.text_if_guarded(&guard),
                    Some("SYNTHETIC_UNSENT")
                );
                assert!(v.terminal_resume.cancel_guarded(&guard));
                v.reconnect_terminal();
                assert!(
                    v.pending_additional_reconnects
                        .get(&(0, recovered_id))
                        .is_some_and(|pending| pending.reattach)
                );
                v.show_terminal_buffer(primary_id);
            })
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let done = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.pending_additional_reconnects
                        .contains_key(&(0, recovered_id))
                })
            });
            if done {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "dead terminal reattach timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let new_pid = cx.update(|_, app| {
            view.update(app, |v, _| {
                assert_eq!(v.windows[v.active].attachment, primary_id);
                let terminal = v
                    .windows
                    .iter()
                    .find(|window| window.attachment == recovered_id)
                    .unwrap();
                let pid = terminal.terminal.process_id().unwrap();
                assert_ne!(pid, original_pid, "{}", v.model.message);
                assert!(
                    v.model.message.contains("reconectada"),
                    "{}",
                    v.model.message
                );
                pid
            })
        });
        assert!(listed.client_pids().unwrap().contains(&new_pid));
        assert_eq!(
            listed
                .run(&[
                    "display-message",
                    "-p",
                    "-t",
                    &format!("{}:", listed.name),
                    "#{pane_pid}"
                ])
                .unwrap(),
            pane_pid
        );
        assert_eq!(held.client_pids().unwrap(), vec![held_pid]);
        let held_pane_pid = held
            .run(&[
                "display-message",
                "-p",
                "-t",
                &format!("{}:", held.name),
                "#{pane_pid}",
            ])
            .unwrap();
        // Retrying while another client owns the fixture must still fail closed.
        cx.update(|_, app| {
            view.update(app, |v, cx| {
                v.show_terminal_buffer(primary_id);
                assert_eq!(
                    v.windows[v.active].attachment, primary_id,
                    "before retry: {}",
                    v.model.message
                );
                v.retry_terminal_recovery();
                assert!(!v.terminal_recovery_attempted.contains(&held_record));
                v.poll(cx);
                assert!(v.terminal_recovery_attempted.contains(&held_record));
                assert!(!v.pending_additional_terminals.is_empty());
                v.retry_terminal_recovery();
                assert!(v.terminal_recovery_attempted.contains(&held_record));
            })
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let finished = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.terminal_recovery_attempted.contains(&held_record)
                        && v.pending_additional_terminals.is_empty()
                })
            });
            if finished {
                break;
            }
            assert!(Instant::now() < deadline, "held retry did not complete");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(held.client_pids().unwrap(), vec![held_pid]);
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert!(v.terminal_recovery_pending.contains(&held_record));
                assert!(!v.windows.iter().any(|w| w.session.name == held.name));
            })
        });
        drop(held_client);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !held.client_pids().unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "held synthetic client did not detach"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| view.update(app, |v, _| v.retry_terminal_recovery()));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            let finished = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.terminal_recovery_pending.contains(&held_record)
                        && v.windows.iter().any(|w| w.session.name == held.name)
                })
            });
            if finished {
                break;
            }
            assert!(Instant::now() < deadline, "detached retry did not recover");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert_eq!(v.windows[v.active].attachment, primary_id);
                assert_eq!(
                    v.windows
                        .iter()
                        .filter(|w| w.session.name == held.name)
                        .count(),
                    1
                );
            })
        });
        assert_eq!(held.client_pids().unwrap().len(), 1);
        assert_eq!(
            held.run(&[
                "display-message",
                "-p",
                "-t",
                &format!("{}:", held.name),
                "#{pane_pid}"
            ])
            .unwrap(),
            held_pane_pid
        );
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.show_terminal_buffer(recovered_id);
                v.vim_delete_terminal_buffer(recovered_id);
                let state = v.recovery_state();
                assert!(!state.terminals.contains(&listed_record));
                assert!(state.terminals.contains(&held_record));
                // A sidebar-only layout must still be able to display a terminal.
                v.show_terminal_buffer(primary_id);
                assert_eq!(v.windows[v.active].attachment, primary_id, "{}", v.model.message);
                let panes = v.editor_panes.as_ref().unwrap();
                assert!(matches!(panes.content(panes.layout.active()), Some(super::PaneContent::Terminal(id)) if *id == primary_id));
                let mut sidebar = super::EditorPanes::terminal(primary_id);
                sidebar.set_content(super::PaneContent::Explorer(0)).unwrap();
                v.editor_panes = Some(sidebar);
                v.model.surface = Surface::Tree;
                v.model.file = None;
                v.model.sidebar = true;
                v.model.agents_visible = false;
                v.show_terminal_buffer(primary_id);
                v.reconcile_sidebar_layout();
                let panes = v.editor_panes.as_ref().unwrap();
                assert_eq!(panes.layout.panes().len(), 1, "terminal switch recreated sidebar");
                assert!(matches!(panes.content(panes.layout.active()), Some(super::PaneContent::Terminal(id)) if *id == primary_id));
                let count = panes.layout.panes().len();
                v.reconcile_sidebar_layout();
                let panes = v.editor_panes.as_ref().unwrap();
                assert_eq!(panes.layout.panes().len(), count, "reconciliation recreated a sidebar");
                assert!(matches!(panes.content(panes.layout.active()), Some(super::PaneContent::Terminal(id)) if *id == primary_id));
                for content in [super::PaneContent::Explorer(0), super::PaneContent::Agents(0)] {
                    let agents = matches!(content, super::PaneContent::Agents(_));
                    let mut sidebar = super::EditorPanes::terminal(primary_id);
                    sidebar.set_content(content).unwrap();
                    v.editor_panes = Some(sidebar);
                    v.model.surface = if agents { Surface::Agents } else { Surface::Tree };
                    v.model.file = None;
                    v.model.sidebar = true;
                    v.model.agents_visible = agents;
                    let file = v.new_file_now().expect("sidebar to synthetic file");
                    v.reconcile_sidebar_layout();
                    let panes = v.editor_panes.as_ref().unwrap();
                    assert_eq!(panes.layout.panes().len(), 1, "file switch recreated sidebar");
                    assert!(matches!(panes.content(panes.layout.active()), Some(super::PaneContent::File(path)) if path == &file));
                    assert!(!v.model.sidebar);
                }
                let mut sidebar = super::EditorPanes::terminal(primary_id);
                sidebar.set_content(super::PaneContent::Explorer(0)).unwrap();
                let explorer = sidebar.layout.active();
                sidebar.layout.split(super::PaneAxis::Column, true).unwrap();
                sidebar.set_content(super::PaneContent::Agents(0)).unwrap();
                v.editor_panes = Some(sidebar);
                v.model.surface = Surface::Agents;
                v.model.file = None;
                v.model.sidebar = true;
                v.model.agents_visible = true;
                let file = v.new_file_now().expect("agents to synthetic file");
                v.reconcile_sidebar_layout();
                let panes = v.editor_panes.as_ref().unwrap();
                assert_eq!(panes.layout.panes().len(), 2);
                assert!(matches!(panes.content(explorer), Some(super::PaneContent::Explorer(0))));
                assert!(matches!(panes.content(panes.layout.active()), Some(super::PaneContent::File(path)) if path == &file));
                assert!(v.model.sidebar);
                assert!(!v.model.agents_visible);
            })
        });
    }

    #[gpui::test]
    fn terminal_command_creates_a_distinct_buffer_and_respects_changed_focus(
        cx: &mut TestAppContext,
    ) {
        use super::{Effect, Project, Terminal, TerminalBackend, TerminalWindow};
        use std::{
            fs,
            time::{Duration, Instant},
        };
        use workspace_app::tmux::{Backend, Event};

        let root = workspace_app::unnamed::id(&std::env::temp_dir());
        workspace_app::storage::private_directory(&root).unwrap();
        let state = root.join("state");
        workspace_app::storage::private_directory(&state).unwrap();
        let project = state.join("demo/workspace");
        fs::create_dir_all(&project).unwrap();
        let backend = Backend::start(project, root.join("tmux"), false);
        let session = loop {
            match backend
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Event::Ready(session, _) => break session,
                Event::Error(error) => panic!("primary: {error}"),
                _ => {}
            }
        };
        struct Cleanup(workspace_app::tmux::Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let cleanup = Cleanup(session.clone());
        let terminal = Terminal::spawn_notifying(session.attach(), None).unwrap();
        let snapshot = terminal.initial_snapshot();
        let layout = workspace_app::storage::Layout {
            state: state.clone(),
            runtime: state,
        };
        let (view, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceView::new(root.join("absent"), true, layout, true, window, cx)
        });
        let (first_pane, file_pane, file_path) = cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.windows.push(TerminalWindow {
                    attachment: 77,
                    project: 0,
                    session,
                    terminal,
                    snapshot,
                });
                v.backends.insert(
                    0,
                    TerminalBackend {
                        backend,
                        attachment: Some(77),
                    },
                );
                v.active = 0;
                v.model.surface = Surface::Normal;
                v.ensure_terminal_layout(0, 77);
                let first = v.editor_panes.as_ref().unwrap().layout.active();
                v.editor_window("v");
                let second = v.editor_panes.as_ref().unwrap().layout.active();
                assert_ne!(first, second);
                let file = v.new_file_now().expect("synthetic file");
                assert_eq!(v.model.surface, Surface::File);
                window.focus(&v.focus);
                cx.notify();
                (first, second, file)
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes(": t e r m i n a l enter");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.windows.len() == 2
                })
            });
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "terminal command did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            view.update(app, |v, _| {
                let panes = v.editor_panes.as_ref().unwrap();
                assert!(matches!(panes.content(first_pane), Some(super::PaneContent::Terminal(77))));
                let new_id = v.windows.iter().find(|w| w.attachment != 77).unwrap().attachment;
                assert!(matches!(panes.content(file_pane), Some(super::PaneContent::Terminal(id)) if *id == new_id));
                assert!(v.editors.contains_key(&file_path));
                assert!(v.additional_backends.contains_key(&(0, new_id)));
                assert_eq!(v.model.project, 0);
                v.activate_editor_file(file_path.clone(), String::new());
                v.set_focus(Surface::File);
                v.request_additional_terminal();
                v.activate_editor_pane(first_pane);
            });
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.windows.len() == 3
                })
            });
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "background terminal did not finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.editor_panes.as_ref().unwrap().layout.active(), first_pane);
            assert!(matches!(v.editor_panes.as_ref().unwrap().content(file_pane), Some(super::PaneContent::File(path)) if path == &file_path));
            assert_eq!(v.pending_additional_terminals.len(), 0);
        });
        let extra = cx.update(|_, app| {
            view.update(app, |v, _| {
                let extra = v
                    .windows
                    .iter()
                    .find(|w| w.attachment != 77)
                    .unwrap()
                    .attachment;
                let name = v
                    .windows
                    .iter()
                    .find(|w| w.attachment == extra)
                    .unwrap()
                    .session
                    .name
                    .clone();
                v.show_terminal_buffer(extra);
                v.vim_delete_terminal_buffer(extra);
                assert!(!v.windows.iter().any(|w| w.attachment == extra));
                let rebound = v
                    .windows
                    .iter()
                    .find(|w| w.session.name == name)
                    .unwrap()
                    .attachment;
                assert_ne!(rebound, extra);
                assert!(v.additional_backends.contains_key(&(0, rebound)));
                v.windows
                    .iter()
                    .find(|w| w.attachment == rebound)
                    .unwrap()
                    .session
                    .clone()
            })
        });
        extra.recover_additional_identity().unwrap();
        assert_eq!(cleanup.0.discover_additional_terminals().unwrap().len(), 2);
        let other_path = cleanup.0.root.join("synthetic-other");
        fs::create_dir_all(&other_path).unwrap();
        let other_backend = Backend::start(
            other_path.clone(),
            cleanup.0.socket.parent().unwrap().to_owned(),
            false,
        );
        let other_session = loop {
            match other_backend
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Event::Ready(session, _) => break session,
                Event::Error(error) => panic!("other workspace: {error}"),
                _ => {}
            }
        };
        let other_terminal = Terminal::spawn_notifying(other_session.attach(), None).unwrap();
        let other_snapshot = other_terminal.initial_snapshot();
        let old_clients = cx.update(|_, app| {
            view.update(app, |v, _| {
                let before = v
                    .windows
                    .iter()
                    .map(|terminal| {
                        (
                            terminal.session.name.clone(),
                            terminal.terminal.process_id(),
                        )
                    })
                    .collect::<Vec<_>>();
                let other = v.model.projects.len();
                v.model.projects.push(Project {
                    label: "synthetic-other".into(),
                    path: other_path,
                });
                v.windows.push(TerminalWindow {
                    attachment: 500,
                    project: other,
                    session: other_session,
                    terminal: other_terminal,
                    snapshot: other_snapshot,
                });
                v.backends.insert(
                    other,
                    TerminalBackend {
                        backend: other_backend,
                        attachment: Some(500),
                    },
                );
                let editor = v.editors.get_mut(&file_path).unwrap();
                editor.key("i");
                editor.insert("unsaved synthetic text");
                editor.key("<Esc>");
                assert!(editor.dirty());
                v.close_workspace();
                assert_eq!(v.model.project, 0);
                assert_eq!(v.parked_terminal_count, 0);
                assert!(v.editors.get(&file_path).unwrap().dirty());
                v.editors.get_mut(&file_path).unwrap().key("u");
                assert!(!v.editors.get(&file_path).unwrap().dirty());
                v.close_workspace();
                assert_eq!(v.model.project, other, "{}", v.model.message);
                assert_eq!(v.parked_terminal_count, 3);
                assert_eq!(v.windows.iter().filter(|w| w.project == 0).count(), 0);
                v.effect(Effect::Project(0));
                assert_eq!(v.parked_terminal_count, 0);
                assert_eq!(v.windows.iter().filter(|w| w.project == 0).count(), 3);
                assert_eq!(
                    v.additional_backends
                        .keys()
                        .filter(|(project, _)| *project == 0)
                        .count(),
                    2
                );
                let after = v
                    .windows
                    .iter()
                    .filter(|w| w.project == 0)
                    .map(|terminal| {
                        (
                            terminal.session.name.clone(),
                            terminal.terminal.process_id(),
                        )
                    })
                    .collect::<Vec<_>>();
                assert_eq!(before, after);
                before
            })
        });
        for (name, pid) in old_clients {
            let clients = cleanup
                .0
                .run(&["list-clients", "-t", &name, "-F", "#{client_pid}"])
                .unwrap();
            assert!(pid.is_some_and(|pid| clients.lines().any(|line| line == pid.to_string())));
        }
    }

    #[gpui::test]
    fn distinct_terminals_route_to_their_verified_backends(cx: &mut TestAppContext) {
        use super::{Effect, Project, Terminal, TerminalBackend, TerminalWindow, Ui};
        use gpui::EntityInputHandler;
        use std::{
            fs,
            time::{Duration, Instant},
        };
        use workspace_app::tmux::{Action, Backend, Event, Session};

        fn ready(backend: &Backend) -> Session {
            loop {
                match backend
                    .events
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap()
                {
                    Event::Ready(session, _) => return session,
                    Event::Error(error) => panic!("synthetic backend: {error}"),
                    _ => {}
                }
            }
        }
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let root = workspace_app::unnamed::id(&std::env::temp_dir());
        workspace_app::storage::private_directory(&root).unwrap();
        let state = root.join("state");
        workspace_app::storage::private_directory(&state).unwrap();
        let project = state.join("demo/workspace");
        fs::create_dir_all(&project).unwrap();
        let other_project = state.join("demo/other");
        fs::create_dir_all(&other_project).unwrap();
        let file = project.join("synthetic.rs");
        let first_backend = Backend::start(project.clone(), root.join("first-tmux"), false);
        let second_backend = Backend::start(project, root.join("second-tmux"), false);
        let other_backend = Backend::start(other_project.clone(), root.join("third-tmux"), false);
        let first_session = ready(&first_backend);
        let second_session = ready(&second_backend);
        let other_session = ready(&other_backend);
        assert_ne!(first_session.socket, second_session.socket);
        let first_cleanup = Cleanup(first_session.clone());
        let second_cleanup = Cleanup(second_session.clone());
        let _other_cleanup = Cleanup(other_session.clone());
        let first = Terminal::spawn_notifying(first_session.attach(), None).unwrap();
        let second = Terminal::spawn_notifying(second_session.attach(), None).unwrap();
        let other = Terminal::spawn_notifying(other_session.attach(), None).unwrap();
        let first_snapshot = first.initial_snapshot();
        let second_snapshot = second.initial_snapshot();
        let other_snapshot = other.initial_snapshot();
        let layout = workspace_app::storage::Layout {
            state: state.clone(),
            runtime: state,
        };
        let (view, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceView::new(
                root.join("absent-synthetic-source"),
                true,
                layout,
                true,
                window,
                cx,
            )
        });
        let other_index = cx.update(|window, app| {
            view.update(app, |v, cx| {
                let other_index = v.model.projects.len();
                v.model.projects.push(Project {
                    label: "synthetic-other".into(),
                    path: other_project,
                });
                v.windows.push(TerminalWindow {
                    attachment: 77,
                    project: 0,
                    session: first_session,
                    terminal: first,
                    snapshot: first_snapshot,
                });
                v.windows.push(TerminalWindow {
                    attachment: 78,
                    project: 0,
                    session: second_session,
                    terminal: second,
                    snapshot: second_snapshot,
                });
                v.windows.push(TerminalWindow {
                    attachment: 79,
                    project: other_index,
                    session: other_session,
                    terminal: other,
                    snapshot: other_snapshot,
                });
                v.backends.insert(
                    0,
                    TerminalBackend {
                        backend: first_backend,
                        attachment: Some(77),
                    },
                );
                v.backends.insert(
                    other_index,
                    TerminalBackend {
                        backend: other_backend,
                        attachment: Some(79),
                    },
                );
                v.active = 0;
                v.model.surface = Surface::Normal;
                v.ensure_terminal_layout(0, 77);
                v.effect(Effect::TerminalBuffer(78));
                assert_eq!(v.terminal_input_context().unwrap().attachment, 78);
                window.focus(&v.focus);
                cx.notify();
                other_index
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("i");
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert_eq!(v.model.surface, Surface::Normal);
                assert!(!v.terminal_resume.active());
                assert!(v.model.message.contains("no corresponde"));
                let context = v.terminal_input_context().unwrap();
                v.terminal_resume.begin(context, Instant::now());
                assert!(
                    v.terminal_resume
                        .push(context, b"TWR_OTHER_SESSION_PROBE")
                        .unwrap()
                );
                v.model.surface = Surface::Terminal;
                assert!(!v.detach_terminal_input_on_pane_focus());
                let guard = v.terminal_resume.retained_guard().unwrap();
                assert_eq!(
                    v.terminal_resume.text_if_guarded(&guard),
                    Some("TWR_OTHER_SESSION_PROBE")
                );
                assert!(v.terminal_resume.cancel_guarded(&guard));
                v.model.surface = Surface::Normal;
            });
        });
        for session in [&first_cleanup.0, &second_cleanup.0] {
            let output = session
                .run(&["capture-pane", "-p", "-t", &format!("{}:", session.name)])
                .unwrap();
            assert!(!output.contains("TWR_OTHER_SESSION_PROBE"));
        }
        // Install the second verified synthetic backend; the same GUI buffer
        // switching and resume paths must now reach each distinct tmux server.
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.additional_backends.insert(
                    (0, 78),
                    TerminalBackend {
                        backend: second_backend,
                        attachment: Some(78),
                    },
                );
                v.backends.get_mut(&0).unwrap().attachment = Some(78);
            });
        });
        cx.simulate_keystrokes("i");
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert_eq!(v.model.surface, Surface::Normal);
                assert!(v.model.message.contains("dos backends"));
                v.backends.get_mut(&0).unwrap().attachment = Some(77);
            });
        });
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                let stale = workspace_app::terminal_resume::Context {
                    project: 0,
                    attachment: 77,
                };
                v.ui(Ui::Tmux(Action::New, Some(stale)), window, cx);
                assert!(v.model.message.contains("control cambió"));
            });
        });
        assert_eq!(first_cleanup.0.windows().unwrap().len(), 1);
        assert_eq!(second_cleanup.0.windows().unwrap().len(), 1);
        let deadline = Instant::now() + Duration::from_secs(10);
        let clients = loop {
            let clients = first_cleanup.0.client_pids().unwrap();
            let pid = cx.update(|_, app| view.read(app).windows[0].terminal.process_id());
            if pid.is_some_and(|pid| clients.contains(&pid)) {
                break clients;
            }
            assert!(
                Instant::now() < deadline,
                "first synthetic frontend did not attach"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert!(v.attach_terminal(0, first_cleanup.0.clone(), Ok(clients)));
                assert_eq!(v.terminal_input_context().unwrap().attachment, 78);
            })
        });
        // A file-focused workspace must recover its own last terminal, not
        // whichever terminal occupies the first slot in the vector.
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.editors
                    .insert(file.clone(), Editor::new("synthetic\n".into()));
                v.activate_editor_file(file.clone(), "synthetic\n".into());
                v.set_focus(Surface::File);
                v.effect(Effect::Project(other_index));
                assert_eq!(v.model.project, other_index);
                v.effect(Effect::Project(0));
                assert_eq!(v.model.project, 0);
                assert_eq!(v.model.surface, Surface::File);
                assert_eq!(v.terminal_input_context().unwrap().attachment, 78);
                v.effect(Effect::TerminalBuffer(78));
                assert_eq!(v.terminal_input_context().unwrap().attachment, 78);
            })
        });
        cx.simulate_keystrokes("i");
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.replace_text_in_range(None, "TWR_SECOND_ONLY", window, cx)
            });
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let settled = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.terminal_resume.active() && v.additional_tmux_windows.contains_key(&(0, 78))
                })
            });
            let second = second_cleanup
                .0
                .run(&[
                    "capture-pane",
                    "-p",
                    "-t",
                    &format!("{}:", second_cleanup.0.name),
                ])
                .unwrap();
            if settled && second.contains("TWR_SECOND_ONLY") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "second terminal did not receive its input"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let first = first_cleanup
            .0
            .run(&[
                "capture-pane",
                "-p",
                "-t",
                &format!("{}:", first_cleanup.0.name),
            ])
            .unwrap();
        assert!(!first.contains("TWR_SECOND_ONLY"));
        // Cmd+t and Cmd+1 must target the selected backend's tmux session.
        cx.simulate_keystrokes("cmd-t");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let updated = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.terminal_resume.active()
                        && v.current_tmux_windows()
                            .is_some_and(|windows| windows.len() == 2)
                })
            });
            if updated && second_cleanup.0.windows().unwrap().len() == 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "second tmux window was not created"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(first_cleanup.0.windows().unwrap().len(), 1);
        cx.simulate_keystrokes("cmd-1");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let updated = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.terminal_resume.active()
                        && v.current_tmux_windows()
                            .is_some_and(|windows| windows.iter().any(|w| w.index == 1 && w.active))
                })
            });
            if updated
                && second_cleanup
                    .0
                    .windows()
                    .unwrap()
                    .iter()
                    .any(|w| w.index == 1 && w.active)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "second tmux window did not switch to index 1"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(first_cleanup.0.windows().unwrap().len(), 1);
        cx.simulate_keystrokes("ctrl-u escape escape");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model.surface == Surface::Normal && !v.terminal_resume.active()
                })
            });
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "second terminal did not return to normal"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.effect(Effect::TerminalBuffer(77));
                assert_eq!(v.terminal_input_context().unwrap().attachment, 77);
                assert_eq!(
                    v.current_tmux_windows().map(|windows| windows.len()),
                    Some(1)
                );
            })
        });
        cx.simulate_keystrokes("i");
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.replace_text_in_range(None, "TWR_FIRST_ONLY", window, cx)
            })
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let settled = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.terminal_resume.active()
                })
            });
            let first = first_cleanup
                .0
                .run(&[
                    "capture-pane",
                    "-p",
                    "-t",
                    &format!("{}:", first_cleanup.0.name),
                ])
                .unwrap();
            if settled && first.contains("TWR_FIRST_ONLY") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "first terminal did not receive its input"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let second = second_cleanup
            .0
            .run(&[
                "capture-pane",
                "-p",
                "-t",
                &format!("{}:", second_cleanup.0.name),
            ])
            .unwrap();
        assert!(!second.contains("TWR_FIRST_ONLY"));
        cx.simulate_keystrokes("ctrl-u escape escape");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model.surface == Surface::Normal && !v.terminal_resume.active()
                })
            });
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "first terminal did not return to normal"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.vim_delete_terminal_buffer(77);
                let replacement = v.windows[0].attachment;
                assert_ne!(replacement, 77, "{}", v.model.message);
                assert_eq!(v.backends.get(&0).unwrap().attachment, Some(replacement));
                v.effect(Effect::TerminalBuffer(replacement));
                assert_eq!(v.terminal_input_context().unwrap().attachment, replacement);
                assert!(
                    super::terminal_backend_for(
                        &v.backends,
                        &v.additional_backends,
                        v.terminal_input_context().unwrap()
                    )
                    .is_ok()
                );
            });
        });
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.windows.clear();
                v.backends.clear();
                v.additional_backends.clear();
            })
        });
    }

    #[gpui::test]
    fn wrapped_scroll_keys_affect_the_active_editor(cx: &mut TestAppContext) {
        let root = workspace_app::unnamed::id(&std::env::temp_dir());
        workspace_app::storage::private_directory(&root).unwrap();
        let state = root.join("state");
        let source = root.join("absent-synthetic-source");
        let layout = workspace_app::storage::Layout {
            state: state.clone(),
            runtime: state.clone(),
        };
        let path = state.join("demo/workspace/wrapped.txt");
        let content = format!("{}\nlast", "x".repeat(12000));
        let (view, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceView::new(source, true, layout, true, window, cx)
        });
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.editors.insert(path.clone(), Editor::new(content.clone()));
                v.activate_editor_file(path.clone(), content.clone());
                v.set_focus(Surface::File);
                window.focus(&v.focus);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes(": s e t space w r a p enter");
        cx.run_until_parked();
        let (pane, initial) = cx.update(|_, app| {
            let panes = view.read(app).editor_panes.as_ref().unwrap();
            let pane = panes.layout.active();
            let state = panes.wrap_state(pane).unwrap();
            assert!(state.enabled);
            (pane, state.top)
        });
        cx.simulate_keystrokes("ctrl-e");
        cx.run_until_parked();
        let after = cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.current_editor().unwrap().mode, EditMode::Normal);
            assert!(!v.current_editor().unwrap().dirty());
            v.editor_panes
                .as_ref()
                .unwrap()
                .wrap_state(pane)
                .unwrap()
                .top
        });
        assert_eq!(after.source_line, initial.source_line);
        assert_eq!(after.visual_row, initial.visual_row + 1);
        cx.simulate_keystrokes("7 ctrl-e");
        cx.run_until_parked();
        cx.update(|_, app| {
            let state = view
                .read(app)
                .editor_panes
                .as_ref()
                .unwrap()
                .wrap_state(pane)
                .unwrap();
            assert_eq!(state.top.visual_row, after.visual_row + 7);
        });
        cx.simulate_keystrokes("v ctrl-y");
        cx.run_until_parked();
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.current_editor().unwrap().mode, EditMode::Visual);
            assert!(!v.current_editor().unwrap().dirty());
            assert_eq!(
                v.editor_panes
                    .as_ref()
                    .unwrap()
                    .wrap_state(pane)
                    .unwrap()
                    .top
                    .visual_row,
                after.visual_row + 6
            );
        });
    }

    #[gpui::test]
    fn file_window_accepts_normal_insert_escape(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler;
        cx.update(|app| app.bind_keys([gpui::KeyBinding::new("cmd-q", crate::Quit, None)]));
        let root = workspace_app::unnamed::id(&std::env::temp_dir());
        workspace_app::storage::private_directory(&root).unwrap();
        let state = root.join("state");
        let source = root.join("absent-synthetic-source");
        let layout = workspace_app::storage::Layout {
            state: state.clone(),
            runtime: state.clone(),
        };
        let path = state.join("demo/workspace/synthetic.txt");
        let (view, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceView::new(source, true, layout, true, window, cx)
        });
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.editors.insert(path.clone(), Editor::new("seed".into()));
                v.activate_editor_file(path.clone(), "seed".into());
                v.set_focus(Surface::File);
                window.focus(&v.focus);
                cx.notify();
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("i x escape");
        cx.update(|_, app| {
            let editor = view.read(app).current_editor().unwrap();
            assert_eq!(editor.contents(), "xseed");
            assert_eq!(editor.mode, EditMode::Normal);
            assert_eq!(editor.position(), (0, 0));
        });
        // This tests repeated GPUI key events, not macOS press-and-hold.
        for _ in 0..3 {
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("l").unwrap(),
                is_held: true,
            });
        }
        cx.update(|_, app| assert_eq!(view.read(app).current_editor().unwrap().position(), (0, 3)));
        cx.simulate_keystrokes("i");
        for _ in 0..3 {
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("k").unwrap(),
                is_held: true,
            });
            // The test platform lacks the macOS backend's held-key callback.
            // Explicitly emulate its committed-text delivery after KeyDown.
            cx.update(|window, app| {
                view.update(app, |v, cx| v.replace_text_in_range(None, "k", window, cx))
            });
        }
        cx.simulate_keystrokes("escape");
        cx.update(|_, app| {
            assert_eq!(
                view.read(app).current_editor().unwrap().contents(),
                "xsekkked"
            )
        });
        cx.simulate_keystrokes("u");
        cx.update(|_, app| {
            assert_eq!(view.read(app).current_editor().unwrap().contents(), "xseed")
        });
        cx.simulate_keystrokes("0");
        let (left, initial_count) = cx.update(|_, app| {
            let panes = view.read(app).editor_panes.as_ref().unwrap();
            (panes.layout.active(), panes.layout.panes().len())
        });
        cx.simulate_keystrokes("ctrl-w v");
        let right = cx.update(|_, app| {
            let panes = view.read(app).editor_panes.as_ref().unwrap();
            assert_eq!(panes.layout.panes().len(), initial_count + 1);
            panes.layout.active()
        });
        assert_ne!(left, right);
        cx.simulate_keystrokes("ctrl-h");
        cx.update(|_, app| {
            assert_eq!(
                view.read(app)
                    .editor_panes
                    .as_ref()
                    .unwrap()
                    .layout
                    .active(),
                left
            )
        });
        cx.simulate_keystrokes("ctrl-l");
        cx.update(|_, app| {
            assert_eq!(
                view.read(app)
                    .editor_panes
                    .as_ref()
                    .unwrap()
                    .layout
                    .active(),
                right
            )
        });
        cx.simulate_keystrokes("space w q");
        cx.update(|_, app| {
            let panes = view.read(app).editor_panes.as_ref().unwrap();
            assert_eq!(panes.layout.panes().len(), initial_count);
            assert_eq!(panes.layout.active(), left);
        });
        cx.simulate_keystrokes("space b d");
        assert!(
            cx.has_pending_prompt(),
            "dirty buffer should prompt before closing"
        );
        cx.simulate_prompt_answer("Cancelar");
        cx.run_until_parked();
        cx.update(|_, app| {
            let v = view.read(app);
            assert!(!v.delete_prompt_open);
            assert_eq!(v.current_editor().unwrap().contents(), "xseed");
        });
        cx.simulate_keystrokes("space b p");
        cx.update(|_, app| {
            let v = view.read(app);
            assert!(
                v.model
                    .pinned_buffers
                    .contains(&v.model.file.as_ref().unwrap().0)
            );
        });
        cx.simulate_keystrokes("space b p");
        cx.update(|_, app| {
            let v = view.read(app);
            assert!(
                !v.model
                    .pinned_buffers
                    .contains(&v.model.file.as_ref().unwrap().0)
            );
        });
        cx.simulate_keystrokes("cmd-q");
        assert!(
            cx.has_pending_prompt(),
            "dirty buffer should prompt before quitting"
        );
        cx.simulate_prompt_answer("Cancelar");
        cx.run_until_parked();
        cx.update(|_, app| {
            assert_eq!(view.read(app).current_editor().unwrap().contents(), "xseed")
        });
    }
    #[gpui::test]
    fn terminal_and_new_file_share_a_split_and_focus_moves(cx: &mut TestAppContext) {
        use super::{
            Effect, PaneContent, PendingTerminalCapture, Terminal, TerminalBackend,
            TerminalCaptureKind, TerminalWindow, Ui,
        };
        use gpui::EntityInputHandler;
        use std::{
            fs,
            time::{Duration, Instant},
        };
        use workspace_app::tmux::{Action, Backend, Event, Session};
        cx.update(|app| app.bind_keys([gpui::KeyBinding::new("cmd-q", crate::Quit, None)]));
        let root = workspace_app::unnamed::id(&std::env::temp_dir());
        workspace_app::storage::private_directory(&root).unwrap();
        let state = root.join("state");
        workspace_app::storage::private_directory(&state).unwrap();
        let project = state.join("demo/workspace");
        fs::create_dir_all(&project).unwrap();
        let backend = Backend::start(project, root.join("tmux"), false);
        let session = loop {
            match backend
                .events
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            {
                Event::Ready(session, _) => break session,
                Event::Error(error) => panic!("synthetic backend: {error}"),
                _ => {}
            }
        };
        struct Cleanup(Session);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.run(&["kill-server"]);
            }
        }
        let _cleanup = Cleanup(session.clone());
        let terminal = Terminal::spawn_notifying(session.attach(), None).unwrap();
        let snapshot = terminal.initial_snapshot();
        backend.send(Action::Normal(true)).unwrap();
        let source = root.join("absent-synthetic-source");
        let layout = workspace_app::storage::Layout {
            state: state.clone(),
            runtime: state.clone(),
        };
        let (view, cx) = cx.add_window_view(move |window, cx| {
            WorkspaceView::new(source, true, layout, true, window, cx)
        });
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.windows.push(TerminalWindow {
                    attachment: 77,
                    project: 0,
                    session,
                    terminal,
                    snapshot,
                });
                v.backends.insert(
                    0,
                    TerminalBackend {
                        backend,
                        attachment: Some(77),
                    },
                );
                v.active = 0;
                v.model.surface = Surface::Normal;
                v.ensure_terminal_layout(0, 77);
                window.focus(&v.focus);
                cx.notify();
            })
        });
        cx.run_until_parked();
        // Simulate a stale ownership association. The selected terminal must
        // reject resume and retain any already queued synthetic input.
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.backends.get_mut(&0).unwrap().attachment = Some(78)
            });
        });
        cx.simulate_keystrokes("i");
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.model.surface, Surface::Normal);
            assert!(!v.terminal_resume.active());
            assert!(v.model.message.contains("no corresponde"));
        });
        cx.update(|_, app| {
            view.update(app, |v, _| {
                let context = v.terminal_input_context().unwrap();
                v.terminal_resume.begin(context, Instant::now());
                assert!(v.terminal_resume.push(context, b"synthetic").unwrap());
                v.model.surface = Surface::Terminal;
                assert!(!v.detach_terminal_input_on_pane_focus());
                assert!(v.terminal_resume.active());
                assert!(v.model.message.contains("no corresponde"));
                // This queue was manufactured by this synthetic test only.
                v.terminal_resume.cancel();
                v.model.surface = Surface::Normal;
                v.backends.get_mut(&0).unwrap().attachment = None;
            });
        });
        // A Ready event arriving after the fence deadline cannot send text.
        cx.update(|_, app| {
            view.update(app, |v, _| {
                let context = v.terminal_input_context().unwrap();
                let now = Instant::now();
                let token = v
                    .terminal_resume
                    .begin(context, now - Duration::from_secs(16));
                assert!(
                    v.terminal_resume
                        .push(context, b"TWR_TIMEOUT_PROBE")
                        .unwrap()
                );
                v.pending_backend_resume = Some((context, token));
                v.model.surface = Surface::Terminal;
                v.terminal_resume.mark_expired(now);
                v.backends.get_mut(&0).unwrap().attachment = Some(77);
                v.finish_pending_backend_resume(0, true);
                assert!(v.pending_backend_resume.is_none());
                let guard = v.terminal_resume.retained_guard().unwrap();
                assert_eq!(
                    v.terminal_resume.text_if_guarded(&guard),
                    Some("TWR_TIMEOUT_PROBE")
                );
                assert!(v.terminal_resume.cancel_guarded(&guard));
                v.model.surface = Surface::Normal;
                v.backends.get_mut(&0).unwrap().attachment = None;
            });
        });
        // If the old client is replaced while verification is pending, the
        // old bytes are retained and must not be sent to the new attachment.
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.begin_terminal_resume();
                v.replace_text_in_range(None, "TWR_REBIND_PROBE", window, cx);
                v.windows[0].attachment = 78;
                v.backends.get_mut(&0).unwrap().attachment = Some(78);
                v.finish_pending_backend_resume(0, true);
                let guard = v.terminal_resume.retained_guard().unwrap();
                assert_eq!(
                    v.terminal_resume.text_if_guarded(&guard),
                    Some("TWR_REBIND_PROBE")
                );
                assert!(v.terminal_resume.cancel_guarded(&guard));
                v.windows[0].attachment = 77;
                v.backends.get_mut(&0).unwrap().attachment = None;
                v.model.surface = Surface::Normal;
            });
        });
        // A failed verification retains the exact unsent synthetic text.
        cx.simulate_keystrokes("i");
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.replace_text_in_range(None, "TWR_REJECT_PROBE", window, cx);
                v.finish_pending_backend_resume(0, false);
                let guard = v.terminal_resume.retained_guard().unwrap();
                assert_eq!(
                    v.terminal_resume.text_if_guarded(&guard),
                    Some("TWR_REJECT_PROBE")
                );
                assert!(v.terminal_resume.cancel_guarded(&guard));
                assert_eq!(v.model.surface, Surface::Normal);
            });
        });
        // An unverified backend holds input until its Ready association is
        // established, then delivers it to this isolated tmux session.
        cx.simulate_keystrokes("i");
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                assert_eq!(v.model.surface, Surface::Terminal);
                assert!(v.pending_backend_resume.is_some());
                v.replace_text_in_range(None, "TWR_READY_PROBE", window, cx);
                assert!(v.terminal_resume.active());
            });
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let clients = loop {
            let clients = _cleanup.0.client_pids().unwrap();
            let pid = cx.update(|_, app| view.read(app).windows[0].terminal.process_id());
            if pid.is_some_and(|pid| clients.contains(&pid)) {
                break clients;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic tmux frontend did not attach"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        cx.update(|_, app| {
            view.update(app, |v, _| {
                assert!(v.attach_terminal(0, _cleanup.0.clone(), Ok(clients)));
                assert_eq!(v.backends.get(&0).unwrap().attachment, Some(77));
                v.finish_pending_backend_resume(0, true);
                assert!(v.pending_backend_resume.is_none());
            });
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let settled = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.terminal_resume.active()
                })
            });
            let output = _cleanup
                .0
                .run(&["capture-pane", "-p", "-t", &format!("{}:", _cleanup.0.name)])
                .unwrap();
            if settled && output.contains("TWR_READY_PROBE") {
                assert!(!output.contains("TWR_REJECT_PROBE"));
                assert!(!output.contains("TWR_TIMEOUT_PROBE"));
                assert!(!output.contains("TWR_REBIND_PROBE"));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "verified backend did not receive queued input"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.simulate_keystrokes("ctrl-u escape escape");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model.surface == Surface::Normal && !v.terminal_resume.active()
                })
            });
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal did not return to normal mode"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let (terminal_pane, explorer_pane, agents_pane, sidebar_right) = cx.update(|_, app| {
            let v = view.read(app);
            let panes = v.editor_panes.as_ref().unwrap();
            let find = |kind: fn(&PaneContent) -> bool| {
                panes
                    .layout
                    .order()
                    .into_iter()
                    .find(|id| panes.content(*id).is_some_and(kind))
                    .unwrap()
            };
            (
                find(|c| matches!(c, PaneContent::Terminal(77))),
                find(|c| matches!(c, PaneContent::Explorer(_))),
                find(|c| matches!(c, PaneContent::Agents(_))),
                v.model.right,
            )
        });
        cx.update(|_, app| {
            view.update(app, |v, cx| {
                let root = v.model.explorer_root().to_owned();
                v.model.entries = (0..8)
                    .map(|i| workspace_app::workspace::Entry {
                        path: root.join(format!("synthetic-{i}")),
                        label: format!("synthetic-{i}"),
                        depth: 0,
                        directory: false,
                    })
                    .collect();
                v.model.tree_selected = 0;
                cx.notify();
            })
        });
        cx.simulate_keystrokes(if sidebar_right { "ctrl-l" } else { "ctrl-h" });
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(
                v.editor_panes.as_ref().unwrap().layout.active(),
                explorer_pane,
                "surface={:?} right={} topology={} bounds={:?} message={}",
                v.model.surface,
                v.model.right,
                v.editor_panes.as_ref().unwrap().layout.root(),
                v.pane_bounds,
                v.model.message
            );
            assert_eq!(v.model.surface, Surface::Tree);
        });
        for _ in 0..3 {
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("j").unwrap(),
                is_held: true,
            });
        }
        cx.update(|_, app| assert_eq!(view.read(app).model.tree_selected, 3));
        cx.simulate_keystrokes("ctrl-j");
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(
                v.editor_panes.as_ref().unwrap().layout.active(),
                agents_pane
            );
            assert!(matches!(
                v.model.surface,
                Surface::Agents | Surface::Backlog
            ));
        });
        let (before, count) = cx.update(|_, app| {
            let v = view.read(app);
            let rows = v.model.rows(workspace_core::agents::Location::Main);
            let index = rows
                .iter()
                .position(|(row, _)| row == &v.model.selected)
                .unwrap();
            (index, rows.len())
        });
        assert!(count > before + 2);
        for _ in 0..2 {
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("j").unwrap(),
                is_held: true,
            });
        }
        cx.update(|_, app| {
            let v = view.read(app);
            let rows = v.model.rows(workspace_core::agents::Location::Main);
            assert_eq!(
                rows.iter().position(|(row, _)| row == &v.model.selected),
                Some(before + 2)
            );
        });
        cx.simulate_keystrokes("ctrl-k");
        cx.update(|_, app| {
            assert_eq!(
                view.read(app)
                    .editor_panes
                    .as_ref()
                    .unwrap()
                    .layout
                    .active(),
                explorer_pane
            )
        });
        cx.simulate_keystrokes(if sidebar_right { "ctrl-h" } else { "ctrl-l" });
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(
                v.editor_panes.as_ref().unwrap().layout.active(),
                terminal_pane
            );
            assert_eq!(v.model.surface, Surface::Normal);
        });
        // Emulate the macOS input handler delivering held-key text while the
        // terminal is resuming. This does not test physical autorepeat.
        cx.simulate_keystrokes("i");
        for _ in 0..3 {
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("q").unwrap(),
                is_held: true,
            });
            cx.update(|window, app| {
                view.update(app, |v, cx| v.replace_text_in_range(None, "q", window, cx))
            });
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let settled = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    !v.terminal_resume.active()
                })
            });
            let output = _cleanup
                .0
                .run(&["capture-pane", "-p", "-t", &format!("{}:", _cleanup.0.name)])
                .unwrap();
            if settled && output.contains("qqq") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal did not receive held-key text"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.simulate_keystrokes("ctrl-u escape escape");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model.surface == Surface::Normal && !v.terminal_resume.active()
                })
            });
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal did not return to normal mode"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let left = cx.update(|_, app| {
            view.read(app)
                .editor_panes
                .as_ref()
                .unwrap()
                .layout
                .active()
        });
        cx.simulate_keystrokes("ctrl-w v");
        let right = cx.update(|_, app| {
            view.read(app)
                .editor_panes
                .as_ref()
                .unwrap()
                .layout
                .active()
        });
        assert_ne!(left, right);
        cx.simulate_keystrokes("space f n");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model.surface == Surface::File
                })
            });
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic file opening timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.run_until_parked();
        cx.update(|_, app| {
            let v = view.read(app);
            let panes = v.editor_panes.as_ref().unwrap();
            assert!(matches!(
                panes.content(left),
                Some(PaneContent::Terminal(77))
            ));
            assert!(matches!(panes.content(right), Some(PaneContent::File(_))));
        });
        cx.simulate_keystrokes("ctrl-h");
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.editor_panes.as_ref().unwrap().layout.active(), left);
            assert_eq!(v.model.surface, Surface::Normal);
        });
        cx.simulate_keystrokes("ctrl-l");
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.editor_panes.as_ref().unwrap().layout.active(), right);
            assert_eq!(v.model.surface, Surface::File);
        });

        // The user's H/L mappings cycle listed buffers inside the focused
        // window: the terminal remains reachable and its PTY is not replaced.
        let (file, pid) = cx.update(|_, app| {
            let v = view.read(app);
            (
                v.model.file.as_ref().unwrap().0.clone(),
                v.windows[v.active].terminal.process_id(),
            )
        });
        cx.simulate_keystrokes("shift-h");
        cx.update(|_, app| {
            let v = view.read(app);
            let panes = v.editor_panes.as_ref().unwrap();
            assert_eq!(panes.layout.active(), right);
            assert!(
                matches!(panes.content(right), Some(PaneContent::Terminal(77))),
                "{}",
                v.model.message
            );
            assert!(matches!(
                panes.content(left),
                Some(PaneContent::Terminal(77))
            ));
            assert_eq!(v.windows[v.active].terminal.process_id(), pid);
        });
        cx.simulate_keystrokes("shift-l");
        cx.update(|_, app| {
            let v = view.read(app);
            let panes = v.editor_panes.as_ref().unwrap();
            assert_eq!(panes.layout.active(), right);
            assert!(
                matches!(panes.content(right), Some(PaneContent::File(path)) if path == &file),
                "{}",
                v.model.message
            );
            assert!(matches!(
                panes.content(left),
                Some(PaneContent::Terminal(77))
            ));
            assert!(v.editors.contains_key(&file));
        });
        cx.simulate_keystrokes(": shift-t e r m i n a l shift-r e c o v e r enter");
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.model.surface, Surface::File);
            assert_eq!(v.model.file.as_ref().unwrap().0, file);
            assert!(
                v.model.message.contains("No hay terminales pendientes"),
                "{}",
                v.model.message
            );
        });

        // Hold keys on the terminal, then simulate a successful disk reply.
        // The real GPUI dispatcher must deliver those keys once to the file.
        cx.simulate_keystrokes("ctrl-h");
        let target = state.join("demo/workspace/pending.txt");
        let generation = cx.update(|_, app| {
            view.update(app, |v, _| {
                v.file_generation += 1;
                let generation = v.file_generation;
                assert!(v.begin_open_input(generation, &target));
                generation
            })
        });
        cx.simulate_keystrokes("i q escape");
        cx.update(|_, app| {
            view.update(app, |v, cx| {
                assert_eq!(v.pending_open_input.as_ref().unwrap().queue.len(), 3);
                v.editors.insert(target.clone(), Editor::new("seed".into()));
                v.activate_editor_file(target.clone(), "seed".into());
                v.set_focus(Surface::File);
                v.finish_open_input_read(generation, &target, true);
                v.pump_open_input(cx);
                cx.notify();
            })
        });
        for _ in 0..16 {
            cx.run_until_parked();
            let done = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.pump_open_input(cx);
                    v.pending_open_input.is_none()
                })
            });
            if done {
                break;
            }
        }
        cx.run_until_parked();
        cx.update(|_, app| {
            let v = view.read(app);
            assert!(v.pending_open_input.is_none());
            let editor = v.current_editor().unwrap();
            assert_eq!(editor.contents(), "qseed");
            assert_eq!(editor.mode, EditMode::Normal);
        });

        // Return to the alternate terminal, start the real async file read and
        // send keys before polling its completion. This uses a private tmux
        // and a real synthetic file; AppKit/IME is not involved.
        cx.simulate_keystrokes("ctrl-6");
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.model.surface, Surface::Normal);
            assert!(matches!(
                v.editor_panes.as_ref().unwrap().content(left),
                Some(PaneContent::Terminal(77))
            ));
        });
        let disk_path = state.join("demo/workspace/on-disk.txt");
        fs::write(&disk_path, "disk").unwrap();
        cx.update(|_, app| {
            view.update(app, |v, cx| {
                v.effect(Effect::File(disk_path.clone()));
                assert!(v.pending_open_input.is_some());
                cx.notify();
            });
        });
        cx.simulate_keystrokes("i z escape");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.pump_open_input(cx);
                    v.model
                        .file
                        .as_ref()
                        .is_some_and(|(path, _)| path == &disk_path)
                        && v.pending_open_input.is_none()
                })
            });
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "async file open timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            let editor = v.current_editor().unwrap();
            assert_eq!(editor.contents(), "zdisk");
            assert_eq!(editor.mode, EditMode::Normal);
            assert_eq!(fs::read_to_string(&disk_path).unwrap(), "disk");
        });

        // Keep the key sequence in one GPUI update so the :e resolver cannot
        // be polled until after the following three keys have been delivered.
        cx.simulate_keystrokes("ctrl-6");
        cx.update(|_, app| assert_eq!(view.read(app).model.surface, Surface::Normal));
        let edit_path = state.join("demo/workspace/other.txt");
        fs::write(&edit_path, "other").unwrap();
        cx.update(|window, app| {
            for key in [
                ":", "e", "space", "o", "t", "h", "e", "r", ".", "t", "x", "t", "enter", "i", "y",
                "escape",
            ] {
                assert!(window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), app));
            }
            let pending = view.read(app).pending_open_input.as_ref().unwrap();
            assert!(pending.edit_resolution);
            assert_eq!(pending.queue.len(), 3);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.pump_open_input(cx);
                    v.model
                        .file
                        .as_ref()
                        .is_some_and(|(path, _)| path == &edit_path)
                        && v.pending_open_input.is_none()
                })
            });
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, ":e open timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let editor = view.read(app).current_editor().unwrap();
            assert_eq!(editor.contents(), "yother");
            assert_eq!(editor.mode, EditMode::Normal);
            assert_eq!(fs::read_to_string(&edit_path).unwrap(), "other");
        });

        cx.simulate_keystrokes("ctrl-6");
        cx.update(|_, app| assert_eq!(view.read(app).model.surface, Surface::Normal));

        // Exercise actual tmux capture, :bdelete and jump reload.
        let history_path = state.join("demo/workspace/history.txt");
        fs::write(&history_path, "first\nsecond\n").unwrap();
        cx.update(|_, app| {
            view.update(app, |v, cx| {
                v.effect(Effect::File(history_path.clone()));
                cx.notify();
            })
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model
                        .file
                        .as_ref()
                        .is_some_and(|(path, _)| path == &history_path)
                        && v.pending_open_input.is_none()
                        && v.pending_terminal_capture.is_none()
                })
            });
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "history file open timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.simulate_keystrokes("ctrl-o");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model.surface == Surface::Normal && !v.terminal_jump_gate.busy()
                })
            });
            if ready {
                break;
            }
            if Instant::now() >= deadline {
                cx.update(|_, app| {
                    let v = view.read(app);
                    panic!(
                        "jump back to terminal timed out: surface={:?}, file={:?}, message={}",
                        v.model.surface,
                        v.model.file.as_ref().map(|(path, _)| path.file_name()),
                        v.model.message
                    );
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|window, app| {
            for key in [":", "b", "d", "space", "#", "enter"] {
                assert!(window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), app));
            }
        });
        cx.update(|_, app| {
            let v = view.read(app);
            assert!(
                !v.editors.contains_key(&history_path),
                "{}",
                v.model.message
            );
            assert!(v.listed_buffers.get(&0).unwrap().is_unloaded(&history_path));
        });
        cx.simulate_keystrokes("ctrl-i");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let (reading, already_open) = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    (
                        v.pending_terminal_jump_load.is_some(),
                        v.model
                            .file
                            .as_ref()
                            .is_some_and(|(path, _)| path == &history_path),
                    )
                })
            });
            if reading {
                break;
            }
            assert!(!already_open, "did not observe the pending jump load");
            assert!(Instant::now() < deadline, "jump load did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        cx.update(|window, app| {
            for key in ["i", "w", "escape"] {
                assert!(window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), app));
            }
            let v = view.read(app);
            assert!(v.pending_terminal_jump_load.is_some());
            assert_eq!(v.pending_open_input.as_ref().unwrap().queue.len(), 3);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.pump_open_input(cx);
                    v.model
                        .file
                        .as_ref()
                        .is_some_and(|(path, _)| path == &history_path)
                        && v.pending_terminal_jump_load.is_none()
                        && v.pending_open_input.is_none()
                })
            });
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "jump to unloaded file timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.current_editor().unwrap().contents(), "wfirst\nsecond\n");
            assert_eq!(
                fs::read_to_string(&history_path).unwrap(),
                "first\nsecond\n"
            );
            assert!(!v.listed_buffers.get(&0).unwrap().is_unloaded(&history_path));
        });

        // A click in the other pane during the read must not redirect or
        // discard the queued keys, even if the file reply arrives later.
        cx.simulate_keystrokes("ctrl-6");
        cx.update(|_, app| assert_eq!(view.read(app).model.surface, Surface::Normal));
        cx.run_until_parked();
        let right_center = cx.update(|_, app| {
            view.read(app)
                .pane_outer_bounds
                .get(&right)
                .unwrap()
                .center()
        });
        let abandoned = state.join("demo/workspace/focus-change.txt");
        fs::write(&abandoned, "unchanged").unwrap();
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.effect(Effect::File(abandoned.clone()));
                assert!(v.pending_open_input.is_some());
                cx.notify();
            });
            for key in ["i", "a", "escape"] {
                assert!(window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), app));
            }
            assert_eq!(
                view.read(app)
                    .pending_open_input
                    .as_ref()
                    .unwrap()
                    .queue
                    .len(),
                3
            );
        });
        cx.simulate_click(right_center, gpui::Modifiers::none());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let retained = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.pending_open_input.as_ref().is_some_and(|pending| {
                        pending.queue.state() == workspace_app::open_input::State::Recover
                    })
                })
            });
            if retained {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "focus change did not retain input"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.editor_panes.as_ref().unwrap().layout.active(), right);
            assert_eq!(v.pending_open_input.as_ref().unwrap().queue.len(), 3);
            assert!(!v.editors.contains_key(&abandoned));
            assert_eq!(fs::read_to_string(&abandoned).unwrap(), "unchanged");
        });
        cx.simulate_keystrokes("cmd-q");
        assert!(cx.has_pending_prompt(), "pending input must block quitting");
        cx.simulate_prompt_answer("Cancelar");
        cx.run_until_parked();
        cx.update(|_, app| {
            assert_eq!(
                view.read(app)
                    .pending_open_input
                    .as_ref()
                    .unwrap()
                    .queue
                    .len(),
                3
            );
            assert_eq!(fs::read_to_string(&abandoned).unwrap(), "unchanged");
        });

        // The only discarded input here is the synthetic failed operation.
        // :e of a missing name keeps the file absent until an explicit :w.
        cx.update(|_, app| view.update(app, |v, _| v.discard_open_input()));
        cx.simulate_keystrokes("ctrl-h");
        cx.update(|_, app| assert_eq!(view.read(app).model.surface, Surface::Normal));
        let new_path = state.join("demo/workspace/fresh.txt");
        assert!(!new_path.exists());
        cx.update(|window, app| {
            for key in [
                ":", "e", "space", "f", "r", "e", "s", "h", ".", "t", "x", "t", "enter", "i", "n",
                "escape",
            ] {
                assert!(window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), app));
            }
            let pending = view.read(app).pending_open_input.as_ref().unwrap();
            assert!(pending.edit_resolution);
            assert_eq!(pending.queue.len(), 3);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.pump_open_input(cx);
                    v.model
                        .file
                        .as_ref()
                        .is_some_and(|(path, _)| path == &new_path)
                        && v.pending_open_input.is_none()
                })
            });
            if ready {
                break;
            }
            assert!(Instant::now() < deadline, "new :e buffer timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.current_editor().unwrap().contents(), "n");
            assert!(!new_path.exists());
        });
        cx.simulate_keystrokes(": w enter");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let saved = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    new_path.exists() && v.current_editor().is_some_and(|editor| !editor.dirty())
                })
            });
            if saved {
                break;
            }
            assert!(Instant::now() < deadline, "writing the new file timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(fs::read_to_string(&new_path).unwrap(), "n");

        // A real binary-file read failure must leave the terminal active and
        // retain every key for review rather than writing into the shell.
        cx.simulate_keystrokes("ctrl-6");
        cx.update(|_, app| assert_eq!(view.read(app).model.surface, Surface::Normal));
        let binary = state.join("demo/workspace/binary.bin");
        fs::write(&binary, b"\0synthetic").unwrap();
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.effect(Effect::File(binary.clone()));
                assert!(v.pending_open_input.is_some());
                cx.notify();
            });
            for key in ["i", "x", "escape"] {
                assert!(window.dispatch_keystroke(gpui::Keystroke::parse(key).unwrap(), app));
            }
            assert_eq!(
                view.read(app)
                    .pending_open_input
                    .as_ref()
                    .unwrap()
                    .queue
                    .len(),
                3
            );
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let retained = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.pending_open_input.as_ref().is_some_and(|pending| {
                        pending.queue.state() == workspace_app::open_input::State::Recover
                    })
                })
            });
            if retained {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "failed read did not enter recovery"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.model.surface, Surface::Normal);
            let pending = v.pending_open_input.as_ref().unwrap();
            assert_eq!(pending.queue.len(), 3);
            assert_eq!(fs::read(&binary).unwrap(), b"\0synthetic");
        });

        // Fix only the synthetic fixture and retry the retained input.
        fs::write(&binary, "valid").unwrap();
        cx.update(|window, app| {
            view.update(app, |v, cx| v.ui(Ui::RetryOpenInput, window, cx));
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let done = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.pump_open_input(cx);
                    v.pending_open_input.is_none()
                        && v.model.file.as_ref().is_some_and(|(p, _)| p == &binary)
                })
            });
            if done {
                break;
            }
            assert!(Instant::now() < deadline, "retry did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            assert_eq!(
                view.read(app).current_editor().unwrap().contents(),
                "xvalid"
            );
            assert_eq!(fs::read_to_string(&binary).unwrap(), "valid");
        });
        // The buffer picker emits this effect. Selecting the terminal must
        // record this file's precise position so Ctrl+o can return to it.
        // Start with a clean synthetic jump list for this independent case.
        cx.update(|_, app| {
            view.update(app, |v, _| {
                v.editor_panes.as_mut().unwrap().layout.clear_jump_history()
            })
        });
        cx.simulate_keystrokes("l l");
        let saved = cx.update(|_, app| view.read(app).current_editor().unwrap().position());
        let old_token = cx.update(|_, app| {
            view.update(app, |v, _| {
                let token = v.terminal_mark_next.unwrap();
                v.terminal_mark_next = token
                    .get()
                    .checked_add(1)
                    .and_then(std::num::NonZeroU64::new);
                let panes = v.editor_panes.as_ref().unwrap();
                let context = workspace_app::editor_panes::OpenedTerminalFileGuard::capture(
                    v.model.project,
                    v.file_generation,
                    &binary,
                    panes,
                )
                .unwrap();
                v.pending_terminal_capture = Some(PendingTerminalCapture {
                    token,
                    guard: v.terminal_open_guard(v.file_generation),
                    focus: panes.layout.focus_guard(),
                    valid: true,
                    kind: TerminalCaptureKind::FileOpened {
                        path: binary.clone(),
                        context,
                    },
                    deferred_jump: None,
                    started: Instant::now(),
                });
                assert!(
                    v.terminal_capture_is_current(v.pending_terminal_capture.as_ref().unwrap())
                );
                token
            })
        });
        cx.simulate_keystrokes("space ,");
        cx.update(|_, app| assert_eq!(view.read(app).model.surface, Surface::Picker));
        for _ in 0..3 {
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse("q").unwrap(),
                is_held: true,
            });
            cx.update(|window, app| {
                view.update(app, |v, cx| v.replace_text_in_range(None, "q", window, cx))
            });
        }
        cx.update(|_, app| assert_eq!(view.read(app).model.picker.as_ref().unwrap().query, "qqq"));
        cx.simulate_keystrokes("backspace backspace backspace");
        cx.simulate_keystrokes("t e r m i n a l");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let ready = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model.picker.as_ref().is_some_and(|picker| {
                        picker
                            .results
                            .get(picker.selected)
                            .and_then(|index| picker.choices.get(*index))
                            .is_some_and(|choice| choice.value == "\0terminal:77")
                    })
                })
            });
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal picker result not ready"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.simulate_keystrokes("enter");
        cx.update(|_, app| assert_eq!(view.read(app).model.surface, Surface::Normal));
        cx.update(|window, app| {
            assert!(window.dispatch_keystroke(gpui::Keystroke::parse("ctrl-o").unwrap(), app));
            view.update(app, |v, _| {
                let new_token = v.pending_terminal_capture.as_ref().unwrap().token;
                assert_ne!(old_token, new_token);
                v.terminal_capture_complete(
                    0,
                    old_token.get(),
                    Err("synthetic late result".into()),
                );
                assert_eq!(
                    v.pending_terminal_capture.as_ref().unwrap().token,
                    new_token
                );
            });
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let returned = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model
                        .file
                        .as_ref()
                        .is_some_and(|(path, _)| path == &binary)
                        && v.model.surface == Surface::File
                })
            });
            if returned {
                break;
            }
            if Instant::now() >= deadline {
                cx.update(|_, app| {
                    let v = view.read(app);
                    panic!(
                        "picker terminal jump did not return: surface={:?}, file={:?}, message={}",
                        v.model.surface,
                        v.model.file.as_ref().map(|(path, _)| path.file_name()),
                        v.model.message
                    );
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.current_editor().unwrap().position(), saved);
            assert_eq!(v.current_editor().unwrap().contents(), "xvalid");
            assert_eq!(fs::read_to_string(&binary).unwrap(), "valid");
        });
        cx.simulate_keystrokes("ctrl-i");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let returned = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model.surface == Surface::Normal && !v.terminal_jump_gate.busy()
                })
            });
            if returned {
                break;
            }
            if Instant::now() >= deadline {
                cx.update(|_, app| {
                    panic!(
                        "forward to terminal failed: {}",
                        view.read(app).model.message
                    )
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.simulate_keystrokes("ctrl-o");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let returned = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.model
                        .file
                        .as_ref()
                        .is_some_and(|(path, _)| path == &binary)
                        && v.model.surface == Surface::File
                })
            });
            if returned {
                break;
            }
            if Instant::now() >= deadline {
                cx.update(|_, app| {
                    panic!(
                        "second back to file failed: {}",
                        view.read(app).model.message
                    )
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.current_editor().unwrap().position(), saved);
            assert_eq!(v.current_editor().unwrap().contents(), "xvalid");
            assert_eq!(fs::read_to_string(&binary).unwrap(), "valid");
        });
        // Neovim copies the jump list on split; :clearjumps must clear only
        // the active window and leave the other window's history intact.
        let original = cx.update(|_, app| {
            view.read(app)
                .editor_panes
                .as_ref()
                .unwrap()
                .layout
                .active()
        });
        cx.simulate_keystrokes("ctrl-w v");
        let (split, original_history) = cx.update(|_, app| {
            let layout = &view.read(app).editor_panes.as_ref().unwrap().layout;
            let split = layout.active();
            assert_ne!(split, original);
            let history = layout.jump_list(original).unwrap().clone();
            assert!(!history.entries().is_empty());
            assert_eq!(
                layout.jump_list(split).unwrap().entries(),
                history.entries()
            );
            (split, history)
        });
        cx.simulate_keystrokes(": c l e a r j enter");
        cx.update(|_, app| {
            let v = view.read(app);
            let layout = &v.editor_panes.as_ref().unwrap().layout;
            assert!(layout.jump_list(split).unwrap().entries().is_empty());
            assert_eq!(layout.jump_list(original).unwrap(), &original_history);
            assert_eq!(v.current_editor().unwrap().contents(), "xvalid");
        });
        cx.simulate_keystrokes("ctrl-o");
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.model.surface, Surface::File);
            assert_eq!(v.current_editor().unwrap().position(), saved);
        });
        // The terminal command prompt uses the same parser and window scope.
        cx.update(|_, app| view.update(app, |v, _| v.effect(Effect::TerminalBuffer(77))));
        cx.update(|_, app| assert_eq!(view.read(app).model.surface, Surface::Normal));
        cx.simulate_keystrokes(": c l e a r j u m p s enter");
        cx.update(|_, app| {
            let v = view.read(app);
            let layout = &v.editor_panes.as_ref().unwrap().layout;
            assert!(layout.jump_list(split).unwrap().entries().is_empty());
            assert_eq!(layout.jump_list(original).unwrap(), &original_history);
            assert_eq!(fs::read_to_string(&binary).unwrap(), "valid");
        });
        cx.simulate_keystrokes("ctrl-o");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let settled = cx.update(|_, app| {
                view.update(app, |v, cx| {
                    v.poll(cx);
                    v.pending_terminal_capture.is_none() && !v.terminal_jump_gate.busy()
                })
            });
            if settled {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "terminal clearjumps did not settle"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cx.update(|_, app| {
            let v = view.read(app);
            assert_eq!(v.model.surface, Surface::Normal);
            assert!(
                v.editor_panes
                    .as_ref()
                    .unwrap()
                    .layout
                    .jump_list(split)
                    .unwrap()
                    .entries()
                    .is_empty()
            );
        });
    }
}
