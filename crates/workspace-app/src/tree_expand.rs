//! Bounded port of the brace behavior in the pinned Neo-tree utils helper.
//! No shell expansion, globbing, environment lookup or command substitution.
use std::sync::OnceLock;

const ITEMS: usize = 256;
const BYTES: usize = 64 * 1024;

fn split(text: &str, separator: char) -> (&str, Option<&str>) {
    let mut depth: usize = 0;
    let mut chars = text.char_indices();
    while let Some((i, ch)) = chars.next() {
        match ch {
            '\\' => {
                chars.next();
            }
            c if c == separator && depth == 0 => return (&text[..i], Some(&text[i + 1..])),
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    (text, None)
}

fn sequence(text: &str) -> Result<Option<Vec<String>>, String> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r"^(-?[0-9]+|[A-Za-z])\.\.(-?[0-9]+|[A-Za-z])(?:\.\.(-?[0-9]+))?$")
            .unwrap()
    });
    let Some(captures) = pattern.captures(text) else {
        return Ok(None);
    };
    let from = &captures[1];
    let to = &captures[2];
    let alpha = from.len() == 1 && from.as_bytes()[0].is_ascii_alphabetic();
    if alpha != (to.len() == 1 && to.as_bytes()[0].is_ascii_alphabetic()) {
        return Ok(None);
    }
    let parse = |s: &str| {
        s.parse::<i64>()
            .map_err(|_| "Secuencia numérica fuera de límites".to_owned())
    };
    let (first, last) = if alpha {
        (from.as_bytes()[0] as i64, to.as_bytes()[0] as i64)
    } else {
        (parse(from)?, parse(to)?)
    };
    let magnitude = captures
        .get(3)
        .map_or(Ok(1), |m| parse(m.as_str()))?
        .checked_abs()
        .filter(|s| *s != 0)
        .ok_or("Paso de secuencia inválido")?;
    let step = if first > last { -magnitude } else { magnitude };
    let padded =
        |s: &str| s.trim_start_matches('-').starts_with('0') && s.trim_start_matches('-').len() > 1;
    let width = if !alpha && (padded(from) || padded(to)) {
        from.len().max(to.len())
    } else {
        0
    };
    let mut values = Vec::new();
    let mut number = first;
    while if step > 0 {
        number <= last
    } else {
        number >= last
    } {
        if values.len() >= ITEMS {
            return Err("Máximo 256 destinos por creación".into());
        }
        if !alpha || number != 92 {
            values.push(if alpha {
                (number as u8 as char).to_string()
            } else if width > 0 {
                format!("{number:0width$}")
            } else {
                number.to_string()
            });
        }
        let Some(next) = number.checked_add(step) else {
            break;
        };
        number = next;
    }
    Ok(Some(values))
}

fn contents(text: Option<&str>) -> Result<Option<Vec<String>>, String> {
    let Some(text) = text else {
        return Ok(Some(vec![String::new()]));
    };
    if text.is_empty() {
        return Ok(Some(vec!["{}".into()]));
    }
    if let Some(sequence) = sequence(text)? {
        return Ok(Some(sequence));
    }
    let mut items = Vec::new();
    let mut tail = Some(text);
    while let Some(text) = tail {
        let (item, rest) = split(text, ',');
        if items.len() == ITEMS {
            return Err("Máximo 256 destinos por creación".into());
        }
        items.push(item.to_owned());
        tail = rest;
    }
    Ok((items.len() > 1).then_some(items))
}

fn expand(text: &str, depth: usize) -> Result<Vec<String>, String> {
    if depth > 16 {
        return Err("Demasiados grupos de expansión".into());
    }
    let (prefix, Some(tail)) = split(text, '{') else {
        return Ok(vec![text.into()]);
    };
    let mut prefix = prefix.to_owned();
    let mut remaining = tail;
    let mut expression = String::new();
    let (items, suffix) = loop {
        let (part, rest) = split(remaining, '}');
        if !expression.is_empty() {
            expression.push('}');
        }
        expression.push_str(part);
        let (expression, rest) = if let Some(rest) = rest {
            (Some(expression.as_str()), rest)
        } else {
            prefix.push('{');
            (None, tail)
        };
        if let Some(items) = contents(expression)? {
            break (items, rest);
        }
        remaining = rest;
    };
    let suffixes = expand(suffix, depth + 1)?;
    if items.len().saturating_mul(suffixes.len()) > ITEMS {
        return Err("Máximo 256 destinos por creación".into());
    }
    let mut result = Vec::new();
    let mut bytes: usize = 0;
    for item in items {
        for suffix in &suffixes {
            let value = format!("{prefix}{item}{suffix}");
            bytes = bytes.saturating_add(value.len());
            if value.len() > 1024 || bytes > BYTES {
                return Err("Expansión de rutas demasiado grande".into());
            }
            result.push(value);
        }
    }
    Ok(result)
}

pub fn names(text: &str) -> Result<Vec<String>, String> {
    if text.is_empty() || text.len() > 1024 {
        return Err("Nombre fuera de límites".into());
    }
    expand(text, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lists_sequences_padding_and_literals_follow_pinned_examples() {
        for (input, expected) in [
            ("x{a..e..2}", vec!["xa", "xc", "xe"]),
            ("file.txt{,.bak}", vec!["file.txt", "file.txt.bak"]),
            (
                "{a,b}/{00..02}.lua",
                vec![
                    "a/00.lua", "a/01.lua", "a/02.lua", "b/00.lua", "b/01.lua", "b/02.lua",
                ],
            ),
            ("{-02..2..2}", vec!["-02", "000", "002"]),
            ("{3..1..-1}", vec!["3", "2", "1"]),
            ("{ñ,界}.txt", vec!["ñ.txt", "界.txt"]),
            ("empty{}", vec!["empty{}"]),
            ("plain{word}", vec!["plain{word}"]),
            ("open{a,b", vec!["open{a,b"]),
            (r"escaped\{a,b}", vec![r"escaped\{a,b}"]),
            ("{a,{b,c}}", vec!["a", "{b,c}"]),
        ] {
            assert_eq!(names(input).unwrap(), expected, "{input}");
        }
    }
    #[test]
    fn expansion_rejects_excessive_products_sequences_and_zero_steps() {
        for input in [
            "{1..257}",
            "{1..17}{1..17}",
            "{1..3..0}",
            "{9999999999999999999999..1}",
        ] {
            assert!(names(input).is_err(), "{input}");
        }
        assert!(names(&"{a,b}".repeat(17)).is_err());
        assert!(names(&"x".repeat(1025)).is_err());
    }
}
