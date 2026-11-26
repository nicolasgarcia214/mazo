use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::adapters::jsonl_files;
use crate::config::Home;
use crate::inventory::Inventory;
use crate::ledger::Agent;
use crate::render::PathExt;
use crate::store::content_hash;
use crate::yaml;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Eval {
    pub file: PathBuf,
    pub date: String,
    pub score: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grade {
    #[serde(flatten)]
    pub eval: Eval,
    pub content_hash: Option<String>,
    pub stale: bool,
}

impl Grade {
    pub fn mark(&self) -> &'static str {
        if self.stale { "stale" } else { "" }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Evals {
    pub content_hash: Option<String>,
    pub grades: Vec<Grade>,
    pub retros: Vec<Eval>,
    pub skipped: Vec<String>,
}

pub fn dirs(evals_dir: &Path, skill: &str) -> [PathBuf; 2] {
    ["grades", "retros"].map(|kind| evals_dir.join(kind).join(skill))
}

pub fn transcript(homes: &[Home], session: &str) -> crate::Result<(Agent, PathBuf)> {
    let usage = |m: String| crate::Error::Usage(m);
    if session.is_empty() {
        return Err(usage("the session is empty: give a transcript's path or a session id".into()));
    }
    if let Some(file) = std::fs::canonicalize(session).ok().filter(|f| f.is_file()) {
        let under = |h: &&Home| std::fs::canonicalize(h.transcripts()).is_ok_and(|t| file.starts_with(t));
        let home =
            homes.iter().find(under).ok_or_else(|| usage(format!("{}: under no home's transcripts", file.inert())))?;
        return Ok((home.agent, file));
    }
    let named = |f: &PathBuf| f.file_name().is_some_and(|n| n.to_string_lossy().contains(session));
    let mut found =
        homes.iter().flat_map(|h| jsonl_files(&h.transcripts()).into_iter().filter(named).map(|f| (h.agent, f)));
    match (found.next(), found.next()) {
        (Some(one), None) => Ok(one),
        (None, _) => Err(usage(format!("no transcript is named {}", crate::render::inert_line(session)))),
        (Some(_), Some(_)) => {
            Err(usage(format!("more than one transcript is named {}", crate::render::inert_line(session))))
        }
    }
}

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
            continue;
        }
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

fn date(text: &str) -> Option<String> {
    let d = text.get(..10)?;
    let shape = d.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    (shape && d.parse::<jiff::civil::Date>().is_ok()).then(|| d.to_string())
}
