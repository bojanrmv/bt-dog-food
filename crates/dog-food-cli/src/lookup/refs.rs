//! `dgf refs`: where a name is written in code, with the declaration each
//! use is in. By name only: nothing is resolved to a type.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dog_food_core::outline::{enclosing, outline_and_uses, path as symbol_path, Symbol};
use dog_food_core::{FunctionMetrics, Language};
use rayon::prelude::*;

use super::{allot, name_parts, shown, CommonArgs, DEFAULT_LINES};
use crate::discover;
use crate::{usage, Failure, EXIT_FAIL};

/// The longest piece of a source line that is printed.
const LINE_LIMIT: usize = 160;

#[derive(clap::Args)]
pub struct Args {
    /// Names to look for, such as PricingOptions; of Cart.Total the name Total is looked for
    #[arg(required = true)]
    symbols: Vec<String>,
    /// Folder to search [default: the current folder]
    #[arg(long = "in", value_name = "DIR")]
    dir: Option<PathBuf>,
    #[command(flatten)]
    common: CommonArgs,
}

/// One line that uses a name.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    /// Which of the names asked for.
    name: usize,
    line: usize,
    /// The name is declared here.
    definition: bool,
    /// The declaration the line is in, `-` when in none.
    inside: String,
    text: String,
}

struct FileRows {
    path: String,
    test: bool,
    rows: Vec<Row>,
}

impl FileRows {
    /// The rows of one of the names asked for.
    fn of(&self, name: usize) -> Vec<&Row> {
        self.rows.iter().filter(|row| row.name == name).collect()
    }
}

pub fn run(args: Args) -> Result<ExitCode, Failure> {
    let settings = args.common.settings()?;
    let dir = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    if !dir.is_dir() {
        return Err(usage(format!("--in {} is not a folder", dir.display())));
    }
    let names: Vec<&str> = args
        .symbols
        .iter()
        .map(|symbol| name_parts(symbol).last().copied().unwrap_or(symbol))
        .collect();
    let files = discover::discover(std::slice::from_ref(&dir), settings.excludes.clone());
    let mut found: Vec<FileRows> = files
        .par_iter()
        .filter_map(|file| search(file, &settings.cwd, &names))
        .collect();
    // Declarations first, tests last.
    found.sort_by_key(|file| {
        let declares = file.rows.iter().any(|row| row.definition);
        (file.test, !declares, file.path.clone())
    });

    let counts: Vec<usize> = (0..names.len())
        .map(|name| {
            found
                .iter()
                .flat_map(|file| &file.rows)
                .filter(|row| row.name == name)
                .count()
        })
        .collect();
    let given = allot(&counts, args.common.budget(DEFAULT_LINES));
    let mut text = String::from(
        "refs: by name from the syntax tree, not resolved by type: another declaration with the same name is listed too\n",
    );
    let place = crate::display_path(&dir.to_string_lossy());
    for (name, asked) in args.symbols.iter().enumerate() {
        let searched = names[name];
        let title = if searched == asked {
            searched.to_string()
        } else {
            format!("{searched} (of {asked})")
        };
        text.push_str(&render(&title, name, &found, given[name], &place));
    }
    crate::progress::finish();
    say!("{text}");
    let code = if counts.iter().all(|count| *count == 0) {
        EXIT_FAIL
    } else {
        0
    };
    Ok(ExitCode::from(code))
}

/// The lines of `file` that use one of `names`; `None` when it has none.
fn search(file: &Path, cwd: &Path, names: &[&str]) -> Option<FileRows> {
    let text = std::fs::read_to_string(file).ok()?;
    if !names.iter().any(|name| text.contains(name)) {
        return None;
    }
    let language = Language::from_path(file)?;
    let rows = rows(&text, language, names);
    (!rows.is_empty()).then(|| FileRows {
        path: shown(file, cwd),
        test: crate::mutate::is_test_code(file),
        rows,
    })
}

/// One row per line that uses a name, in the order written.
fn rows(text: &str, language: Language, names: &[&str]) -> Vec<Row> {
    let (symbols, found) = outline_and_uses(text, language, names).unwrap_or_default();
    let mut scopes = Scopes {
        text,
        language,
        symbols,
        functions: None,
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut rows: Vec<Row> = Vec::new();
    for used in found {
        let declared = scopes.symbols.iter().position(|s| s.name_byte == used.byte);
        let inside = scopes.inside(declared, used.line);
        let same_line = rows
            .iter_mut()
            .find(|row| row.name == used.name && row.line == used.line);
        let Some(row) = same_line else {
            rows.push(Row {
                name: used.name,
                line: used.line,
                definition: declared.is_some(),
                inside,
                text: shorten(lines.get(used.line - 1).copied().unwrap_or_default()),
            });
            continue;
        };
        if declared.is_some() {
            row.definition = true;
            row.inside = inside;
        }
    }
    rows
}

/// What the lines of one file are inside.
struct Scopes<'a> {
    text: &'a str,
    language: Language,
    symbols: Vec<Symbol>,
    /// Functions the outline does not list, read when a use needs them.
    functions: Option<Vec<FunctionMetrics>>,
}

impl Scopes<'_> {
    /// For a declaration, what it is a member of; for another use, the
    /// declaration or function around `line`. `-` when there is none.
    fn inside(&mut self, declared: Option<usize>, line: usize) -> String {
        if let Some(index) = declared {
            return owner(&self.symbols, index);
        }
        if let Some(index) = enclosing(&self.symbols, line) {
            return symbol_path(&self.symbols, index).join(".");
        }
        let (text, language) = (self.text, self.language);
        self.functions
            .get_or_insert_with(|| {
                dog_food_core::analyze_source(text, language).unwrap_or_default()
            })
            .iter()
            .filter(|f| f.start_line <= line && line <= f.end_line)
            .min_by_key(|f| f.end_line - f.start_line)
            .map_or_else(|| "-".to_string(), |f| f.name.clone())
    }
}

/// What the declaration `symbols[index]` is a member of, `-` for none.
fn owner(symbols: &[Symbol], index: usize) -> String {
    match symbols[index].parent {
        Some(parent) => symbol_path(symbols, parent).join("."),
        None => "-".to_string(),
    }
}

fn shorten(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= LINE_LIMIT {
        return line.to_string();
    }
    let kept: String = line.chars().take(LINE_LIMIT).collect();
    format!("{kept}...")
}

/// The uses of one name: a count, then its first `given` rows by file.
fn render(title: &str, name: usize, found: &[FileRows], given: usize, place: &str) -> String {
    let files: Vec<(&FileRows, Vec<&Row>)> = found
        .iter()
        .map(|file| (file, file.of(name)))
        .filter(|(_, rows)| !rows.is_empty())
        .collect();
    let total: usize = files.iter().map(|(_, rows)| rows.len()).sum();
    if total == 0 {
        return format!(
            "{title}: no uses under {place}; names are matched whole and with their case\n"
        );
    }
    let definitions = files
        .iter()
        .flat_map(|(_, rows)| rows)
        .filter(|row| row.definition)
        .count();
    let mut text = format!(
        "{title}: {total} lines in {} files, declared on {definitions} of them\n",
        files.len()
    );
    // How many rows of each file fit in what is left.
    let mut left = given;
    let limits: Vec<usize> = files
        .iter()
        .map(|(_, rows)| {
            let take = rows.len().min(left);
            left -= take;
            take
        })
        .collect();
    let printed = limits.iter().filter(|limit| **limit > 0).count();
    let first_test = files.iter().position(|(file, _)| file.test);
    for (index, ((file, rows), limit)) in files.iter().zip(&limits).enumerate().take(printed) {
        if first_test == Some(index) {
            text.push_str("  tests:\n");
        }
        text.push_str(&file_rows(file, rows, *limit));
    }
    if total > given {
        text.push_str(&format!(
            "  ... {} more lines, {} more files; narrow with --in or raise --max-lines\n",
            total - given,
            files.len() - printed
        ));
    }
    text
}

/// The path of a file and up to `limit` of its rows.
fn file_rows(file: &FileRows, rows: &[&Row], limit: usize) -> String {
    let mut text = format!("  {}\n", file.path);
    for row in rows.iter().take(limit) {
        let mark = if row.definition { "def" } else { "   " };
        text.push_str(&format!(
            "    {:>5}  {mark}  {}  {}\n",
            row.line, row.inside, row.text
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const STARTUP: &str = "\
// PricingOptions are bound once.
public sealed class PricingOptions
{
    public string Mode { get; set; } = \"PricingOptions\";
}

public class Startup
{
    public void Configure(IServiceCollection services)
    {
        services.Configure<PricingOptions>(o => o.Mode = nameof(PricingOptions));
    }
}
";

    fn listed(text: &str, language: Language, names: &[&str]) -> Vec<String> {
        rows(text, language, names)
            .iter()
            .map(|row| {
                format!(
                    "{} {}{} in {} | {}",
                    names[row.name],
                    row.line,
                    if row.definition { " def" } else { "" },
                    row.inside,
                    row.text
                )
            })
            .collect()
    }

    #[test]
    fn uses_name_their_declaration_and_skip_comments_and_strings() {
        assert_eq!(
            listed(STARTUP, Language::CSharp, &["PricingOptions", "Mode"]),
            [
                "PricingOptions 2 def in - | public sealed class PricingOptions",
                "Mode 4 def in PricingOptions | public string Mode { get; set; } = \"PricingOptions\";",
                "PricingOptions 11 in Startup.Configure | services.Configure<PricingOptions>(o => o.Mode = nameof(PricingOptions));",
                "Mode 11 in Startup.Configure | services.Configure<PricingOptions>(o => o.Mode = nameof(PricingOptions));",
            ]
        );
    }

    #[test]
    fn a_use_names_the_declaration_it_is_in() {
        let spec = "\
import { load } from './load';
const run = () => {
  load(1);
};
";
        assert_eq!(
            listed(spec, Language::TypeScript, &["load"]),
            [
                "load 1 in - | import { load } from './load';",
                "load 3 in run | load(1);",
            ]
        );
    }

    fn files() -> Vec<FileRows> {
        let row = |line, definition| Row {
            name: 0,
            line,
            definition,
            inside: "Cart".to_string(),
            text: "x".to_string(),
        };
        vec![
            FileRows {
                path: "src/Cart.cs".to_string(),
                test: false,
                rows: vec![row(3, true), row(9, false)],
            },
            FileRows {
                path: "tests/CartTests.cs".to_string(),
                test: true,
                rows: vec![row(5, false)],
            },
        ]
    }

    #[test]
    fn rows_are_grouped_by_file_with_tests_named() {
        assert_eq!(
            render("Total", 0, &files(), 10, "."),
            "\
Total: 3 lines in 2 files, declared on 1 of them
  src/Cart.cs
        3  def  Cart  x
        9       Cart  x
  tests:
  tests/CartTests.cs
        5       Cart  x
"
        );
    }

    #[test]
    fn a_cut_list_says_what_is_left_and_a_missing_name_says_so() {
        let cut = render("Total", 0, &files(), 1, ".");
        assert!(cut.contains("        3  def  Cart  x\n"), "{cut}");
        assert!(!cut.contains("        9 "), "{cut}");
        assert!(
            cut.ends_with(
                "  ... 2 more lines, 1 more files; narrow with --in or raise --max-lines\n"
            ),
            "{cut}"
        );
        assert_eq!(
            render("Totl", 1, &files(), 10, "src"),
            "Totl: no uses under src; names are matched whole and with their case\n"
        );
    }
}
