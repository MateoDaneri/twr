//! Bounded literal LSP snippets. No Lua, environment lookup, transforms or IO.
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};
type Spans = BTreeMap<u16, Vec<Range<usize>>>;

#[derive(Clone)]
enum Part {
    Text(String),
    Stop(u16),
}
#[derive(Clone)]
pub struct Template {
    parts: Vec<Part>,
    values: BTreeMap<u16, String>,
    order: Vec<u16>,
}
#[derive(Clone)]
pub struct Session {
    template: Template,
    pub text: String,
    spans: Spans,
    active: usize,
    pub selected: bool,
}
fn number(chars: &[char], at: &mut usize) -> Result<u16, String> {
    let start = *at;
    let mut value = 0u16;
    while let Some(c) = chars.get(*at).filter(|c| c.is_ascii_digit()) {
        if *at - start == 4 {
            return Err("Snippet: tabstop supera 9999".into());
        }
        value = value * 10 + (*c as u16 - '0' as u16);
        *at += 1;
    }
    if start == *at {
        return Err("Snippet: variables y transformaciones aún no soportadas".into());
    }
    Ok(value)
}
fn escaped(chars: &[char], at: &mut usize, out: &mut String) {
    *at += 1;
    if let Some(c) = chars.get(*at).filter(|c| matches!(c, '$' | '}' | '\\')) {
        out.push(*c);
        *at += 1;
    } else {
        out.push('\\');
    }
}
impl Template {
    pub fn parse(source: &str) -> Result<Self, String> {
        if source.len() > 16 * 1024 {
            return Err("Snippet supera 16 KiB".into());
        }
        let normalized = source.replace("\r\n", "\n");
        if normalized
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err("Snippet contiene controles no admitidos".into());
        }
        let chars: Vec<_> = normalized.chars().collect();
        let mut at = 0;
        let mut parts = Vec::new();
        let mut values = BTreeMap::new();
        let mut text = String::new();
        let mut stops = 0;
        while at < chars.len() {
            match chars[at] {
                '\\' => escaped(&chars, &mut at, &mut text),
                '$' if chars.get(at + 1).is_some_and(|c| {
                    c.is_ascii_digit() || *c == '{' || c.is_ascii_alphabetic() || *c == '_'
                }) =>
                {
                    if !text.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut text)));
                    }
                    at += 1;
                    let braced = chars.get(at) == Some(&'{');
                    if braced {
                        at += 1;
                    }
                    let id = number(&chars, &mut at)?;
                    stops += 1;
                    if stops > 128 {
                        return Err("Snippet supera 128 tabstops".into());
                    }
                    if braced {
                        if chars.get(at) == Some(&':') {
                            at += 1;
                            let mut value = String::new();
                            while at < chars.len() && chars[at] != '}' {
                                match chars[at] {
                                    '\\' => escaped(&chars, &mut at, &mut value),
                                    '$' => {
                                        return Err(
                                            "Snippet: placeholders anidados aún no soportados"
                                                .into(),
                                        );
                                    }
                                    c => {
                                        value.push(c);
                                        at += 1;
                                    }
                                }
                            }
                            values.entry(id).or_insert(value);
                        }
                        if chars.get(at) != Some(&'}') {
                            return Err("Snippet: sintaxis incompleta o choices/transformaciones no soportadas".into());
                        }
                        at += 1;
                    }
                    parts.push(Part::Stop(id));
                }
                c => {
                    text.push(c);
                    at += 1;
                }
            }
        }
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
        let order: BTreeSet<_> = parts
            .iter()
            .filter_map(|p| match p {
                Part::Stop(id) if *id != 0 => Some(*id),
                _ => None,
            })
            .collect();
        Ok(Self {
            parts,
            values,
            order: order.into_iter().collect(),
        })
    }
    fn render(&self) -> Result<(String, Spans), String> {
        let mut text = String::new();
        let mut spans: BTreeMap<u16, Vec<Range<usize>>> = BTreeMap::new();
        let mut offset = 0;
        for part in &self.parts {
            let (value, id) = match part {
                Part::Text(text) => (text.as_str(), None),
                Part::Stop(id) => (
                    self.values.get(id).map(String::as_str).unwrap_or(""),
                    Some(*id),
                ),
            };
            if text.len().saturating_add(value.len()) > 64 * 1024 {
                return Err("Snippet expandido supera 64 KiB".into());
            }
            let end = offset + value.chars().count();
            if let Some(id) = id {
                spans.entry(id).or_default().push(offset..end);
            }
            text.push_str(value);
            offset = end;
        }
        Ok((text, spans))
    }
    pub fn session(self) -> Result<Session, String> {
        let (text, spans) = self.render()?;
        Ok(Session {
            template: self,
            text,
            spans,
            active: 0,
            selected: true,
        })
    }
}
impl Session {
    pub fn range(&self) -> Option<Range<usize>> {
        self.spans
            .get(self.template.order.get(self.active)?)?
            .first()
            .cloned()
    }
    pub fn final_cursor(&self) -> usize {
        self.spans
            .get(&0)
            .and_then(|s| s.first())
            .map_or_else(|| self.text.chars().count(), |r| r.start)
    }
    pub fn cursor(&self) -> usize {
        self.range()
            .map_or_else(|| self.final_cursor(), |r| r.start)
    }
    /// None exits at $0. Backward at the first field keeps that field selected.
    pub fn jump(&mut self, backward: bool) -> Option<Range<usize>> {
        if backward {
            self.active = self.active.saturating_sub(1);
        } else {
            self.active += 1;
        }
        self.selected = true;
        self.range()
    }
    pub fn replace(
        &self,
        offset: usize,
        left: usize,
        right: usize,
        input: &str,
        caret: usize,
    ) -> Result<(Self, usize), String> {
        if input.len() > 64 * 1024 || caret > input.chars().count() {
            return Err("Edición de snippet fuera de límites".into());
        }
        let range = self.range().ok_or("Snippet sin campo activo")?;
        let old: Vec<_> = self.text.chars().collect();
        let position = offset
            .checked_sub(range.start)
            .filter(|p| *p <= range.len())
            .ok_or("Cursor fuera del campo")?;
        if left > position || right > range.len() - position {
            return Err("Edición fuera del campo".into());
        }
        let mut value = old[range.clone()].to_vec();
        let edit = if self.selected {
            0..value.len()
        } else {
            position - left..position + right
        };
        let cursor = edit.start + caret;
        value.splice(edit, input.chars());
        let value: String = value.iter().collect();
        let mut next = self.clone();
        let id = next.template.order[next.active];
        next.template.values.insert(id, value);
        (next.text, next.spans) = next.template.render()?;
        next.selected = false;
        let start = next.range().ok_or("Campo perdido")?.start;
        Ok((next, start + cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn placeholders_mirrors_order_escape_and_final_cursor() {
        let mut s = Template::parse(r"fn ${2:name}(${1:ñ😀}, $1) { $0 } \$x \} \\")
            .unwrap()
            .session()
            .unwrap();
        assert_eq!(s.text, "fn name(ñ😀, ñ😀) {  } $x } \\");
        assert_eq!(s.range(), Some(8..10));
        let (next, caret) = s.replace(8, 0, 0, "valor", 5).unwrap();
        s = next;
        assert_eq!(caret, 13);
        assert!(s.text.contains("(valor, valor)"));
        assert_eq!(s.jump(false), Some(3..7));
        assert_eq!(s.jump(true), Some(8..13));
        assert_eq!(s.jump(false), Some(3..7));
        assert_eq!(s.jump(false), None);
        assert_eq!(s.final_cursor(), 24);
        let s = Template::parse("$1 ${1:later}").unwrap().session().unwrap();
        assert_eq!(s.text, "later later");
    }
    #[test]
    fn rejects_unsupported_or_large_templates_without_external_effects() {
        for source in [
            "${1",
            "${VAR}",
            "$HOME",
            "${1:x$2}",
            "${1|a,b|}",
            "${1/x/y/}",
            "$12345",
            "a\u{1b}b",
        ] {
            assert!(Template::parse(source).is_err(), "{source}");
        }
        assert!(Template::parse(&"x".repeat(16385)).is_err());
        assert!(Template::parse(&"$1".repeat(129)).is_err());
        let large = format!("${{1:{}}}{}", "x".repeat(1000), "$1".repeat(100));
        assert!(Template::parse(&large).unwrap().session().is_err());
        let s = Template::parse("${1:x}-$1$0").unwrap().session().unwrap();
        assert!(s.replace(0, 1, 0, "x", 1).is_err());
        assert_eq!(s.text, "x-x");
    }
}
