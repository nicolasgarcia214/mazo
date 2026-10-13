//! `.grades/` and `.retros/` frontmatter and grade staleness (SPEC §6.3).
//! Read at report time; never copied or written.

use std::path::{Path, PathBuf};

use crate::inventory::git;
use crate::strict_yaml::{self, Yaml};

/// One loaded grade or retro file.
#[derive(Debug, Clone, PartialEq)]
pub struct Eval {
    pub file: PathBuf,
    /// The frontmatter `date`, normalised to `YYYY-MM-DD`.
    pub date: String,
    /// The frontmatter `score` as written (`8.6`, `9`), if any.
    pub score: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Grade {
    pub eval: Eval,
    /// The skill changed after this grade was committed, or has uncommitted changes.
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Evals {
    /// The latest grade, by date then filename.
    pub grade: Option<Grade>,
    /// Every retro, oldest first; the last one is the latest.
    pub retros: Vec<Eval>,
}

/// The evaluation artifacts of one skill under `source`.
pub fn load(source: &Path, skill: &str) -> Evals {
    let grade = read_dir(&source.join(".grades").join(skill)).pop().map(|eval| {
        let stale = grade_stale(source, skill, &eval.file);
        Grade { eval, stale }
    });
    Evals { grade, retros: read_dir(&source.join(".retros").join(skill)) }
}

/// Every `*.md` in `dir` with loadable frontmatter and a `date`, sorted by
/// (date, filename). Other files are skipped, a rejected one with a warning.
fn read_dir(dir: &Path) -> Vec<Eval> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let file = entry.path();
        if !file.is_file() || file.extension().is_none_or(|x| x != "md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !text.trim_start_matches('\u{feff}').starts_with("---") {
            continue; // no frontmatter: not an evaluation artifact
        }
        let fm = match strict_yaml::frontmatter(&text) {
            Ok(fm) => fm,
            Err(e) => {
                eprintln!("warning: {}: {e}; skipped", file.display());
                continue;
            }
        };
        let Some(date) = fm.get("date").and_then(Yaml::as_date) else {
            eprintln!("warning: {}: no YYYY-MM-DD date; skipped", file.display());
            continue;
        };
        let score = fm.get("score").map(|s| match s {
            Yaml::Str(s) => s.clone(),
            other => other.to_string(),
        });
        out.push(Eval { file, date, score });
    }
    out.sort_by(|a, b| (&a.date, &a.file).cmp(&(&b.date, &b.file)));
    out
}

/// SPEC §6.3: uncommitted changes to the skill make any grade stale; otherwise
/// the grade is stale when a commit after `G` (the last commit touching the
/// grade file) changed the skill. An uncommitted grade has no `G`.
fn grade_stale(source: &Path, skill: &str, grade: &Path) -> bool {
    let nonempty = |args: &[&str]| git(source, args).is_some_and(|out| !out.trim().is_empty());
    if nonempty(&["status", "--porcelain", "--", skill]) {
        return true;
    }
    let grade = grade.to_string_lossy();
    let Some(g) = git(source, &["log", "-1", "--format=%H", "--", &grade]).map(|s| s.trim().to_string()) else {
        return false;
    };
    !g.is_empty() && nonempty(&["rev-list", &format!("{g}..HEAD"), "--", skill])
}
