//! `dgf show`: the code of named declarations, several files in one call.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dog_food_core::outline::{enclosing, outline, path as symbol_path, Symbol};
use dog_food_core::{FunctionMetrics, Language};
use rayon::prelude::*;

use super::{allot, name_parts, nearest, range, read, shown, CommonArgs};
use crate::discover;
use crate::{Failure, Settings, EXIT_FAIL};

/// Lines of code printed when `--max-lines` is not given: bodies are what
/// was asked for by name, so more than the other commands.
const DEFAULT_LINES: usize = 400;
/// With a line that is in no declaration, the lines printed around it.
const AROUND: usize = 10;
/// Definitions of one name printed in full; the rest are listed.
const DEFINITIONS: usize = 8;

#[derive(clap::Args)]
pub struct Args {
    /// What to print: FILE:NAME, FILE:Type.Member, FILE:LINE (the declaration around the line), FILE:FROM-TO, or a NAME to look for under --in
    #[arg(required = true)]
    targets: Vec<String>,
    /// Where a NAME without a file is looked for [default: the current folder]
    #[arg(long = "in", value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Print only each declaration's first line and the comment above it
    #[arg(long)]
    signature: bool,
    /// Also print this many lines before and after
    #[arg(long, value_name = "N", default_value_t = 0)]
    context: usize,
    #[command(flatten)]
    common: CommonArgs,
}

/// What a target asks for in its file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Want {
    Name(String),
    Line(usize),
    Lines(usize, usize),
}

/// One target as written, split into its file and what is wanted there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    file: Option<String>,
    want: Option<Want>,
}

/// One piece of code to print.
struct Piece {
    path: String,
    start: usize,
    end: usize,
    title: String,
    lines: Vec<String>,
}

/// What became of one target.
enum Outcome {
    Found(Vec<Piece>),
    Missing(String),
}

pub fn run(args: Args) -> Result<ExitCode, Failure> {
    let settings = args.common.settings()?;
    let outcomes: Vec<Outcome> = args
        .targets
        .iter()
        .map(|raw| resolve(raw, &args, &settings))
        .collect();
    let pieces: Vec<&Piece> = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            Outcome::Found(pieces) => Some(pieces),
            Outcome::Missing(_) => None,
        })
        .flatten()
        .collect();
    let sizes: Vec<usize> = pieces.iter().map(|p| p.lines.len()).collect();
    let given = allot(&sizes, args.common.budget(DEFAULT_LINES));
    let mut given = given.into_iter();
    let mut text = String::new();
    for outcome in &outcomes {
        match outcome {
            Outcome::Missing(message) => {
                text.push_str(message);
                text.push('\n');
            }
            Outcome::Found(found) => {
                for piece in found {
                    text.push_str(&render(piece, given.next().unwrap_or(0)));
                }
            }
        }
    }
    crate::progress::finish();
    say!("{text}");
    let code = if pieces.is_empty() { EXIT_FAIL } else { 0 };
    Ok(ExitCode::from(code))
}

/// Splits `raw` at the colon after its file: the first one that leaves an
/// existing file on the left, else the first that leaves a source file's
/// name. `Cart::total` and `Total` name no file.
fn parse(raw: &str, is_file: impl Fn(&str) -> bool) -> Target {
    let colons: Vec<usize> = raw.match_indices(':').map(|(at, _)| at).collect();
    let split = colons
        .iter()
        .find(|at| is_file(&raw[..**at]))
        .or_else(|| {
            colons
                .iter()
                .find(|at| Language::from_path(Path::new(&raw[..**at])).is_some())
        })
        .copied();
    match split {
        Some(at) => Target {
            file: Some(raw[..at].to_string()),
            want: want(&raw[at + 1..]),
        },
        None if is_file(raw) || Language::from_path(Path::new(raw)).is_some() => Target {
            file: Some(raw.to_string()),
            want: None,
        },
        None => Target {
            file: None,
            want: want(raw),
        },
    }
}

fn want(text: &str) -> Option<Want> {
    let number = |digits: &str| digits.parse::<usize>().ok().filter(|n| *n > 0);
    if text.is_empty() {
        return None;
    }
    if let Some(line) = number(text) {
        return Some(Want::Line(line));
    }
    if let Some((from, to)) = text.split_once('-') {
        if let (Some(from), Some(to)) = (number(from), number(to)) {
            return Some(Want::Lines(from.min(to), from.max(to)));
        }
    }
    Some(Want::Name(text.to_string()))
}

fn resolve(raw: &str, args: &Args, settings: &Settings) -> Outcome {
    let target = parse(raw, |path| Path::new(path).is_file());
    match (target.file, target.want) {
        (Some(file), Some(want)) => in_file(raw, Path::new(&file), &want, args, settings),
        (Some(file), None) => Outcome::Missing(format!(
            "{raw}: name a declaration or a line, as in {file}:Name or {file}:120; `dgf outline {file}` lists what is in it"
        )),
        (None, Some(Want::Name(name))) => by_name(raw, &name, args, settings),
        (None, _) => Outcome::Missing(format!("{raw}: a line needs its file, as in FILE:{raw}")),
    }
}

/// One source file, read once.
struct Source {
    path: String,
    lines: Vec<String>,
    symbols: Vec<Symbol>,
    text: String,
    language: Language,
}

impl Source {
    fn load(file: &Path, cwd: &Path) -> Result<Source, String> {
        let (text, language) = read(file)?;
        let symbols = outline(&text, language).map_err(|err| err.to_string())?;
        Ok(Source {
            path: shown(file, cwd),
            lines: text.lines().map(str::to_string).collect(),
            symbols,
            text,
            language,
        })
    }

    /// Lines `start..=end` with `context` more on both sides.
    fn piece(&self, start: usize, end: usize, doc: usize, title: String, args: &Args) -> Piece {
        let last = self.lines.len().max(1);
        let (start, end) = (start.clamp(1, last), end.clamp(1, last));
        let (from, to) = if args.signature {
            (doc.min(start), start)
        } else {
            (
                start.saturating_sub(args.context).max(1),
                (end + args.context).min(last),
            )
        };
        let lines = (from..=to)
            .filter_map(|line| Some(format!("{line}\t{}", self.lines.get(line - 1)?)))
            .collect();
        Piece {
            path: self.path.clone(),
            start,
            end,
            title,
            lines,
        }
    }

    fn symbol_piece(&self, index: usize, args: &Args) -> Piece {
        let symbol = &self.symbols[index];
        self.piece(
            symbol.start_line,
            symbol.end_line,
            symbol.doc_line,
            symbol.signature.clone(),
            args,
        )
    }

    /// The declarations named `name`: `Total`, or `Cart.Total` for the one
    /// in `Cart`.
    fn named(&self, name: &str) -> Vec<usize> {
        let wanted = name_parts(name);
        (0..self.symbols.len())
            .filter(|index| symbol_path(&self.symbols, *index).ends_with(&wanted))
            .collect()
    }

    /// Functions the outline does not list: local functions and functions
    /// passed to a call.
    fn functions(&self) -> Vec<FunctionMetrics> {
        dog_food_core::analyze_source(&self.text, self.language).unwrap_or_default()
    }
}

/// The last part of a function's name: `Cart.Total`, `outer/inner`.
fn last_part(name: &str) -> &str {
    name.rsplit(['.', '/', ':']).next().unwrap_or(name)
}

fn in_file(raw: &str, file: &Path, want: &Want, args: &Args, settings: &Settings) -> Outcome {
    let source = match Source::load(file, &settings.cwd) {
        Ok(source) => source,
        Err(err) if file.is_file() => return Outcome::Missing(format!("{raw}: {err}")),
        Err(_) => return Outcome::Missing(format!("{raw}: file not found")),
    };
    match want {
        Want::Lines(from, to) => {
            let title = String::new();
            Outcome::Found(vec![source.piece(*from, *to, *from, title, args)])
        }
        Want::Line(line) => at_line(raw, &source, *line, args),
        Want::Name(name) => named_in(raw, &source, name, args),
    }
}

/// The declarations of `source` named `name`, else the functions inside
/// other functions with that name.
fn named_in(raw: &str, source: &Source, name: &str, args: &Args) -> Outcome {
    let found = source.named(name);
    if !found.is_empty() {
        let pieces = found
            .iter()
            .map(|i| source.symbol_piece(*i, args))
            .collect();
        return Outcome::Found(pieces);
    }
    let wanted = last_part(name);
    let functions = source.functions();
    let local: Vec<Piece> = functions
        .iter()
        .filter(|f| last_part(&f.name) == wanted)
        .map(|f| source.piece(f.start_line, f.end_line, f.start_line, f.name.clone(), args))
        .collect();
    if local.is_empty() {
        return Outcome::Missing(missing_name(raw, wanted, source));
    }
    Outcome::Found(local)
}

/// `raw: not found`, with the declarations whose names are close.
fn missing_name(raw: &str, wanted: &str, source: &Source) -> String {
    let close = nearest(wanted, source.symbols.iter().map(|s| s.name.as_str()), 5);
    let places: Vec<String> = close
        .iter()
        .filter_map(|name| source.symbols.iter().find(|s| s.name == *name))
        .map(|s| format!("{} ({})", s.name, range(s.start_line, s.end_line)))
        .collect();
    if places.is_empty() {
        format!(
            "{raw}: not found; `dgf outline {}` lists what is in the file",
            source.path
        )
    } else {
        format!("{raw}: not found; nearest: {}", places.join(", "))
    }
}

/// The member around `line`; a function inside it when the outline has no
/// member there; else the lines around it.
fn at_line(raw: &str, source: &Source, line: usize, args: &Args) -> Outcome {
    if line > source.lines.len() {
        return Outcome::Missing(format!("{raw}: the file has {} lines", source.lines.len()));
    }
    // A line of the comment above a declaration is that declaration's.
    let commented = source
        .symbols
        .iter()
        .position(|s| s.doc_line <= line && line < s.start_line);
    let around = commented.or_else(|| enclosing(&source.symbols, line));
    if let Some(member) =
        around.filter(|index| !source.symbols[*index].container || commented.is_some())
    {
        return Outcome::Found(vec![source.symbol_piece(member, args)]);
    }
    let functions = source.functions();
    let function = functions
        .iter()
        .filter(|f| f.start_line <= line && line <= f.end_line)
        .min_by_key(|f| f.end_line - f.start_line);
    if let Some(f) = function {
        let piece = source.piece(f.start_line, f.end_line, f.start_line, f.name.clone(), args);
        return Outcome::Found(vec![piece]);
    }
    let title = match around {
        Some(index) => format!("in {}, in no member of it", source.symbols[index].signature),
        None => "in no declaration".to_string(),
    };
    let from = line.saturating_sub(AROUND).max(1);
    Outcome::Found(vec![source.piece(from, line + AROUND, from, title, args)])
}

/// Every declaration named `name` in the source files under `--in`: the
/// first few printed, the rest listed after the last one printed.
fn by_name(raw: &str, name: &str, args: &Args, settings: &Settings) -> Outcome {
    let dir = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    if !dir.is_dir() {
        return Outcome::Missing(format!("{raw}: --in {} is not a folder", dir.display()));
    }
    let wanted = last_part(name);
    let sources = declaring(name, wanted, &dir, settings);
    let all: Vec<(&Source, usize)> = sources
        .iter()
        .flat_map(|(source, found)| found.iter().map(move |index| (source, *index)))
        .collect();
    let mut pieces: Vec<Piece> = all
        .iter()
        .take(DEFINITIONS)
        .map(|(source, index)| source.symbol_piece(*index, args))
        .collect();
    let Some(last) = pieces.last_mut() else {
        return Outcome::Missing(format!(
            "{raw}: no declaration with this name under {}; `dgf refs {wanted}` lists where the name is used",
            crate::display_path(&dir.to_string_lossy())
        ));
    };
    let listed: Vec<String> = all
        .iter()
        .skip(DEFINITIONS)
        .map(|(source, index)| {
            let symbol = &source.symbols[*index];
            let lines = range(symbol.start_line, symbol.end_line);
            format!("{}:{lines}  {}", source.path, symbol.signature)
        })
        .collect();
    if !listed.is_empty() {
        let count = listed.len();
        last.lines
            .push(format!("... {count} more declarations named {wanted}:"));
        last.lines.extend(listed);
    }
    Outcome::Found(pieces)
}

/// The source files under `dir` that declare `name`, by path, each with
/// the declarations in it.
fn declaring(
    name: &str,
    wanted: &str,
    dir: &Path,
    settings: &Settings,
) -> Vec<(Source, Vec<usize>)> {
    let files = discover::discover(&[dir.to_path_buf()], settings.excludes.clone());
    let mut sources: Vec<(Source, Vec<usize>)> = files
        .par_iter()
        .filter_map(|file| {
            let text = std::fs::read_to_string(file).ok()?;
            if !text.contains(wanted) {
                return None;
            }
            let source = Source::load(file, &settings.cwd).ok()?;
            let found = source.named(name);
            (!found.is_empty()).then_some((source, found))
        })
        .collect();
    sources.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    sources
}

/// The heading of a piece and its first `given` lines.
fn render(piece: &Piece, given: usize) -> String {
    let mut text = format!("{}:{}", piece.path, range(piece.start, piece.end));
    if !piece.title.is_empty() {
        text.push_str(&format!("  {}", piece.title));
    }
    text.push('\n');
    for line in piece.lines.iter().take(given) {
        text.push_str(line);
        text.push('\n');
    }
    let cut = piece.lines.len().saturating_sub(given);
    if cut > 0 {
        let next = piece
            .lines
            .get(given)
            .and_then(|line| line.split('\t').next())
            .and_then(|number| number.parse::<usize>().ok())
            .unwrap_or(piece.start);
        let rest = range(next, piece.end.max(next));
        text.push_str(&format!(
            "... {cut} more lines; `dgf show {}:{rest}` prints them, or raise --max-lines\n",
            piece.path
        ));
    }
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }

    fn args(extra: &[&str]) -> Args {
        let mut line = vec!["dgf", "x"];
        line.extend(extra);
        Cli::parse_from(line).args
    }

    fn source(text: &str, language: Language) -> Source {
        Source {
            path: "src/Cart.cs".to_string(),
            lines: text.lines().map(str::to_string).collect(),
            symbols: outline(text, language).unwrap(),
            text: text.to_string(),
            language,
        }
    }

    const CART: &str = "\
namespace Shop;

public class Cart
{
    private int _count;

    /// <summary>Adds one.</summary>
    public void Add(Item item)
    {
        Local();
        void Local() { _count++; }
    }

    public void Add(Item item, int times) { }
}
";

    #[test]
    fn targets_split_at_the_colon_after_the_file() {
        let exists = |path: &str| path == r"D:\repo\src\A.cs" || path == "lib.rs";
        let target = |raw: &str| parse(raw, exists);
        assert_eq!(
            target(r"D:\repo\src\A.cs:Handle"),
            Target {
                file: Some(r"D:\repo\src\A.cs".into()),
                want: Some(Want::Name("Handle".into())),
            }
        );
        assert_eq!(
            target("lib.rs:Cart::total").want,
            Some(Want::Name("Cart::total".into()))
        );
        assert_eq!(target("lib.rs:120").want, Some(Want::Line(120)));
        assert_eq!(target("lib.rs:40-12").want, Some(Want::Lines(12, 40)));
        // A file that is not there is still a file.
        assert_eq!(target("gone/B.ts:load").file, Some("gone/B.ts".into()));
        assert_eq!(target("lib.rs").want, None);
        let bare = target("Cart::total");
        assert_eq!(bare.file, None);
        assert_eq!(bare.want, Some(Want::Name("Cart::total".into())));
    }

    fn found(outcome: Outcome) -> Vec<Piece> {
        match outcome {
            Outcome::Found(pieces) => pieces,
            Outcome::Missing(message) => panic!("{message}"),
        }
    }

    fn named(name: &str, extra: &[&str]) -> Vec<Piece> {
        let source = source(CART, Language::CSharp);
        let args = args(extra);
        source
            .named(name)
            .iter()
            .map(|index| source.symbol_piece(*index, &args))
            .collect()
    }

    #[test]
    fn a_name_gives_every_overload_with_numbered_lines() {
        let pieces = named("Add", &[]);
        assert_eq!(pieces.len(), 2);
        assert_eq!((pieces[0].start, pieces[0].end), (8, 12));
        assert_eq!(pieces[0].lines[0], "8\t    public void Add(Item item)");
        assert_eq!(pieces[0].lines.len(), 5);
        assert_eq!(
            render(&pieces[1], 10),
            "src/Cart.cs:14  public void Add(Item item, int times) { }\n14\t    public void Add(Item item, int times) { }\n\n"
        );
        assert_eq!(named("Cart.Add", &[]).len(), 2);
        assert!(named("Other.Add", &[]).is_empty());
    }

    #[test]
    fn signature_prints_the_comment_and_the_first_line() {
        let pieces = named("Add", &["--signature"]);
        assert_eq!(
            pieces[0].lines,
            [
                "7\t    /// <summary>Adds one.</summary>",
                "8\t    public void Add(Item item)"
            ]
        );
        let wider = named("_count", &["--context", "2"]);
        assert_eq!(wider[0].lines.len(), 5);
        assert!(wider[0].lines[0].starts_with("3\t"));
    }

    #[test]
    fn a_line_gives_the_member_around_it_and_a_gap_gives_its_surroundings() {
        let source = source(CART, Language::CSharp);
        let args = args(&[]);
        let member = found(at_line("x", &source, 11, &args));
        assert_eq!((member[0].start, member[0].end), (8, 12));
        // The comment above `Add`.
        let commented = found(at_line("x", &source, 7, &args));
        assert_eq!((commented[0].start, commented[0].end), (8, 12));
        let gap = found(at_line("x", &source, 6, &args));
        assert_eq!(gap[0].title, "in public class Cart, in no member of it");
        assert_eq!(gap[0].lines.len(), CART.lines().count());
        assert!(matches!(
            at_line("x", &source, 99, &args),
            Outcome::Missing(message) if message == "x: the file has 15 lines"
        ));
    }

    #[test]
    fn a_function_inside_a_call_is_found_by_its_line() {
        let spec = "\
describe('cart', () => {
  it('adds', () => {
    expect(add(1)).toBe(2);
  });
});
";
        let source = source(spec, Language::TypeScript);
        let piece = found(at_line("x", &source, 3, &args(&[])));
        assert_eq!((piece[0].start, piece[0].end), (2, 4));
    }

    #[test]
    fn a_missing_name_lists_the_nearest_ones() {
        let source = source(CART, Language::CSharp);
        assert_eq!(
            missing_name("src/Cart.cs:Ad", "Ad", &source),
            "src/Cart.cs:Ad: not found; nearest: Add (8-12)"
        );
        assert_eq!(
            missing_name("src/Cart.cs:Zebra", "Zebra", &source),
            "src/Cart.cs:Zebra: not found; `dgf outline src/Cart.cs` lists what is in the file"
        );
    }

    #[test]
    fn a_cut_piece_says_how_to_get_the_rest() {
        let pieces = named("Add", &[]);
        let text = render(&pieces[0], 2);
        assert_eq!(text.lines().count(), 5);
        assert!(
            text.contains(
                "... 3 more lines; `dgf show src/Cart.cs:10-12` prints them, or raise --max-lines"
            ),
            "{text}"
        );
    }
}
