//! `.grades/` and `.retros/` frontmatter and grade staleness (SPEC §6.3).
//! Read at report time; never copied or written.

use std::path::{Path, PathBuf};

use crate::inventory::git;
use crate::yaml;

/// One loaded grade or retro file.
#[derive(Debug, Clone, PartialEq)]
pub struct Eval {
    pub file: PathBuf,
    /// The first 10 characters of the frontmatter `date`, a `YYYY-MM-DD`.
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
        let fm = match yaml::frontmatter(&text) {
            Ok(fm) => fm,
            Err(e) => {
                eprintln!("warning: {}: {e}; skipped", file.display());
                continue;
            }
        };
        let Some(date) = yaml::get(&fm, "date").and_then(yaml::text).and_then(date) else {
            eprintln!("warning: {}: no YYYY-MM-DD date; skipped", file.display());
            continue;
        };
        let score = yaml::get(&fm, "score").and_then(yaml::text).map(str::to_string);
        out.push(Eval { file, date, score });
    }
    out.sort_by(|a, b| (&a.date, &a.file).cmp(&(&b.date, &b.file)));
    out
}

/// The leading `YYYY-MM-DD` of a date scalar, quoted or not (SPEC §6.3).
fn date(text: &str) -> Option<String> {
    let d = text.get(..10)?;
    let shape = d.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    shape.then(|| d.to_string())
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
