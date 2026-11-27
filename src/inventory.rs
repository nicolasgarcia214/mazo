use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::Result;
use crate::history::History;
use crate::store::{Locked, Project};

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub source: Option<PathBuf>,
    pub roots: Vec<PathBuf>,
    pub skills: BTreeSet<String>,
    pub origins: BTreeMap<String, Vec<PathBuf>>,
    pub known: BTreeSet<String>,
    pub worktree: Option<PathBuf>,
    pub history: Option<History>,
    pub installed: BTreeMap<String, Locked>,
    pub project: Option<Project>,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    Project,
}

impl Scope {
    pub fn suffix(self) -> &'static str {
        match self {
            Scope::Global => "",
            Scope::Project => " (project)",
        }
    }
}

impl Inventory {
    pub fn load(source: &Path) -> Result<Inventory> {
        Inventory::load_roots(Some(source), &[source])
    }

    pub fn load_roots(source: Option<&Path>, roots: &[&Path]) -> Result<Inventory> {
        let worktree = source.and_then(|s| git(s, &["rev-parse", "--show-toplevel"])).map(|s| PathBuf::from(s.trim()));
        let history = source.zip(worktree.as_deref()).and_then(|(s, w)| History::load(s, w));
        let mut inv = Inventory {
            source: source.map(Path::to_path_buf),
            roots: roots.iter().map(|r| r.to_path_buf()).collect(),
            worktree,
            history,
            ..Inventory::default()
        };
        inv.index()?;
        Ok(inv)
    }

    pub fn with_root(mut self, root: &Path) -> Result<Inventory> {
        self.roots.push(root.to_path_buf());
        self.index()?;
        Ok(self)
    }

    fn index(&mut self) -> Result<()> {
        self.origins.clear();
        for root in &self.roots {
            if Some(root) != self.source.as_ref() && !root.exists() {
                continue;
            }
            for name in canonical_skills(root)? {
                self.origins.entry(name).or_default().push(root.clone());
            }
        }
        self.skills = self.origins.keys().cloned().collect();
        self.known = self.skills.iter().cloned().chain(self.history.iter().flat_map(History::skill_names)).collect();
        Ok(())
    }

    pub fn root(&self, name: &str) -> Option<&Path> {
        self.origins.get(name).and_then(|r| r.first()).map(PathBuf::as_path)
    }

    pub fn description(&self, name: &str) -> Option<String> {
        description(&self.root(name)?.join(name))
    }
}

pub fn description(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let meta = crate::yaml::frontmatter(&text).ok()?;
    crate::yaml::get(&meta, "description").and_then(crate::yaml::text).map(crate::render::inert)
}

pub fn canonical_skills(source: &Path) -> Result<BTreeSet<String>> {
    let entries = fs::read_dir(source).map_err(crate::at(source))?;
    let mut out = BTreeSet::new();
    for e in entries.flatten() {
        let path = e.path();
        if e.file_name().as_encoded_bytes().starts_with(b".") || !path.join("SKILL.md").is_file() {
            continue;
        }
        match e.file_name().into_string() {
            Ok(name) if crate::render::inert_line(&name) == name => {
                out.insert(name);
            }
            _ => eprintln!("warning: {path:?}: skill directory name is not UTF-8 or holds a control; skipped"),
        }
    }
    Ok(out)
}

pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("--no-optional-locks").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub type Graph = BTreeMap<String, BTreeSet<String>>;

pub struct Citations {
    pub held: Graph,
    pub named: Graph,
}

pub fn citations(inv: &Inventory) -> Citations {
    let needles: Vec<_> =
        inv.skills.iter().map(|b| (b, format!("the {b} skill"), format!("owned by {b}"), format!("{b}/"))).collect();
    let mut out = Citations { held: Graph::new(), named: Graph::new() };
    for (a, roots) in &inv.origins {
        let text = citing_text(&roots[0].join(a));
        let (held, named) = (out.held.entry(a.clone()).or_default(), out.named.entry(a.clone()).or_default());
        for (b, the, owned, dir) in needles.iter().filter(|(b, ..)| *b != a) {
            if text.contains(the.as_str())
                || text.match_indices(owned.as_str()).any(|(i, m)| !text[i + m.len()..].starts_with(word))
                || text.match_indices(dir.as_str()).any(|(i, _)| !text[..i].ends_with(word))
            {
                held.insert((*b).clone());
            } else if names(&text, b) {
                named.insert((*b).clone());
            }
        }
    }
    out
}

fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn names(text: &str, b: &str) -> bool {
    text.match_indices(b)
        .any(|(i, m)| !text[..i].ends_with(word) && !text[i + m.len()..].starts_with(|c| word(c) || c == '/'))
}

fn citing_text(dir: &Path) -> String {
    let skill_md = fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
    let mut parts = vec![crate::yaml::split_frontmatter(&skill_md).map_or(skill_md, |(_, body)| body)];
    let mut stack = vec![dir.join("references")];
    let mut refs = Vec::new();
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                refs.push(p);
            }
        }
    }
    refs.sort();
    parts.extend(refs.iter().filter_map(|p| fs::read_to_string(p).ok()));
    parts.join(" ").replace('`', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_directory_name_that_is_not_utf8_or_holds_a_control_is_skipped() {
        use std::os::unix::ffi::OsStrExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = |name: &[u8]| tmp.path().join(std::ffi::OsStr::from_bytes(name));
        for name in [
            &b"ok"[..],
            b".hidden\xff",
            b"bad\xff\x1b",
            b"esc\x1b]0;t\x07",
            "rlo\u{202e}".as_bytes(),
            b"nl\nclaude: in sync",
        ] {
            match fs::create_dir(dir(name)) {
                Err(e) if e.raw_os_error() == Some(libc::EILSEQ) => continue,
                r => r.unwrap(),
            }
            fs::write(dir(name).join("SKILL.md"), "").unwrap();
        }
        assert_eq!(canonical_skills(tmp.path()).unwrap(), ["ok".to_string()].into());
    }

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
    fn named_citations_drop_self_mentions_and_held_edges() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [("goal-plan", "goal-plan, then goal-smart."), ("goal-smart", "Read goal-plan/SKILL.md.")] {
            fs::create_dir(dir.path().join(name)).unwrap();
            fs::write(dir.path().join(name).join("SKILL.md"), format!("---\nname: {name}\n---\n{body}\n")).unwrap();
        }
        let inv = Inventory::load(dir.path()).unwrap();
        let m = citations(&inv).named;
        assert_eq!(m["goal-plan"], ["goal-smart".to_string()].into());
        assert!(m["goal-smart"].is_empty(), "goal-plan/ is an edge, not a mention");
    }
}
