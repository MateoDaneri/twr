//! Bounded Vim pattern subset. Unsupported atoms fail instead of changing meaning.
use regex::{Regex, RegexBuilder};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Magic {
    Default,
    Very,
    No,
    Literal,
}
fn literal(out: &mut String, ch: char) {
    out.push_str(&regex::escape(&ch.to_string()));
}
fn branch_end(chars: &[char], mut i: usize) -> bool {
    while chars.get(i) == Some(&'\\')
        && chars
            .get(i + 1)
            .is_some_and(|c| matches!(c, 'v' | 'm' | 'M' | 'V' | 'c' | 'C'))
    {
        i += 2;
    }
    i == chars.len() || chars[i..].starts_with(&['\\', '|']) || chars[i..].starts_with(&['\\', ')'])
}
fn class(ch: char) -> Option<&'static str> {
    Some(match ch {
        'd' => "[0-9]",
        'D' => "[^0-9\\n]",
        'x' => "[0-9A-Fa-f]",
        'X' => "[^0-9A-Fa-f\\n]",
        'o' => "[0-7]",
        'O' => "[^0-7\\n]",
        'w' => "[0-9A-Za-z_]",
        'W' => "[^0-9A-Za-z_\\n]",
        'h' => "[A-Za-z_]",
        'H' => "[^A-Za-z_\\n]",
        'a' => "[A-Za-z]",
        'A' => "[^A-Za-z\\n]",
        'l' => "[a-z]",
        'L' => "[^a-z\\n]",
        'u' => "[A-Z]",
        'U' => "[^A-Z\\n]",
        's' => "[ \\t]",
        'S' => "[^ \\t\\n]",
        _ => return None,
    })
}
fn collection(chars: &[char], i: &mut usize, out: &mut String) -> Result<(), String> {
    let start = out.len();
    let mut newline = false;
    out.push('[');
    let negative = chars.get(*i) == Some(&'^');
    if negative {
        out.push_str("^\\n");
        *i += 1;
    }
    let mut first = true;
    while let Some(&ch) = chars.get(*i) {
        *i += 1;
        if ch == ']' && !first {
            out.push(']');
            let collection = out.split_off(start);
            out.push_str("(?:[");
            out.push_str(&collection);
            out.push_str("&&[^\\n]]");
            if newline && !negative {
                out.push_str("|\\n");
            }
            out.push(')');
            return Ok(());
        }
        first = false;
        match ch {
            ']' => out.push_str("\\]"),
            '[' if chars.get(*i) == Some(&':') => {
                let start = *i;
                let Some(end) = chars[start..].windows(2).position(|w| w == [':', ']']) else {
                    return Err("Clase POSIX sin cerrar".into());
                };
                let end = start + end;
                let name: String = chars[start + 1..end].iter().collect();
                if !matches!(
                    name.as_str(),
                    "alnum"
                        | "alpha"
                        | "blank"
                        | "cntrl"
                        | "digit"
                        | "graph"
                        | "lower"
                        | "print"
                        | "punct"
                        | "space"
                        | "upper"
                        | "xdigit"
                ) {
                    return Err("Clase POSIX no soportada".into());
                }
                out.push('[');
                out.extend(&chars[start..end + 2]);
                *i = end + 2;
            }
            '[' | '&' | '~' | '^' => {
                out.push('\\');
                out.push(ch);
            }
            '\\' => {
                let ch = *chars.get(*i).ok_or("Escape incompleto en clase")?;
                *i += 1;
                match ch {
                    'n' | 't' | 'r' | '\\' | ']' | '-' | '^' => {
                        newline |= ch == 'n';
                        out.push('\\');
                        out.push(ch);
                    }
                    _ => return Err(format!("Escape de clase no soportado: \\{ch}")),
                }
            }
            _ => out.push(ch),
        }
    }
    Err("Colección sin cerrar".into())
}
fn repetition(chars: &[char], i: &mut usize, out: &mut String) -> Result<(), String> {
    let start = *i;
    let Some(length) = chars[start..].iter().position(|c| *c == '}') else {
        return Err("Repetición sin cerrar".into());
    };
    let end = start + length;
    let mut body: String = chars[start..end].iter().collect();
    *i = end + 1;
    let lazy = body.starts_with('-');
    if lazy {
        body.remove(0);
    }
    if !body.chars().all(|c| c.is_ascii_digit() || c == ',') || body.matches(',').count() > 1 {
        return Err("Repetición no soportada".into());
    }
    for n in body.split(',').filter(|s| !s.is_empty()) {
        if n.parse::<usize>().map_or(true, |n| n > 10000) {
            return Err("Repetición limitada a 10000".into());
        }
    }
    if body.is_empty() {
        out.push('*');
    } else {
        if body.starts_with(',') {
            body.insert(0, '0');
        }
        out.push('{');
        out.push_str(&body);
        out.push('}');
    }
    if lazy {
        out.push('?');
    }
    Ok(())
}
pub fn compile(query: &str) -> Result<Regex, String> {
    if query.len() > 4096 {
        return Err("Patrón limitado a 4 KiB".into());
    }
    let chars: Vec<_> = query.chars().collect();
    let (mut mode, mut out, mut i) = (Magic::Default, String::new(), 0);
    let (mut force_case, mut uppercase, mut branch_start) = (None, false, true);
    let mut repeated = false;
    while let Some(&raw) = chars.get(i) {
        i += 1;
        let escaped = raw == '\\';
        let ch = if escaped {
            let ch = *chars.get(i).ok_or("Escape incompleto")?;
            i += 1;
            ch
        } else {
            raw
        };
        if escaped {
            match ch {
                'v' => {
                    mode = Magic::Very;
                    continue;
                }
                'm' => {
                    mode = Magic::Default;
                    continue;
                }
                'M' => {
                    mode = Magic::No;
                    continue;
                }
                'V' => {
                    mode = Magic::Literal;
                    continue;
                }
                'c' => {
                    force_case = Some(true);
                    continue;
                }
                'C' => {
                    force_case = Some(false);
                    continue;
                }
                '_' => {
                    let ch = *chars.get(i).ok_or("Átomo \\_ incompleto")?;
                    i += 1;
                    match ch {
                        '.' => out.push_str("(?s:.)"),
                        '^' | '$' => out.push(ch),
                        _ => {
                            let atom = class(ch).ok_or("Átomo \\_ no soportado")?;
                            out.push_str("(?:(?-i:");
                            out.push_str(atom);
                            out.push_str(")|\\n)");
                        }
                    }
                    branch_start = false;
                    repeated = false;
                    continue;
                }
                'n' | 'r' | 't' | 'e' | 'b' => {
                    out.push_str(match ch {
                        'n' => "\\n",
                        'r' => "\\r",
                        't' => "\\t",
                        'e' => "\\x1b",
                        _ => "\\x08",
                    });
                    branch_start = false;
                    repeated = false;
                    continue;
                }
                _ if class(ch).is_some() => {
                    out.push_str("(?-i:");
                    out.push_str(class(ch).unwrap());
                    out.push(')');
                    branch_start = false;
                    repeated = false;
                    continue;
                }
                _ if ch.is_ascii_alphanumeric()
                    || matches!(ch, '%' | '@' | '&')
                    || ch == '~' && matches!(mode, Magic::No | Magic::Literal) =>
                {
                    return Err(format!("Átomo Vim no soportado: \\{ch}"));
                }
                _ => {}
            }
        } else if ch.is_uppercase() {
            uppercase = true;
        }
        let special = match ch {
            '.' | '*' | '[' => match mode {
                Magic::Default | Magic::Very => !escaped,
                _ => escaped,
            },
            '(' | ')' | '|' | '+' | '?' | '=' | '{' | '<' | '>' => {
                if mode == Magic::Very {
                    !escaped
                } else {
                    escaped
                }
            }
            '^' | '$' => {
                if mode == Magic::Literal {
                    escaped
                } else {
                    !escaped
                }
            }
            '~' | '&' | '@' | '%' => {
                mode == Magic::Very && !escaped || mode == Magic::Default && !escaped && ch == '~'
            }
            _ => false,
        };
        if !special {
            literal(&mut out, ch);
            branch_start = false;
            repeated = false;
            continue;
        }
        let multi = matches!(ch, '*' | '+' | '?' | '=' | '{');
        if multi && (repeated || branch_start) {
            return Err("Repetición sin átomo o repetición apilada no soportada".into());
        }
        repeated = multi;
        match ch {
            '[' => {
                let before = i;
                collection(&chars, &mut i, &mut out)?;
                uppercase |= chars[before..i].iter().any(|c| c.is_uppercase());
            }
            '{' => repetition(&chars, &mut i, &mut out)?,
            '<' => out.push_str("\\b{start}"),
            '>' => out.push_str("\\b{end}"),
            '=' => out.push('?'),
            '^' if branch_start || mode == Magic::Very => out.push('^'),
            '^' => literal(&mut out, ch),
            '$' if branch_end(&chars, i) || mode == Magic::Very => out.push('$'),
            '$' => literal(&mut out, ch),
            '.' | '*' | '+' | '?' | '(' | ')' | '|' => out.push(ch),
            _ => return Err(format!("Átomo Vim no soportado: {ch}")),
        }
        branch_start = ch == '(' || ch == '|';
    }
    RegexBuilder::new(&out)
        .multi_line(true)
        .case_insensitive(force_case.unwrap_or(!uppercase))
        .size_limit(1_048_576)
        .dfa_size_limit(524_288)
        .nest_limit(64)
        .build()
        .map_err(|e| format!("Patrón inválido: {e}"))
}
