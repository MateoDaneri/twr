//! Single-line picker input editing at UTF-8 grapheme boundaries.
use unicode_segmentation::UnicodeSegmentation;

pub fn boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .take_while(|&i| i <= cursor)
        .last()
        .unwrap_or(0)
}
pub fn previous(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .take_while(|&i| i < cursor)
        .last()
        .unwrap_or(0)
}
pub fn next(text: &str, cursor: usize) -> usize {
    text.grapheme_indices(true)
        .map(|(i, _)| i)
        .find(|&i| i > cursor)
        .unwrap_or(text.len())
}
pub fn normal(text: &str, cursor: usize) -> usize {
    boundary(text, cursor).min(previous(text, text.len()))
}
pub fn insert(text: &mut String, cursor: &mut usize, value: &str) -> bool {
    if value.is_empty() || text.chars().count() + value.chars().count() > 256 {
        return false;
    }
    *cursor = boundary(text, *cursor);
    text.insert_str(*cursor, value);
    *cursor += value.len();
    // A combining insertion can join a cluster on either side.
    if *cursor != text.len() && !text.grapheme_indices(true).any(|(i, _)| i == *cursor) {
        *cursor = next(text, *cursor);
    }
    true
}
fn keyword(item: &str) -> bool {
    item.chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Some(changed) means the input owns the key, including no-op boundary edits.
pub fn key(text: &mut String, cursor: &mut usize, insert: bool, key: &str) -> Option<bool> {
    *cursor = boundary(text, *cursor);
    match key {
        "<Left>" | "h" if key == "<Left>" || !insert => *cursor = previous(text, *cursor),
        "<Right>" | "l" if key == "<Right>" || !insert => *cursor = next(text, *cursor),
        "<Home>" | "0" if key == "<Home>" || !insert => *cursor = 0,
        "<End>" | "$" if key == "<End>" || !insert => *cursor = text.len(),
        "<BS>" | "<C-h>" if insert => {
            let start = previous(text, *cursor);
            if start == *cursor {
                return Some(false);
            }
            text.drain(start..*cursor);
            *cursor = start;
            return Some(true);
        }
        "<Delete>" | "x" if key == "<Delete>" || !insert => {
            let end = next(text, *cursor);
            if end == *cursor {
                return Some(false);
            }
            text.drain(*cursor..end);
            if !insert {
                *cursor = normal(text, *cursor);
            }
            return Some(true);
        }
        "<C-u>" if insert => {
            if *cursor == 0 {
                return Some(false);
            }
            text.drain(..*cursor);
            *cursor = 0;
            return Some(true);
        }
        "<C-w>" if insert => {
            let end = *cursor;
            let mut items = text[..end].grapheme_indices(true).rev().peekable();
            let mut start = end;
            while let Some(&(offset, item)) = items.peek() {
                if !item.chars().all(char::is_whitespace) {
                    break;
                }
                start = offset;
                items.next();
            }
            if let Some((offset, item)) = items.next() {
                start = offset;
                let class = keyword(item);
                for (offset, item) in items {
                    if item.chars().all(char::is_whitespace) || keyword(item) != class {
                        break;
                    }
                    start = offset;
                }
            }
            if start == end {
                return Some(false);
            }
            text.drain(start..end);
            *cursor = start;
            return Some(true);
        }
        _ => return None,
    }
    if !insert {
        *cursor = normal(text, *cursor);
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clean_neovim_input_reference_at_each_key() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-picker-input.json"
        ))
        .unwrap();
        for case in reference["cases"].as_array().unwrap() {
            let mut text = case["query"].as_str().unwrap().to_owned();
            let mut cursor = text.len();
            let mut insert_mode = true;
            for (stroke, expected) in case["keys"]
                .as_array()
                .unwrap()
                .iter()
                .zip(case["observations"].as_array().unwrap())
            {
                let stroke = stroke.as_str().unwrap();
                let stroke = if stroke == "<Del>" {
                    "<Delete>"
                } else {
                    stroke
                };
                if key(&mut text, &mut cursor, insert_mode, stroke).is_none() {
                    match stroke {
                        "<Esc>" if insert_mode => {
                            cursor = previous(&text, cursor);
                            insert_mode = false;
                        }
                        "i" | "a" | "I" | "A" if !insert_mode => {
                            cursor = match stroke {
                                "a" => next(&text, cursor),
                                "I" => 0,
                                "A" => text.len(),
                                _ => cursor,
                            };
                            insert_mode = true;
                        }
                        _ if insert_mode => {
                            assert!(insert(&mut text, &mut cursor, stroke));
                        }
                        _ => panic!("Unhandled reference stroke {stroke}"),
                    }
                }
                assert_eq!(
                    text,
                    expected["text"].as_str().unwrap(),
                    "{} / {stroke}",
                    case["label"]
                );
                assert_eq!(
                    cursor,
                    expected["cursor"].as_u64().unwrap() as usize,
                    "{} / {stroke}",
                    case["label"]
                );
                assert_eq!(
                    insert_mode,
                    expected["mode"] == "i",
                    "{} / {stroke}",
                    case["label"]
                );
            }
        }
    }
    #[test]
    fn utf8_cursor_and_query_budget_are_preserved() {
        let mut text = "ñ👩‍💻e\u{301}".to_owned();
        let mut cursor = 4; // Inside a UTF-8 sequence and emoji cluster.
        assert_eq!(boundary(&text, cursor), 2);
        key(&mut text, &mut cursor, true, "<Right>");
        assert_eq!(cursor, "ñ👩‍💻".len());
        assert!(insert(&mut text, &mut cursor, "X"));
        assert_eq!(text, "ñ👩‍💻Xe\u{301}");
        let mut text = "a".repeat(256);
        let mut cursor = 1;
        assert!(!insert(&mut text, &mut cursor, "b"));
        assert_eq!(cursor, 1);
        assert_eq!(text.len(), 256);
    }
}
