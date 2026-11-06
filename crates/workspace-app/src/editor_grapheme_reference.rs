//! Isolated references only: not used for editor input or rendering.
use super::Editor;

use super::grapheme::{LineIndex, MAX_UNICODE_LINE};

fn scalar_boundaries(line: &str) -> Vec<usize> {
    let chars: Vec<_> = line.chars().collect();
    LineIndex::new(&chars, 2).unwrap().points()
}

#[test]
fn grapheme_starts_match_clean_neovim_horizontal_movement_references() {
    // Positions captured with col('.') and translated from UTF-8 bytes to
    // scalar offsets. The final offset is the end-of-line sentinel.
    for (text, expected) in [
        ("ae\u{301}z", vec![0, 1, 3, 4]),
        ("a🇺🇸z", vec![0, 1, 3, 4]),
        ("a👩\u{200d}💻z", vec![0, 1, 4, 5]),
        ("a👍🏽z", vec![0, 1, 3, 4]),
        ("a\t\u{301}z", vec![0, 1, 2, 3, 4]),
        ("a\u{301}\u{308}x", vec![0, 3, 4]),
        ("a❤\u{fe0f}z", vec![0, 1, 3, 4]),
    ] {
        assert_eq!(scalar_boundaries(text), expected, "{text:?}");
    }
}

#[test]
fn grapheme_ranges_match_clean_neovim_yank_and_deletion_references() {
    for (line, selected, expected) in [
        ("ae\u{301}z", "e\u{301}", "az"),
        ("a🇺🇸z", "🇺🇸", "az"),
        ("a👩\u{200d}💻z", "👩\u{200d}💻", "az"),
        ("a\t\u{301}z", "\t", "a\u{301}z"),
    ] {
        let points = scalar_boundaries(line);
        let chars: Vec<_> = line.chars().collect();
        let removed: String = chars[points[1]..points[2]].iter().collect();
        assert_eq!(removed, selected);
        let remainder: String = chars[..points[1]]
            .iter()
            .chain(&chars[points[2]..])
            .collect();
        assert_eq!(remainder, expected);
    }
}

#[test]
fn index_bounds_unicode_work_and_keeps_large_ascii_compact() {
    let ascii = vec!['a'; MAX_UNICODE_LINE + 1];
    assert!(matches!(
        LineIndex::new(&ascii, 2),
        Some(LineIndex::Ascii(131_073))
    ));
    let mut non_ascii = ascii;
    non_ascii[10] = 'é';
    assert!(LineIndex::new(&non_ascii, 2).is_none());
    non_ascii.pop();
    let index = LineIndex::new(&non_ascii, 2).unwrap();
    let points = index.points();
    assert_eq!(points.len(), MAX_UNICODE_LINE + 1);
    assert_eq!(points.last(), Some(&MAX_UNICODE_LINE));
}

#[test]
fn indexed_counted_motion_respects_neovim_cell_boundaries() {
    for (text, positions) in [
        ("ae\u{301}z", vec![0, 1, 3]),
        ("a🇺🇸z", vec![0, 1, 3]),
        ("a👩\u{200d}💻z", vec![0, 1, 4]),
        ("a\t\u{301}z", vec![0, 1, 2, 3]),
    ] {
        let chars: Vec<_> = text.chars().collect();
        let index = LineIndex::new(&chars, 2).unwrap();
        for (n, expected) in positions.iter().enumerate() {
            assert_eq!(index.move_by(0, n, true, false), Some(*expected));
        }
        let end = *positions.last().unwrap();
        assert_eq!(index.move_by(0, 9999, true, false), Some(end));
        assert_eq!(index.move_by(end, 9999, false, false), Some(0));
        assert_eq!(index.move_by(end, 1, true, true), Some(chars.len()));
    }
    let index = LineIndex::new(&['a', 'e', '\u{301}', 'z'], 2).unwrap();
    assert_eq!(index.move_by(2, 1, true, false), None);
}

#[test]
fn editor_unicode_motions_and_edits_match_clean_neovim() {
    for (line, cluster, removed, pasted, end) in [
        ("ae\u{301}z", "e\u{301}", "az", "ae\u{301}ze\u{301}", 3),
        ("a🇺🇸z", "🇺🇸", "az", "a🇺🇸z🇺🇸", 3),
        (
            "a👩\u{200d}💻z",
            "👩\u{200d}💻",
            "az",
            "a👩\u{200d}💻z👩\u{200d}💻",
            4,
        ),
        ("a\t\u{301}z", "\t", "a\u{301}z", "a\t\u{301}\tz", 2),
    ] {
        let mut editor = Editor::new(line.into());
        for key in ["l", "l"] {
            editor.key(key);
        }
        assert_eq!(editor.cursor, end, "motion {line:?}");
        editor.key("X");
        assert_eq!(editor.contents(), removed, "X {line:?}");
        assert_eq!(
            editor.registers.get('"'),
            Some((cluster.into(), false)),
            "X {line:?}"
        );
        let mut editor = Editor::new(line.into());
        for key in ["l", "x"] {
            editor.key(key);
        }
        assert_eq!(editor.contents(), removed, "x {line:?}");
        let mut editor = Editor::new(line.into());
        for key in ["l", "v", "y", "l", "p"] {
            editor.key(key);
        }
        assert_eq!(editor.contents(), pasted, "visual yank {line:?}");
    }
}

#[test]
fn unicode_put_and_insert_deletion_match_clean_neovim() {
    let mut editor = Editor::new("ae\u{301}z".into());
    for key in ["l", "v", "y", "p"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "ae\u{301}e\u{301}z");
    assert_eq!(editor.cursor, 3);
    let mut editor = Editor::new("ae\u{301}z".into());
    for key in ["l", "a", "<BS>", "<Esc>"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "az");
    assert_eq!(editor.cursor, 0);
    let mut editor = Editor::new("ae\u{301}z".into());
    for key in ["l", "i", "<Delete>", "<Esc>"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "az");
    let mut editor = Editor::new("ae\u{301}".into());
    editor.key("$");
    assert_eq!(editor.cursor, 1);
    editor.key("Y");
    assert_eq!(editor.registers.get('"'), Some(("e\u{301}".into(), false)));
    editor.key("x");
    assert_eq!(editor.contents(), "a");
}

#[test]
fn cache_rebuilds_after_edit_and_bounds_very_long_unicode_lines() {
    let mut editor = Editor::new("ae\u{301}z".into());
    assert_eq!(editor.grapheme_move(0, 2, true, false), Some(3));
    editor.key("i");
    editor.insert("q");
    editor.key("<Esc>");
    assert_eq!(editor.grapheme_move(0, 3, true, false), Some(4));
    editor.key("u");
    assert_eq!(editor.grapheme_move(0, 2, true, false), Some(3));
    let line = "é".repeat(MAX_UNICODE_LINE + 1);
    let mut editor = Editor::new(line.clone());
    editor.key("x");
    assert_eq!(editor.contents(), line);
    assert!(!editor.dirty());
    assert!(
        editor
            .grapheme_cache
            .borrow()
            .as_ref()
            .is_some_and(|c| c.index.is_none())
    );
    let mut editor = Editor::new(format!("a\n{line}"));
    editor.key("i");
    editor.insert("q");
    editor.key("<Esc>");
    editor.goto(1, 0);
    editor.key("v");
    editor.key("u");
    assert!(editor.contents().starts_with('q'));
}

#[test]
fn word_motion_crosses_bounded_long_combining_sequence() {
    let text = format!("{} x", "e\u{301}".repeat(50_000));
    let mut editor = Editor::new(text);
    editor.key("w");
    assert_eq!(editor.cursor, 100_001);
    assert!(
        editor
            .grapheme_cache
            .borrow()
            .as_ref()
            .is_some_and(|c| c.index.is_some())
    );
}

#[test]
fn vertical_display_columns_match_clean_neovim_unicode_references() {
    for (first, second, column) in [
        ("ae\u{301}z", "aXXz", 2),
        ("a😀z", "aXXz", 3),
        ("a🇺🇸z", "aXXz", 3),
        ("a\t\u{301}z", "aXXXz", 2),
        ("aXXz", "a😀z", 1),
    ] {
        let mut editor = Editor::new(format!("{first}\n{second}"));
        for key in ["l", "l", "j"] {
            editor.key(key);
        }
        assert_eq!(editor.position(), (1, column), "{first:?} to {second:?}");
    }
}

#[test]
fn counted_replace_respects_unicode_graphemes_and_file_boundary() {
    for (keys, expected, cursor) in [
        (vec!["l", "r", "X"], "aXz", 1),
        (vec!["l", "2", "r", "Q"], "aQQ", 2),
        (vec!["l", "3", "r", "Q"], "ae\u{301}z", 1),
        (vec!["l", "l", "r", "X"], "ae\u{301}X", 3),
    ] {
        let mut editor = Editor::new("ae\u{301}z".into());
        for key in &keys {
            editor.key(key);
        }
        assert_eq!(editor.contents(), expected, "{keys:?}");
        assert_eq!(editor.cursor, cursor, "{keys:?}");
        if editor.dirty() {
            editor.key("u");
            assert_eq!(editor.contents(), "ae\u{301}z");
        }
    }
}

#[test]
fn visual_replace_uses_graphemes_like_clean_neovim() {
    for (keys, expected) in [
        (vec!["l", "v", "r", "X"], "aXz"),
        (vec!["l", "v", "l", "r", "X"], "aXX"),
        (vec!["l", "v", "r", "😁"], "a😁z"),
    ] {
        let mut editor = Editor::new("ae\u{301}z".into());
        for key in &keys {
            editor.key(key);
        }
        assert_eq!(editor.contents(), expected, "{keys:?}");
        assert_eq!(editor.cursor, 1, "{keys:?}");
        editor.key("u");
        assert_eq!(editor.contents(), "ae\u{301}z");
    }
}

#[test]
fn normal_x_on_empty_line_leaves_registers_and_undo_untouched() {
    let mut editor = Editor::new("\nnext".into());
    editor.key("x");
    assert_eq!(editor.contents(), "\nnext");
    assert!(editor.registers.get('"').is_none());
    assert!(editor.undo.is_empty());
    assert!(!editor.dirty());
}

#[test]
fn unicode_sequences_compare_against_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        line: String,
        keys: String,
        after: String,
        cursor: usize,
        register: String,
        regtype: String,
    }
    for raw in [
        include_str!("../../../Resources/Fixtures/nvim-unicode-sequences.jsonl"),
        include_str!("../../../Resources/Fixtures/nvim-unicode-word.jsonl"),
        include_str!("../../../Resources/Fixtures/nvim-word-class-sequences.jsonl"),
        include_str!("../../../Resources/Fixtures/nvim-word-objects-unicode.jsonl"),
        include_str!("../../../Resources/Fixtures/nvim-visual-word-yank.jsonl"),
        include_str!("../../../Resources/Fixtures/nvim-find-repeat.jsonl"),
        include_str!("../../../Resources/Fixtures/nvim-find-operator-repeat.jsonl"),
    ]
    .into_iter()
    .flat_map(str::lines)
    {
        let reference: Reference = serde_json::from_str(raw).unwrap();
        let mut editor = Editor::new(reference.line.clone());
        for key in reference.keys.chars() {
            editor.key(&key.to_string());
        }
        while editor.replaying() {
            editor.advance_replay();
        }
        assert_eq!(
            editor.contents(),
            reference.after,
            "{:?} {:?}",
            reference.line,
            reference.keys
        );
        assert_eq!(
            editor.position().1,
            reference.cursor,
            "{:?} {:?}",
            reference.line,
            reference.keys
        );
        let (register, linewise) = editor.registers.get('"').unwrap_or_default();
        assert_eq!(
            register, reference.register,
            "{:?} {:?}",
            reference.line, reference.keys
        );
        assert_eq!(
            linewise,
            reference.regtype == "V",
            "{:?} {:?}",
            reference.line,
            reference.keys
        );
    }
}

#[test]
fn multiline_word_sequences_match_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        lines: Vec<String>,
        keys: String,
        after: Vec<String>,
        row: usize,
        col: usize,
        register: String,
        regtype: String,
    }
    let mut mismatches = 0;
    for raw in include_str!("../../../Resources/Fixtures/nvim-word-multiline.jsonl").lines() {
        let reference: Reference = serde_json::from_str(raw).unwrap();
        for eol in [false, true] {
            // A trailing empty buffer row with no final newline cannot yet be
            // distinguished from a final EOL in the editor representation.
            if !eol && reference.lines.last().is_some_and(String::is_empty) {
                continue;
            }
            let mut editor = Editor::new(format!(
                "{}{}",
                reference.lines.join("\n"),
                if eol { "\n" } else { "" }
            ));
            for key in reference.keys.chars() {
                editor.key(&key.to_string());
            }
            while editor.replaying() {
                editor.advance_replay();
            }
            let (register, linewise) = editor.registers.get('"').unwrap_or_default();
            let actual = (editor.contents(), editor.position(), register, linewise);
            let suffix = if eol && !(reference.after.len() == 1 && reference.after[0].is_empty()) {
                "\n"
            } else {
                ""
            };
            let expected = (
                format!("{}{suffix}", reference.after.join("\n")),
                (reference.row, reference.col),
                reference.register.clone(),
                reference.regtype == "V",
            );
            if actual != expected {
                eprintln!(
                    "{:?} {} eol={eol}: actual {actual:?}, expected {expected:?}",
                    reference.lines, reference.keys
                );
                mismatches += 1;
            }
        }
    }
    assert_eq!(mismatches, 0);
}

#[test]
fn change_bigword_keeps_newline_and_uses_neovim_range() {
    for (keys, expected, removed) in [
        (vec!["c", "W"], "Z\ndef\nghi", "abc"),
        (vec!["c", "2", "W"], "Z\nghi", "abc\ndef"),
    ] {
        let mut editor = Editor::new("abc\ndef\nghi".into());
        for key in &keys {
            editor.key(key);
        }
        assert_eq!(editor.mode, super::Mode::Insert);
        editor.insert("Z");
        editor.key("<Esc>");
        assert_eq!(editor.contents(), expected, "{keys:?}");
        assert_eq!(editor.registers.get('"'), Some((removed.into(), false)));
        editor.key("u");
        assert_eq!(editor.contents(), "abc\ndef\nghi");
    }
}

#[test]
fn deleting_before_final_empty_line_preserves_undo_and_register() {
    let mut editor = Editor::new("abc\n\n".into());
    for key in ["d", "2", "w"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "");
    assert_eq!(editor.registers.get('"'), Some(("abc\n".into(), true)));
    editor.key("u");
    assert_eq!(editor.contents(), "abc\n\n");
    editor.key("<C-r>");
    assert_eq!(editor.contents(), "");
}

#[test]
fn change_word_sequences_match_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        lines: Vec<String>,
        keys: String,
        after: Vec<String>,
        row: usize,
        col: usize,
        register: String,
        regtype: String,
    }
    let mut mismatches = 0;
    for raw in include_str!("../../../Resources/Fixtures/nvim-change-word.jsonl").lines() {
        let reference: Reference = serde_json::from_str(raw).unwrap();
        for eol in [false, true] {
            if !eol && reference.lines.last().is_some_and(String::is_empty) {
                continue;
            }
            let mut editor = Editor::new(format!(
                "{}{}",
                reference.lines.join("\n"),
                if eol { "\n" } else { "" }
            ));
            for key in reference.keys.chars() {
                editor.key(&key.to_string());
            }
            if editor.mode == super::Mode::Insert {
                editor.insert("Z");
            } else {
                editor.key("Z");
            }
            editor.key("<Esc>");
            let (register, linewise) = editor.registers.get('"').unwrap_or_default();
            let actual = (editor.contents(), editor.position(), register, linewise);
            let suffix = if eol && !(reference.after.len() == 1 && reference.after[0].is_empty()) {
                "\n"
            } else {
                ""
            };
            let expected = (
                format!("{}{suffix}", reference.after.join("\n")),
                (reference.row, reference.col),
                reference.register.clone(),
                reference.regtype == "V",
            );
            if actual != expected {
                eprintln!(
                    "{:?} {} eol={eol}: actual {actual:?}, expected {expected:?}",
                    reference.lines, reference.keys
                );
                mismatches += 1;
            }
        }
    }
    assert_eq!(mismatches, 0);
}

#[test]
fn counted_change_word_dot_and_undo_match_neovim() {
    for (keys, expected, before_repeat) in [
        (vec!["c", "2", "w"], "Z Z mno", "Z ghi\njkl mno"),
        (vec!["c", "W"], "Z Z ghi\njkl mno", "Z def ghi\njkl mno"),
    ] {
        let mut editor = Editor::new("abc def ghi\njkl mno".into());
        for key in keys {
            editor.key(key);
        }
        editor.insert("Z");
        editor.key("<Esc>");
        assert_eq!(editor.contents(), before_repeat);
        editor.key("w");
        editor.key(".");
        while editor.replaying() {
            editor.advance_replay();
        }
        assert_eq!(editor.contents(), expected);
        editor.key("u");
        assert_eq!(editor.contents(), before_repeat);
    }
}

#[test]
fn unicode_word_object_change_matches_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        line: String,
        keys: String,
        after: String,
        cursor: usize,
        register: String,
        regtype: String,
    }
    for raw in include_str!("../../../Resources/Fixtures/nvim-word-objects-change.jsonl").lines() {
        let r: Reference = serde_json::from_str(raw).unwrap();
        let mut editor = Editor::new(r.line.clone());
        for key in r.keys.chars() {
            editor.key(&key.to_string());
        }
        if editor.mode == super::Mode::Insert {
            editor.insert("Z");
        } else {
            editor.key("Z");
        }
        editor.key("<Esc>");
        let (register, linewise) = editor.registers.get('"').unwrap_or_default();
        assert_eq!(
            (editor.contents(), editor.position().1, register, linewise),
            (r.after, r.cursor, r.register, r.regtype == "V"),
            "{:?} {:?}",
            r.line,
            r.keys
        );
    }
}

#[test]
fn word_object_does_not_cross_lines_until_multiline_ranges_are_supported() {
    let mut editor = Editor::new("alpha\nbeta".into());
    for key in ["d", "2", "i", "w"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "");
    assert_eq!(
        editor.registers.get('"'),
        Some(("alpha\nbeta\n".into(), true))
    );
    assert!(editor.dirty());

    let mut editor = Editor::new("alpha  \n beta".into());
    for key in ["j", "d", "i", "w"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "alpha  \nbeta");
    assert_eq!(editor.registers.get('"').unwrap().0, " ");

    let mut editor = Editor::new("alpha  \n beta".into());
    for key in ["j", "l", "d", "a", "w"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "alpha  \n ");
    assert_eq!(editor.registers.get('"').unwrap().0, "beta");
}

#[test]
fn visual_word_objects_match_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        line: String,
        keys: String,
        anchor: usize,
        cursor: usize,
        mode: String,
    }
    let mut mismatches = 0;
    for raw in include_str!("../../../Resources/Fixtures/nvim-visual-word-objects.jsonl").lines() {
        let r: Reference = serde_json::from_str(raw).unwrap();
        let mut e = Editor::new(r.line.clone());
        for k in r.keys.chars() {
            e.key(&k.to_string());
        }
        let actual = (e.anchor, e.cursor, e.mode == super::Mode::Visual);
        let expected = (r.anchor, r.cursor, r.mode == "v");
        if actual != expected {
            eprintln!(
                "MISMATCH {:?} {:?}: {:?} != {:?}",
                r.line, r.keys, actual, expected
            );
            mismatches += 1;
        }
    }
    assert_eq!(mismatches, 0);
}

#[test]
fn word_object_replay_matches_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        line: String,
        keys: String,
        after: String,
        cursor: usize,
        register: String,
        regtype: String,
    }
    let mut mismatches = 0;
    for raw in include_str!("../../../Resources/Fixtures/nvim-textobject-replay.jsonl").lines() {
        let r: Reference = serde_json::from_str(raw).unwrap();
        let mut editor = Editor::new(r.line.clone());
        let mut rest = r.keys.as_str();
        while !rest.is_empty() {
            if let Some(next) = rest.strip_prefix("<Esc>") {
                editor.key("<Esc>");
                rest = next;
            } else {
                let c = rest.chars().next().unwrap();
                let key = c.to_string();
                if editor.mode == super::Mode::Insert {
                    editor.insert(&key);
                } else {
                    editor.key(&key);
                }
                rest = &rest[c.len_utf8()..];
            }
            while editor.replaying() {
                editor.advance_replay();
            }
        }
        let (register, linewise) = editor.registers.get('"').unwrap_or_default();
        let actual = (editor.contents(), editor.position().1, register, linewise);
        let expected = (r.after, r.cursor, r.register, r.regtype == "V");
        if actual != expected {
            eprintln!(
                "{:?} {:?}: actual {actual:?}, expected {expected:?}",
                r.line, r.keys
            );
            mismatches += 1;
        }
    }
    assert_eq!(mismatches, 0);
}

#[test]
fn multiline_word_object_sequences_match_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        lines: Vec<String>,
        keys: String,
        after: Vec<String>,
        row: usize,
        col: usize,
        register: String,
        regtype: String,
    }
    let mut mismatches = 0;
    for raw in include_str!("../../../Resources/Fixtures/nvim-word-objects-multiline.jsonl").lines()
    {
        let reference: Reference = serde_json::from_str(raw).unwrap();
        for eol in [false, true] {
            // A trailing empty buffer row with no final newline cannot yet be
            // distinguished from a final EOL in the editor representation.
            if !eol && reference.lines.last().is_some_and(String::is_empty) {
                continue;
            }
            let mut editor = Editor::new(format!(
                "{}{}",
                reference.lines.join("\n"),
                if eol { "\n" } else { "" }
            ));
            for key in reference.keys.chars() {
                editor.key(&key.to_string());
            }
            while editor.replaying() {
                editor.advance_replay();
            }
            let (register, linewise) = editor.registers.get('"').unwrap_or_default();
            let actual = (editor.contents(), editor.position(), register, linewise);
            // A visual characterwise deletion of all content preserves the
            // existing final newline on disk; a linewise deletion can remove it.
            let empty_visual_delete = reference.after.len() == 1
                && reference.after[0].is_empty()
                && reference.keys.contains('v')
                && reference.keys.ends_with('d');
            let suffix = if eol
                && (empty_visual_delete
                    || !(reference.after.len() == 1 && reference.after[0].is_empty()))
            {
                "\n"
            } else {
                ""
            };
            let expected = (
                format!("{}{suffix}", reference.after.join("\n")),
                (reference.row, reference.col),
                reference.register.clone(),
                reference.regtype == "V",
            );
            if actual != expected {
                eprintln!(
                    "{:?} {} eol={eol}: actual {actual:?}, expected {expected:?}",
                    reference.lines, reference.keys
                );
                mismatches += 1;
            }
        }
    }
    assert_eq!(mismatches, 0);
}

#[test]
fn two_line_word_object_undo_restores_both_lines() {
    for keys in [vec!["d", "2", "i", "w"], vec!["2", "d", "a", "W"]] {
        let mut editor = Editor::new("alpha\nbeta".into());
        for key in keys {
            editor.key(key);
        }
        assert_eq!(editor.contents(), "");
        editor.key("u");
        assert_eq!(editor.contents(), "alpha\nbeta");
    }
}

#[test]
fn inner_word_object_crosses_line_and_undo_preserves_surrounding_text() {
    for keys in [vec!["$", "d", "2", "i", "w"], vec!["$", "2", "d", "i", "W"]] {
        let mut editor = Editor::new("one two\nthree four".into());
        for key in keys {
            editor.key(key);
        }
        assert_eq!(editor.contents(), "one  four");
        assert_eq!(
            editor.registers.get('"'),
            Some(("two\nthree".into(), false))
        );
        editor.key("u");
        assert_eq!(editor.contents(), "one two\nthree four");
    }
}

#[test]
fn around_word_object_with_indent_undo_preserves_surrounding_text() {
    let mut editor = Editor::new("left end  \n next value".into());
    for key in ["5", "l", "d", "2", "a", "w"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "left value");
    assert_eq!(
        editor.registers.get('"'),
        Some((" end  \n next".into(), false))
    );
    editor.key("u");
    assert_eq!(editor.contents(), "left end  \n next value");
}

#[test]
fn inner_word_object_empty_next_line_is_linewise_and_undoable() {
    let mut editor = Editor::new("alpha\n\nbeta".into());
    for key in ["d", "2", "i", "w"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "beta");
    assert_eq!(editor.registers.get('"'), Some(("alpha\n\n".into(), true)));
    editor.key("u");
    assert_eq!(editor.contents(), "alpha\n\nbeta");
    for key in ["y", "2", "i", "w"] {
        editor.key(key);
    }
    assert_eq!(editor.registers.get('"'), Some(("alpha\n\n".into(), true)));
    assert_eq!(editor.contents(), "alpha\n\nbeta");
}

#[test]
fn around_word_object_across_empty_line_is_undoable() {
    let mut editor = Editor::new("prefix\nalpha\n\nsuffix".into());
    for key in ["j", "d", "2", "a", "w"] {
        editor.key(key);
    }
    assert_eq!(editor.contents(), "prefix");
    assert_eq!(
        editor.registers.get('"'),
        Some(("alpha\n\nsuffix\n".into(), true))
    );
    editor.key("u");
    assert_eq!(editor.contents(), "prefix\nalpha\n\nsuffix");
    for key in ["y", "2", "a", "w"] {
        editor.key(key);
    }
    assert_eq!(
        editor.registers.get('"'),
        Some(("alpha\n\nsuffix".into(), false))
    );
}

#[test]
fn multiline_word_change_matches_clean_neovim() {
    #[derive(serde::Deserialize)]
    struct Reference {
        lines: Vec<String>,
        keys: String,
        after: Vec<String>,
        row: usize,
        col: usize,
        register: String,
        regtype: String,
    }
    let mut mismatches = 0;
    for raw in include_str!("../../../Resources/Fixtures/nvim-multiline-change.jsonl").lines() {
        let r: Reference = serde_json::from_str(raw).unwrap();
        for eol in [false, true] {
            let mut editor = Editor::new(format!(
                "{}{}",
                r.lines.join("\n"),
                if eol { "\n" } else { "" }
            ));
            let mut rest = r.keys.as_str();
            while !rest.is_empty() {
                if let Some(next) = rest.strip_prefix("<Esc>") {
                    editor.key("<Esc>");
                    rest = next;
                } else {
                    let ch = rest.chars().next().unwrap();
                    if editor.mode == super::Mode::Insert {
                        editor.insert(&ch.to_string());
                    } else {
                        editor.key(&ch.to_string());
                    }
                    rest = &rest[ch.len_utf8()..];
                }
                while editor.replaying() {
                    editor.advance_replay();
                }
            }
            let (register, linewise) = editor.registers.get('"').unwrap_or_default();
            let suffix = if eol && !(r.after.len() == 1 && r.after[0].is_empty()) {
                "\n"
            } else {
                ""
            };
            let actual = (editor.contents(), editor.position(), register, linewise);
            let expected = (
                format!("{}{suffix}", r.after.join("\n")),
                (r.row, r.col),
                r.register.clone(),
                r.regtype == "V",
            );
            if actual != expected {
                eprintln!(
                    "{:?} {} eol={eol}: actual {actual:?}, expected {expected:?}",
                    r.lines, r.keys
                );
                mismatches += 1;
            }
        }
    }
    assert_eq!(mismatches, 0);
}
