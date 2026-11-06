//! Bounded CSV index. Offsets are character indices, shared with the modal editor.
use std::ops::Range;
fn skip_row_separator(text: &[char], position: &mut usize) {
    if text.get(*position) == Some(&'\r') {
        *position += 1;
    }
    if text.get(*position) == Some(&'\n') {
        *position += 1;
    }
}
#[derive(Clone, Debug)]
pub struct Field {
    pub raw: Range<usize>,
    pub inner: Range<usize>,
    pub value: String,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub range: Range<usize>,
    pub fields: Vec<Field>,
}
#[derive(Clone, Debug)]
pub struct Table {
    pub rows: Vec<Row>,
    pub widths: Vec<usize>,
}
impl Table {
    pub fn parse(text: &[char], delimiter: char) -> Result<Self, String> {
        if text.len() > 2 * 1024 * 1024 {
            return Err("CSV supera 2 Mi caracteres".into());
        }
        let mut rows = Vec::new();
        let mut widths = Vec::new();
        let mut i = 0;
        let mut fields_total = 0;
        while i < text.len() {
            let row_start = i;
            if text[i] == '#' || (text[i] == '/' && text.get(i + 1) == Some(&'/')) {
                while i < text.len() && !matches!(text[i], '\r' | '\n') {
                    i += 1;
                }
                skip_row_separator(text, &mut i);
                continue;
            }
            let mut fields = Vec::new();
            loop {
                let start = i;
                let quoted = text.get(i) == Some(&'"');
                let inner_start = i + usize::from(quoted);
                let mut value = String::new();
                let inner_end;
                if quoted {
                    i += 1;
                    loop {
                        match text.get(i) {
                            None => return Err("Campo CSV con comillas sin cerrar".into()),
                            Some('"') if text.get(i + 1) == Some(&'"') => {
                                value.push('"');
                                i += 2;
                            }
                            Some('"') => {
                                inner_end = i;
                                i += 1;
                                break;
                            }
                            Some(c) => {
                                value.push(*c);
                                i += 1;
                            }
                        }
                    }
                    if text
                        .get(i)
                        .is_some_and(|c| *c != delimiter && *c != '\n' && *c != '\r')
                    {
                        return Err("Contenido después de comillas CSV".into());
                    }
                } else {
                    while i < text.len()
                        && text[i] != delimiter
                        && text[i] != '\n'
                        && text[i] != '\r'
                    {
                        value.push(text[i]);
                        i += 1;
                    }
                    inner_end = i;
                }
                let col = fields.len();
                let width = value
                    .lines()
                    .map(|l| l.chars().count())
                    .max()
                    .unwrap_or(0)
                    .clamp(4, 36);
                if col >= widths.len() {
                    widths.push(width);
                } else {
                    widths[col] = widths[col].max(width);
                }
                fields.push(Field {
                    raw: start..i,
                    inner: inner_start..inner_end,
                    value,
                });
                fields_total += 1;
                if fields_total > 10000 || rows.len() >= 5000 {
                    return Err("CSV supera el índice de 10.000 campos / 5.000 filas".into());
                }
                if text.get(i) == Some(&delimiter) {
                    i += 1;
                    continue;
                }
                let end = i;
                skip_row_separator(text, &mut i);
                rows.push(Row {
                    range: row_start..end,
                    fields,
                });
                break;
            }
        }
        Ok(Self { rows, widths })
    }
    pub fn at(&self, pos: usize) -> Option<(usize, usize)> {
        let row = self
            .rows
            .iter()
            .position(|r| r.range.start <= pos && pos <= r.range.end)?;
        let col = self.rows[row]
            .fields
            .iter()
            .position(|f| pos <= f.raw.end)
            .unwrap_or(self.rows[row].fields.len() - 1);
        Some((row, col))
    }
    pub fn object(&self, pos: usize, n: usize, around: bool) -> Option<Range<usize>> {
        let (r, c) = self.at(pos)?;
        let row = &self.rows[r];
        let last = (c + n.max(1) - 1).min(row.fields.len() - 1);
        let mut range = if around {
            row.fields[c].raw.start..row.fields[last].raw.end
        } else {
            row.fields[c].inner.start..row.fields[last].inner.end
        };
        if around {
            if last + 1 < row.fields.len() {
                range.end += 1;
            } else if c > 0 {
                range.start = range.start.saturating_sub(1);
            }
        }
        Some(range)
    }
    pub fn jump(&self, pos: usize, key: &str) -> Option<usize> {
        let (r, c) = self.at(pos)?;
        let (r, c) = match key {
            "<Tab>" => {
                if c + 1 < self.rows[r].fields.len() {
                    (r, c + 1)
                } else {
                    ((r + 1).min(self.rows.len() - 1), 0)
                }
            }
            "<S-Tab>" => {
                if c > 0 {
                    (r, c - 1)
                } else {
                    let r = r.saturating_sub(1);
                    (r, self.rows[r].fields.len() - 1)
                }
            }
            "<Enter>" => ((r + 1).min(self.rows.len() - 1), c),
            "<S-Enter>" => (r.saturating_sub(1), c),
            _ => return None,
        };
        let field = &self.rows[r].fields[c.min(self.rows[r].fields.len() - 1)];
        Some(
            field
                .inner
                .end
                .saturating_sub(usize::from(!field.inner.is_empty())),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cr_terminated_comments_do_not_hide_following_data_rows() {
        let text: Vec<char> = "# comment\rname,note\rñ,x\r".chars().collect();
        let table = Table::parse(&text, ',').unwrap();
        assert_eq!(
            table.rows.len(),
            2,
            "CR comment swallowed subsequent CSV rows"
        );
        assert_eq!(table.rows[0].fields[0].value, "name");
        assert_eq!(table.rows[1].fields[0].value, "ñ");
    }
    #[test]
    fn both_comment_prefixes_share_cr_lf_crlf_offsets_and_field_navigation() {
        for end in ["\r", "\n", "\r\n"] {
            for comment in ["# 🦀 comment", "// 🦀 comment"] {
                let text: Vec<char> = format!("{comment}{end}name,note{end}ñ,x{end}")
                    .chars()
                    .collect();
                let table = Table::parse(&text, ',').unwrap();
                assert_eq!(table.rows.len(), 2);
                let pos = text.iter().position(|c| *c == 'ñ').unwrap();
                assert_eq!(table.rows[1].range.start, pos);
                assert_eq!(table.at(pos), Some((1, 0)));
                assert_eq!(
                    table.jump(pos, "<Tab>"),
                    Some(table.rows[1].fields[1].inner.start)
                );
                assert_eq!(
                    text[table.object(pos, 1, false).unwrap()]
                        .iter()
                        .collect::<String>(),
                    "ñ"
                );
                assert_eq!(table.rows[1].fields[1].value, "x");
            }
        }
    }
    #[test]
    fn mixed_separators_keep_quoted_comment_markers_and_literal_line_endings() {
        let text: Vec<char> =
            "// first\r# second\nname,note\r\nñ,\"a,\"\"q\"\"\r\n// literal\"\rx,z\n"
                .chars()
                .collect();
        let before = text.clone();
        let table = Table::parse(&text, ',').unwrap();
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.rows[1].fields[1].value, "a,\"q\"\r\n// literal");
        let field = &table.rows[1].fields[1];
        assert_eq!(
            text[table.object(field.inner.start, 1, false).unwrap()]
                .iter()
                .collect::<String>(),
            "a,\"\"q\"\"\r\n// literal"
        );
        assert_eq!(table.at(field.inner.start), Some((1, 1)));
        assert_eq!(
            table.jump(field.inner.start, "<Enter>"),
            Some(table.rows[2].fields[1].inner.start)
        );
        assert_eq!(text, before);
    }
    #[test]
    fn comment_at_eof_creates_no_phantom_data_row() {
        for suffix in ["# end", "// end", "# end\r", "// end\n", "# end\r\n"] {
            let only: Vec<char> = suffix.chars().collect();
            assert!(Table::parse(&only, ',').unwrap().rows.is_empty());
            let text: Vec<char> = format!("a,b\r{suffix}").chars().collect();
            let table = Table::parse(&text, ',').unwrap();
            assert_eq!(table.rows.len(), 1);
            assert_eq!(table.rows[0].fields[1].value, "b");
        }
    }
    #[test]
    fn quoted_multiline_unicode_empty_comments_and_navigation() {
        let text: Vec<char> = "# comment\nname,note,last\nñ,\"a,b\n\"\"quoted\"\"\",\n"
            .chars()
            .collect();
        let t = Table::parse(&text, ',').unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1].fields.len(), 3);
        assert_eq!(t.rows[1].fields[1].value, "a,b\n\"quoted\"");
        let p = t.rows[1].fields[1].inner.start;
        assert_eq!(
            text[t.object(p, 1, false).unwrap()]
                .iter()
                .collect::<String>(),
            "a,b\n\"\"quoted\"\""
        );
        assert_eq!(t.jump(p, "<S-Tab>"), Some(t.rows[1].fields[0].inner.start));
        assert!(Table::parse(&"x,\"unterminated".chars().collect::<Vec<_>>(), ',').is_err());
    }
}
