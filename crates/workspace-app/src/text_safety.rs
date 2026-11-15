//! Neutralizing invisible characters in untrusted display text (Git output,
//! PR text, agent and project labels). Never applied to editor buffers or
//! terminal output.

/// Unicode bidirectional embedding, override, isolate and mark controls.
/// They can reorder how a label reads without changing what it contains.
pub fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// Drop bidi controls from `text`, borrowing when there is nothing to remove.
pub fn strip_bidi(text: &str) -> std::borrow::Cow<'_, str> {
    if text.chars().any(is_bidi_control) {
        text.chars().filter(|c| !is_bidi_control(*c)).collect::<String>().into()
    } else {
        text.into()
    }
}
