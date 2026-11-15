//! Semantic terminal bindings matched against the installed Ghostty keymap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Copy,
    Paste,
    Font(&'static str),
    Direction(char),
    Resize(char, u16),
    PaneKey(&'static str),
    Bytes(&'static [u8]),
    Scroll(&'static str),
    SelectAll,
    Search(bool),
    Find,
    EndSearch,
    Clear,
    Cycle(bool),
    Equalize,
}
pub fn resolve(key: &str, cmd: bool, ctrl: bool, alt: bool, shift: bool) -> Option<Shortcut> {
    use Shortcut::*;
    if !cmd && key == "insert" {
        if shift && !ctrl && !alt {
            return Some(Paste);
        }
        if ctrl && !shift && !alt {
            return Some(Copy);
        }
    }
    if !cmd && ctrl && shift && !alt && key == "c" {
        return Some(Copy);
    }
    if !cmd && ctrl && !alt && key == "tab" {
        return Some(Cycle(!shift));
    }
    if cmd && !ctrl && !alt {
        return match key {
            "c" if !shift => Some(Copy),
            "v" if !shift => Some(Paste),
            "=" | "+" => Some(Font("+")),
            "-" if !shift => Some(Font("-")),
            "0" if !shift => Some(Font("13")),
            "left" if !shift => Some(PaneKey("C-a")),
            "right" if !shift => Some(PaneKey("C-e")),
            "backspace" if !shift => Some(PaneKey("C-u")),
            "home" if !shift => Some(Scroll("history-top")),
            "end" if !shift => Some(Scroll("history-bottom")),
            "pageup" if !shift => Some(Scroll("page-up")),
            "pagedown" if !shift => Some(Scroll("page-down")),
            "a" if !shift => Some(SelectAll),
            "f" if !shift => Some(Find),
            "f" if shift => Some(EndSearch),
            "g" => Some(Search(!shift)),
            "k" if !shift => Some(Clear),
            _ => None,
        };
    }
    if !cmd && alt && !ctrl && !shift {
        return match key {
            "left" => Some(Bytes(b"\x1bb")),
            "right" => Some(Bytes(b"\x1bf")),
            _ => None,
        };
    }
    None
}
/// Ghostty resize amounts are logical pixels; tmux resize-pane accepts cells.
/// Round to the nearest available cell rather than multiplying the movement
/// by the font width/line height. Tmux cannot represent sub-cell dividers.
pub fn resize_cells(amount: u16, direction: char, cell_width: f32, line_height: f32) -> u16 {
    let unit = if matches!(direction, 'L' | 'R') {
        cell_width
    } else {
        line_height
    };
    if !unit.is_finite() || unit <= 0. {
        return 1;
    }
    (f32::from(amount) / unit).round().clamp(1., 100.) as u16
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ghostty_pixel_resize_is_quantized_to_terminal_cells() {
        assert_eq!(resize_cells(100, 'L', 8., 21.), 13);
        assert_eq!(resize_cells(10, 'R', 8., 21.), 1);
        assert_eq!(resize_cells(100, 'U', 8., 21.), 5);
        assert_eq!(resize_cells(10, 'D', 8., 21.), 1);
        assert_eq!(resize_cells(100, 'D', f32::NAN, 0.), 1);
    }
    #[test]
    fn custom_ghostty_navigation_clipboard_resize_and_line_editing_are_unambiguous() {
        for key in ["h", "j", "k", "l", "left", "down", "up", "right"] {
            assert_eq!(resolve(key, true, true, false, false), None);
            assert_eq!(resolve(key, true, false, false, true), None);
        }
        assert_eq!(
            resolve("c", true, false, false, false),
            Some(Shortcut::Copy)
        );
        assert_eq!(
            resolve("insert", false, true, false, false),
            Some(Shortcut::Copy)
        );
        assert_eq!(
            resolve("insert", false, false, false, true),
            Some(Shortcut::Paste)
        );
        assert_eq!(
            resolve("left", true, false, false, false),
            Some(Shortcut::PaneKey("C-a"))
        );
        assert_eq!(
            resolve("left", false, false, true, false),
            Some(Shortcut::Bytes(b"\x1bb"))
        );
        for key in ["=", "+"] {
            assert_eq!(
                resolve(key, true, true, false, false),
                None
            );
        }
        for key in ["c", "a", "f", "left", "insert"] {
            assert_eq!(resolve(key, false, false, false, false), None);
        }
    }
}
