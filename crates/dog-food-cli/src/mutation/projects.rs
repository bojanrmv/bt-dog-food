//! The C# projects of a repository: which are test projects, what they
//! reference, and which test projects a mutation run uses with which filter.
//! Everything here reads the repository itself, before any sandbox exists.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::discover::absolute;

/// Which test projects and tests a run uses (`--test-filter`,
/// `test_projects`, `exclude_test_projects`).
#[derive(Debug, Default, Clone)]
pub struct TestSelection {
    /// "filter" for every test project, or "Project:filter" for one.
    pub filters: Vec<String>,
    /// Glob patterns of test projects to use; every test project when empty.
    pub include: Vec<String>,
    /// Glob patterns of test projects to leave out.
    pub exclude: Vec<String>,
}

/// A test project chosen for the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestProject {
    /// Relative to the repository root.
    pub path: PathBuf,
    /// The assembly its tests come from, which the collector reports.
    pub assembly: String,
    /// `dotnet test` filter for its tests.
    pub filter: Option<String>,
}

impl TestProject {
    /// The project's file name without `.csproj`, as filters name it.
    pub fn name(&self) -> String {
        stem(&self.path)
    }
}

/// Folders never searched for projects.
const SKIP: &[&str] = &[
    ".git",
    ".dgf",
    "bin",
    "obj",
    "node_modules",
    "TestResults",
    "target",
];

/// Every .csproj under `root`, as absolute paths.
pub fn all(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .filter_entry(|e| !SKIP.contains(&e.file_name().to_string_lossy().as_ref()))
        .build()
        .flatten()
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "csproj"))
        .map(|p| absolute(&p))
        .collect();
    found.sort();
    found
}

fn is_test(text: &str) -> bool {
    text.contains("Microsoft.NET.Test.Sdk") || text.contains("<IsTestProject>true")
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The test projects to run for mutants in `mutated` (absolute .csproj paths):
/// those the globs allow that reference a mutated project, each with its filter.
pub fn select(
    root: &Path,
    mutated: &HashSet<PathBuf>,
    selection: &TestSelection,
) -> Result<Vec<TestProject>, String> {
    let root = absolute(root);
    let tests: Vec<(PathBuf, String)> = all(&root)
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|text| (p, text)))
        .filter(|(_, text)| is_test(text))
        .collect();
    if tests.is_empty() {
        return Err(
            "no test project found (a .csproj that references Microsoft.NET.Test.Sdk)".into(),
        );
    }
    let allowed = Globs::new(selection)?;
    let candidates: Vec<&(PathBuf, String)> = tests
        .iter()
        .filter(|(p, _)| allowed.allows(&relative(&root, p)))
        .collect();
    if candidates.is_empty() {
        return Err("test_projects and exclude_test_projects leave no test project".into());
    }
    let relevant: Vec<&(PathBuf, String)> = candidates
        .iter()
        .copied()
        .filter(|(p, _)| reaches(p, mutated))
        .collect();
    let chosen = if relevant.is_empty() {
        candidates
    } else {
        relevant
    };
    let names: Vec<String> = tests.iter().map(|(p, _)| stem(p)).collect();
    let filters = Filters::parse(&selection.filters, &names);
    let projects: Vec<TestProject> = chosen
        .into_iter()
        .filter_map(|(path, text)| {
            let filter = filters.for_project(&stem(path))?;
            Some(TestProject {
                path: relative(&root, path),
                assembly: assembly_of(path, text),
                filter,
            })
        })
        .collect();
    if projects.is_empty() {
        return Err(
            "the test filters name none of the test projects that use the mutated code".into(),
        );
    }
    Ok(projects)
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

/// The include and exclude globs, matched against paths relative to the root.
struct Globs {
    include: Option<GlobSet>,
    exclude: GlobSet,
}

impl Globs {
    fn new(selection: &TestSelection) -> Result<Globs, String> {
        let include = (!selection.include.is_empty())
            .then(|| glob_set(&selection.include))
            .transpose()?;
        Ok(Globs {
            include,
            exclude: glob_set(&selection.exclude)?,
        })
    }

    fn allows(&self, path: &Path) -> bool {
        let text = path.to_string_lossy().replace('\\', "/");
        self.include.as_ref().is_none_or(|set| set.is_match(&text)) && !self.exclude.is_match(&text)
    }
}

fn glob_set(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|err| format!("invalid glob `{pattern}`: {err}"))?;
        builder.add(glob);
    }
    builder.build().map_err(|err| err.to_string())
}

/// Test filters, split into ones for every project and ones for one project.
struct Filters {
    global: Vec<String>,
    per_project: Vec<(String, String)>,
}

impl Filters {
    /// "Name:filter" is for one project when Name is a test project's name;
    /// anything else is a filter for every project.
    fn parse(raw: &[String], names: &[String]) -> Filters {
        let mut filters = Filters {
            global: Vec::new(),
            per_project: Vec::new(),
        };
        for text in raw {
            match text.split_once(':') {
                Some((name, filter)) if names.iter().any(|n| n == name.trim()) => filters
                    .per_project
                    .push((name.trim().to_string(), filter.trim().to_string())),
                _ => filters.global.push(text.trim().to_string()),
            }
        }
        filters
    }

    /// `None` when the project does not run; `Some(None)` when it runs unfiltered.
    fn for_project(&self, name: &str) -> Option<Option<String>> {
        let own: Vec<&String> = self
            .per_project
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, f)| f)
            .collect();
        if !own.is_empty() {
            return Some(Some(any_of(&own)));
        }
        if !self.per_project.is_empty() && self.global.is_empty() {
            return None;
        }
        let global: Vec<&String> = self.global.iter().collect();
        Some((!global.is_empty()).then(|| any_of(&global)))
    }
}

/// One filter that matches a test when any of `filters` does.
fn any_of(filters: &[&String]) -> String {
    if filters.len() == 1 {
        return filters[0].clone();
    }
    filters
        .iter()
        .map(|f| format!("({f})"))
        .collect::<Vec<_>>()
        .join("|")
}

/// Whether `project` is one of `targets` or reaches one through its
/// `<ProjectReference>` chain.
pub fn reaches(project: &Path, targets: &HashSet<PathBuf>) -> bool {
    closure(&[project.to_path_buf()])
        .iter()
        .any(|p| targets.contains(p))
}

/// `projects` and every project they reference, directly or not.
pub fn closure(projects: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut queue: Vec<PathBuf> = projects.iter().map(|p| absolute(p)).collect();
    while let Some(current) = queue.pop() {
        if seen.insert(current.clone()) {
            queue.extend(references(&current));
        }
    }
    seen
}

/// The projects a .csproj references, as absolute paths.
pub fn references(project: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(project).unwrap_or_default();
    let folder = project.parent().unwrap_or(Path::new("."));
    text.split("<ProjectReference")
        .skip(1)
        .filter_map(|tag| tag.split("Include=\"").nth(1)?.split('"').next())
        .map(|include| absolute(&folder.join(include.replace('\\', "/"))))
        .collect()
}

/// `<AssemblyName>` if the project sets one, else the project file's name.
pub fn assembly_of(path: &Path, text: &str) -> String {
    let named = text
        .split("<AssemblyName>")
        .nth(1)
        .and_then(|rest| rest.split("</AssemblyName>").next())
        .map(|name| name.trim().to_string());
    named.unwrap_or_else(|| stem(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SDK: &str = r#"<PackageReference Include="Microsoft.NET.Test.Sdk" />"#;

    fn write(root: &Path, file: &str, text: &str) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn test_project(references: &str) -> String {
        format!(
            r#"<Project><ItemGroup>{TEST_SDK}<ProjectReference Include="{references}" /></ItemGroup></Project>"#
        )
    }

    /// Lib <- App <- App.Tests, App.Acceptance.Tests; Other.Tests uses nothing.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Lib/Lib.csproj", "<Project></Project>");
        write(
            root,
            "App/App.csproj",
            r#"<Project><ItemGroup><ProjectReference Include="..\Lib\Lib.csproj" /></ItemGroup></Project>"#,
        );
        write(
            root,
            "App.Tests/App.Tests.csproj",
            &test_project(r"..\App\App.csproj"),
        );
        write(
            root,
            "App.Acceptance.Tests/App.Acceptance.Tests.csproj",
            &test_project(r"..\App\App.csproj"),
        );
        write(
            root,
            "Other.Tests/Other.Tests.csproj",
            &format!("<Project>{TEST_SDK}</Project>"),
        );
        dir
    }

    fn lib(root: &Path) -> HashSet<PathBuf> {
        HashSet::from([absolute(&root.join("Lib/Lib.csproj"))])
    }

    fn names(projects: &[TestProject]) -> Vec<(String, Option<String>)> {
        projects
            .iter()
            .map(|p| (p.name(), p.filter.clone()))
            .collect()
    }

    #[test]
    fn test_projects_reach_mutated_projects_through_references() {
        let dir = repo();
        let chosen = select(dir.path(), &lib(dir.path()), &TestSelection::default()).unwrap();
        assert_eq!(
            names(&chosen),
            [
                ("App.Acceptance.Tests".into(), None),
                ("App.Tests".into(), None)
            ]
        );
        assert_eq!(
            chosen[1].path,
            Path::new("App.Tests").join("App.Tests.csproj")
        );
        assert_eq!(
            closure(&[dir.path().join("App.Tests/App.Tests.csproj")]).len(),
            3
        );
    }

    #[test]
    fn globs_leave_test_projects_out() {
        let dir = repo();
        let selection = TestSelection {
            exclude: vec!["**/*.Acceptance.Tests.csproj".into()],
            ..TestSelection::default()
        };
        let chosen = select(dir.path(), &lib(dir.path()), &selection).unwrap();
        assert_eq!(names(&chosen), [("App.Tests".into(), None)]);
        let none = TestSelection {
            include: vec!["nothing/**".into()],
            ..TestSelection::default()
        };
        assert!(select(dir.path(), &lib(dir.path()), &none).is_err());
    }

    #[test]
    fn filters_apply_to_every_project_or_to_one() {
        let dir = repo();
        let global = TestSelection {
            filters: vec!["FullyQualifiedName~Pricing".into(), "Category=Fast".into()],
            ..TestSelection::default()
        };
        let chosen = select(dir.path(), &lib(dir.path()), &global).unwrap();
        let both = Some("(FullyQualifiedName~Pricing)|(Category=Fast)".to_string());
        assert_eq!(
            names(&chosen),
            [
                ("App.Acceptance.Tests".into(), both.clone()),
                ("App.Tests".into(), both)
            ]
        );
        // A filter for one project: only that project runs.
        let one = TestSelection {
            filters: vec!["App.Tests:FullyQualifiedName~A.B:C".into()],
            ..TestSelection::default()
        };
        let chosen = select(dir.path(), &lib(dir.path()), &one).unwrap();
        assert_eq!(
            names(&chosen),
            [("App.Tests".into(), Some("FullyQualifiedName~A.B:C".into()))]
        );
    }

    #[test]
    fn assembly_name_comes_from_the_project() {
        assert_eq!(
            assembly_of(Path::new("x/Shop.Tests.csproj"), "<Project></Project>"),
            "Shop.Tests"
        );
        assert_eq!(
            assembly_of(
                Path::new("x/A.csproj"),
                "<AssemblyName> Custom </AssemblyName>"
            ),
            "Custom"
        );
    }
}
