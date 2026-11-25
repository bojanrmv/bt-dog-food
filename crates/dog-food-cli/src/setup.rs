//! `dgf hook install|uninstall` and `dgf skill install`: small helpers that
//! write files into a repository (or the user's Claude folder).

use std::path::{Path, PathBuf};

use crate::git;

const HOOK_MARKER: &str = "Installed by dog-food";

/// The pre-commit hook: compares what is staged with HEAD.
const HOOK: &str = "#!/bin/sh
# Installed by dog-food (dgf hook install). Remove with: dgf hook uninstall
# Blocks a commit that adds a function above the fail limit or makes one worse.
# Skip it once with: git commit --no-verify
exec dgf changes --staged
";

/// The Claude Code skill that teaches the agent to use and read dgf.
pub const SKILL: &str = include_str!("../assets/SKILL.md");

/// The stages of a piece of work, each a skill of its own, in the order they run.
pub const WORKFLOW: [(&str, &str); 4] = [
    ("dgf-spec", include_str!("../assets/skills/dgf-spec.md")),
    ("dgf-build", include_str!("../assets/skills/dgf-build.md")),
    ("dgf-clean", include_str!("../assets/skills/dgf-clean.md")),
    ("dgf-harden", include_str!("../assets/skills/dgf-harden.md")),
];

/// The text of a skill dgf ships, by its name.
pub fn skill(name: &str) -> Option<&'static str> {
    let stage = WORKFLOW.iter().find(|(stage, _)| *stage == name);
    stage
        .map(|(_, text)| *text)
        .or((name == "dog-food").then_some(SKILL))
}

/// The names `skill` knows.
pub fn skill_names() -> Vec<&'static str> {
    std::iter::once("dog-food")
        .chain(WORKFLOW.iter().map(|(name, _)| *name))
        .collect()
}

pub fn install_hook(root: &Path, force: bool) -> Result<PathBuf, String> {
    let path = git::git_path(root, "hooks/pre-commit")?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if !existing.is_empty() && !existing.contains(HOOK_MARKER) && !force {
        return Err(format!(
            "{} already exists and was not written by dog-food; add `exec dgf changes --staged` to it yourself, or pass --force to replace it",
            path.display()
        ));
    }
    write(&path, HOOK)?;
    make_executable(&path)?;
    Ok(path)
}

pub fn uninstall_hook(root: &Path) -> Result<PathBuf, String> {
    let path = git::git_path(root, "hooks/pre-commit")?;
    let existing = std::fs::read_to_string(&path)
        .map_err(|_| format!("no pre-commit hook at {}", path.display()))?;
    if !existing.contains(HOOK_MARKER) {
        return Err(format!(
            "{} was not written by dog-food; leaving it alone",
            path.display()
        ));
    }
    std::fs::remove_file(&path)
        .map_err(|err| format!("could not remove {}: {err}", path.display()))?;
    Ok(path)
}

/// `<root>/.claude/skills/<name>/SKILL.md` for the `dog-food` skill, and with `workflow`
/// for every stage skill too; under the user's home with `global`. Says what it wrote.
pub fn install_skills(root: &Path, global: bool, workflow: bool) -> Result<Vec<PathBuf>, String> {
    let base = if global { home()? } else { root.to_path_buf() };
    let stages = WORKFLOW.iter().filter(|_| workflow).map(|(name, _)| *name);
    let mut written = Vec::new();
    for name in std::iter::once("dog-food").chain(stages) {
        let path = base
            .join(".claude")
            .join("skills")
            .join(name)
            .join("SKILL.md");
        write(&path, skill(name).unwrap_or_default())?;
        written.push(path);
    }
    Ok(written)
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| "cannot find the home folder (USERPROFILE or HOME)".to_string())
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|err| format!("could not write {}: {err}", path.display()))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|err| err.to_string())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        dir
    }

    #[test]
    fn hook_installs_refuses_foreign_hooks_and_uninstalls() {
        let dir = repo();
        let path = install_hook(dir.path(), false).unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("dgf changes --staged"));
        // Reinstalling our own hook is fine.
        install_hook(dir.path(), false).unwrap();
        uninstall_hook(dir.path()).unwrap();
        assert!(!path.exists());

        std::fs::write(&path, "#!/bin/sh\necho mine\n").unwrap();
        assert!(install_hook(dir.path(), false)
            .unwrap_err()
            .contains("--force"));
        assert!(uninstall_hook(dir.path()).is_err());
        install_hook(dir.path(), true).unwrap();
    }

    #[test]
    fn skill_is_written_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let written = install_skills(dir.path(), false, false).unwrap();
        assert_eq!(
            written.len(),
            1,
            "only dog-food unless the stages are asked for"
        );
        assert!(written[0].ends_with(".claude/skills/dog-food/SKILL.md"));
        let text = std::fs::read_to_string(&written[0]).unwrap();
        assert!(text.starts_with("---\nname: dog-food\ndescription: "));
        assert!(text.contains("--format agent"));
    }

    #[test]
    fn the_stage_skills_are_installed_beside_it_when_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let written = install_skills(dir.path(), false, true).unwrap();
        let folder = |path: &PathBuf| {
            let folder = path.parent().unwrap().file_name().unwrap();
            folder.to_string_lossy().into_owned()
        };
        let folders: Vec<String> = written.iter().map(folder).collect();
        assert_eq!(
            folders,
            [
                "dog-food",
                "dgf-spec",
                "dgf-build",
                "dgf-clean",
                "dgf-harden"
            ]
        );
        assert!(written.iter().all(|path| path.is_file()));
    }

    /// A skill is found by the name in its first lines, and each stage hands over to the next.
    #[test]
    fn every_stage_skill_carries_its_name_and_names_the_stage_after_it() {
        let next = [
            "/dgf-build <name>",
            "/dgf-clean <name>",
            "/dgf-harden <name>",
            "review, then QA",
        ];
        for ((name, text), next) in WORKFLOW.iter().zip(next) {
            let text = text.replace("\r\n", "\n");
            let start = format!("---\nname: {name}\ndescription: Stage ");
            assert!(text.starts_with(&start), "{name}");
            assert!(text.contains(&format!("Next: {next}")), "{name} to {next}");
            assert!(
                text.contains(".dgf/work/<name>.md"),
                "{name}: the work file"
            );
        }
    }

    #[test]
    fn a_skill_is_found_by_its_name() {
        assert_eq!(skill("dog-food"), Some(SKILL));
        assert!(skill("dgf-clean")
            .unwrap()
            .contains("# Clean: stage 3 of 4"));
        assert_eq!(skill("dgf-qa"), None);
        assert_eq!(
            skill_names(),
            [
                "dog-food",
                "dgf-spec",
                "dgf-build",
                "dgf-clean",
                "dgf-harden"
            ]
        );
    }
}
