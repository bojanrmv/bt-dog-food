//! String literals read into their value and written back, for mutants that
//! change a value inside one (a regex pattern). Only what the regex mutator
//! needs: a literal this does not read gets no such mutants.

/// One-character escapes shared by C# and JavaScript, with their value.
const SIMPLE: &[(char, char)] = &[
    ('n', '\n'),
    ('r', '\r'),
    ('t', '\t'),
    ('0', '\0'),
    ('b', '\u{8}'),
    ('f', '\u{c}'),
    ('v', '\u{b}'),
];

/// The value of a C# `"..."`, `@"..."` or `"""..."""` literal; `None` for
/// an interpolated or UTF-8 one.
pub fn csharp_value(text: &str) -> Option<String> {
    if let Some(body) = text.strip_prefix("@\"") {
        return Some(body.strip_suffix('"')?.replace("\"\"", "\""));
    }
    if text.starts_with("\"\"\"") {
        return raw_value(text);
    }
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    unescape(body, csharp_escape)
}

/// A raw string's content: one line as written, or the lines between the
/// quote lines with the closing line's indentation taken off.
fn raw_value(text: &str) -> Option<String> {
    let quotes = text.len() - text.trim_start_matches('"').len();
    let inner = text.get(quotes..text.len().checked_sub(quotes)?)?;
    if !inner.contains('\n') {
        return Some(inner.to_string());
    }
    let lines: Vec<&str> = inner.split('\n').collect();
    let indent = lines.last()?.trim_end_matches('\r');
    let body = &lines[1..lines.len() - 1];
    let content: Vec<&str> = body
        .iter()
        .map(|line| line.strip_prefix(indent).unwrap_or(line.trim_start()))
        .collect();
    Some(content.join("\n").trim_end_matches('\r').to_string())
}

fn csharp_escape(c: char, rest: &mut std::str::Chars) -> Option<String> {
    match c {
        'u' => hex(rest, 4).map(String::from),
        'U' => hex(rest, 8).map(String::from),
        'x' => variable_hex(rest).map(String::from),
        'a' => Some("\u{7}".into()),
        'e' => Some("\u{1b}".into()),
        _ => simple(c),
    }
}

fn simple(c: char) -> Option<String> {
    if let Some(&(_, value)) = SIMPLE.iter().find(|(e, _)| *e == c) {
        return Some(value.to_string());
    }
    matches!(c, '\\' | '"' | '\'').then(|| c.to_string())
}

fn hex(rest: &mut std::str::Chars, digits: usize) -> Option<char> {
    let text: String = rest.by_ref().take(digits).collect();
    (text.len() == digits)
        .then(|| u32::from_str_radix(&text, 16).ok())
        .flatten()
        .and_then(char::from_u32)
}

/// C#'s `\x` takes one to four hex digits.
fn variable_hex(rest: &mut std::str::Chars) -> Option<char> {
    let ahead = rest.as_str();
    let digits = ahead
        .chars()
        .take(4)
        .take_while(char::is_ascii_hexdigit)
        .count();
    if digits == 0 {
        return None;
    }
    hex(rest, digits)
}

fn unescape(
    body: &str,
    escape: fn(char, &mut std::str::Chars) -> Option<String>,
) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let next = chars.next()?;
            out.push_str(&escape(next, &mut chars)?);
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `value` as a C# literal like `original`: verbatim when the original is
/// verbatim or raw and the value has no line break, else a regular string.
pub fn csharp_literal(value: &str, original: &str) -> String {
    let verbatim = original.starts_with('@') || original.starts_with("\"\"\"");
    if verbatim && !value.contains(['\r', '\n']) {
        return format!("@\"{}\"", value.replace('"', "\"\""));
    }
    quoted(value, '"')
}

/// `value` between `quote`s, escaped.
fn quoted(value: &str, quote: char) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for c in value.chars() {
        out.push_str(&escaped(c, quote));
    }
    out.push(quote);
    out
}

fn escaped(c: char, quote: char) -> String {
    if c == '\\' || c == quote {
        return format!("\\{c}");
    }
    if let Some(&(letter, _)) = SIMPLE.iter().find(|(_, v)| *v == c) {
        return format!("\\{letter}");
    }
    if c.is_control() {
        return format!("\\u{:04x}", c as u32);
    }
    c.to_string()
}

/// The value of a JavaScript `'...'` or `"..."` literal.
pub fn javascript_value(text: &str) -> Option<String> {
    let quote = text.chars().next().filter(|q| matches!(q, '"' | '\''))?;
    let body = text.strip_prefix(quote)?.strip_suffix(quote)?;
    unescape(body, javascript_escape)
}

fn javascript_escape(c: char, rest: &mut std::str::Chars) -> Option<String> {
    match c {
        'u' if rest.as_str().starts_with('{') => braced_hex(rest).map(String::from),
        'u' => hex(rest, 4).map(String::from),
        'x' => hex(rest, 2).map(String::from),
        '\n' => Some(String::new()),
        _ => Some(simple(c).unwrap_or_else(|| c.to_string())),
    }
}

/// `\u{1F600}`, from its `{`.
fn braced_hex(rest: &mut std::str::Chars) -> Option<char> {
    let ahead = rest.as_str();
    let close = ahead.find('}')?;
    let code = u32::from_str_radix(&ahead[1..close], 16).ok()?;
    rest.nth(close);
    char::from_u32(code)
}

/// `value` as a JavaScript literal with `original`'s quotes.
pub fn javascript_literal(value: &str, original: &str) -> String {
    quoted(value, original.chars().next().unwrap_or('"'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csharp_literals_read_and_write() {
        assert_eq!(
            csharp_value(r#""a\\d+\"\x41""#).as_deref(),
            Some(r#"a\d+"A"#)
        );
        assert_eq!(csharp_value(r#"@"^\d+""x""#).as_deref(), Some(r#"^\d+"x"#));
        assert_eq!(csharp_value(r#""""^\d$""""#).as_deref(), Some(r"^\d$"));
        assert_eq!(
            csharp_value("\"\"\"\n    ^a\n      b$\n    \"\"\"").as_deref(),
            Some("^a\n  b$")
        );
        assert_eq!(csharp_value(r#"$"a{b}""#), None);
        assert_eq!(csharp_literal(r#"^\d"x"#, r#"@"^\d""#), r#"@"^\d""x""#);
        assert_eq!(csharp_literal(r"^\d", r#""^\\d""#), r#""^\\d""#);
        assert_eq!(csharp_literal("a\nb", r#"@"a""#), r#""a\nb""#);
    }

    #[test]
    fn javascript_literals_read_and_write() {
        assert_eq!(
            javascript_value(r"'\\d+\u{41}\''").as_deref(),
            Some(r"\d+A'")
        );
        assert_eq!(javascript_literal(r"\D'", "'x'"), r"'\\D\''");
    }
}
