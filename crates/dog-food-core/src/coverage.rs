//! Line coverage from the reports test tools already write: Cobertura XML
//! (coverlet for .NET, coverage.py, many others), OpenCover XML (coverlet,
//! OpenCover) and LCOV (Jest, Vitest, c8, cargo-llvm-cov). dgf never runs
//! tests; it only reads these files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// Hit counts per line number, for one source file.
pub type Lines = HashMap<usize, u64>;

#[derive(Debug, Default)]
pub struct Coverage {
    /// Source path from the report (as lower-case path components) and its lines.
    files: Vec<(Vec<String>, Lines)>,
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
            parse_lcov(&text)
        } else if text.contains("<CoverageSession") {
            parse_opencover(&text).map_err(|err| format!("{}: {err}", path.display()))?
        } else {
            parse_cobertura(&text).map_err(|err| format!("{}: {err}", path.display()))?
        };
        for (file, lines) in files {
            self.merge(&file, lines);
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

    /// The report entry for `path`: the one sharing the longest run of trailing
    /// path components, since reports use absolute or project-relative paths.
    pub fn for_file(&self, path: &Path) -> Option<&Lines> {
        let wanted = components(path);
        self.files
            .iter()
            .map(|(key, lines)| (shared_suffix(key, &wanted), lines))
            .filter(|(shared, _)| *shared > 0)
            .max_by_key(|(shared, _)| *shared)
            .map(|(_, lines)| lines)
    }
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
    let mut files: Vec<(String, Lines)> = Vec::new();
    for line in text.lines() {
        if let Some(file) = line.strip_prefix("SF:") {
            files.push((file.trim().to_string(), Lines::new()));
        } else if let (Some(data), Some((_, lines))) = (line.strip_prefix("DA:"), files.last_mut())
        {
            if let Some((number, hits)) = lcov_line(data) {
                *lines.entry(number).or_insert(0) += hits;
            }
        }
    }
    files
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
    files: Vec<(String, Lines)>,
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

    fn finish(mut self) -> Vec<(String, Lines)> {
        if let [source] = self.sources.as_slice() {
            for (file, _) in &mut self.files {
                *file = Path::new(source)
                    .join(&*file)
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

fn cobertura_tag(tag: &BytesStart, files: &mut Vec<(String, Lines)>) {
    match tag.name().as_ref() {
        "class" => {
            if let Some(file) = attribute(tag, "filename") {
                files.push((file, Lines::new()));
            }
        }
        "line" => {
            let number = attribute(tag, "number").and_then(|n| n.parse::<usize>().ok());
            let hits = attribute(tag, "hits").and_then(|h| h.parse::<u64>().ok());
            if let (Some(number), Some(hits), Some((_, lines))) = (number, hits, files.last_mut()) {
                // A line can appear under both <methods> and <lines>; keep the higher count.
                let entry = lines.entry(number).or_insert(0);
                *entry = (*entry).max(hits);
            }
        }
        _ => {}
    }
}

/// `<File uid="…" fullPath="…"/>` entries, and `<SequencePoint vc="…"
/// sl="…" el="…" fileid="…"/>` with the hit count of lines `sl..=el`. File
/// ids are numbered per `<Module>`.
pub fn parse_opencover(text: &str) -> Result<Vec<(String, Lines)>, String> {
    let mut state = OpenCover::default();
    each_event(text, "OpenCover", |event| {
        if let Event::Start(tag) | Event::Empty(tag) = event {
            state.tag(&tag);
        }
    })?;
    Ok(state.finish())
}

/// What has been read so far from an OpenCover report.
#[derive(Default)]
struct OpenCover {
    module: usize,
    paths: HashMap<(usize, String), String>,
    lines: HashMap<(usize, String), Lines>,
}

impl OpenCover {
    fn tag(&mut self, tag: &BytesStart) {
        match tag.name().as_ref() {
            "Module" => self.module += 1,
            "File" => {
                if let (Some(uid), Some(path)) = (attribute(tag, "uid"), attribute(tag, "fullPath"))
                {
                    self.paths.insert((self.module, uid), path);
                }
            }
            "SequencePoint" => self.sequence_point(tag),
            _ => {}
        }
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

    fn finish(self) -> Vec<(String, Lines)> {
        let paths = self.paths;
        self.lines
            .into_iter()
            .filter_map(|(key, lines)| paths.get(&key).map(|path| (path.clone(), lines)))
            .collect()
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
