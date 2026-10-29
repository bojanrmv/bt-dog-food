//! The report as text for people, compact text for agents, or JSON.

use std::fmt::Write;

use crate::{CloneClass, HoleKind, Report};

/// Lines of a template shown before it is cut.
const TEMPLATE_LINES: usize = 20;

pub fn table(report: &Report, top: usize) -> String {
    let mut out = summary(report);
    for (n, class) in report.classes.iter().take(shown(report, top)).enumerate() {
        if let Some(run) = run(class) {
            let _ = writeln!(
                out,
                "\n{}. {} statements in a row with one shape, saves about {} lines\n   {run}",
                n + 1,
                class.copies.len(),
                class.saves
            );
        } else {
            let _ = writeln!(
                out,
                "\n{}. {} copies of {} lines ({} statement{}), saves about {} lines",
                n + 1,
                class.copies.len(),
                class.lines,
                class.statements,
                if class.statements == 1 { "" } else { "s" },
                class.saves
            );
            for copy in &class.copies {
                let _ = writeln!(out, "   {}", place(copy));
            }
        }
        out.push_str("   template:\n");
        for line in template_lines(class) {
            let _ = writeln!(out, "     | {line}");
        }
        for hole in &class.holes {
            let _ = writeln!(
                out,
                "   ⟨{}⟩ {}: {}",
                hole.index,
                hole.kind.name(),
                hole.values.join(" | ")
            );
        }
        let _ = writeln!(out, "   unite: {}", class.unite);
        for caution in &class.cautions {
            let _ = writeln!(out, "   caution: {caution}");
        }
    }
    let rest = report.classes.len().saturating_sub(shown(report, top));
    if rest > 0 {
        let _ = writeln!(out, "\n... and {rest} more (--top 0 for all)");
    }
    out
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
    out.push_str("each class: copies (file:lines function, `(changed)` when the branch changed it), the template with ⟨n⟩ holes, what each hole is, and how to unite them\n");
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
            "\n## class {} · {} copies · {} lines each · saves ~{}",
            n + 1,
            class.copies.len(),
            class.lines,
            class.saves
        );
        for copy in &class.copies {
            let _ = writeln!(
                out,
                "copy {}:{}-{} {}{}",
                copy.file,
                copy.start_line,
                copy.end_line,
                copy.function.as_deref().unwrap_or("-"),
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
    out.push_str("template:\n");
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
    let _ = writeln!(out, "unite: {}", class.unite);
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

fn summary(report: &Report) -> String {
    let scope = report
        .scope
        .as_ref()
        .map(|s| format!("{s}\n"))
        .unwrap_or_default();
    scope
        + &format!(
            "{} clone classes, {} duplicated lines ({:.1}% of {} statement lines in {} files)\n",
            report.classes.len(),
            report.duplicated_lines,
            report.percent(),
            report.lines,
            report.files
        )
}

/// `file:start-end  in Function`.
fn place(copy: &crate::Copy) -> String {
    format!(
        "{}:{}-{}{}{}",
        copy.file,
        copy.start_line,
        copy.end_line,
        copy.function
            .as_deref()
            .map(|f| format!("  in {f}"))
            .unwrap_or_default(),
        if copy.changed { "  (changed)" } else { "" }
    )
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
    let mut shown: Vec<String> = lines
        .iter()
        .take(TEMPLATE_LINES)
        .map(|l| l.to_string())
        .collect();
    if lines.len() > TEMPLATE_LINES {
        shown.push(format!("... ({} more lines)", lines.len() - TEMPLATE_LINES));
    }
    shown
}
