use std::collections::BTreeMap;

use serde::Serialize;

use crate::Language;

/// Complexity of one function, method, closure or arrow function.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionMetrics {
    /// Qualified name, e.g. `Cart::total` or `outer/closure@12:5`.
    pub name: String,
    /// `function`, `method`, `closure`, `arrow`, `lambda`, `accessor`, ...
    pub kind: &'static str,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    /// Byte range in the source, end exclusive: tells apart functions that
    /// share lines (two lambdas or accessors on one line). Not in reports.
    #[serde(skip)]
    pub start_byte: usize,
    #[serde(skip)]
    pub end_byte: usize,
    /// Cyclomatic complexity: 1 + the number of decision points.
    pub complexity: u32,
    /// How many decision points of each category were counted.
    pub breakdown: BTreeMap<&'static str, u32>,
    /// Cognitive complexity (SonarSource): branches and loops cost more the
    /// deeper they are nested; `else` and sequences of `&&` / `||` add 1.
    pub cognitive: u32,
    /// Deepest nesting of branches and loops inside the function.
    pub max_nesting: u32,
    /// Lines from the function's first line to its last.
    pub lines: usize,
    pub parameters: u32,
    /// Set when a `dgf-ignore` comment sits right above the function: the
    /// reason written after it, or an empty string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReport {
    pub path: String,
    pub language: Language,
    pub functions: Vec<FunctionMetrics>,
    /// The parser hit a syntax error; results for that region may be partial.
    pub has_errors: bool,
}
