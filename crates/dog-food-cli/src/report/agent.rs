//! `--format agent`: compact plain text for AI assistants. One header line
//! with the result, a column legend, then one line per function: every
//! function above a limit, then the most complex of the rest. No decoration.

use std::fmt::Write;

use super::{shown, table::crap_limits, Row, Run};
use crate::limits::Status;

pub fn render(run: &Run, top: usize) -> String {
    let coverage = !run.coverage.is_empty();
    let mut out = header(run);
    let _ = writeln!(out, "columns: {}", columns(coverage));

    let attention: Vec<&Row> = run
        .rows
        .iter()
        .filter(|r| matches!(r.status, Status::Warn | Status::Fail))
        .collect();
    let _ = writeln!(out, "\n## above limits ({})", attention.len());
    for row in &attention {
        out.push_str(&line(row, coverage));
    }
    let rest: Vec<&Row> = run
        .rows
        .iter()
        .filter(|r| r.status == Status::Ok)
        .take(shown(run, top))
        .collect();
    let _ = writeln!(
        out,
        "\n## most complex within limits (top {} of {})",
        rest.len(),
        run.count(Status::Ok)
    );
    for row in &rest {
        out.push_str(&line(row, coverage));
    }
    let ignored: Vec<&Row> = run
        .rows
        .iter()
        .filter(|r| r.status == Status::Ignored)
        .collect();
    if !ignored.is_empty() {
        let _ = writeln!(
            out,
            "\n## ignored ({}, excluded from limits)",
            ignored.len()
        );
        for row in &ignored {
            out.push_str(&line(row, coverage));
        }
    }
    out
}

fn header(run: &Run) -> String {
    let result = match (run.count(Status::Fail), run.count(Status::Warn)) {
        (0, 0) => "PASS",
        (0, _) => "PASS_WITH_WARNINGS",
        _ => "FAIL",
    };
    let mut out = format!(
        "dgf analyze: {result}. {} files, {} functions, {} ms. fail={} warn={} ok={} ignored={}\n",
        run.files,
        run.rows.len(),
        run.elapsed.as_millis(),
        run.count(Status::Fail),
        run.count(Status::Warn),
        run.count(Status::Ok),
        run.count(Status::Ignored)
    );
    let _ = writeln!(out, "limits: cyclomatic {}", run.limits.global.describe());
    if !run.coverage.is_empty() {
        let _ = writeln!(out, "limits: CRAP {}", crap_limits(run));
    }
    out
}

fn columns(coverage: bool) -> &'static str {
    if coverage {
        "status cyclomatic cognitive nesting lines params coverage% crap location name | decisions"
    } else {
        "status cyclomatic cognitive nesting lines params location name | decisions"
    }
}

fn line(row: &Row, coverage: bool) -> String {
    let f = row.function;
    let mut out = format!(
        "{} {} {} {} {} {}",
        status(row.status),
        f.complexity,
        f.cognitive,
        f.max_nesting,
        f.lines,
        f.parameters
    );
    if coverage {
        out.push_str(&match (row.coverage, row.crap) {
            (Some(c), Some(crap)) => format!(" {:.0} {crap:.1}", c * 100.0),
            _ => " - -".to_string(),
        });
    }
    let _ = write!(
        out,
        " {}:{}-{} {} | {}",
        row.file,
        f.start_line,
        f.end_line,
        f.name,
        decisions(row)
    );
    out.push('\n');
    out
}

fn status(status: Status) -> &'static str {
    match status {
        Status::Fail => "FAIL",
        Status::Warn => "WARN",
        Status::Ok => "ok",
        Status::Ignored => "ignored",
    }
}

/// "if=2 loop=1 &&=1", or "none".
pub fn categories(breakdown: &std::collections::BTreeMap<&'static str, u32>) -> String {
    let parts: Vec<String> = breakdown
        .iter()
        .map(|(category, count)| format!("{}={count}", category.replace(' ', "_")))
        .collect();
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" ")
    }
}

/// The decision counts, plus why an ignored function is ignored.
fn decisions(row: &Row) -> String {
    let mut parts = vec![categories(&row.function.breakdown)];
    if let Some(reason) = &row.function.ignored {
        parts.push(format!(
            "(dgf-ignore: {})",
            if reason.is_empty() {
                "no reason"
            } else {
                reason
            }
        ));
    }
    if let Some(score) = row.baseline {
        parts.push(format!("(baseline accepts {score})"));
    }
    if let Some(score) = row.mutation {
        parts.push(format!("mutation={score:.0}%"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::tests::{file, function, limits, run};

    #[test]
    fn lists_functions_above_limits_first_with_a_legend() {
        let report = file(vec![
            function(
                "OrderService.CalculateDiscount",
                8,
                &[("if", 4), ("match arm", 3)],
            ),
            function("OrderService.Total", 2, &[("if", 1)]),
        ]);
        let limits = limits(5, 6);
        let text = render(&run(&report, &limits), 20);
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("dgf analyze: FAIL. 1 files, 2 functions"),
            "{text}"
        );
        assert!(text.contains(
            "columns: status cyclomatic cognitive nesting lines params location name | decisions"
        ));
        assert!(text.contains("## above limits (1)\nFAIL 8 0 0 0 0 src/Orders/OrderService.cs:10-20 OrderService.CalculateDiscount | if=4 match_arm=3\n"), "{text}");
        assert!(
            text.contains("## most complex within limits (top 1 of 1)\nok 2 "),
            "{text}"
        );
        assert!(!text.contains("## ignored"));
    }
}
