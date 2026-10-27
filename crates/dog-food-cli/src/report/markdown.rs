//! GitHub-flavored Markdown, for PR comments and CI job summaries.

use std::fmt::Write;

use super::{breakdown_text, shown, Row, Run};
use crate::limits::Status;

pub fn render(run: &Run, top: usize) -> String {
    let mut out = String::from("## dog-food complexity report\n\n");
    let _ = writeln!(out, "{}\n", verdict(run));
    out.push_str(&summary(run));

    let over: Vec<&Row> = run
        .rows
        .iter()
        .filter(|r| matches!(r.status, Status::Warn | Status::Fail))
        .collect();
    if !over.is_empty() {
        out.push_str("\n### Above the limits\n\n");
        out.push_str(&table(over.into_iter(), !run.coverage.is_empty()));
    }
    let shown = shown(run, top);
    let _ = writeln!(
        out,
        "\n### Most complex functions (top {shown} of {})\n",
        run.rows.len()
    );
    out.push_str(&table(run.rows[..shown].iter(), !run.coverage.is_empty()));

    let _ = writeln!(
        out,
        "\n<sub>dog-food {} · `dgf analyze --format markdown`{}</sub>",
        env!("CARGO_PKG_VERSION"),
        run.config
            .as_ref()
            .map(|_| " · settings from `dgf.toml`")
            .unwrap_or("")
    );
    out
}

fn verdict(run: &Run) -> String {
    let (fail, warn) = (run.count(Status::Fail), run.count(Status::Warn));
    let global = run.limits.global;
    match (fail, warn) {
        (0, 0) => "**Passed.** Every function is within the limits.".to_string(),
        (0, _) => format!(
            "**Passed with warnings.** {warn} {} above the warn limit ({}).",
            functions(warn),
            limit(global.warn)
        ),
        _ => format!(
            "**Failed.** {fail} {} above the fail limit ({}).",
            functions(fail),
            limit(global.fail)
        ),
    }
}

fn summary(run: &Run) -> String {
    format!(
        "| Files | Functions | Highest score | Limits | Result | Time |\n\
         | ---: | ---: | ---: | --- | --- | ---: |\n\
         | {} | {} | {} | {} | {} ok · {} warn · {} fail · {} ignored | {} ms |\n",
        run.files,
        run.rows.len(),
        run.max_complexity(),
        run.limits.global.describe(),
        run.count(Status::Ok),
        run.count(Status::Warn),
        run.count(Status::Fail),
        run.count(Status::Ignored),
        run.elapsed.as_millis()
    )
}

fn table<'a>(rows: impl Iterator<Item = &'a Row<'a>>, coverage: bool) -> String {
    let (head, align) = if coverage {
        ("| Coverage | CRAP ", "| ---: | ---: ")
    } else {
        ("", "")
    };
    let mut out = format!(
        "| Score | Cognitive {head}| Status | Function | Location | Decisions |\n| ---: | ---: {align}| --- | --- | --- | --- |\n"
    );
    for row in rows {
        let f = row.function;
        let extra = if coverage {
            coverage_cells(row)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "| {} | {} {extra}| {} | `{}` | `{}:{}-{}` | {} |",
            f.complexity,
            f.cognitive,
            status_cell(row.status),
            cell(&f.name),
            cell(&row.file),
            f.start_line,
            f.end_line,
            cell(&breakdown_text(f))
        );
    }
    out
}

fn coverage_cells(row: &Row) -> String {
    match (row.coverage, row.crap) {
        (Some(coverage), Some(crap)) => format!("| {:.0}% | {crap:.1} ", coverage * 100.0),
        _ => "| – | – ".to_string(),
    }
}

fn status_cell(status: Status) -> &'static str {
    match status {
        Status::Ignored => "ignored",
        Status::Ok => "ok",
        Status::Warn => "warn",
        Status::Fail => "**FAIL**",
    }
}

/// Text that is safe inside a table cell (a C# `operator |` would end the cell).
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

fn functions(count: usize) -> &'static str {
    if count == 1 {
        "function is"
    } else {
        "functions are"
    }
}

fn limit(value: Option<u32>) -> String {
    value.map_or_else(|| "none".to_string(), |v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::tests::{file, function, limits, run};

    #[test]
    fn failing_run_lists_functions_above_the_limits() {
        let report = file(vec![
            function(
                "OrderService.CalculateDiscount",
                8,
                &[("if", 4), ("case", 3)],
            ),
            function("OrderService.Describe", 6, &[("switch arm", 5)]),
            function("OrderService.Total", 1, &[]),
        ]);
        let limits = limits(5, 6);
        let text = render(&run(&report, &limits), 20);

        assert!(
            text.contains("**Failed.** 1 function is above the fail limit (6)."),
            "{text}"
        );
        assert!(
            text.contains("| 1 ok · 1 warn · 1 fail · 0 ignored |"),
            "{text}"
        );
        let above = text.split("### Above the limits").nth(1).unwrap();
        let above = above.split("###").next().unwrap();
        assert!(above.contains("`OrderService.CalculateDiscount`") && above.contains("**FAIL**"));
        assert!(above.contains("`OrderService.Describe`") && above.contains("| warn |"));
        assert!(!above.contains("OrderService.Total"));
        assert!(text.contains("case ×3, if ×4"));
    }

    #[test]
    fn passing_run_has_no_over_limit_section() {
        let report = file(vec![function("A.B", 2, &[("if", 1)])]);
        let limits = limits(5, 6);
        let text = render(&run(&report, &limits), 20);
        assert!(text.contains("**Passed.**"));
        assert!(!text.contains("Above the limits"));
    }

    #[test]
    fn pipes_in_names_are_escaped() {
        assert_eq!(cell("Flags.operator |"), "Flags.operator \\|");
    }
}
