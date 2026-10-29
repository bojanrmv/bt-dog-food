//! How risky each class's copies are: the complexity of the functions they
//! are in, and with a coverage report their CRAP score. A copied function
//! that is complex and untested is where a fix made in one copy and not the
//! others hurts most, so `--rank risk` puts it first.

use std::collections::BTreeMap;
use std::path::Path;

use dog_food_core::coverage::{self, Coverage};
use dog_food_core::FunctionMetrics;
use rayon::prelude::*;
use serde::Serialize;

use crate::{CloneClass, Copy, Report};

/// The riskiest copy of a class: the function it is in, its cyclomatic
/// complexity, and with coverage its covered share and CRAP score.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Risk {
    pub function: String,
    pub complexity: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crap: Option<f64>,
}

impl Risk {
    /// What ranks it: the CRAP score where there is one, else the complexity.
    pub fn score(&self) -> f64 {
        self.crap.unwrap_or(f64::from(self.complexity))
    }
}

/// Sets each class's risk, from the functions its copies are in.
pub fn assess(report: &mut Report, root: &Path, coverage: Option<&Coverage>) {
    let files: Vec<&String> = report.file_lines.keys().collect();
    let functions: BTreeMap<&String, Vec<FunctionMetrics>> = files
        .par_iter()
        .filter_map(|&file| {
            let analyzed = dog_food_core::analyze_file(&root.join(file)).ok()?;
            Some((file, analyzed.functions))
        })
        .collect();
    let risks: Vec<Option<Risk>> = report
        .classes
        .iter()
        .map(|class| riskiest(class, &functions, root, coverage))
        .collect();
    for (class, risk) in report.classes.iter_mut().zip(risks) {
        class.risk = risk;
    }
}

fn riskiest(
    class: &CloneClass,
    functions: &BTreeMap<&String, Vec<FunctionMetrics>>,
    root: &Path,
    coverage: Option<&Coverage>,
) -> Option<Risk> {
    class
        .copies
        .iter()
        .filter_map(|copy| risk_of(copy, functions.get(&copy.file)?, root, coverage))
        .max_by(|a, b| a.score().total_cmp(&b.score()))
}

/// The risk of the innermost function holding a copy.
fn risk_of(
    copy: &Copy,
    functions: &[FunctionMetrics],
    root: &Path,
    coverage: Option<&Coverage>,
) -> Option<Risk> {
    let function = functions
        .iter()
        .filter(|f| f.start_line <= copy.start_line && copy.end_line <= f.end_line)
        .min_by_key(|f| f.end_line - f.start_line)?;
    let covered = coverage
        .and_then(|c| c.for_file(&root.join(&copy.file)))
        .and_then(|lines| coverage::ratio(lines, function.start_line, function.end_line));
    Some(Risk {
        function: copy
            .function
            .clone()
            .unwrap_or_else(|| function.name.clone()),
        complexity: function.complexity,
        coverage: covered,
        crap: covered.map(|c| coverage::crap(function.complexity, c)),
    })
}

/// Riskiest first, then by lines saved.
pub fn rank(report: &mut Report) {
    let score = |c: &CloneClass| c.risk.as_ref().map_or(0.0, Risk::score);
    report
        .classes
        .sort_by(|a, b| score(b).total_cmp(&score(a)).then(b.saves.cmp(&a.saves)));
}
