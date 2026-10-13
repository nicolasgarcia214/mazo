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
    // Reports run `git status`; without this it may rewrite the index under the user.
    let out = Command::new("git").arg("--no-optional-locks").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Fan-in edges: citing skill → cited skills (SPEC §5.3). Filled in by `fan_in`.
pub type Graph = BTreeMap<String, BTreeSet<String>>;

/// SPEC §5.3: A → B when the body of A's `SKILL.md` (frontmatter dropped) or
/// any `.md` under `A/references/`, with backticks removed and whitespace
/// collapsed, contains `the B skill`, `owned by B`, or `B/` as a path
/// component. Self-edges are dropped. Every skill in `skills` gets a key.
pub fn fan_in(source: &Path, skills: &BTreeSet<String>) -> Graph {
    let name = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut graph = Graph::new();
    for a in skills {
        let text = citing_text(&source.join(a));
        let cited = skills.iter().filter(|b| *b != a).filter(|b| {
            text.contains(&format!("the {b} skill"))
                || text.match_indices(&format!("owned by {b}")).any(|(i, m)| !text[i + m.len()..].starts_with(name))
                || text.match_indices(&format!("{b}/")).any(|(i, _)| !text[..i].ends_with(name))
        });
        graph.insert(a.clone(), cited.cloned().collect());
    }
    graph
}

/// A skill's citing text: the `SKILL.md` body and every `references/**/*.md`,
/// backticks removed and each whitespace run collapsed to one space.
fn citing_text(dir: &Path) -> String {
    let skill_md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
    let mut parts = vec![crate::strict_yaml::split_frontmatter(&skill_md).map_or(skill_md, |(_, body)| body)];
    let mut stack = vec![dir.join("references")];
    let mut refs = Vec::new();
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                refs.push(p);
            }
        }
    }
    refs.sort();
    parts.extend(refs.iter().filter_map(|p| std::fs::read_to_string(p).ok()));
    parts.join(" ").replace('`', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// When the skill was first committed to the source repo (following renames of
/// its `SKILL.md`), or None when it has no commit (SPEC F3 `new`).
pub fn first_commit(source: &Path, skill: &str) -> Option<jiff::Timestamp> {
    let out = git(source, &["log", "--follow", "--format=%ct", "--", &format!("{skill}/SKILL.md")])?;
    let secs: i64 = out.lines().last()?.trim().parse().ok()?;
    jiff::Timestamp::from_second(secs).ok()
}
