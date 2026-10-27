//! Grade and retro frontmatter under `evals_dir`, and grade staleness by
//! content hash. Read at report time; never copied or written.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::inventory::Inventory;
use crate::render::PathExt;
use crate::store::content_hash;
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
    /// The frontmatter `content_hash`: the skill's [`content_hash`] when it was graded.
    pub content_hash: Option<String>,
    /// `content_hash` is missing or is not the skill's current one.
    pub stale: bool,
}

impl Grade {
    /// `stale` for a stale grade, else empty.
    pub fn mark(&self) -> &'static str {
        if self.stale { "stale" } else { "" }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Evals {
    /// The skill's current [`content_hash`]; None for a name in no root.
    pub content_hash: Option<String>,
    /// Every grade, oldest first; the last one is the latest.
    pub grades: Vec<Grade>,
    /// Every retro, oldest first; the last one is the latest.
    pub retros: Vec<Eval>,
    /// Each grade or retro file with frontmatter that was skipped, and why.
    pub skipped: Vec<String>,
}

/// `skill`'s grade and retro directories under `evals_dir`.
pub fn dirs(evals_dir: &Path, skill: &str) -> [PathBuf; 2] {
    ["grades", "retros"].map(|kind| evals_dir.join(kind).join(skill))
}

/// The evaluation artifacts under `evals_dir` of `skill`, a source or store
/// skill of `inv`; none for a name in no root.
pub fn load(evals_dir: &Path, inv: &Inventory, skill: &str) -> Evals {
    let Some(root) = inv.root(skill) else {
        return Evals::default();
    };
    let current = content_hash(&root.join(skill)).ok();
    let [grades, retros] = dirs(evals_dir, skill);
    let mut skipped = Vec::new();
    let grades = read_dir(&grades, &mut skipped)
        .into_iter()
        .map(|(eval, hash)| Grade { stale: hash.is_none() || hash != current, content_hash: hash, eval })
        .collect();
    let retros = read_dir(&retros, &mut skipped).into_iter().map(|(eval, _)| eval).collect();
    Evals { grades, retros, content_hash: current, skipped }
}

/// Every `*.md` in `dir` with loadable frontmatter and a `date`, with its
/// frontmatter `content_hash`, sorted by (date, filename). Other files are
/// skipped, one with frontmatter noted in `skipped` with why.
fn read_dir(dir: &Path, skipped: &mut Vec<String>) -> Vec<(Eval, Option<String>)> {
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
        // A file's name and YAML error are anyone's text.
        let skip = |why: &str| format!("{}: {}", file.inert(), crate::render::inert(why));
        let fm = match yaml::frontmatter(&text) {
            Ok(fm) => fm,
            Err(e) => {
                skipped.push(skip(&e.to_string()));
                continue;
            }
        };
        let Some(date) = yaml::get(&fm, "date").and_then(yaml::text).and_then(date) else {
            skipped.push(skip("no YYYY-MM-DD date"));
            continue;
        };
        let field = |key| yaml::get(&fm, key).and_then(yaml::text).map(str::to_string);
        out.push((Eval { file, date, score: field("score"), verdict: field("verdict") }, field("content_hash")));
    }
    out.sort_by(|(a, _), (b, _)| (&a.date, &a.file).cmp(&(&b.date, &b.file)));
    out
}

/// The leading `YYYY-MM-DD` of a date scalar, quoted or not, when it is a
/// calendar date.
fn date(text: &str) -> Option<String> {
    let d = text.get(..10)?;
    let shape = d.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    (shape && d.parse::<jiff::civil::Date>().is_ok()).then(|| d.to_string())
}
