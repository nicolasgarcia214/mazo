use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::inventory::git;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub date: String,
    pub subject: String,
    #[serde(skip)]
    files: Vec<Vec<PathBuf>>,
}

impl Commit {
    fn touches(&self, path: &Path) -> bool {
        self.files.iter().all(|diff| diff.iter().any(|f| f.starts_with(path)))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct History {
    commits: Vec<Commit>,
    dirty: Vec<PathBuf>,
}

impl History {
    pub fn load(source: &Path, worktree: &Path) -> Option<History> {
        let prefix = source.canonicalize().ok()?.strip_prefix(worktree).ok()?.to_path_buf();
        let status = git(source, &["status", "--porcelain", "-z", "--untracked-files=all", "--", "."])?;
        let log = git(source, &["log", "-m", "--relative", "--name-only", "--format=%H%x00%cI%x00%s", "--", "."]);
        Some(History { commits: parse_log(&log.unwrap_or_default()), dirty: parse_status(&status, &prefix) })
    }

    pub fn skill_names(&self) -> impl Iterator<Item = String> + '_ {
        self.commits
            .iter()
            .flat_map(|c| c.files.iter().flatten())
            .filter(|f| f.file_name().is_some_and(|n| n == "SKILL.md"))
            .filter_map(|f| f.parent()?.file_name()?.to_str().map(str::to_string))
    }

    pub fn first(&self, skill: &str) -> Option<&Commit> {
        self.commits.iter().rev().find(|c| c.touches(Path::new(skill)))
    }

    pub fn last(&self, skill: &str) -> Option<&Commit> {
        self.commits.iter().find(|c| c.touches(Path::new(skill)))
    }

    pub fn wip(&self, skill: &str) -> bool {
        self.dirty.iter().any(|f| f.starts_with(skill))
    }
}

fn parse_log(log: &str) -> Vec<Commit> {
    let mut commits: Vec<Commit> = Vec::new();
    for line in log.lines().filter(|l| !l.is_empty()) {
        match line.split('\0').collect::<Vec<_>>()[..] {
            [hash, _, _] if commits.last().is_some_and(|c| c.hash == hash) => {
                commits.last_mut().expect("the guard saw a last commit").files.push(Vec::new());
            }
            [hash, date, subject] => commits.push(Commit {
                hash: hash.into(),
                date: date.into(),
                subject: subject.into(),
                files: vec![Vec::new()],
            }),
            _ => commits.last_mut().into_iter().filter_map(|c| c.files.last_mut()).for_each(|f| f.push(line.into())),
        }
    }
    commits
}

fn parse_status(status: &str, prefix: &Path) -> Vec<PathBuf> {
    let mut entries = status.split('\0').filter(|e| !e.is_empty());
    let mut paths = Vec::new();
    while let Some(e) = entries.next() {
        let Some(path) = e.get(3..) else { continue };
        paths.push(path);
        if matches!(e.as_bytes()[0], b'R' | b'C') {
            paths.extend(entries.next());
        }
    }
    paths.into_iter().filter_map(|p| Path::new(p).strip_prefix(prefix).ok().map(Path::to_path_buf)).collect()
}
