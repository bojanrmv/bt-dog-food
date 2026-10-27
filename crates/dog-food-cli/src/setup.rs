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

/// `<root>/.claude/skills/dog-food/SKILL.md`, or under the user's home with `global`.
pub fn install_skill(root: &Path, global: bool) -> Result<PathBuf, String> {
    let base = if global { home()? } else { root.to_path_buf() };
    let path = base
        .join(".claude")
        .join("skills")
        .join("dog-food")
        .join("SKILL.md");
    write(&path, SKILL)?;
    Ok(path)
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
        let path = install_skill(dir.path(), false).unwrap();
        assert!(path.ends_with(".claude/skills/dog-food/SKILL.md"));
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.starts_with("---\nname: dog-food\ndescription: "));
        assert!(text.contains("--format agent"));
    }
}
