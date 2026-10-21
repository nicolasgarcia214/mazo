//! The canonical skill set, known names from git history,
//! the fan-in citation graph and the mentions beside it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;
use crate::history::History;

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub source: PathBuf,
    /// The directories skills are read from: the source, then the store.
    pub roots: Vec<PathBuf>,
    /// Non-hidden child directories of a root that contain `SKILL.md`.
    pub skills: BTreeSet<String>,
    /// Each skill's roots, in root order: more than one is a conflict.
    pub origins: BTreeMap<String, Vec<PathBuf>>,
    /// Canonical skills plus every directory that ever held a `SKILL.md` in git.
    pub known: BTreeSet<String>,
    /// The git worktree containing the source, for the authoring rule.
    pub worktree: Option<PathBuf>,
    /// The source's git history; None outside a git worktree.
    pub history: Option<History>,
    /// Each store skill's lockfile `installed_at` (RFC 3339).
    pub installed: BTreeMap<String, String>,
}

impl Inventory {
    pub fn load(source: &Path) -> Result<Inventory> {
        Inventory::load_roots(source, &[source])
    }

    /// The inventory over every root; `source` keeps the history and reports.
    pub fn load_roots(source: &Path, roots: &[&Path]) -> Result<Inventory> {
        let mut origins: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for root in roots {
            // The store does not exist before the first `add`.
            if *root != source && !root.exists() {
                continue;
            }
            for name in canonical_skills(root)? {
                origins.entry(name).or_default().push(root.to_path_buf());
            }
        }
        let skills: BTreeSet<String> = origins.keys().cloned().collect();
        let worktree = git(source, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim()));
        let history = worktree.as_deref().and_then(|w| History::load(source, w));
        let mut known = skills.clone();
        known.extend(history.iter().flat_map(History::skill_names));
        let roots = roots.iter().map(|r| r.to_path_buf()).collect();
        let installed = BTreeMap::new();
        Ok(Inventory { source: source.to_path_buf(), roots, skills, origins, known, worktree, history, installed })
    }

    /// The root a skill is read from; the source for a name in no root.
    pub fn root(&self, name: &str) -> &Path {
        self.origins.get(name).and_then(|r| r.first()).unwrap_or(&self.source)
    }
}

/// Any non-hidden child directory of the root that contains `SKILL.md`.
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

/// Runs `git -C dir args…` and returns stdout on success.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    // Reports run `git status`; without this it may rewrite the index under the user.
    let out = Command::new("git").arg("--no-optional-locks").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Fan-in edges: citing skill → cited skills. Filled in by `fan_in`.
pub type Graph = BTreeMap<String, BTreeSet<String>>;

/// A → B when the body of A's `SKILL.md` (frontmatter dropped) or
/// any `.md` under `A/references/`, with backticks removed and whitespace
/// collapsed, contains `the B skill`, `owned by B`, or `B/` as a path
/// component. Self-edges are dropped. Every skill of `inv` gets a key.
pub fn fan_in(inv: &Inventory) -> Graph {
    let skills = &inv.skills;
    let mut graph = Graph::new();
    for a in skills {
        let text = citing_text(&inv.root(a).join(a));
        let cited = skills.iter().filter(|b| *b != a).filter(|b| {
            text.contains(&format!("the {b} skill"))
                || text.match_indices(&format!("owned by {b}")).any(|(i, m)| !text[i + m.len()..].starts_with(word))
                || text.match_indices(&format!("{b}/")).any(|(i, _)| !text[..i].ends_with(word))
        });
        graph.insert(a.clone(), cited.cloned().collect());
    }
    graph
}

/// A → B when A's citing text names B as a whole word (case-sensitive) and
/// A → B is not in `edges`. Mentions mark boundaries between siblings, so they
/// never hold a skill. Self-mentions are dropped. Every skill of `inv` gets a key.
pub fn mentions(inv: &Inventory, edges: &Graph) -> Graph {
    let skills = &inv.skills;
    let mut graph = Graph::new();
    for a in skills {
        let text = citing_text(&inv.root(a).join(a));
        let named = skills
            .iter()
            .filter(|b| *b != a && !edges.get(a).is_some_and(|e| e.contains(*b)))
            .filter(|b| names(&text, b));
        graph.insert(a.clone(), named.cloned().collect());
    }
    graph
}

/// A character of a skill name.
fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// `b` occurs in `text` after the start or a non-name character, and before
/// the end or a character that is neither a name character nor `/`: `/b` is a
/// name, `b/` is a path.
fn names(text: &str, b: &str) -> bool {
    text.match_indices(b)
        .any(|(i, m)| !text[..i].ends_with(word) && !text[i + m.len()..].starts_with(|c| word(c) || c == '/'))
}

/// A skill's citing text: the `SKILL.md` body and every `references/**/*.md`,
/// backticks removed and each whitespace run collapsed to one space.
fn citing_text(dir: &Path) -> String {
    let skill_md = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
    let mut parts = vec![crate::yaml::split_frontmatter(&skill_md).map_or(skill_md, |(_, body)| body)];
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

#[cfg(test)]
mod tests {
    use super::{Inventory, fan_in, mentions, names};

    #[test]
    fn a_name_is_a_whole_word_that_is_not_a_path() {
        assert!(names("run /goal-plan next", "goal-plan"));
        assert!(names("goal-plan", "goal-plan"));
        assert!(names("| Plan | goal-plan |", "goal-plan"));
        assert!(names("see goal-plan.", "goal-plan"));
        for text in ["goal-plan/SKILL.md", "xgoal-plan", "goal-planx", "x-goal-plan", "goal-plan_v2", "Goal-Plan"] {
            assert!(!names(text, "goal-plan"), "{text}");
        }
    }

    #[test]
    fn mentions_drop_self_mentions_and_edges() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [("goal-plan", "goal-plan, then goal-smart."), ("goal-smart", "Read goal-plan/SKILL.md.")] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
            std::fs::write(dir.path().join(name).join("SKILL.md"), format!("---\nname: {name}\n---\n{body}\n"))
                .unwrap();
        }
        let inv = Inventory::load(dir.path()).unwrap();
        let m = mentions(&inv, &fan_in(&inv));
        assert_eq!(m["goal-plan"], ["goal-smart".to_string()].into());
        assert!(m["goal-smart"].is_empty(), "goal-plan/ is an edge, not a mention");
    }
}
