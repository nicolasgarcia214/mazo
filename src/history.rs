//! One-pass git history of the source (SPEC §6.3 Cost): one `git log` and one
//! `git status` per report, from which every skill's first and last commit,
//! `+N` and `+wip` are derived.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::inventory::git;

/// A commit that touched the source, with the paths it changed under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub hash: String,
    /// The committer date, strict ISO 8601 (`%cI`).
    pub date: String,
    pub subject: String,
    /// Parents rewritten to the nearest commits in the log (`--parents`).
    parents: Vec<String>,
    /// Source-relative paths changed against each parent (`-m`): one list for
    /// a plain commit, one per parent for a merge.
    files: Vec<Vec<PathBuf>>,
}

impl Commit {
    /// A merge touches `path` only when it differs from every parent there.
    fn touches(&self, path: &Path) -> bool {
        self.files.iter().all(|diff| diff.iter().any(|f| f.starts_with(path)))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct History {
    /// Newest first.
    commits: Vec<Commit>,
    /// Source-relative paths with uncommitted changes, untracked files included.
    dirty: Vec<PathBuf>,
}

impl History {
    /// The history of `source`, which sits in the git worktree `worktree`.
    /// None when git cannot read it. A repository with no commit yet has an
    /// empty log.
    pub fn load(source: &Path, worktree: &Path) -> Option<History> {
        let prefix = source.canonicalize().ok()?.strip_prefix(worktree).ok()?.to_path_buf();
        // Porcelain paths are relative to the worktree whatever the working directory.
        let status = git(source, &["status", "--porcelain", "-z", "--untracked-files=all", "--", "."])?;
        let log = git(
            source,
            &["log", "--parents", "-m", "--relative", "--name-only", "--format=%H%x00%P%x00%cI%x00%s", "--", "."],
        );
        Some(History { commits: parse_log(&log.unwrap_or_default()), dirty: parse_status(&status, &prefix) })
    }

    /// The basename of the parent of every `SKILL.md` path in the log (E15:
    /// nested layouts such as `claude/goal-audit` yield `goal-audit`).
    pub fn skill_names(&self) -> impl Iterator<Item = String> + '_ {
        self.commits
            .iter()
            .flat_map(|c| c.files.iter().flatten())
            .filter(|f| f.file_name().is_some_and(|n| n == "SKILL.md"))
            .filter_map(|f| f.parent()?.file_name()?.to_str().map(str::to_string))
    }

    /// The oldest commit that touched `skill`.
    pub fn first(&self, skill: &str) -> Option<&Commit> {
        self.commits.iter().rev().find(|c| c.touches(Path::new(skill)))
    }

    /// The newest commit that touched `skill`.
    pub fn last(&self, skill: &str) -> Option<&Commit> {
        self.commits.iter().find(|c| c.touches(Path::new(skill)))
    }

    /// `+N`: commits and merges that touched `skill` and are not ancestors of
    /// `G`, the last commit that touched `file` (source-relative), in the
    /// source-wide rewritten graph (SPEC §6.3). 0 when `file` was never committed.
    pub fn since(&self, skill: &str, file: &Path) -> usize {
        let Some(g) = self.commits.iter().find(|c| c.touches(file)) else {
            return 0;
        };
        let by_hash: HashMap<&str, &Commit> = self.commits.iter().map(|c| (c.hash.as_str(), c)).collect();
        let mut ancestors = HashSet::from([g.hash.as_str()]);
        let mut stack = vec![g];
        while let Some(c) = stack.pop() {
            for p in &c.parents {
                if ancestors.insert(p) {
                    stack.extend(by_hash.get(p.as_str()));
                }
            }
        }
        self.commits.iter().filter(|c| !ancestors.contains(c.hash.as_str()) && c.touches(Path::new(skill))).count()
    }

    /// `+wip`: `skill` has uncommitted changes.
    pub fn wip(&self, skill: &str) -> bool {
        self.dirty.iter().any(|f| f.starts_with(skill))
    }
}

/// `%H%x00%P%x00%cI%x00%s` header lines, each followed by the file names of
/// one diff. `-m` repeats a merge's header once per parent.
fn parse_log(log: &str) -> Vec<Commit> {
    let mut commits: Vec<Commit> = Vec::new();
    for line in log.lines().filter(|l| !l.is_empty()) {
        match line.split('\0').collect::<Vec<_>>()[..] {
            [hash, _, _, _] if commits.last().is_some_and(|c| c.hash == hash) => {
                commits.last_mut().unwrap().files.push(Vec::new());
            }
            [hash, parents, date, subject] => commits.push(Commit {
                hash: hash.into(),
                date: date.into(),
                subject: subject.into(),
                parents: parents.split_whitespace().map(String::from).collect(),
                files: vec![Vec::new()],
            }),
            _ => commits.last_mut().into_iter().filter_map(|c| c.files.last_mut()).for_each(|f| f.push(line.into())),
        }
    }
    commits
}

/// `git status --porcelain -z` entries `XY path`; a rename or copy is followed
/// by its original path. Paths are made relative to the source.
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
