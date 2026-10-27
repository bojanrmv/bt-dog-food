//! Newer syntax the tree-sitter grammars do not know yet, rewritten for
//! parsing only into syntax they do, byte for byte the same length: every
//! node keeps its position, and its text is still read from the real source.
//! `parse` uses this only for a file that does not parse as written, and
//! keeps whichever tree has fewer errors.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// C# 14 and 15 syntax that tree-sitter-c-sharp 0.23 reads as errors.
pub fn csharp(source: &str) -> String {
    let mut text = blank(&DIRECTIVE, source);
    text = CLOSED
        .replace_all(&text, |c: &Captures| format!("sealed{}", &c[1]))
        .into_owned();
    text = LABELED_JUMP
        .replace_all(&text, |c: &Captures| {
            format!("{}{}{}{}", &c[1], &c[2], " ".repeat(c[3].len()), &c[4])
        })
        .into_owned();
    text = EXTENSION
        .replace_all(&text, |c: &Captures| {
            let header = c[1].len();
            let class = if header >= "class extension".len() {
                "class extension"
            } else {
                "class E"
            };
            format!("{class:<header$}{}", &c[2])
        })
        .into_owned();
    text = null_conditional_assignments(&text);
    debug_assert_eq!(text.len(), source.len());
    text
}

/// `#:sdk Cake.Sdk`, `#:package X` at the top of a .NET 10 file-based app.
static DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#:[^\r\n]*").unwrap());

/// `closed class` / `closed record` (C# 15), read as `sealed`.
static CLOSED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\bclosed(\s+(?:(?:public|internal|private|protected|partial|unsafe|file)\s+)*(?:record|class)\b)",
    )
    .unwrap()
});

/// `break outer;` / `continue outer;` (C# 15): the label becomes spaces.
static LABELED_JUMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(break|continue)(\s+)([A-Za-z_][A-Za-z0-9_]*)(\s*;)").unwrap());

/// `extension(T x) {` and `extension<T>(T x) {` blocks (C# 14), read as a
/// nested class named `extension` (or `E` when the header is short).
static EXTENSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\bextension\s*(?:<[^<>{};]*>)?\s*\([^(){};]*\))(\s*\{)").unwrap()
});

/// `#if` blocks whose branches only parse one at a time (`#if A` an
/// expression body `#else` another body `#endif`): the first branch stays,
/// the other branches and the `#if`, `#elif`, `#else` and `#endif` lines
/// become spaces. Mutants are made only in the first branch.
pub fn first_branches(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    // For each open `#if`: whether its kept branch is over.
    let mut open: Vec<bool> = Vec::new();
    for line in source.split_inclusive('\n') {
        let directive = conditional(line);
        if let Some(directive) = directive {
            follow(&mut open, directive);
        }
        if directive.is_some() || open.contains(&true) {
            out.extend(line.chars().map(blanked));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// The open `#if` blocks after `directive`.
fn follow(open: &mut Vec<bool>, directive: &str) {
    match directive {
        "if" => open.push(false),
        "endif" => {
            open.pop();
        }
        _ => {
            if let Some(over) = open.last_mut() {
                *over = true;
            }
        }
    }
}

/// `c` as spaces of the same length in bytes; line breaks stay.
fn blanked(c: char) -> String {
    match c {
        '\r' | '\n' => c.to_string(),
        _ => " ".repeat(c.len_utf8()),
    }
}

/// `if`, `elif`, `else` or `endif` for a conditional directive line.
fn conditional(line: &str) -> Option<&'static str> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    let word = rest.split(|c: char| !c.is_ascii_alphabetic()).next()?;
    ["if", "elif", "else", "endif"]
        .into_iter()
        .find(|w| *w == word)
}

fn blank(pattern: &Regex, text: &str) -> String {
    pattern
        .replace_all(text, |c: &Captures| " ".repeat(c[0].len()))
        .into_owned()
}

/// `a?.B = c` and `a?.B += c` (C# 14): the `?.` of a statement's assignment
/// target becomes ` .`. A statement starts a line or follows `;`, `{` or `}`.
fn null_conditional_assignments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive(['\n', ';', '{', '}']) {
        out.push_str(&assignment_target(piece).unwrap_or_else(|| piece.to_string()));
    }
    out
}

/// The line with its target's `?.` replaced, when it starts with a target
/// such as `holder?.Value` (names, dots, `?.` and indexers) that is then
/// assigned.
fn assignment_target(line: &str) -> Option<String> {
    let start = line.len() - line.trim_start().len();
    let rest = &line[start..];
    let end = rest
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '?' | '[' | ']')))
        .map_or(rest.len(), |(i, _)| i);
    let target = &rest[..end];
    let after = rest[end..].trim_start();
    let assigns = [
        "=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "??=", "<<=", ">>=",
    ]
    .iter()
    .any(|op| after.starts_with(op) && !after[op.len()..].starts_with(['=', '>']));
    if !target.contains("?.") || !assigns || target.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let fixed = target.replace("?.", " .");
    Some(format!("{}{fixed}{}", &line[..start], &rest[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csharp_15_files_parse_and_name_extension_members() {
        let source = "public static class Ext\n{\n    extension(IEnumerable<int> sequence)\n    {\n        public int Doubled(int i) => sequence.ElementAt(i) > 2 ? 4 : 2;\n    }\n}\npublic closed record class Gate;\nclass C { void M(H? h, int[] xs) { outer: foreach (var x in xs) { if (x < 0) continue outer; } h?.Value = 1; } }\n";
        let functions = crate::analyze_source(source, crate::Language::CSharp).unwrap();
        let names: Vec<&str> = functions.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"Ext.extension.Doubled"), "{names:?}");
        assert!(names.contains(&"C.M"), "{names:?}");
        let found = crate::mutate::mutants(source, crate::Language::CSharp).unwrap();
        assert!(found.iter().any(|m| m.original == ">" && m.line == 5));
        assert!(found
            .iter()
            .any(|m| m.original == "continue outer;" && m.replacement == ";"));
    }

    #[test]
    fn conditional_branches_after_the_first_are_blanked() {
        let source = "class K\n{\n    public static string Part() =>\n#if NET6_0\n        Guid.NewGuid().ToString()[..8];\n#else\n        Guid.NewGuid().ToString().Substring(0, 8); // café\n#endif\n    int M(int a) => a > 1 ? 2 : 3;\n}\n";
        let text = first_branches(source);
        assert_eq!(text.len(), source.len());
        assert!(text.contains("[..8];") && !text.contains("Substring") && !text.contains('#'));
        let (functions, errors) =
            crate::analyze(source, crate::Language::CSharp, "<memory>").unwrap();
        assert!(!errors, "the first branch parses");
        assert_eq!(functions.len(), 2);
    }

    #[test]
    fn rewrites_keep_every_byte_in_place() {
        let source = "#:package X\npublic closed record class Gate;\nclass C {\n  extension(IEnumerable<int> items) {\n    public int First => items.First();\n  }\n  void M(H? h) {\n    outer: for (;;) { break outer; }\n    h?.Value = 1;\n    h?.Count += 2;\n    var same = a?.B == c;\n  }\n}\n";
        let text = csharp(source);
        assert_eq!(text.len(), source.len());
        assert!(text.starts_with("           \npublic sealed record"));
        assert!(
            text.contains("  class extension ") && !text.contains("extension("),
            "{text}"
        );
        assert!(text.contains("break      ;"));
        assert!(text.contains("h .Value = 1;") && text.contains("h .Count += 2;"));
        assert!(text.contains("a?.B == c"), "comparisons stay");
    }
}
