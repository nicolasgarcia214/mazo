//! The canonical skill set, known names from git history (SPEC §4.1, §5.1) and
//! the fan-in citation graph (SPEC §5.3).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub source: PathBuf,
    /// Non-hidden child directories of the source that contain `SKILL.md`.
    pub skills: BTreeSet<String>,
    /// Canonical skills plus every directory that ever held a `SKILL.md` in git.
    pub known: BTreeSet<String>,
    /// The git worktree containing the source, for the authoring rule.
    pub worktree: Option<PathBuf>,
}

impl Inventory {
    pub fn load(source: &Path) -> Result<Inventory> {
        let skills = canonical_skills(source)?;
        let mut known = skills.clone();
        known.extend(git_skill_names(source));
        let worktree = git(source, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim()));
        Ok(Inventory { source: source.to_path_buf(), skills, known, worktree })
    }
}

/// SPEC §5.1: any non-hidden child directory of the source that contains `SKILL.md`.
pub fn canonical_skills(source: &Path) -> Result<BTreeSet<String>> {
    let entries = std::fs::read_dir(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let mut out = BTreeSet::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && e.path().join("SKILL.md").is_file() {
            out.insert(name);
        }
    }
    Ok(out)
}

/// The basename of the parent of every `SKILL.md` path in the history of the
/// source (E15: nested layouts such as `skills/claude/goal-audit` yield `goal-audit`).
fn git_skill_names(source: &Path) -> BTreeSet<String> {
    let Some(out) = git(source, &["log", "--name-only", "--format=", "--", "."]) else {
        return BTreeSet::new();
    };
    out.lines()
        .filter(|l| l.ends_with("/SKILL.md"))
        .filter_map(|l| Path::new(l).parent()?.file_name()?.to_str().map(str::to_string))
        .collect()
}

/// Runs `git -C dir args…` and returns stdout on success.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Fan-in edges: citing skill → cited skills (SPEC §5.3). Filled in by `fan_in`.
pub type Graph = BTreeMap<String, BTreeSet<String>>;
