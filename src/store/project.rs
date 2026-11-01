//! A project's scope: its git root, its store and `mazo.lock` there, and
//! the links of its skills in the project skill dirs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{Lock, Locked};
use crate::inventory::{Inventory, Scope};
use crate::links;
use crate::render::PathExt;
use crate::{Error, Result};

/// A project's store, under its git root; committed with the project.
const PROJECT_STORE: &str = ".mazo/skills";

/// The store of the project at the git root `project`.
pub fn project_dir(project: &Path) -> PathBuf {
    project.join(PROJECT_STORE)
}

/// The project skill dirs, each two levels under the git root: Claude Code's,
/// and the one Codex and Pi share.
const PROJECT_LINKS: [&str; 2] = [".claude/skills", ".agents/skills"];

fn current_dir() -> Result<PathBuf> {
    Ok(std::env::current_dir().map_err(|e| format!("current directory: {e}"))?)
}

/// The git root of `cwd`, resolved through symlinks: the nearest ancestor
/// holding `.git`, a directory or a linked worktree's file. No `git` runs, so
/// a report that loads the project calls git as it did before.
fn git_root(cwd: &Path) -> Option<PathBuf> {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    cwd.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// The git root of the current directory ([`git_root`]), which `<verb>
/// --project` acts on.
pub fn project_root(verb: &str) -> Result<PathBuf> {
    let cwd = current_dir()?;
    git_root(&cwd).ok_or_else(|| Error::Usage(format!("{verb} --project: {} is not in a git repository", cwd.inert())))
}

/// `inv`, under `project` with the project's store as a further root, and
/// each store and project skill's install date.
pub fn scoped_inventory(inv: Inventory, project: Option<&Path>) -> Result<Inventory> {
    let Some(project) = project else { return Ok(inv) };
    let store = project_dir(project);
    let roots: Vec<&Path> = inv.roots.iter().map(PathBuf::as_path).chain([store.as_path()]).collect();
    let mut with = Inventory::load_roots(inv.source.as_deref(), &roots)?;
    // `load_roots` knows no install dates: the global store's, then the project's.
    let project_lock = Lock::load(project)?.skills.into_iter().map(|l| (l.name.clone(), l));
    with.installed = inv.installed.into_iter().chain(project_lock).collect();
    Ok(with)
}

/// What a command that found no store skill `name` adds when the current
/// directory's project has one: a pointer to `--project`.
pub fn project_hint(name: &str) -> &'static str {
    let in_project =
        project_root("").and_then(|p| Lock::load(&p)).is_ok_and(|l| l.skills.iter().any(|s| s.name == name));
    if in_project { "; it is a project skill, so pass --project" } else { "" }
}

/// `name`'s links in the project skill dirs, each with the relative target
/// it holds, so that it resolves on any checkout of the project.
pub(super) fn project_links(project: &Path, name: &str) -> [(PathBuf, PathBuf); 2] {
    PROJECT_LINKS.map(|d| (project.join(d).join(name), Path::new("../..").join(PROJECT_STORE).join(name)))
}

/// Removes `name`'s links in the project skill dirs, those that still point
/// at its project store directory, and returns the lines reporting it.
pub(super) fn unlink_project(project: &Path, name: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (link, target) in project_links(project, name) {
        if fs::read_link(&link).is_ok_and(|t| t == target) {
            fs::remove_file(&link).map_err(crate::at(&link))?;
            out.push(format!("pruned    {}", link.inert()));
        }
    }
    Ok(out)
}

/// A project with a `mazo.lock` at its git root, whose skills `status` and
/// `usage` list beside the global ones.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    /// The skills of its store.
    pub skills: BTreeSet<String>,
    /// Each skill's entry in its `mazo.lock`.
    pub installed: BTreeMap<String, Locked>,
}

impl Project {
    /// The project at the current directory's git root ([`git_root`], as
    /// `--project` finds it); `None` outside one.
    pub fn current() -> Result<Option<Project>> {
        git_root(&current_dir()?).map_or(Ok(None), |root| Project::at(&root))
    }

    /// The project at git root `root`, resolved through symlinks; `None` when
    /// it has no `mazo.lock`.
    pub fn at(root: &Path) -> Result<Option<Project>> {
        let root = &fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if !root.join("mazo.lock").exists() {
            return Ok(None);
        }
        let store = root.join(PROJECT_STORE);
        let skills = if store.exists() { crate::inventory::canonical_skills(&store)? } else { BTreeSet::new() };
        let installed = Lock::load(root)?.skills.into_iter().map(|l| (l.name.clone(), l)).collect();
        Ok(Some(Project { root: root.to_path_buf(), skills, installed }))
    }

    /// The project skill whose directory, in the store or a project skill
    /// dir, holds `path`, as written or resolved through symlinks; `None` for
    /// a path of no project skill. Only a path naming a project skill is
    /// resolved, through its deepest ancestor that still exists, so a link
    /// since removed under a symlinked root, as `/var` is on macOS, still
    /// counts.
    pub fn skill_of(&self, path: &str) -> Option<&str> {
        let of = |path: &Path| {
            let rest = path.strip_prefix(&self.root).ok()?;
            let dir = [PROJECT_STORE].iter().chain(&PROJECT_LINKS).find(|d| rest.starts_with(d))?;
            let name = rest.strip_prefix(dir).ok()?.components().next()?.as_os_str().to_str()?;
            self.skills.get(name).map(String::as_str)
        };
        let path = Path::new(path);
        let named = || path.iter().any(|c| c.to_str().is_some_and(|c| self.skills.contains(c)));
        let resolved =
            || path.ancestors().find_map(|a| Some(fs::canonicalize(a).ok()?.join(path.strip_prefix(a).ok()?)));
        of(path).or_else(|| named().then(resolved).flatten().and_then(|p| of(&p)))
    }

    /// Each skill's link in each project skill dir, as `status` checks it;
    /// an entry's `home` is the dir, relative to the root.
    pub fn links(&self) -> Vec<links::Entry> {
        let entries = |name| {
            PROJECT_LINKS.into_iter().zip(project_links(&self.root, name)).map(|(dir, (path, target))| {
                let state = links::state(&path, &target);
                links::Entry { home: dir.into(), path, state, scope: Scope::Project }
            })
        };
        self.skills.iter().flat_map(|name| entries(name)).collect()
    }
}
