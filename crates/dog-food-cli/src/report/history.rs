//! Output for `dgf history`: table, JSON, Markdown and an HTML overview page.

use std::fmt::Write;

use super::page::page;
use super::{now, tool, Format};
use crate::history::{utc, Mover, Overview};

const TEMPLATE: &str = include_str!("history.html");

pub fn render(overview: &Overview, format: Format, top: usize) -> Result<String, String> {
    Ok(match format {
        Format::Table => table(overview, top),
        Format::Json => {
            serde_json::to_string_pretty(&json(overview)).expect("report serializes") + "\n"
        }
        Format::Markdown => markdown(overview, top),
        Format::Html => page(TEMPLATE, &json(overview)),
        Format::Agent => agent(overview, top),
        Format::Sarif | Format::MutationReport => {
            return Err(format!(
                "{} describes code, not history; use another format",
                format.name()
            ))
        }
    })
}

/// Compact text for AI assistants: the trend first, then runs and movers.
fn agent(overview: &Overview, top: usize) -> String {
    let (Some(first), Some(last)) = (overview.runs.first(), overview.runs.last()) else {
        return "dgf history: no saved runs\n".to_string();
    };
    let (f, l) = (&first.summary, &last.summary);
    let mut out = format!(
        "dgf history: {} runs from {} to {} UTC\ntrend (first -> latest): functions {}->{}, highest {}->{}, median {}->{}, warn {}->{}, fail {}->{}, total score {}->{}\n",
        overview.runs.len(),
        utc(first.generated_at),
        utc(last.generated_at),
        f.functions, l.functions,
        f.max_complexity, l.max_complexity,
        f.median_complexity, l.median_complexity,
        f.warn, l.warn,
        f.fail, l.fail,
        f.total_complexity, l.total_complexity
    );
    out.push_str(
        "\n## runs, newest first (time commit branch functions highest median warn fail)\n",
    );
    for run in overview.runs.iter().rev().take(10) {
        let s = &run.summary;
        let _ = writeln!(
            out,
            "{} {} {} {} {} {} {} {}",
            utc(run.generated_at),
            run.commit.as_deref().unwrap_or("-"),
            run.branch.as_deref().unwrap_or("-"),
            s.functions,
            s.max_complexity,
            s.median_complexity,
            s.warn,
            s.fail
        );
    }
    let _ = writeln!(
        out,
        "\n## movers since first run (delta before->after file name), {} total",
        overview.movers.len()
    );
    for m in movers(overview, top) {
        let _ = writeln!(
            out,
            "{} {}->{} {} {}",
            signed(m.delta),
            score(m.before).replace('–', "na"),
            score(m.after).replace('–', "na"),
            m.file,
            m.name
        );
    }
    out
}

pub fn json(overview: &Overview) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "tool": tool(),
        "generatedAt": now(),
        "runs": overview.runs,
        "movers": overview.movers,
        "warnings": overview.warnings,
    })
}

fn movers(overview: &Overview, top: usize) -> impl Iterator<Item = &Mover> {
    let limit = if top == 0 { usize::MAX } else { top };
    overview.movers.iter().take(limit)
}

fn signed(delta: i64) -> String {
    if delta > 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}

fn score(value: Option<u32>) -> String {
    value.map_or_else(|| "–".to_string(), |v| v.to_string())
}

fn table(overview: &Overview, top: usize) -> String {
    let mut out = format!(
        "{:<19}  {:<9}  {:<16}  {:>9}  {:>7}  {:>6}  {:>4}  {:>4}\n",
        "RUN (UTC)", "COMMIT", "BRANCH", "FUNCTIONS", "HIGHEST", "MEDIAN", "WARN", "FAIL"
    );
    for run in overview.runs.iter().rev() {
        let s = &run.summary;
        let _ = writeln!(
            out,
            "{:<19}  {:<9}  {:<16}  {:>9}  {:>7}  {:>6}  {:>4}  {:>4}",
            utc(run.generated_at),
            run.commit.as_deref().unwrap_or("-"),
            run.branch.as_deref().unwrap_or("-"),
            s.functions,
            s.max_complexity,
            s.median_complexity,
            s.warn,
            s.fail
        );
    }
    if overview.movers.is_empty() {
        return out;
    }
    out.push_str("\nBiggest changes since the first run:\n");
    for m in movers(overview, top) {
        let pair = format!("{} → {}", score(m.before), score(m.after));
        let _ = writeln!(
            out,
            "{:>5}  {pair:>7}  {}  {}",
            signed(m.delta),
            m.name,
            m.file
        );
    }
    out
}

fn markdown(overview: &Overview, top: usize) -> String {
    let mut out = format!(
        "## dog-food: complexity over time\n\n{} runs saved.\n\n",
        overview.runs.len()
    );
    out.push_str("| Run (UTC) | Commit | Branch | Functions | Highest | Median | Warn | Fail |\n| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |\n");
    for run in overview.runs.iter().rev() {
        let s = &run.summary;
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} | {} | {} | {} | {} |",
            utc(run.generated_at),
            run.commit.as_deref().unwrap_or("-"),
            run.branch.as_deref().unwrap_or("-"),
            s.functions,
            s.max_complexity,
            s.median_complexity,
            s.warn,
            s.fail
        );
    }
    if !overview.movers.is_empty() {
        out.push_str("\n### Biggest changes since the first run\n\n| Change | Score | Function | File |\n| ---: | --- | --- | --- |\n");
        for m in movers(overview, top) {
            let _ = writeln!(
                out,
                "| {} | {} → {} | `{}` | `{}` |",
                signed(m.delta),
                score(m.before),
                score(m.after),
                m.name.replace('|', "\\|"),
                m.file
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{RunSummary, Summary};
    use crate::limits::{Level, Status};
    use crate::report::page::tests::assert_slots;

    fn overview() -> Overview {
        let summary = |functions, max| Summary {
            files: 3,
            functions,
            max_complexity: max,
            median_complexity: 2,
            total_complexity: 300,
            ok: functions,
            warn: 0,
            fail: 0,
            ignored: 0,
            limits: Level {
                warn: Some(5),
                fail: Some(6),
            },
        };
        Overview {
            runs: vec![
                RunSummary {
                    generated_at: 1_792_600_000,
                    commit: Some("aaa1111".into()),
                    branch: Some("main".into()),
                    summary: summary(100, 5),
                },
                RunSummary {
                    generated_at: 1_792_657_503,
                    commit: Some("bbb2222".into()),
                    branch: Some("main".into()),
                    summary: summary(120, 7),
                },
            ],
            movers: vec![Mover {
                file: "a.rs".into(),
                name: "grow".into(),
                before: Some(3),
                after: Some(7),
                delta: 4,
                status: Status::Fail,
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn table_lists_newest_run_first_and_movers() {
        let text = render(&overview(), Format::Table, 10).unwrap();
        let newest = text.find("2026-10-22 08:25:03").unwrap();
        let oldest = text
            .find("bbb2222")
            .map(|_| text.find("aaa1111").unwrap())
            .unwrap();
        assert!(newest < oldest, "{text}");
        assert!(text.contains("+4    3 → 7  grow  a.rs"), "{text}");
    }

    #[test]
    fn markdown_and_html_render() {
        let text = render(&overview(), Format::Markdown, 10).unwrap();
        assert!(text.contains("| +4 | 3 → 7 | `grow` | `a.rs` |"), "{text}");
        assert_slots(TEMPLATE);
        assert!(render(&overview(), Format::Html, 10)
            .unwrap()
            .contains(r#""commit":"bbb2222""#));
    }
}
