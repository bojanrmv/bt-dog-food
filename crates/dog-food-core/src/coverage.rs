//! Line coverage from the reports test tools already write: Cobertura XML
//! (coverlet for .NET, coverage.py, many others), OpenCover XML (coverlet,
//! OpenCover) and LCOV (Jest, Vitest, c8, cargo-llvm-cov). dgf never runs
//! tests; it only reads these files.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// Hit counts per line number, for one source file.
pub type Lines = HashMap<usize, u64>;

/// The hit count of every branch that starts on a line, per line number,
/// for the lines that branch.
pub type Branches = HashMap<usize, Vec<u64>>;

/// What one report says about one source file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileCoverage {
    pub file: String,
    pub lines: Lines,
    pub branches: Branches,
}

impl FileCoverage {
    fn new(file: String) -> Self {
        FileCoverage {
            file,
            ..FileCoverage::default()
        }
    }
}

#[derive(Debug, Default)]
pub struct Coverage {
    /// Source path from the report (as lower-case path components) and its lines.
    files: Vec<(Vec<String>, Lines)>,
    /// The same for the lines that branch.
    branches: Vec<(Vec<String>, Branches)>,
    /// The coverage reports that were read.
    pub sources: Vec<PathBuf>,
}

impl Coverage {
    /// Reads every given report; a folder is searched for report files.
    pub fn load(paths: &[PathBuf]) -> Result<Coverage, String> {
        let mut coverage = Coverage::default();
        for path in paths {
            for report in reports_in(path)? {
                coverage.read(&report)?;
            }
        }
        if coverage.sources.is_empty() {
            return Err("no coverage report found (looked for *.cobertura.xml, cobertura.xml, *.opencover.xml, lcov.info, *.lcov)".into());
        }
        Ok(coverage)
    }

    fn read(&mut self, path: &Path) -> Result<(), String> {
        let text =
            std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
        let files = if is_lcov(path, &text) {
            read_lcov(&text)
        } else if text.contains("<CoverageSession") {
            read_opencover(&text).map_err(|err| format!("{}: {err}", path.display()))?
        } else {
            read_cobertura(&text).map_err(|err| format!("{}: {err}", path.display()))?
        };
        for found in files {
            self.merge(&found.file, found.lines);
            self.merge_branches(&found.file, found.branches);
        }
        self.sources.push(path.to_path_buf());
        Ok(())
    }

    /// Several reports may cover the same file (one per test project): hits add up.
    fn merge(&mut self, file: &str, lines: Lines) {
        let key = components(Path::new(file));
        match self.files.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, existing)) => {
                for (line, hits) in lines {
                    *existing.entry(line).or_insert(0) += hits;
                }
            }
            None => self.files.push((key, lines)),
        }
    }

    /// Branch hits of the same file add up branch by branch. A report that
    /// only counts them (Cobertura's `1/2`) does not say which were taken,
    /// so two such reports give the higher count, not the union.
    fn merge_branches(&mut self, file: &str, branches: Branches) {
        if branches.is_empty() {
            return;
        }
        let key = components(Path::new(file));
        let found = self
            .branches
            .iter_mut()
            .find(|(existing, _)| *existing == key);
        match found {
            Some((_, existing)) => {
                for (line, hits) in branches {
                    add_branches(existing.entry(line).or_default(), &hits, |a, b| a + b);
                }
            }
            None => self.branches.push((key, branches)),
        }
    }

    /// The report entry for `path`: the one sharing the longest run of trailing
    /// path components, since reports use absolute or project-relative paths.
    pub fn for_file(&self, path: &Path) -> Option<&Lines> {
        best_match(&self.files, path)
    }

    /// The lines and branches of `path` when a report surely means this
    /// file: the report's whole path ends `path`, or the paths share all of
    /// `relative`, the file's path inside its repository. A file that only
    /// shares its name with one in the reports (`Program.cs`) is not it.
    pub fn of(&self, path: &Path, relative: &Path) -> Option<(&Lines, Option<&Branches>)> {
        let wanted = components(path);
        let whole = components(relative).len();
        let (key, lines) = self
            .files
            .iter()
            .map(|(key, lines)| (shared_suffix(key, &wanted), key, lines))
            .filter(|(shared, key, _)| *shared > 0 && (*shared == key.len() || *shared >= whole))
            .max_by_key(|(shared, _, _)| *shared)
            .map(|(_, key, lines)| (key, lines))?;
        let branches = self.branches.iter().find(|(other, _)| other == key);
        Some((lines, branches.map(|(_, found)| found)))
    }

    /// The branches of `path`, matched as [`Coverage::for_file`] does; `None`
    /// when no report has a branch in it.
    pub fn branches_for(&self, path: &Path) -> Option<&Branches> {
        best_match(&self.branches, path)
    }
}

fn best_match<'a, T>(files: &'a [(Vec<String>, T)], path: &Path) -> Option<&'a T> {
    let wanted = components(path);
    files
        .iter()
        .map(|(key, found)| (shared_suffix(key, &wanted), found))
        .filter(|(shared, _)| *shared > 0)
        .max_by_key(|(shared, _)| *shared)
        .map(|(_, found)| found)
}

/// Combines the hits of `more` into `hits`, branch by branch.
fn add_branches(hits: &mut Vec<u64>, more: &[u64], combine: impl Fn(u64, u64) -> u64) {
    if hits.len() < more.len() {
        hits.resize(more.len(), 0);
    }
    for (hit, other) in hits.iter_mut().zip(more) {
        *hit = combine(*hit, *other);
    }
}

/// How many branches start in `start..=end`, and how many of them ran:
/// `(covered, total)`.
pub fn branch_counts(branches: &Branches, start: usize, end: usize) -> (usize, usize) {
    branches
        .iter()
        .filter(|(line, _)| (start..=end).contains(*line))
        .fold((0, 0), |(covered, total), (_, hits)| {
            let taken = hits.iter().filter(|hit| **hit > 0).count();
            (covered + taken, total + hits.len())
        })
}

/// Covered share of the instrumented lines in `start..=end`, from 0 to 1;
/// `None` when the report has no instrumented line there.
pub fn ratio(lines: &Lines, start: usize, end: usize) -> Option<f64> {
    let (covered, total) = (start..=end)
        .filter_map(|line| lines.get(&line))
        .fold((0u32, 0u32), |(covered, total), hits| {
            (covered + u32::from(*hits > 0), total + 1)
        });
    (total > 0).then(|| f64::from(covered) / f64::from(total))
}

/// Change Risk Anti-Patterns: complexity² × (1 − coverage)³ + complexity.
pub fn crap(complexity: u32, coverage: f64) -> f64 {
    let c = f64::from(complexity);
    c * c * (1.0 - coverage).powi(3) + c
}

/// Lower-case path parts without drive or `.`; both `/` and `\` separate,
/// because a report written on Windows may be read on Linux and back.
fn components(path: &Path) -> Vec<String> {
    path.to_string_lossy()
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && !part.ends_with(':'))
        .map(str::to_lowercase)
        .collect()
}

fn shared_suffix(a: &[String], b: &[String]) -> usize {
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

const REPORT_NAMES: &[&str] = &["cobertura.xml", "coverage.xml", "lcov.info"];

fn is_report(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    REPORT_NAMES.contains(&name.as_str())
        || name.ends_with(".cobertura.xml")
        || name.ends_with(".opencover.xml")
        || name.ends_with(".lcov")
}

/// `path` itself, or every report file below it when it is a folder.
fn reports_in(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(format!("coverage report not found: {}", path.display()));
    }
    let mut found: Vec<PathBuf> = ignore::WalkBuilder::new(path)
        .standard_filters(false)
        .build()
        .flatten()
        .map(|entry| entry.into_path())
        .filter(|file| file.is_file() && is_report(file))
        .collect();
    found.sort();
    Ok(found)
}

fn is_lcov(path: &Path, text: &str) -> bool {
    let lcov_name = path
        .extension()
        .is_some_and(|ext| ext == "info" || ext == "lcov");
    lcov_name || text.lines().any(|line| line.starts_with("SF:"))
}

/// `SF:<file>`, then `DA:<line>,<hits>` lines, until `end_of_record`.
pub fn parse_lcov(text: &str) -> Vec<(String, Lines)> {
    without_branches(read_lcov(text))
}

fn without_branches(files: Vec<FileCoverage>) -> Vec<(String, Lines)> {
    files.into_iter().map(|f| (f.file, f.lines)).collect()
}

/// One branch in an LCOV record: its line, block and branch.
type LcovArm = (usize, String, String);

/// One `SF:` record while it is read.
struct LcovRecord {
    found: FileCoverage,
    arms: BTreeMap<LcovArm, u64>,
}

impl LcovRecord {
    fn new(file: &str) -> Self {
        LcovRecord {
            found: FileCoverage::new(file.trim().to_string()),
            arms: BTreeMap::new(),
        }
    }

    /// Takes a `DA:` or `BRDA:` line; other lines say nothing dgf reads.
    fn read(&mut self, line: &str) {
        if let Some((number, hits)) = line.strip_prefix("DA:").and_then(lcov_line) {
            *self.found.lines.entry(number).or_insert(0) += hits;
        } else if let Some((arm, hits)) = line.strip_prefix("BRDA:").and_then(lcov_branch) {
            *self.arms.entry(arm).or_insert(0) += hits;
        }
    }

    fn finish(mut self) -> FileCoverage {
        for ((line, _, _), hits) in self.arms {
            self.found.branches.entry(line).or_default().push(hits);
        }
        self.found
    }
}

/// [`parse_lcov`] with the branches: `BRDA:<line>,<block>,<branch>,<taken>`,
/// where `-` is a branch whose line never ran.
pub fn read_lcov(text: &str) -> Vec<FileCoverage> {
    let mut records: Vec<LcovRecord> = Vec::new();
    for line in text.lines() {
        match (line.strip_prefix("SF:"), records.last_mut()) {
            (Some(file), _) => records.push(LcovRecord::new(file)),
            (None, Some(record)) => record.read(line),
            (None, None) => {}
        }
    }
    records.into_iter().map(LcovRecord::finish).collect()
}

fn lcov_branch(data: &str) -> Option<(LcovArm, u64)> {
    let mut parts = data.split(',');
    let line = parts.next()?.trim().parse().ok()?;
    let block = parts.next()?.trim().to_string();
    let branch = parts.next()?.trim().to_string();
    let hits = parts.next()?.trim().parse().unwrap_or(0);
    Some(((line, block, branch), hits))
}

fn lcov_line(data: &str) -> Option<(usize, u64)> {
    let mut parts = data.split(',');
    let number = parts.next()?.trim().parse().ok()?;
    let hits = parts.next()?.trim().parse().ok()?;
    Some((number, hits))
}

/// `<class filename="…">` elements with `<line number="…" hits="…"/>` inside.
/// Relative file names are joined to the report's single `<source>`, if any.
pub fn parse_cobertura(text: &str) -> Result<Vec<(String, Lines)>, String> {
    read_cobertura(text).map(without_branches)
}

/// [`parse_cobertura`] with the branches: `condition-coverage="50% (1/2)"`
/// on a line says how many start there and how many ran, not which.
pub fn read_cobertura(text: &str) -> Result<Vec<FileCoverage>, String> {
    let mut state = Cobertura::default();
    each_event(text, "Cobertura", |event| state.read(event))?;
    Ok(state.finish())
}

/// Hands every event of the XML in `text` to `each`; `format` names the
/// report when it is not valid XML.
fn each_event<'a>(
    text: &'a str,
    format: &str,
    mut each: impl FnMut(Event<'a>),
) -> Result<(), String> {
    let mut reader = Reader::from_str(text);
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("not valid {format} XML: {err}"))?;
        match event {
            Event::Eof => return Ok(()),
            event => each(event),
        }
    }
}

/// What has been read so far from a Cobertura report.
#[derive(Default)]
struct Cobertura {
    files: Vec<FileCoverage>,
    sources: Vec<String>,
    /// The text so far of the `<source>` being read.
    source: Option<String>,
}

impl Cobertura {
    /// Takes one XML event.
    fn read(&mut self, event: Event) {
        match event {
            Event::Start(tag) | Event::Empty(tag) => {
                self.end_source();
                self.source = (tag.name().as_ref() == "source").then(String::new);
                cobertura_tag(&tag, &mut self.files);
            }
            Event::Text(text) => self.source_text(&text.xml10_content()),
            // The reader hands `&amp;` in a path over on its own.
            Event::GeneralRef(entity) => self.source_text(&referenced(&entity)),
            Event::End(_) => self.end_source(),
            _ => {}
        }
    }

    fn source_text(&mut self, text: &str) {
        if let Some(source) = &mut self.source {
            source.push_str(text);
        }
    }

    fn end_source(&mut self) {
        let source = self.source.take().unwrap_or_default();
        if !source.trim().is_empty() {
            self.sources.push(source.trim().to_string());
        }
    }

    fn finish(mut self) -> Vec<FileCoverage> {
        if let [source] = self.sources.as_slice() {
            for found in &mut self.files {
                found.file = Path::new(source)
                    .join(&found.file)
                    .to_string_lossy()
                    .into_owned();
            }
        }
        self.files
    }
}

/// What `&name;` or `&#number;` stands for: a character, or one of the
/// five names XML knows. Any other name stands for nothing here.
fn referenced(entity: &BytesRef) -> String {
    match entity.resolve_char_ref() {
        Ok(Some(character)) => character.to_string(),
        _ => quick_xml::escape::resolve_predefined_entity(entity)
            .unwrap_or_default()
            .to_string(),
    }
}

fn cobertura_tag(tag: &BytesStart, files: &mut Vec<FileCoverage>) {
    match tag.name().as_ref() {
        "class" => {
            if let Some(file) = attribute(tag, "filename") {
                files.push(FileCoverage::new(file));
            }
        }
        "line" => {
            if let Some(found) = files.last_mut() {
                cobertura_line(tag, found);
            }
        }
        _ => {}
    }
}

fn cobertura_line(tag: &BytesStart, found: &mut FileCoverage) {
    let number = attribute(tag, "number").and_then(|n| n.parse::<usize>().ok());
    let hits = attribute(tag, "hits").and_then(|h| h.parse::<u64>().ok());
    let (Some(number), Some(hits)) = (number, hits) else {
        return;
    };
    // A line can appear under both <methods> and <lines>; keep the higher count.
    let entry = found.lines.entry(number).or_insert(0);
    *entry = (*entry).max(hits);
    let counted = attribute(tag, "condition-coverage");
    if let Some((covered, total)) = counted.as_deref().and_then(condition_coverage) {
        let taken: Vec<u64> = (0..total).map(|arm| u64::from(arm < covered)).collect();
        add_branches(found.branches.entry(number).or_default(), &taken, u64::max);
    }
}

/// `50% (1/2)`: 1 of 2.
fn condition_coverage(text: &str) -> Option<(usize, usize)> {
    let counts = text.split_once('(')?.1.strip_suffix(')')?;
    let (covered, total) = counts.split_once('/')?;
    Some((covered.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// `<File uid="…" fullPath="…"/>` entries, and `<SequencePoint vc="…"
/// sl="…" el="…" fileid="…"/>` with the hit count of lines `sl..=el`. File
/// ids are numbered per `<Module>`.
pub fn parse_opencover(text: &str) -> Result<Vec<(String, Lines)>, String> {
    read_opencover(text).map(without_branches)
}

/// [`parse_opencover`] with the branches: `<BranchPoint vc="..." sl="..."
/// offset="..." path="..." fileid="..."/>`, one per branch.
pub fn read_opencover(text: &str) -> Result<Vec<FileCoverage>, String> {
    let mut state = OpenCover::default();
    each_event(text, "OpenCover", |event| {
        if let Event::Start(tag) | Event::Empty(tag) = event {
            state.tag(&tag);
        }
    })?;
    Ok(state.finish())
}

/// The hits of each branch of a file: its line, offset and path.
type OpenCoverArms = BTreeMap<(usize, usize, usize), u64>;

/// What has been read so far from an OpenCover report.
#[derive(Default)]
struct OpenCover {
    module: usize,
    paths: HashMap<(usize, String), String>,
    lines: HashMap<(usize, String), Lines>,
    /// Per file, the hits of its branches.
    branches: HashMap<(usize, String), OpenCoverArms>,
}

impl OpenCover {
    fn tag(&mut self, tag: &BytesStart) {
        match tag.name().as_ref() {
            "Module" => self.module += 1,
            "File" => self.file(tag),
            "SequencePoint" => self.sequence_point(tag),
            "BranchPoint" => self.branch_point(tag),
            _ => {}
        }
    }

    fn file(&mut self, tag: &BytesStart) {
        if let (Some(uid), Some(path)) = (attribute(tag, "uid"), attribute(tag, "fullPath")) {
            self.paths.insert((self.module, uid), path);
        }
    }

    fn branch_point(&mut self, tag: &BytesStart) {
        let number = |name: &str| attribute(tag, name).and_then(|v| v.parse::<usize>().ok());
        let (Some(hits), Some(line), Some(file)) =
            (number("vc"), number("sl"), attribute(tag, "fileid"))
        else {
            return;
        };
        let arm = (
            line,
            number("offset").unwrap_or(0),
            number("path").unwrap_or(0),
        );
        let arms = self.branches.entry((self.module, file)).or_default();
        let entry = arms.entry(arm).or_insert(0);
        *entry = (*entry).max(hits as u64);
    }

    fn sequence_point(&mut self, tag: &BytesStart) {
        let number = |name: &str| attribute(tag, name).and_then(|v| v.parse::<usize>().ok());
        let (Some(hits), Some(start), Some(end), Some(file)) = (
            number("vc"),
            number("sl"),
            number("el"),
            attribute(tag, "fileid"),
        ) else {
            return;
        };
        let lines = self.lines.entry((self.module, file)).or_default();
        for line in start..=end.max(start) {
            let entry = lines.entry(line).or_insert(0);
            *entry = (*entry).max(hits as u64);
        }
    }

    fn finish(mut self) -> Vec<FileCoverage> {
        let mut files = Vec::new();
        for (key, lines) in self.lines {
            let Some(path) = self.paths.get(&key) else {
                continue;
            };
            let mut found = FileCoverage::new(path.clone());
            found.lines = lines;
            for ((line, _, _), hits) in self.branches.remove(&key).unwrap_or_default() {
                found.branches.entry(line).or_default().push(hits);
            }
            files.push(found);
        }
        files
    }
}

fn attribute(tag: &BytesStart, name: &str) -> Option<String> {
    let found = tag
        .attributes()
        .flatten()
        .find(|attr| attr.key.as_ref() == name)?;
    let value = found.normalized_value(XmlVersion::Implicit1_0).ok()?;
    Some(value.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COBERTURA: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<coverage line-rate="0.5" version="1.9">
  <sources><source>D:\shop\</source></sources>
  <packages><package name="Shop"><classes>
    <class name="Shop.Cart" filename="src\Cart.cs" line-rate="0.5">
      <methods><method name="Add"><lines><line number="10" hits="3" /></lines></method></methods>
      <lines>
        <line number="10" hits="3" />
        <line number="11" hits="0" />
        <line number="12" hits="1" branch="True" condition-coverage="50% (1/2)" />
        <line number="13" hits="0" />
      </lines>
    </class>
  </classes></package></packages>
</coverage>"#;

    const LCOV: &str = "TN:\nSF:src/checkout.ts\nDA:1,1\nDA:2,0\nDA:3,4\nend_of_record\nSF:src/other.ts\nDA:1,0\nend_of_record\n";

    #[test]
    fn reads_opencover_sequence_points_per_module() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<CoverageSession>
  <Modules>
    <Module hash="a">
      <Files><File uid="1" fullPath="C:\src\Shop\Pricing.cs" /></Files>
      <Classes><Class><Methods><Method><SequencePoints>
        <SequencePoint vc="3" uspid="1" sl="10" sc="9" el="11" ec="10" fileid="1" />
        <SequencePoint vc="0" uspid="2" sl="12" sc="9" el="12" ec="20" fileid="1" />
      </SequencePoints></Method></Methods></Class></Classes>
    </Module>
    <Module hash="b">
      <Files><File uid="1" fullPath="C:\src\Other\Unused.cs" /></Files>
      <Classes><Class><Methods><Method><SequencePoints>
        <SequencePoint vc="1" uspid="1" sl="5" sc="9" el="5" ec="20" fileid="1" />
      </SequencePoints></Method></Methods></Class></Classes>
    </Module>
  </Modules>
</CoverageSession>"#;
        let mut files = parse_opencover(xml).unwrap();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(files.len(), 2);
        let (path, lines) = &files[1];
        assert!(path.ends_with("Pricing.cs"), "{path}");
        assert_eq!(lines[&10], 3);
        assert_eq!(lines[&11], 3);
        assert_eq!(lines[&12], 0);
        assert_eq!(ratio(lines, 10, 12), Some(2.0 / 3.0));
        assert_eq!(files[0].1[&5], 1);
        assert!(is_report(Path::new("coverage.opencover.xml")));
    }

    #[test]
    fn reads_cobertura_lines_and_joins_the_source_folder() {
        let files = parse_cobertura(COBERTURA).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].0.ends_with("src\\Cart.cs") || files[0].0.ends_with("src/Cart.cs"));
        assert!(files[0].0.starts_with("D:"));
        assert_eq!(files[0].1.get(&10), Some(&3));
        assert_eq!(files[0].1.get(&11), Some(&0));
    }

    /// `R&D` is written `R&amp;D` (or with the character's number) in XML.
    #[test]
    fn a_folder_with_an_ampersand_in_its_name_is_read_whole() {
        let report = r#"<coverage>
  <sources><source>
    /work/R&amp;D &#x26; more/
  </source></sources>
  <packages><package><classes>
    <class filename="src/A&amp;B.cs"><lines><line number="1" hits="2"/></lines></class>
  </classes></package></packages>
</coverage>"#;
        let files = parse_cobertura(report).unwrap();
        let file = files[0].0.replace('\\', "/");
        assert_eq!(file, "/work/R&D & more/src/A&B.cs");
        assert_eq!(files[0].1.get(&1), Some(&2));
        // Two sources: the files stay as the report names them.
        let two = report.replace(
            "</source>",
            "</source><source>/other &unknown; one/</source>",
        );
        assert_eq!(parse_cobertura(&two).unwrap()[0].0, "src/A&B.cs");
    }

    #[test]
    fn reads_lcov_records() {
        let files = parse_lcov(LCOV);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "src/checkout.ts");
        assert_eq!(files[0].1.get(&3), Some(&4));
    }

    #[test]
    fn every_format_gives_the_branches_of_a_line() {
        let cobertura = read_cobertura(COBERTURA).unwrap();
        assert_eq!(cobertura[0].branches[&12], [1, 0]);
        assert_eq!(cobertura[0].branches.len(), 1);

        let lcov = read_lcov(
            "SF:a.ts\nDA:3,2\nBRDA:3,0,0,2\nBRDA:3,0,1,-\nBRDA:7,1,0,0\nBRDA:3,0,0,1\nend_of_record\n",
        );
        assert_eq!(lcov[0].branches[&3], [3, 0]);
        assert_eq!(lcov[0].branches[&7], [0]);
        assert_eq!(branch_counts(&lcov[0].branches, 1, 5), (1, 2));
        assert_eq!(branch_counts(&lcov[0].branches, 1, 9), (1, 3));
        assert_eq!(branch_counts(&lcov[0].branches, 8, 9), (0, 0));

        let opencover = read_opencover(
            r#"<CoverageSession><Modules><Module>
      <Files><File uid="1" fullPath="C:\src\A.cs" /></Files>
      <Classes><Class><Methods><Method>
        <SequencePoints><SequencePoint vc="3" sl="10" el="10" fileid="1" /></SequencePoints>
        <BranchPoints>
          <BranchPoint vc="3" uspid="1" ordinal="0" offset="8" sl="10" path="0" fileid="1" />
          <BranchPoint vc="0" uspid="2" ordinal="1" offset="8" sl="10" path="1" fileid="1" />
          <BranchPoint vc="1" uspid="3" ordinal="2" offset="30" sl="14" path="0" fileid="1" />
        </BranchPoints>
      </Method></Methods></Class></Classes>
    </Module></Modules></CoverageSession>"#,
        )
        .unwrap();
        assert_eq!(opencover[0].branches[&10], [3, 0]);
        assert_eq!(opencover[0].branches[&14], [1]);
    }

    #[test]
    fn merging_reports_adds_branch_hits_arm_by_arm() {
        let mut coverage = Coverage::default();
        coverage.merge_branches("src/a.ts", [(4, vec![1, 0])].into_iter().collect());
        coverage.merge_branches(
            "SRC/A.ts",
            [(4, vec![0, 2, 1]), (9, vec![0])].into_iter().collect(),
        );
        let branches = coverage.branches_for(Path::new("src/a.ts")).unwrap();
        assert_eq!(branches[&4], [1, 2, 1]);
        assert_eq!(branches[&9], [0]);
        assert!(coverage.branches_for(Path::new("src/b.ts")).is_none());
    }

    #[test]
    fn ratio_counts_only_instrumented_lines() {
        let lines: Lines = [(10, 3), (11, 0), (12, 1), (13, 0)].into_iter().collect();
        assert_eq!(ratio(&lines, 10, 13), Some(0.5));
        assert_eq!(ratio(&lines, 1, 9), None);
        assert_eq!(ratio(&lines, 12, 20), Some(0.5));
    }

    #[test]
    fn crap_matches_the_formula() {
        assert_eq!(crap(10, 1.0), 10.0);
        assert_eq!(crap(10, 0.0), 110.0);
        assert_eq!(crap(4, 0.5), 6.0);
    }

    #[test]
    fn files_match_by_longest_shared_path_suffix() {
        let mut coverage = Coverage::default();
        coverage.merge("D:/shop/src/Cart.cs", [(1, 1)].into_iter().collect());
        coverage.merge("D:/shop/tests/Cart.cs", [(1, 0)].into_iter().collect());
        let lines = coverage
            .for_file(Path::new("C:/ci/work/shop/src/Cart.cs"))
            .unwrap();
        assert_eq!(lines.get(&1), Some(&1));
        assert!(coverage.for_file(Path::new("src/Order.cs")).is_none());
    }

    #[test]
    fn a_file_that_only_shares_its_name_is_not_in_the_reports() {
        let mut coverage = Coverage::default();
        coverage.merge(
            "C:/ci/shop/tests/Program.cs",
            [(1, 1)].into_iter().collect(),
        );
        coverage.merge("projects/ops/src/app.ts", [(2, 0)].into_iter().collect());
        coverage.merge_branches(
            "projects/ops/src/app.ts",
            [(2, vec![0])].into_iter().collect(),
        );
        let of = |path: &str, relative: &str| coverage.of(Path::new(path), Path::new(relative));
        assert!(of("D:/shop/src/Program.cs", "src/Program.cs").is_none());
        // Another checkout of the same repository.
        assert!(of("D:/shop/tests/Program.cs", "tests/Program.cs").is_some());
        // A report with paths relative to a project folder.
        let (lines, branches) = of(
            "D:/web/frontend/projects/ops/src/app.ts",
            "frontend/projects/ops/src/app.ts",
        )
        .unwrap();
        assert_eq!(lines.get(&2), Some(&0));
        assert_eq!(branches.unwrap()[&2], [0]);
    }

    #[test]
    fn merging_reports_adds_hits() {
        let mut coverage = Coverage::default();
        coverage.merge("src/a.ts", [(1, 1), (2, 0)].into_iter().collect());
        coverage.merge("SRC/A.ts", [(2, 2)].into_iter().collect());
        assert_eq!(
            coverage.for_file(Path::new("src/a.ts")).unwrap().get(&2),
            Some(&2)
        );
    }

    #[test]
    fn folders_are_searched_for_reports() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("TestResults/0f3a/coverage.cobertura.xml");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, COBERTURA).unwrap();
        std::fs::write(dir.path().join("lcov.info"), LCOV).unwrap();
        std::fs::write(dir.path().join("notes.xml"), "<x/>").unwrap();
        let coverage = Coverage::load(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(coverage.sources.len(), 2);
        assert!(coverage.for_file(Path::new("src/checkout.ts")).is_some());
    }
}
