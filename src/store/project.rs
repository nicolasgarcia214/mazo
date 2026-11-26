use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{Lock, Locked};
use crate::inventory::{Inventory, Scope};
use crate::links;
use crate::render::PathExt;
use crate::{Error, Result};

const PROJECT_STORE: &str = ".mazo/skills";

pub fn project_dir(project: &Path) -> PathBuf {
    project.join(PROJECT_STORE)
}

const PROJECT_LINKS: [&str; 2] = [".claude/skills", ".agents/skills"];

fn current_dir() -> Result<PathBuf> {
    Ok(std::env::current_dir().map_err(|e| format!("current directory: {e}"))?)
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    cwd.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

pub fn project_root(verb: &str) -> Result<PathBuf> {
    let cwd = current_dir()?;
    git_root(&cwd).ok_or_else(|| Error::Usage(format!("{verb} --project: {} is not in a git repository", cwd.inert())))
}

pub fn scoped_inventory(inv: Inventory, project: Option<&Path>) -> Result<Inventory> {
    let Some(project) = project else { return Ok(inv) };
    let store = project_dir(project);
    let roots: Vec<&Path> = inv.roots.iter().map(PathBuf::as_path).chain([store.as_path()]).collect();
    let mut with = Inventory::load_roots(inv.source.as_deref(), &roots)?;
    let project_lock = Lock::load(project)?.skills.into_iter().map(|l| (l.name.clone(), l));
    with.installed = inv.installed.into_iter().chain(project_lock).collect();
    Ok(with)
}

pub fn project_hint(name: &str) -> &'static str {
    let in_project =
        project_root("").and_then(|p| Lock::load(&p)).is_ok_and(|l| l.skills.iter().any(|s| s.name == name));
    if in_project { "; it is a project skill, so pass --project" } else { "" }
}

pub(super) fn project_links(project: &Path, name: &str) -> [(PathBuf, PathBuf); 2] {
    PROJECT_LINKS.map(|d| (project.join(d).join(name), Path::new("../..").join(PROJECT_STORE).join(name)))
}

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

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub skills: BTreeSet<String>,
    pub installed: BTreeMap<String, Locked>,
}

impl Project {
    pub fn current() -> Result<Option<Project>> {
        git_root(&current_dir()?).map_or(Ok(None), |root| Project::at(&root))
    }

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
