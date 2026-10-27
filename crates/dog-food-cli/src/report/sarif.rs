//! SARIF 2.1.0: the format GitHub code scanning, Azure DevOps and many
//! editors read. Only functions above a limit become results.

use crate::changes::ChangedFunction;
use crate::limits::{Level, Status};

use super::Row;

const RULE: &str = "dgf/complexity";

/// One function that may become a SARIF result.
pub struct Finding<'a> {
    pub file: &'a str,
    pub start_line: usize,
    pub end_line: usize,
    pub name: &'a str,
    pub complexity: u32,
    pub cognitive: u32,
    pub status: Status,
    pub limits: Level,
    pub crap: Option<f64>,
}

impl<'a> Finding<'a> {
    pub fn from_row(row: &'a Row) -> Self {
        Finding {
            file: &row.file,
            start_line: row.function.start_line,
            end_line: row.function.end_line,
            name: &row.function.name,
            complexity: row.function.complexity,
            cognitive: row.function.cognitive,
            status: row.status,
            limits: row.limits,
            crap: row.crap,
        }
    }

    /// Only functions that can fail a `dgf changes` run.
    pub fn from_change(row: &'a ChangedFunction) -> Option<Self> {
        let after = row.after.filter(|_| row.counted)?;
        Some(Finding {
            file: &row.file,
            start_line: row.start_line,
            end_line: row.end_line,
            name: &row.name,
            complexity: after,
            cognitive: 0,
            status: row.status,
            limits: row.limits,
            crap: None,
        })
    }

    fn message(&self) -> String {
        let limit = match self.status {
            Status::Fail => self.limits.fail.map(|l| format!("fail limit {l}")),
            _ => self.limits.warn.map(|l| format!("warn limit {l}")),
        };
        let mut text = format!(
            "`{}` has cyclomatic complexity {} (above the {})",
            self.name,
            self.complexity,
            limit.unwrap_or_else(|| "limit".into())
        );
        if self.cognitive > 0 {
            text.push_str(&format!(", cognitive complexity {}", self.cognitive));
        }
        if let Some(crap) = self.crap {
            text.push_str(&format!(", CRAP score {crap:.1}"));
        }
        text + ". Split it into smaller functions or simplify its branches."
    }

    fn result(&self) -> serde_json::Value {
        serde_json::json!({
            "ruleId": RULE,
            "level": if self.status == Status::Fail { "error" } else { "warning" },
            "message": { "text": self.message() },
            "locations": [{
                "physicalLocation": {
                    "artifactLocation": { "uri": self.file.replace('\\', "/") },
                    "region": { "startLine": self.start_line, "endLine": self.end_line }
                }
            }]
        })
    }
}

pub fn render<'a>(findings: impl Iterator<Item = Finding<'a>>) -> String {
    let results: Vec<serde_json::Value> = findings
        .filter(|f| matches!(f.status, Status::Warn | Status::Fail))
        .map(|f| f.result())
        .collect();
    let sarif = serde_json::json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "dog-food",
                "semanticVersion": env!("CARGO_PKG_VERSION"),
                "rules": [{
                    "id": RULE,
                    "name": "FunctionTooComplex",
                    "shortDescription": { "text": "Function is above the complexity limit" },
                    "fullDescription": { "text": "Cyclomatic complexity counts the independent paths through a function: 1 plus every if, loop, case, catch, ?:, && and ||. Limits are set in dgf.toml." },
                    "defaultConfiguration": { "level": "warning" }
                }]
            }},
            "results": results
        }]
    });
    serde_json::to_string_pretty(&sarif).expect("SARIF serializes") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::tests::{file, function, limits, run};

    #[test]
    fn only_functions_above_a_limit_become_results() {
        let report = file(vec![
            function("A.Big", 9, &[]),
            function("A.Mid", 6, &[]),
            function("A.Ok", 2, &[]),
        ]);
        let limits = limits(5, 6);
        let run = run(&report, &limits);
        let value: serde_json::Value =
            serde_json::from_str(&render(run.rows.iter().map(Finding::from_row))).unwrap();
        let results = value["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["level"], "error");
        assert_eq!(results[1]["level"], "warning");
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            10
        );
        assert!(results[0]["message"]["text"]
            .as_str()
            .unwrap()
            .contains("complexity 9 (above the fail limit 6)"));
        assert_eq!(value["version"], "2.1.0");
    }
}
