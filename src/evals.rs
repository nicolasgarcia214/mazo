//! `.grades/` and `.retros/` frontmatter and grade staleness.
//! Read at report time; never copied or written.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::inventory::Inventory;
use crate::yaml;

/// One loaded grade or retro file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Eval {
    pub file: PathBuf,
    /// The first 10 characters of the frontmatter `date`, a `YYYY-MM-DD`.
    pub date: String,
    /// The frontmatter `score` as written (`8.6`, `9`), if any.
    pub score: Option<String>,
    /// A retro's frontmatter `verdict`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grade {
    #[serde(flatten)]
    pub eval: Eval,
    /// `+N`: commits that changed the skill after this grade was committed.
    pub since: usize,
    /// `+wip`: the skill has uncommitted changes.
    pub wip: bool,
}

impl Grade {
    /// The skill changed after the grade, committed or not. An
    /// uncommitted grade has no `G`, so only `wip` can make it stale.
    pub fn stale(&self) -> bool {
        self.since > 0 || self.wip
    }

    /// `+N` when commits changed the skill after the grade, else `+wip` when
    /// only uncommitted changes did, else empty.
    pub fn mark(&self) -> String {
        match (self.since, self.wip) {
            (0, false) => String::new(),
            (0, true) => "+wip".into(),
            (n, _) => format!("+{n}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Evals {
    /// Every grade, oldest first; the last one is the latest.
    pub grades: Vec<Grade>,
    /// Every retro, oldest first; the last one is the latest.
    pub retros: Vec<Eval>,
}

/// The evaluation artifacts of one skill under the inventory's source.
pub fn load(inv: &Inventory, skill: &str) -> Evals {
    let Some(source) = &inv.source else { return Evals { grades: Vec::new(), retros: Vec::new() } };
    let history = inv.history.as_ref();
    let wip = history.is_some_and(|h| h.wip(skill));
    // Outside a git worktree nothing comes after a grade.
    let since = |file: &Path| history.map_or(0, |h| file.strip_prefix(source).map_or(0, |g| h.since(skill, g)));
    let grades = read_dir(&source.join(".grades").join(skill))
        .into_iter()
        .map(|eval| Grade { since: since(&eval.file), wip, eval })
        .collect();
    Evals { grades, retros: read_dir(&source.join(".retros").join(skill)) }
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
        let field = |key| yaml::get(&fm, key).and_then(yaml::text).map(str::to_string);
        out.push(Eval { file, date, score: field("score"), verdict: field("verdict") });
    }
    out.sort_by(|a, b| (&a.date, &a.file).cmp(&(&b.date, &b.file)));
    out
}

/// The leading `YYYY-MM-DD` of a date scalar, quoted or not.
fn date(text: &str) -> Option<String> {
    let d = text.get(..10)?;
    let shape = d.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    shape.then(|| d.to_string())
}
