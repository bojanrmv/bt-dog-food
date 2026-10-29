//! Line coverage from the reports test tools already write: Cobertura XML
//! (coverlet for .NET, coverage.py, many others), OpenCover XML (coverlet,
//! OpenCover) and LCOV (Jest, Vitest, c8, cargo-llvm-cov). dgf never runs
//! tests; it only reads these files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

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
    let mut reader = Reader::from_str(text);
    let mut state = Cobertura::default();
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("not valid Cobertura XML: {err}"))?;
        if !state.read(event) {
            break;
        }
    }
    Ok(state.finish())
}

/// What has been read so far from a Cobertura report.
#[derive(Default)]
struct Cobertura {
    files: Vec<(String, Lines)>,
    sources: Vec<String>,
    in_source: bool,
}

impl Cobertura {
    /// Takes one XML event; `false` at the end of the document.
    fn read(&mut self, event: Event) -> bool {
        match event {
            Event::Start(tag) | Event::Empty(tag) => {
                self.in_source = tag.name().as_ref() == b"source";
                cobertura_tag(&tag, &mut self.files);
            }
            Event::Text(text) if self.in_source => self.sources.extend(source_text(&text)),
            Event::End(_) => self.in_source = false,
            Event::Eof => return false,
            _ => {}
        }
        true
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

fn source_text(text: &quick_xml::events::BytesText) -> Option<String> {
    let text = text.unescape().ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn cobertura_tag(tag: &BytesStart, files: &mut Vec<(String, Lines)>) {
    match tag.name().as_ref() {
        b"class" => {
            if let Some(file) = attribute(tag, b"filename") {
                files.push((file, Lines::new()));
            }
        }
        b"line" => {
            let number = attribute(tag, b"number").and_then(|n| n.parse::<usize>().ok());
            let hits = attribute(tag, b"hits").and_then(|h| h.parse::<u64>().ok());
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
    let mut reader = Reader::from_str(text);
    let mut state = OpenCover::default();
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("not valid OpenCover XML: {err}"))?;
        match event {
            Event::Start(tag) | Event::Empty(tag) => state.tag(&tag),
            Event::Eof => break,
            _ => {}
        }
    }
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
            b"Module" => self.module += 1,
            b"File" => {
                if let (Some(uid), Some(path)) =
                    (attribute(tag, b"uid"), attribute(tag, b"fullPath"))
                {
                    self.paths.insert((self.module, uid), path);
                }
            }
            b"SequencePoint" => self.sequence_point(tag),
            _ => {}
        }
    }

    fn sequence_point(&mut self, tag: &BytesStart) {
        let number = |name: &[u8]| attribute(tag, name).and_then(|v| v.parse::<usize>().ok());
        let (Some(hits), Some(start), Some(end), Some(file)) = (
            number(b"vc"),
            number(b"sl"),
            number(b"el"),
            attribute(tag, b"fileid"),
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

fn attribute(tag: &BytesStart, name: &[u8]) -> Option<String> {
    tag.attributes()
        .flatten()
        .find(|attr| attr.key.as_ref() == name)
        .and_then(|attr| attr.unescape_value().ok().map(|v| v.into_owned()))
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
