//! The report as text for people, compact text for agents, or JSON.

use std::fmt::Write;

use crate::{CloneClass, HoleKind, Report};

/// Lines of a template shown before it is cut.
const TEMPLATE_LINES: usize = 20;
/// Characters of a template line shown before it is cut.
const LINE_CHARS: usize = 300;
/// Differences of a near-miss class shown before they are cut.
const DIFFERENCES: usize = 8;

pub fn table(report: &Report, top: usize) -> String {
    let mut out = summary(report);
    for (n, class) in report.classes.iter().take(shown(report, top)).enumerate() {
        table_head(&mut out, n + 1, class);
        table_body(&mut out, class);
    }
    let rest = report.classes.len().saturating_sub(shown(report, top));
    if rest > 0 {
        let _ = writeln!(out, "\n... and {rest} more (--top 0 for all)");
    }
    out
}

/// A class's title line and where its copies are.
fn table_head(out: &mut String, n: usize, class: &CloneClass) {
    if let Some(run) = run(class) {
        let _ = writeln!(
            out,
            "\n{n}. {} statements in a row with one shape, saves about {} lines\n   {run}",
            class.copies.len(),
            class.saves
        );
        return;
    }
    let _ = writeln!(
        out,
        "\n{n}. {} copies of {} lines ({} statement{}), {}, saves about {} lines",
        class.copies.len(),
        class.lines,
        class.statements,
        if class.statements == 1 { "" } else { "s" },
        kind_text(class),
        class.saves
    );
    for copy in &class.copies {
        let _ = writeln!(out, "   {}", place(copy));
    }
}

/// A class's template, holes, differences, rules, advice and cautions.
fn table_body(out: &mut String, class: &CloneClass) {
    out.push_str(if class.kind == "query" {
        "   suggested extension:\n"
    } else {
        "   template:\n"
    });
    for line in template_lines(class) {
        let _ = writeln!(out, "     | {line}");
    }
    table_findings(out, class);
    let _ = writeln!(out, "   unite: {}", class.unite);
    for caution in &class.cautions {
        let _ = writeln!(out, "   caution: {caution}");
    }
}

/// A class's holes, differences and rules.
fn table_findings(out: &mut String, class: &CloneClass) {
    for hole in &class.holes {
        let _ = writeln!(
            out,
            "   ⟨{}⟩ {}: {}",
            hole.index,
            hole.kind.name(),
            hole.values.join(" | ")
        );
    }
    for line in finding_lines(class) {
        let _ = writeln!(out, "   {line}");
    }
}

/// How the copies differ, the rules that made them equal, and the evidence lines.
fn finding_lines(class: &CloneClass) -> Vec<String> {
    let mut lines = difference_lines(class);
    if !class.rules.is_empty() {
        lines.push(format!("same after: {}", class.rules.join("; ")));
    }
    lines.extend(evidence_lines(class));
    lines
}

/// What running, the complexity and the compiler add about a class.
fn evidence_lines(class: &CloneClass) -> impl Iterator<Item = String> {
    [
        confirmation_line(class),
        risk_line(class),
        signature_line(class),
    ]
    .into_iter()
    .flatten()
}

/// What an extracted copy returns: its outputs, else what its returns return.
fn returns_text(s: &crate::semantic::Signature) -> String {
    match s.returns.as_slice() {
        [] if s.exits.iter().any(|e| e == "return") => {
            "what its return statements return".to_string()
        }
        [] => "nothing".to_string(),
        [one] => one.clone(),
        many => format!("({})", many.join(", ")),
    }
}

/// A C# copy's signature as a method, from the compiler.
fn signature_line(class: &CloneClass) -> Option<String> {
    let s = class.signature.as_ref()?;
    let returns = returns_text(s);
    let mut line = format!(
        "as a method (from the compiler): takes ({}), returns {returns}",
        s.parameters.join(", ")
    );
    let jumps: Vec<&str> = s
        .exits
        .iter()
        .map(String::as_str)
        .filter(|e| *e != "return" || !s.returns.is_empty())
        .collect();
    if !jumps.is_empty() {
        line += &format!(", jumps out with {}", jumps.join(", "));
    }
    if s.uses_this {
        line += ", needs this (an instance method)";
    }
    Some(line)
}

/// The riskiest copy's function: its complexity, and with coverage its CRAP score.
fn risk_line(class: &CloneClass) -> Option<String> {
    let r = class.risk.as_ref()?;
    Some(match (r.crap, r.coverage) {
        (Some(crap), Some(covered)) => format!(
            "risk: CRAP {crap:.0} in {} (complexity {}, {:.0}% covered)",
            r.function,
            r.complexity,
            covered * 100.0
        ),
        _ => format!("risk: complexity {} in {}", r.complexity, r.function),
    })
}

/// With --confirm: what running the methods showed.
fn confirmation_line(class: &CloneClass) -> Option<String> {
    let c = class.confirmation.as_ref()?;
    Some(match c.status.as_str() {
        "confirmed" => format!(
            "confirmed: the same results on {} generated inputs",
            c.tries
        ),
        _ => format!(
            "not run: {}",
            c.reason.as_deref().unwrap_or("no reason given")
        ),
    })
}

/// Compact text for AI assistants: every class, one block each.
pub fn agent(report: &Report, top: usize) -> String {
    let mut out = format!(
        "dgf-dup: {} clone classes, {} duplicated lines ({:.1}% of {} lines in {} files)\n",
        report.classes.len(),
        report.duplicated_lines,
        report.percent(),
        report.lines,
        report.files
    );
    if let Some(scope) = &report.scope {
        let _ = writeln!(out, "scope: {scope}");
    }
    out.push_str(&suppressed(report));
    out.push_str("each class: its type (1 the same code, 2 renamed, 3 near miss: ⟪…⟫ marks what not every copy has), copies (file:lines function, `(changed)` when the branch changed it), the template with ⟨n⟩ holes, what each hole is, what each near-miss copy adds, leaves out or has instead, and how to unite them\n");
    for (n, class) in report.classes.iter().take(shown(report, top)).enumerate() {
        if let Some(run) = run(class) {
            let _ = writeln!(
                out,
                "\n## class {} · {} statements in a row with one shape · saves ~{}\nrun {run}",
                n + 1,
                class.copies.len(),
                class.saves
            );
            agent_template(&mut out, class);
            continue;
        }
        let _ = writeln!(
            out,
            "\n## class {} · {} copies · {} · {} lines each · saves ~{}",
            n + 1,
            class.copies.len(),
            kind_text(class),
            class.lines,
            class.saves
        );
        review_line(&mut out, report, class);
        for copy in &class.copies {
            let _ = writeln!(
                out,
                "copy {}:{}-{} {}{}{}",
                copy.file,
                copy.start_line,
                copy.end_line,
                copy.function.as_deref().unwrap_or("-"),
                parts_text(copy),
                if copy.changed { " (changed)" } else { "" }
            );
        }
        agent_template(&mut out, class);
        for caution in &class.cautions {
            let _ = writeln!(out, "caution: {caution}");
        }
    }
    out
}

/// A class's template, its holes (locals left out) and how to unite it.
fn agent_template(out: &mut String, class: &CloneClass) {
    out.push_str(if class.kind == "query" {
        "suggested extension:\n"
    } else {
        "template:\n"
    });
    for line in template_lines(class) {
        let _ = writeln!(out, "  {line}");
    }
    for hole in class.holes.iter().filter(|h| h.kind != HoleKind::Local) {
        let _ = writeln!(
            out,
            "hole ⟨{}⟩ {}: {}",
            hole.index,
            hole.kind.name(),
            hole.values.join(" | ")
        );
    }
    for line in finding_lines(class) {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out, "unite: {}", class.unite);
}

/// A class's review decision (with `--decisions`), with its note.
fn review_line(out: &mut String, report: &Report, class: &CloneClass) {
    if let Some(decision) = report.decisions.get(&class.id) {
        let note = if decision.note.is_empty() {
            String::new()
        } else {
            format!(": {}", decision.note)
        };
        let _ = writeln!(out, "review: {}{note}", decision.status);
    }
}

/// The clone type in words.
fn kind_text(class: &CloneClass) -> String {
    match (class.clone_type, class.similarity) {
        (1, _) => "type 1: the same code".to_string(),
        (2, _) => "type 2: renamed".to_string(),
        (4, _) => "type 4: the same behaviour, written differently".to_string(),
        (0, _) => format!(
            "repeated query on {}",
            class.entity.as_deref().unwrap_or("a table")
        ),
        (_, similarity) => near_text(class, similarity),
    }
}

/// A type 3 class in words: in another order, or how alike.
fn near_text(class: &CloneClass, similarity: Option<f64>) -> String {
    if class.evidence == "dependence" {
        return "type 3: the same statements and data flow, in another order or mixed with others"
            .to_string();
    }
    similarity.map_or("type 3: near miss".to_string(), |s| {
        format!("type 3: near miss, {:.0}% alike", s * 100.0)
    })
}

/// What each near-miss copy adds, leaves out or has instead, against the
/// first copy.
fn difference_lines(class: &CloneClass) -> Vec<String> {
    let clip = |text: &str| {
        if text.chars().count() > 80 {
            format!("{}...", text.chars().take(80).collect::<String>())
        } else {
            text.to_string()
        }
    };
    let mut lines: Vec<String> = class
        .differences
        .iter()
        .take(DIFFERENCES)
        .map(|d| {
            let copy = &class.copies[d.copy];
            let who = copy.function.as_deref().unwrap_or(copy.file.as_str());
            let line = d.line;
            match d.kind {
                "added" => format!("differs: {who} (line {line}) adds `{}`", clip(&d.other)),
                "removed" => format!(
                    "differs: {who} (line {line}) leaves out `{}`",
                    clip(&d.first)
                ),
                _ => format!(
                    "differs: {who} (line {line}) has `{}` for `{}`",
                    clip(&d.other),
                    clip(&d.first)
                ),
            }
        })
        .collect();
    if class.differences.len() > DIFFERENCES {
        lines.push(format!(
            "differs: ... and {} more (all in --format json)",
            class.differences.len() - DIFFERENCES
        ));
    }
    lines
}

/// A self-contained page: where the copies are, file by file, and a card
/// per group with its template, holes and how to unite it.
pub fn html(report: &Report) -> String {
    // The data goes inside a <script>: nothing in it may close the tag.
    let data = serde_json::to_string(report)
        .expect("the report serializes")
        .replace("</", "<\\/")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    include_str!("report.html").replace("/*DATA*/", &data)
}

pub fn json(report: &Report) -> String {
    serde_json::to_string_pretty(report).expect("the report serializes") + "\n"
}

/// Classes left out by review decisions, when any were.
fn suppressed(report: &Report) -> String {
    if report.suppressed == 0 {
        return String::new();
    }
    format!(
        "{} groups left out as reviewed (intentional or a false positive, in --decisions)\n",
        report.suppressed
    )
}

fn summary(report: &Report) -> String {
    let scope = report
        .scope
        .as_ref()
        .map(|s| format!("{s}\n"))
        .unwrap_or_default();
    scope
        + &suppressed(report)
        + &format!(
            "{} clone classes, {} duplicated lines ({:.1}% of {} statement lines in {} files)\n",
            report.classes.len(),
            report.duplicated_lines,
            report.percent(),
            report.lines,
            report.files
        )
}

/// `file:start-end  in Function`, with the lines that are part of a copy
/// mixed with other statements.
fn place(copy: &crate::Copy) -> String {
    format!(
        "{}:{}-{}{}{}{}",
        copy.file,
        copy.start_line,
        copy.end_line,
        copy.function
            .as_deref()
            .map(|f| format!("  in {f}"))
            .unwrap_or_default(),
        parts_text(copy),
        if copy.changed { "  (changed)" } else { "" }
    )
}

/// `  (lines 3-5, 9)` for a copy with parts.
fn parts_text(copy: &crate::Copy) -> String {
    if copy.parts.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = copy
        .parts
        .iter()
        .map(|&[s, e]| {
            if s == e {
                s.to_string()
            } else {
                format!("{s}-{e}")
            }
        })
        .collect();
    format!("  (lines {})", parts.join(", "))
}

/// For a repeated run, where the whole run is.
fn run(class: &CloneClass) -> Option<String> {
    if class.kind != "repeated" {
        return None;
    }
    let first = class.copies.first()?;
    let last = class.copies.last()?;
    Some(place(&crate::Copy {
        end_line: last.end_line,
        code: String::new(),
        parts: Vec::new(),
        ..first.clone()
    }))
}

fn shown(report: &Report, top: usize) -> usize {
    if top == 0 {
        report.classes.len()
    } else {
        top.min(report.classes.len())
    }
}

fn template_lines(class: &CloneClass) -> Vec<String> {
    let lines: Vec<&str> = class.template.lines().collect();
    // A canonical expression (type 4) is one long line: shown cut.
    let mut shown: Vec<String> = lines
        .iter()
        .take(TEMPLATE_LINES)
        .map(|l| match l.char_indices().nth(LINE_CHARS) {
            Some((at, _)) => format!("{}... (all of it in --format json)", &l[..at]),
            None => l.to_string(),
        })
        .collect();
    if lines.len() > TEMPLATE_LINES {
        shown.push(format!("... ({} more lines)", lines.len() - TEMPLATE_LINES));
    }
    shown
}
