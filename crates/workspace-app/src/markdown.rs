//! CommonMark display data only. No HTML execution, resource fetches or file reads.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Paragraph,
    Heading(u8),
    Code,
    Quote,
    Item,
    TableRow,
    Rule,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub strong: bool,
    pub emphasis: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
}
#[derive(Clone, Debug)]
pub struct Span {
    pub range: Range<usize>,
    pub style: InlineStyle,
}
#[derive(Clone, Debug)]
pub struct Block {
    pub kind: Kind,
    pub text: String,
    pub spans: Vec<Span>,
    pub cells: Vec<Range<usize>>,
    pub table_header: bool,
}
impl Block {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            text: String::new(),
            spans: Vec::new(),
            cells: Vec::new(),
            table_header: false,
        }
    }
    fn append(&mut self, text: &str, style: InlineStyle) {
        let start = self.text.len();
        self.text.push_str(text);
        if start == self.text.len() || style == InlineStyle::default() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.range.end == start
            && last.style == style
        {
            last.range.end = self.text.len();
        } else {
            self.spans.push(Span {
                range: start..self.text.len(),
                style,
            });
        }
    }
}
pub fn parse(source: &str) -> Result<Vec<Block>, String> {
    if source.len() > 1024 * 1024 {
        return Err("Markdown supera 1 MiB".into());
    }
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut blocks = Vec::new();
    let mut block = Block::new(Kind::Paragraph);
    let mut quote: usize = 0;
    let mut items: usize = 0;
    let mut strong = 0usize;
    let mut emphasis = 0usize;
    let mut strike = 0usize;
    let mut link = 0usize;
    let mut cell_start = 0usize;
    let flush = |blocks: &mut Vec<Block>, block: &mut Block| {
        if !block.text.is_empty() || !block.cells.is_empty() {
            let kind = block.kind;
            blocks.push(std::mem::replace(block, Block::new(kind)));
        }
    };
    for (count, event) in Parser::new_ext(source, options).enumerate() {
        if count >= 20000 || blocks.len() >= 2000 {
            return Err("Markdown supera el límite de bloques".into());
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    flush(&mut blocks, &mut block);
                    block.kind = Kind::Heading(level as u8);
                }
                Tag::CodeBlock(_) => {
                    flush(&mut blocks, &mut block);
                    block.kind = Kind::Code;
                }
                Tag::BlockQuote(_) => {
                    quote += 1;
                    block.kind = Kind::Quote;
                }
                Tag::Item => {
                    flush(&mut blocks, &mut block);
                    items += 1;
                    block.kind = Kind::Item;
                    block.text.push_str("• ");
                }
                Tag::Paragraph => {
                    if items == 0 {
                        flush(&mut blocks, &mut block);
                        block.kind = if quote > 0 {
                            Kind::Quote
                        } else {
                            Kind::Paragraph
                        };
                    }
                }
                Tag::TableHead | Tag::TableRow => {
                    flush(&mut blocks, &mut block);
                    block.kind = Kind::TableRow;
                    block.table_header = matches!(tag, Tag::TableHead);
                }
                Tag::Strong => strong += 1,
                Tag::Emphasis => emphasis += 1,
                Tag::Strikethrough => strike += 1,
                Tag::Link { .. } => link += 1,
                Tag::TableCell => {
                    cell_start = block.text.len();
                }
                Tag::Image { .. } => {
                    block.text.push_str("[imagen: ");
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::Paragraph
                | TagEnd::TableHead
                | TagEnd::TableRow => {
                    flush(&mut blocks, &mut block);
                    block.kind = if items > 0 {
                        Kind::Item
                    } else {
                        Kind::Paragraph
                    };
                }
                TagEnd::Item => {
                    flush(&mut blocks, &mut block);
                    items = items.saturating_sub(1);
                }
                TagEnd::BlockQuote(_) => {
                    flush(&mut blocks, &mut block);
                    quote = quote.saturating_sub(1);
                }
                TagEnd::Strong => strong = strong.saturating_sub(1),
                TagEnd::Emphasis => emphasis = emphasis.saturating_sub(1),
                TagEnd::Strikethrough => strike = strike.saturating_sub(1),
                TagEnd::Link => link = link.saturating_sub(1),
                TagEnd::TableCell => {
                    if block.cells.len() >= 64 {
                        return Err("Tabla Markdown supera 64 columnas".into());
                    }
                    block.cells.push(cell_start..block.text.len());
                    block.text.push('\t');
                }
                TagEnd::Image => {
                    block.text.push(']');
                }
                // Link destinations are intentionally inert, never opened or fetched.
                _ => {}
            },
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                block.append(
                    &t,
                    InlineStyle {
                        strong: strong > 0,
                        emphasis: emphasis > 0,
                        strike: strike > 0,
                        link: link > 0,
                        code: false,
                    },
                );
            }
            Event::Code(t) => block.append(
                &t,
                InlineStyle {
                    code: true,
                    strong: strong > 0,
                    emphasis: emphasis > 0,
                    strike: strike > 0,
                    link: link > 0,
                },
            ),
            Event::SoftBreak | Event::HardBreak => block.text.push('\n'),
            Event::TaskListMarker(done) => block.text.push_str(if done { "[x] " } else { "[ ] " }),
            Event::Rule => {
                flush(&mut blocks, &mut block);
                blocks.push(Block::new(Kind::Rule));
            }
            _ => {}
        }
        if block.text.len() > 65536 {
            return Err("Bloque Markdown supera 64 KiB".into());
        }
    }
    flush(&mut blocks, &mut block);
    Ok(blocks)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structure_unicode_tables_and_untrusted_html_are_inert_display_data() {
        let blocks = parse("# Título 🦀\n\n- [x] listo\n\n> cita\n\n```rust\nlet x = 1;\n```\n\n|a|b|\n|-|-|\n|ñ|2|\n\n<script>fetch('https://invalid')</script>\n\n![alt](file:///private/secret) [link](https://invalid)\n").unwrap();
        assert!(
            blocks
                .iter()
                .any(|b| b.kind == Kind::Heading(1) && b.text.contains('🦀'))
        );
        assert!(
            blocks
                .iter()
                .any(|b| b.kind == Kind::Item && b.text.contains("[x] listo"))
        );
        assert!(
            blocks
                .iter()
                .any(|b| b.kind == Kind::Code && b.text.contains("let x"))
        );
        assert!(
            blocks
                .iter()
                .any(|b| b.kind == Kind::TableRow && b.text.contains('ñ'))
        );
        assert!(blocks.iter().any(|b| b.text.contains("<script>")));
        assert!(!blocks.iter().any(|b| b.text.contains("private/secret")));
        assert!(parse(&"a".repeat(1024 * 1024 + 1)).is_err());
    }
    #[test]
    fn nested_inline_styles_and_table_cells_keep_unicode_byte_boundaries() {
        let blocks = parse("**á *ñ*** y `🦀` ~~fin~~ [enlace](https://invalid)\n\n| Cabecera | Otra |\n| - | - |\n| **ñ** | `🦀` |\n").unwrap();
        let paragraph = &blocks[0];
        assert!(paragraph.spans.iter().any(|s| s.style.strong
            && s.style.emphasis
            && &paragraph.text[s.range.clone()] == "ñ"));
        assert!(
            paragraph
                .spans
                .iter()
                .any(|s| s.style.code && &paragraph.text[s.range.clone()] == "🦀")
        );
        assert!(paragraph.spans.iter().any(|s| s.style.strike));
        assert!(paragraph.spans.iter().any(|s| s.style.link));
        for block in &blocks {
            let mut end = 0;
            for span in &block.spans {
                assert!(span.range.start >= end);
                assert!(
                    block.text.is_char_boundary(span.range.start)
                        && block.text.is_char_boundary(span.range.end)
                );
                end = span.range.end;
            }
        }
        let rows = blocks
            .iter()
            .filter(|b| b.kind == Kind::TableRow)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].table_header);
        assert!(!rows[1].table_header);
        assert_eq!(
            rows[1]
                .cells
                .iter()
                .map(|r| &rows[1].text[r.clone()])
                .collect::<Vec<_>>(),
            vec!["ñ", "🦀"]
        );
        let wide = format!(
            "|{}|\n|{}|\n",
            vec!["x"; 65].join("|"),
            vec!["-"; 65].join("|")
        );
        assert!(parse(&wide).is_err());
    }
}
