use std::fmt::Write;

use super::{shown, status_label, Row, Run};
use crate::limits::Status;

pub fn render(run: &Run, top: usize) -> String {
    let shown = shown(run, top);
    let mutation = run.rows.iter().any(|r| r.mutation.is_some());
    let mut out = rows(&run.rows[..shown], !run.coverage.is_empty(), mutation);
    out.push('\n');
    out.push_str(&summary(run, shown));
    out
}

fn rows(rows: &[Row], coverage: bool, mutation: bool) -> String {
    let width = rows
        .iter()
        .map(|r| r.function.name.len())
        .max()
        .unwrap_or(8)
        .clamp(8, 60);
    let extra = format!(
        "{}{}",
        if coverage { "  COVERAGE    CRAP" } else { "" },
        if mutation { "  MUTATION" } else { "" }
    );
    let mut out = format!(
        "{:>10}  {:>9}  {:<7}{extra}  {:<width$}  LOCATION\n",
        "COMPLEXITY", "COGNITIVE", "STATUS", "FUNCTION"
    );
    for row in rows {
        let f = row.function;
        let extra = extra_cells(row, coverage, mutation);
        let _ = writeln!(
            out,
            "{:>10}  {:>9}  {:<7}{extra}  {:<width$}  {}:{}-{}",
            f.complexity,
            f.cognitive,
            status_label(row.status),
            f.name,
            row.file,
            f.start_line,
            f.end_line
        );
    }
    out
}

fn extra_cells(row: &Row, coverage: bool, mutation: bool) -> String {
    let mut cells = if coverage {
        coverage_cells(row)
    } else {
        String::new()
    };
    if mutation {
        let score = row
            .mutation
            .map_or_else(|| "–".to_string(), |s| format!("{s:.0}%"));
        cells.push_str(&format!("  {score:>8}"));
    }
    cells
}

/// "      100%     4.0" or "         –       –" when the report has no lines there.
fn coverage_cells(row: &Row) -> String {
    match (row.coverage, row.crap) {
        (Some(coverage), Some(crap)) => format!("  {:>7.0}%  {crap:>6.1}", coverage * 100.0),
        _ => format!("  {:>8}  {:>6}", "–", "–"),
    }
}

fn summary(run: &Run, shown: usize) -> String {
    let total = run.rows.len();
    let mut out = format!(
        "{total} functions in {} files, {:.3} s",
        run.files,
        run.elapsed.as_secs_f64()
    );
    if shown < total {
        let _ = write!(out, " (showing top {shown}, use --top 0 for all)");
    }
    let _ = write!(out, "\nlimits: {}", limits_text(run));
    let _ = writeln!(
        out,
        "\nresult: {} ok, {} warn, {} fail, {} ignored",
        run.count(Status::Ok),
        run.count(Status::Warn),
        run.count(Status::Fail),
        run.count(Status::Ignored)
    );
    if let Some(path) = &run.config {
        let _ = writeln!(out, "settings: {path}");
    }
    for source in &run.coverage {
        let _ = writeln!(out, "coverage: {source}");
    }
    out
}

/// "warn above 5, fail above 6 (1 path override); CRAP warn above 30, no fail limit"
fn limits_text(run: &Run) -> String {
    let mut text = run.limits.global.describe();
    match run.limits.override_count() {
        0 => {}
        1 => text.push_str(" (1 path override)"),
        n => text.push_str(&format!(" ({n} path overrides)")),
    }
    if !run.coverage.is_empty() {
        text.push_str(&format!("; CRAP {}", crap_limits(run)));
    }
    text
}

pub fn crap_limits(run: &Run) -> String {
    let crap = run.limits.crap;
    let part = |name: &str, limit: Option<f64>| match limit {
        Some(limit) => format!("{name} above {limit}"),
        None => format!("no {name} limit"),
    };
    format!("{}, {}", part("warn", crap.warn), part("fail", crap.fail))
}
