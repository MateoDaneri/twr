use super::*;
use serde::Deserialize;

fn picker_model() -> Workspace {
    let mut workspace = model();
    workspace.focus(Surface::Tree);
    workspace.open_locations(
        PickerKind::Buffers,
        (0..60)
            .map(|index| Choice {
                label: format!("row item {:02}", index + 1),
                detail: "Synthetic reference".into(),
                value: format!("/example/demo/row-{index}.txt"),
            })
            .collect(),
    );
    let picker = workspace.picker.as_mut().unwrap();
    picker.query = "row".into();
    picker.query_cursor = 3;
    picker.results = (0..60).collect();
    workspace
}

#[derive(Deserialize)]
struct Reference {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    label: String,
    keys: Vec<String>,
    observations: Vec<State>,
}
#[derive(Deserialize)]
struct State {
    open: bool,
    focus: String,
    mode: String,
    query: String,
    cursor: usize,
    selected: usize,
    marks: usize,
}

#[test]
fn picker_focus_and_navigation_replay_real_snacks_reference() {
    let reference: Reference = serde_json::from_str(include_str!(
        "../../../Resources/Fixtures/snacks-focus-reference.json"
    ))
    .unwrap();
    let mut states = 0;
    for case in reference.cases {
        let mut workspace = picker_model();
        assert_eq!(case.keys.len(), case.observations.len());
        for (key, expected) in case.keys.iter().zip(case.observations) {
            if workspace.picker.as_ref().is_some_and(|p| p.input_active())
                && key.chars().count() == 1
            {
                workspace.picker_text(key);
            } else if key == "gg" {
                workspace.key("g", b"g");
                workspace.key("g", b"g");
            } else {
                workspace.key(key, key.as_bytes());
            }
            assert_eq!(
                workspace.picker.is_some(),
                expected.open,
                "{} {key}",
                case.label
            );
            if let Some(picker) = &workspace.picker {
                assert_eq!(
                    if picker.focus == PickerFocus::Input {
                        "input"
                    } else {
                        "list"
                    },
                    expected.focus,
                    "{} {key}",
                    case.label
                );
                assert_eq!(
                    if picker.insert { "i" } else { "n" },
                    expected.mode,
                    "{} {key}",
                    case.label
                );
                assert_eq!(picker.query, expected.query, "{} {key}", case.label);
                assert_eq!(picker.query_cursor, expected.cursor, "{} {key}", case.label);
                assert_eq!(picker.selected, expected.selected, "{} {key}", case.label);
                assert_eq!(picker.marked.len(), expected.marks, "{} {key}", case.label);
            } else {
                assert_eq!(workspace.surface, Surface::Tree);
            }
            states += 1;
        }
    }
    assert!(states >= 40);
}

#[test]
fn list_owns_text_editing_keys_without_mutating_filter_or_generation() {
    let mut workspace = picker_model();
    workspace.key("<M-w>", &[]);
    let generation = workspace.picker.as_ref().unwrap().generation;
    for key in [
        "a", "A", "I", "h", "l", "0", "$", "x", "<BS>", "<Delete>", "<Home>", "<End>", "<C-w>",
    ] {
        workspace.key(key, key.as_bytes());
        workspace.picker_text("must not enter the filter");
        let picker = workspace.picker.as_ref().unwrap();
        assert_eq!(picker.query, "row");
        assert_eq!(picker.query_cursor, 3);
        assert_eq!(picker.generation, generation);
        assert_eq!(picker.focus, PickerFocus::List);
        assert!(!picker.insert);
    }
    workspace.key("i", b"i");
    workspace.picker_text("/");
    assert_eq!(workspace.picker.as_ref().unwrap().query, "row/");
}

#[test]
fn list_click_selection_is_bounded_and_confirmation_is_separate() {
    let mut workspace = picker_model();
    assert!(!workspace.select_picker_row(60));
    assert!(workspace.picker.as_ref().unwrap().input_active());
    assert!(workspace.select_picker_row(42));
    let picker = workspace.picker.as_ref().unwrap();
    assert_eq!(picker.focus, PickerFocus::List);
    assert_eq!(picker.selected, 42);
    assert_eq!(workspace.surface, Surface::Picker);
    assert_eq!(
        workspace.choose_picker(),
        Effect::File("/example/demo/row-42.txt".into())
    );
    assert!(!workspace.select_picker_row(0));
}

#[test]
fn terminal_picker_returns_to_normal_before_guarding_async_open() {
    use crate::editor_panes::TerminalOpenGuard;
    use workspace_core::editor_panes::PaneId;
    let mut workspace = model();
    workspace.focus(Surface::Normal);
    workspace.open_locations(
        PickerKind::Files,
        vec![Choice {
            label: "example".into(),
            detail: "synthetic".into(),
            value: "/example/demo/example.txt".into(),
        }],
    );
    workspace.picker.as_mut().unwrap().results = vec![0];
    assert_eq!(workspace.choose_picker(), Effect::File("/example/demo/example.txt".into()));
    assert_eq!(workspace.surface, Surface::Normal);
    let guard = TerminalOpenGuard {
        generation: 1,
        project: 0,
        pane: Some(PaneId(1)),
        attachment: Some(42),
    };
    assert!(guard.valid(0, Some(PaneId(1)), Some(42), true));
    assert!(!guard.valid(0, Some(PaneId(2)), Some(42), true));
    assert!(!guard.valid(0, Some(PaneId(1)), Some(42), false));
}

#[test]
fn top_prefix_expires_and_focus_changes_clear_it_while_empty_lists_are_safe() {
    let mut workspace = picker_model();
    workspace.key("<Esc>", b"\x1b");
    workspace.picker.as_mut().unwrap().selected = 30;
    workspace.key("g", b"g");
    workspace.picker.as_mut().unwrap().top_prefix =
        Instant::now().checked_sub(std::time::Duration::from_millis(301));
    workspace.key("g", b"g");
    assert_eq!(workspace.picker.as_ref().unwrap().selected, 30);
    workspace.key("g", b"g");
    assert_eq!(workspace.picker.as_ref().unwrap().selected, 0);
    workspace.key("g", b"g");
    workspace.key("/", b"/");
    assert!(workspace.picker.as_ref().unwrap().top_prefix.is_none());
    workspace.picker.as_mut().unwrap().results.clear();
    for key in ["j", "k", "G", "g", "g", "<Tab>", "<S-Tab>"] {
        workspace.key(key, key.as_bytes());
    }
    assert_eq!(workspace.picker.as_ref().unwrap().selected, 0);
}
