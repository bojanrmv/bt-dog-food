//! Output for `dgf changes`: table, JSON, Markdown and an HTML page.

use std::fmt::Write;

use super::page::page;
use super::{categories_text, now, status_label, tool, Format};
use crate::changes::{Change, ChangedFunction, ChangesRun, FailOn, Scope};
use crate::limits::Status;

const TEMPLATE: &str = include_str!("changes.html");

pub fn render(run: &ChangesRun, format: Format) -> String {
    match format {
        Format::Table => table(run),
        // `dgf` refuses the mutation report before comparing (`only_for_mutate`).
        Format::Json | Format::MutationReport => {
            serde_json::to_string_pretty(&json(run)).expect("report serializes") + "\n"
        }
        Format::Markdown => markdown(run),
        Format::Html => page(TEMPLATE, &json(run)),
        Format::Sarif => super::sarif::render(
            run.rows
                .iter()
                .filter_map(super::sarif::Finding::from_change),
        ),
        Format::Agent => agent(run),
    }
}

/// Compact text for AI assistants: what can fail the run first, then score
/// changes, then counts for the rest.
fn agent(run: &ChangesRun) -> String {
    let (_, why) = verdict(run);
    let result = match run.verdict() {
        Status::Fail => "FAIL",
        Status::Warn => "PASS_WITH_WARNINGS",
        _ => "PASS",
    };
    let rule = match run.fail_on {
        FailOn::NewOrWorse => "only new functions and higher scores can fail",
        FailOn::Touched => "every changed function can fail",
    };
    let mut out = format!(
        "dgf changes: {result}. {why}\n{}\n",
        context(run).replace('`', "")
    );
    let _ = writeln!(
        out,
        "counts: {}\nrule: {rule}; limits: cyclomatic {}",
        counts(run),
        run.limits.describe()
    );
    out.push_str("columns: change before->after status location name | decisions (na = no score: new or removed)\n");
    let attention: Vec<&ChangedFunction> = run
        .counted(None)
        .filter(|r| matches!(r.status, Status::Warn | Status::Fail))
        .collect();
    agent_section(&mut out, "needs attention", &attention);
    let moved: Vec<&ChangedFunction> = run
        .rows
        .iter()
        .filter(|r| {
            matches!(r.change, Change::Worse | Change::Better)
                && !attention.iter().any(|a| std::ptr::eq(*a, *r))
        })
        .collect();
    agent_section(&mut out, "other score changes", &moved);
    let fresh: Vec<&ChangedFunction> = run
        .rows
        .iter()
        .filter(|r| r.change == Change::New && r.status == Status::Ok)
        .take(10)
        .collect();
    agent_section(&mut out, "new within limits (most complex 10)", &fresh);
    out
}

fn agent_section(out: &mut String, title: &str, rows: &[&ChangedFunction]) {
    let _ = writeln!(out, "\n## {title} ({})", rows.len());
    for row in rows {
        let _ = writeln!(
            out,
            "{} {}->{} {} {}:{}-{} {} | {}",
            change_label(row.change),
            score(row.before).replace('–', "na"),
            score(row.after).replace('–', "na"),
            row_status(row).replace('–', "na"),
            row.file,
            row.start_line,
            row.end_line,
            row.name,
            super::agent::categories(&row.breakdown)
        );
    }
}

pub fn json(run: &ChangesRun) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "tool": tool(),
        "generatedAt": now(),
        "base": run.base,
        "baseCommit": run.base_commit,
        "head": run.head,
        "branch": run.branch,
        "scope": run.scope,
        "failOn": run.fail_on,
        "limits": run.limits,
        "verdict": run.verdict(),
        "summary": {
            "files": run.files.len(),
            "functions": run.rows.len(),
            "new": run.count(Change::New),
            "worse": run.count(Change::Worse),
            "better": run.count(Change::Better),
            "same": run.count(Change::Same),
            "removed": run.count(Change::Removed),
            "net": run.net(),
            "failing": run.counted(Some(Status::Fail)).count(),
            "warning": run.counted(Some(Status::Warn)).count(),
            "durationMs": run.elapsed.as_millis(),
        },
        "files": run.files,
        "functions": run.rows,
        "warnings": run.warnings,
    })
}

/// "Failed." / "Passed with warnings." / "Passed." plus why.
pub fn verdict(run: &ChangesRun) -> (&'static str, String) {
    let who = match run.fail_on {
        FailOn::NewOrWorse => "new or worse",
        FailOn::Touched => "changed",
    };
    let over = |status: Status, limit: Option<u32>, name: &str| {
        let n = run.counted(Some(status)).count();
        format!(
            "{n} {who} {} above the {name} limit ({}).",
            functions(n),
            limit.unwrap_or(0)
        )
    };
    match run.verdict() {
        Status::Fail => ("Failed.", over(Status::Fail, run.limits.fail, "fail")),
        Status::Warn => (
            "Passed with warnings.",
            over(Status::Warn, run.limits.warn, "warn"),
        ),
        Status::Ok | Status::Ignored => ("Passed.", net_text(run.net())),
    }
}

fn net_text(net: i64) -> String {
    match net.signum() {
        1 => format!("Within the limits; complexity went up by {net}."),
        -1 => format!("Within the limits; complexity went down by {}.", -net),
        _ => "Within the limits; complexity did not change.".to_string(),
    }
}

fn functions(n: usize) -> &'static str {
    if n == 1 {
        "function is"
    } else {
        "functions are"
    }
}

/// "Compared with main (merge base abc1234), changed lines in 3 files"
fn context(run: &ChangesRun) -> String {
    let scope = match run.scope {
        Scope::Lines => "changed lines",
        Scope::Files => "all functions",
    };
    format!(
        "Compared with `{}` (merge base `{}`), {scope} in {} changed source {}",
        run.base,
        run.base_commit,
        run.files.len(),
        if run.files.len() == 1 {
            "file"
        } else {
            "files"
        }
    )
}

/// "1 new · 2 worse · 1 better · 2 same · 1 removed · net +5"
fn counts(run: &ChangesRun) -> String {
    let net = run.net();
    format!(
        "{} new · {} worse · {} better · {} same · {} removed · net {}{net}",
        run.count(Change::New),
        run.count(Change::Worse),
        run.count(Change::Better),
        run.count(Change::Same),
        run.count(Change::Removed),
        if net > 0 { "+" } else { "" }
    )
}

fn change_label(change: Change) -> &'static str {
    match change {
        Change::New => "new",
        Change::Worse => "worse",
        Change::Better => "better",
        Change::Same => "same",
        Change::Removed => "removed",
    }
}

fn score(value: Option<u32>) -> String {
    value.map_or_else(|| "–".to_string(), |v| v.to_string())
}

/// "6 → 9", "– → 7"
fn scores(row: &ChangedFunction) -> String {
    format!("{} → {}", score(row.before), score(row.after))
}

fn row_status(row: &ChangedFunction) -> &'static str {
    if row.change == Change::Removed {
        "–"
    } else {
        status_label(row.status)
    }
}

fn table(run: &ChangesRun) -> String {
    let mut out = format!("{}\n\n", context(run).replace('`', ""));
    let width = run
        .rows
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(8)
        .clamp(8, 60);
    let _ = writeln!(
        out,
        "{:<8} {:>9}  {:<6}  {:<width$}  LOCATION",
        "CHANGE", "SCORE", "STATUS", "FUNCTION"
    );
    for row in &run.rows {
        let _ = writeln!(
            out,
            "{:<8} {:>9}  {:<6}  {:<width$}  {}:{}-{}",
            change_label(row.change),
            scores(row),
            row_status(row),
            row.name,
            row.file,
            row.start_line,
            row.end_line
        );
    }
    let (title, why) = verdict(run);
    let _ = write!(
        out,
        "\n{} functions: {}\nresult: {title} {why}\n",
        run.rows.len(),
        counts(run)
    );
    out
}

fn markdown(run: &ChangesRun) -> String {
    let (title, why) = verdict(run);
    let mut out = format!(
        "## dog-food: complexity changes\n\n**{title}** {why}\n\n{}: {}\n",
        context(run),
        counts(run)
    );
    if run.rows.is_empty() {
        out.push_str("\nNo functions changed.\n");
        return out;
    }
    out.push_str("\n| Change | Score | Status | Function | Location | Decisions |\n| --- | ---: | --- | --- | --- | --- |\n");
    for row in &run.rows {
        let _ = writeln!(
            out,
            "| {} | {} | {} | `{}` | `{}:{}-{}` | {} |",
            change_cell(row.change),
            scores(row),
            row_status(row),
            cell(&row.name),
            cell(&row.file),
            row.start_line,
            row.end_line,
            cell(&categories_text(&row.breakdown))
        );
    }
    let _ = writeln!(
        out,
        "\n<sub>dog-food {} · `dgf changes --format markdown`</sub>",
        env!("CARGO_PKG_VERSION")
    );
    out
}

fn change_cell(change: Change) -> String {
    match change {
        Change::New | Change::Worse => format!("**{}**", change_label(change)),
        _ => change_label(change).to_string(),
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use super::*;
    use crate::limits::Level;
    use crate::report::page::tests::assert_slots;

    fn row(
        name: &str,
        change: Change,
        before: Option<u32>,
        after: Option<u32>,
        counted: bool,
    ) -> ChangedFunction {
        let limits = Level {
            warn: Some(5),
            fail: Some(6),
        };
        ChangedFunction {
            change,
            file: "src/Cart.cs".into(),
            name: name.into(),
            kind: "method",
            start_line: 10,
            end_line: 30,
            before,
            after,
            status: after.map_or(Status::Ok, |a| limits.status(a)),
            limits,
            counted,
            breakdown: BTreeMap::from([("if", 3)]),
        }
    }

    fn run(rows: Vec<ChangedFunction>) -> ChangesRun {
        ChangesRun {
            base: "main".into(),
            base_commit: "abc1234".into(),
            head: Some("def5678".into()),
            branch: Some("feature".into()),
            scope: Scope::Lines,
            fail_on: FailOn::NewOrWorse,
            limits: Level {
                warn: Some(5),
                fail: Some(6),
            },
            files: vec!["src/Cart.cs".into()],
            rows,
            elapsed: Duration::from_millis(5),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn worse_function_above_fail_limit_fails() {
        let run = run(vec![
            row("Cart.Add", Change::Worse, Some(6), Some(9), true),
            row("Cart.Old", Change::Same, Some(8), Some(8), false),
            row("Cart.Gone", Change::Removed, Some(4), None, false),
        ]);
        assert_eq!(run.verdict(), Status::Fail);
        let text = render(&run, Format::Markdown);
        assert!(
            text.contains("**Failed.** 1 new or worse function is above the fail limit (6)."),
            "{text}"
        );
        assert!(
            text.contains("0 new · 1 worse · 0 better · 1 same · 1 removed · net -1"),
            "{text}"
        );
        assert!(
            text.contains("| **worse** | 6 → 9 | FAIL | `Cart.Add` |"),
            "{text}"
        );
        assert!(
            text.contains("| removed | 4 → – | – | `Cart.Gone` |"),
            "{text}"
        );
    }

    #[test]
    fn existing_debt_alone_passes() {
        let run = run(vec![row("Cart.Old", Change::Same, Some(8), Some(8), false)]);
        assert_eq!(run.verdict(), Status::Ok);
        let text = render(&run, Format::Table);
        assert!(
            text.contains("result: Passed. Within the limits; complexity did not change."),
            "{text}"
        );
        assert!(
            text.lines()
                .any(|l| l.starts_with("same") && l.contains("8 → 8  FAIL    Cart.Old")),
            "{text}"
        );
    }

    #[test]
    fn json_and_html_carry_the_summary() {
        let run = run(vec![row("Cart.New", Change::New, None, Some(4), true)]);
        let value = json(&run);
        assert_eq!(value["summary"]["new"], 1);
        assert_eq!(value["summary"]["net"], 4);
        assert_eq!(value["verdict"], "ok");
        assert_eq!(value["functions"][0]["before"], serde_json::Value::Null);
        assert_slots(TEMPLATE);
        assert!(render(&run, Format::Html).contains(r#""name":"Cart.New""#));
    }
}
