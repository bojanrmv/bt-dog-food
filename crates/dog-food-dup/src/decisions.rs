//! Review decisions: what someone decided about a clone class, kept in a
//! file by the class's id (`dgf-dup --decisions dup-decisions.json`). The
//! HTML report writes this file; dgf-dup reads it.
//!
//! - `to-fix`: still reported, marked;
//! - `intentional` and `false-positive`: left out of the report (counted),
//!   so a gate such as `--fail-on-copies` only sees what nobody has looked at.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Report;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// `to-fix`, `intentional` or `false-positive`.
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// When it was decided, as the page wrote it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
}

impl Decision {
    /// Decided as nothing to do: left out of the report.
    pub fn settles(&self) -> bool {
        matches!(self.status.as_str(), "intentional" | "false-positive")
    }
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    decisions: BTreeMap<String, Decision>,
}

/// The decisions in `path`, by class id.
pub fn load(path: &Path) -> Result<BTreeMap<String, Decision>, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let file: File =
        serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(file.decisions)
}

impl Report {
    /// Leaves out the classes decided as nothing to do, and keeps every
    /// decision in the report, for the HTML page to show.
    pub fn apply(&mut self, decisions: BTreeMap<String, Decision>) {
        let before = self.classes.len();
        self.classes
            .retain(|c| !decisions.get(&c.id).is_some_and(Decision::settles));
        self.suppressed = before - self.classes.len();
        self.duplicated_lines = self
            .classes
            .iter()
            .map(|c| c.lines * (c.copies.len() - 1))
            .sum();
        self.decisions = decisions;
    }
}
