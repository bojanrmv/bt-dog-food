//! Turns one analysis run into a table, JSON, Markdown or an HTML page.

pub mod agent;
pub mod changes;
pub mod history;
mod html;
mod markdown;
pub mod mutation;
mod page;
mod sarif;
mod table;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::ValueEnum;
use dog_food_core::{FileReport, FunctionMetrics, Language};
use serde::Serialize;

use crate::limits::{Level, Limits, Status};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Aligned columns for the terminal
    Table,
    /// Everything, for other tools
    Json,
    /// GitHub-flavored Markdown, for PR comments and job summaries
    Markdown,
    /// A self-contained page with charts
    Html,
    /// SARIF 2.1.0, for GitHub code scanning and editors
    Sarif,
    /// Compact text for AI assistants: only what needs attention
    Agent,
    /// The mutation-testing-elements JSON report (`dgf mutate --run` only)
    MutationReport,
}

impl Format {
    pub fn name(self) -> &'static str {
        FORMAT_NAMES
            .iter()
            .find(|(format, _)| *format == self)
            .map_or("JSON", |(_, name)| name)
    }
}

const FORMAT_NAMES: &[(Format, &str)] = &[
    (Format::Table, "table"),
    (Format::Json, "JSON"),
    (Format::Markdown, "Markdown"),
    (Format::Html, "HTML"),
    (Format::Sarif, "SARIF"),
    (Format::Agent, "agent"),
    (Format::MutationReport, "mutation-testing-elements JSON"),
];

/// One function with the file it is in and how it compares to its limits.
#[derive(Serialize)]
pub struct Row<'a> {
    pub file: String,
    pub language: Language,
    pub status: Status,
    pub limits: Level,
    /// Covered share of the function's instrumented lines (0 to 1), with `--coverage`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
    /// CRAP score, with `--coverage`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crap: Option<f64>,
    /// The accepted score when the baseline excuses this function.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<u32>,
    /// Mutation score from `dgf mutate --run`, while the function is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation: Option<f64>,
    #[serde(flatten)]
    pub function: &'a FunctionMetrics,
}

impl<'a> Row<'a> {
    pub fn new(
        file: String,
        report: &'a FileReport,
        function: &'a FunctionMetrics,
        limits: Level,
    ) -> Self {
        let status = match function.ignored {
            Some(_) => Status::Ignored,
            None => limits.status(function.complexity),
        };
        Row {
            file,
            language: report.language,
            status,
            limits,
            coverage: None,
            crap: None,
            baseline: None,
            mutation: None,
            function,
        }
    }
}

/// Everything one analysis run produced, rows sorted most complex first.
pub struct Run<'a> {
    pub paths: Vec<String>,
    /// Coverage reports that were read, when `--coverage` was given.
    pub coverage: Vec<String>,
    pub rows: Vec<Row<'a>>,
    pub files: usize,
    pub elapsed: Duration,
    pub limits: &'a Limits,
    pub config: Option<String>,
    pub warnings: Vec<String>,
}

impl Run<'_> {
    pub fn count(&self, status: Status) -> usize {
        self.rows.iter().filter(|row| row.status == status).count()
    }

    pub fn max_complexity(&self) -> u32 {
        self.rows.first().map_or(0, |row| row.function.complexity)
    }
}

pub fn render(run: &Run, format: Format, top: usize) -> String {
    match format {
        Format::Table => table::render(run, top),
        // `dgf` refuses the mutation report before analyzing (`only_for_mutate`).
        Format::Json | Format::MutationReport => {
            serde_json::to_string_pretty(&json(run)).expect("report serializes") + "\n"
        }
        Format::Markdown => markdown::render(run, top),
        Format::Html => html::render(run),
        Format::Sarif => sarif::render(run.rows.iter().map(sarif::Finding::from_row)),
        Format::Agent => agent::render(run, top),
    }
}

/// The JSON report; the HTML page embeds the same data.
pub fn json(run: &Run) -> serde_json::Value {
    let generated_at = now();
    serde_json::json!({
        "schemaVersion": 1,
        "tool": tool(),
        "generatedAt": generated_at,
        "paths": run.paths,
        "config": run.config,
        "summary": {
            "files": run.files,
            "functions": run.rows.len(),
            "durationMs": run.elapsed.as_millis(),
            "maxComplexity": run.max_complexity(),
            "limits": run.limits.global,
            "pathOverrides": run.limits.override_count(),
            "ok": run.count(Status::Ok),
            "warn": run.count(Status::Warn),
            "fail": run.count(Status::Fail),
            "ignored": run.count(Status::Ignored),
            "maxCognitive": run.rows.iter().map(|r| r.function.cognitive).max().unwrap_or(0),
            "crapLimits": (!run.coverage.is_empty()).then_some(run.limits.crap),
        },
        "coverage": run.coverage,
        "functions": run.rows,
        "warnings": run.warnings,
    })
}

/// Seconds since 1970.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn tool() -> serde_json::Value {
    serde_json::json!({ "name": "dog-food", "version": env!("CARGO_PKG_VERSION") })
}

pub fn status_label(status: Status) -> &'static str {
    match status {
        Status::Ignored => "ignored",
        Status::Ok => "ok",
        Status::Warn => "warn",
        Status::Fail => "FAIL",
    }
}

/// How many rows a "top N" view shows; 0 means all.
fn shown(run: &Run, top: usize) -> usize {
    if top == 0 {
        run.rows.len()
    } else {
        top.min(run.rows.len())
    }
}

/// "if ×2, loop ×1"
fn breakdown_text(function: &FunctionMetrics) -> String {
    categories_text(&function.breakdown)
}

fn categories_text(breakdown: &std::collections::BTreeMap<&'static str, u32>) -> String {
    let parts: Vec<String> = breakdown
        .iter()
        .map(|(category, count)| format!("{category} ×{count}"))
        .collect();
    if parts.is_empty() {
        "no branches".to_string()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::config::LimitsConfig;

    pub fn limits(warn: u32, fail: u32) -> Limits {
        let config = LimitsConfig {
            warn: Some(warn),
            fail: Some(fail),
            ..LimitsConfig::default()
        };
        Limits::new(&config, Path::new("."), None, None).unwrap()
    }

    pub fn function(
        name: &str,
        complexity: u32,
        breakdown: &[(&'static str, u32)],
    ) -> FunctionMetrics {
        FunctionMetrics {
            name: name.to_string(),
            kind: "method",
            start_line: 10,
            end_line: 20,
            complexity,
            breakdown: breakdown.iter().copied().collect::<BTreeMap<_, _>>(),
            ..FunctionMetrics::default()
        }
    }

    pub fn file(functions: Vec<FunctionMetrics>) -> FileReport {
        FileReport {
            path: "src/Orders/OrderService.cs".to_string(),
            language: Language::CSharp,
            functions,
            has_errors: false,
        }
    }

    /// A run over `report`, rows sorted most complex first.
    pub fn run<'a>(report: &'a FileReport, limits: &'a Limits) -> Run<'a> {
        let mut rows: Vec<Row> = report
            .functions
            .iter()
            .map(|f| Row::new(report.path.clone(), report, f, limits.global))
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.function.complexity));
        Run {
            paths: vec![".".to_string()],
            coverage: Vec::new(),
            rows,
            files: 1,
            elapsed: Duration::from_millis(12),
            limits,
            config: Some("dgf.toml".to_string()),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn json_has_summary_counts_and_functions() {
        let report = file(vec![
            function("A.Big", 9, &[("if", 8)]),
            function("A.Small", 1, &[]),
        ]);
        let limits = limits(5, 6);
        let value = json(&run(&report, &limits));
        assert_eq!(value["summary"]["fail"], 1);
        assert_eq!(value["summary"]["ok"], 1);
        assert_eq!(value["summary"]["maxComplexity"], 9);
        assert_eq!(value["functions"][0]["name"], "A.Big");
        assert_eq!(value["functions"][0]["status"], "fail");
    }

    #[test]
    fn breakdown_text_lists_categories() {
        assert_eq!(
            breakdown_text(&function("f", 4, &[("if", 2), ("&&", 1)])),
            "&& ×1, if ×2"
        );
        assert_eq!(breakdown_text(&function("f", 1, &[])), "no branches");
    }
}
