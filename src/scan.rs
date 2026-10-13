//! The scan driver (SPEC §6.2, F1): for each home, read new lines of every
//! transcript in scan order, classify, and append under the append rule.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Result;
use crate::adapters::{self, ParseCtx, SkillRoots};
use crate::classify::{Classifier, Roots};
use crate::config::Config;
use crate::inventory::Inventory;
use crate::ledger::{self, Agent, Channel, Coverage, Event, Ledger};
use crate::tail::{self, Cursors, Prefilter};

#[derive(Debug, Default)]
pub struct HomeReport {
    pub new_events: usize,
    pub files: usize,
    pub io_errors: Vec<String>,
    pub unparsed: usize,
    pub unparsed_samples: Vec<String>,
    /// Files with a skill marker that yielded no event: the parser canary.
    pub canary: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub homes: BTreeMap<String, HomeReport>,
}

/// Markers whose presence in a file without any event trips the canary (SPEC §4.6).
const CANARY: [&str; 3] = ["Base directory for this skill", "<skill>", "<skill name="];

pub fn roots(cfg: &Config, inv: &Inventory) -> SkillRoots {
    SkillRoots {
        source: cfg.source.clone(),
        home_skills: cfg.homes.iter().map(|h| h.skills_dir()).collect(),
        known: inv.known.clone(),
    }
}

/// Incremental scan, or a full reparse (SPEC §6.2 Reparse) when `reparse` is set.
pub fn scan(cfg: &Config, inv: &Inventory, data: &Path, reparse: bool) -> Result<ScanReport> {
    let mut ledger = Ledger::open_locked(data)?;
    let mut cursors: Cursors = if reparse { Cursors::new() } else { ledger::read_json(&data.join("cursors.json"))? };
    let mut coverage = ledger::load_coverage(data)?;
    let roots = roots(cfg, inv);
    let source = cfg.source.to_string_lossy().into_owned();
    let home_skills: Vec<String> = roots.home_skills.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let filter = Prefilter::new(std::iter::once(source.as_str()).chain(home_skills.iter().map(String::as_str)));

    let mut report = ScanReport::default();
    let mut lines: Vec<Event> = Vec::new();
    let mut parsed_files: BTreeSet<String> = BTreeSet::new();
    let mut fresh = Ledger::empty_like(&ledger);
    let now = jiff::Timestamp::now().to_string();

    for home in &cfg.homes {
        let adapter = adapters::for_agent(home.agent);
        let ctx = ParseCtx { home, roots: &roots };
        let classifier = Classifier { inv, home, homes: &cfg.homes };
        let mut roots_cache = Roots::default();
        let mut hr = HomeReport::default();
        let first_scan = !coverage.contains_key(&home.name);
        let mut earliest: Option<String> = None;

        for path in adapter.files(home) {
            let key = path.to_string_lossy().into_owned();
            let chunk = match tail::read(&path, cursors.get(&key), &filter) {
                Ok(c) => c,
                Err(e) => {
                    hr.io_errors.push(format!("{key}: {e}"));
                    continue;
                }
            };
            hr.files += 1;
            parsed_files.insert(key.clone());
            if first_scan {
                if let Some(ts) = first_timestamp(&path) {
                    if earliest.as_ref().is_none_or(|e| ts < *e) {
                        earliest = Some(ts);
                    }
                }
            }
            if chunk.lines.is_empty() {
                cursors.insert(key, chunk.cursor);
                continue;
            }
            let header = tail::first_line(&path).ok().flatten();
            let parsed = adapter.parse(&ctx, &path, header.as_deref(), &chunk);
            hr.unparsed += parsed.unparsed.len();
            for s in parsed.unparsed {
                if hr.unparsed_samples.len() < 5 {
                    hr.unparsed_samples.push(s);
                }
            }
            if parsed.raws.is_empty() && !parsed.skipped && has_canary(&chunk) {
                hr.canary.push(key.clone());
            }
            for raw in parsed.raws {
                let event = classifier.event(&mut roots_cache, raw, &path);
                let target = if reparse { &mut fresh } else { &mut ledger };
                if let Some(line) = target.admit(event) {
                    lines.push(line);
                    hr.new_events += 1;
                }
            }
            cursors.insert(key, chunk.cursor);
        }

        if home.agent == Agent::Claude && first_scan && !reparse {
            for raw in adapters::claude::backfill(home, &inv.known) {
                let history = home.path.join("history.jsonl");
                let event = classifier.event(&mut roots_cache, raw, &history);
                if let Some(line) = ledger.admit(event) {
                    lines.push(line);
                    hr.new_events += 1;
                }
            }
        }

        let entry = coverage.entry(home.name.clone()).or_insert_with(|| Coverage { first: earliest.clone(), last_scan: None });
        if hr.io_errors.is_empty() {
            entry.last_scan = Some(now.clone());
        }
        report.homes.insert(home.name.clone(), hr);
    }

    if reparse {
        // Typed events and events whose transcript was not parsed are kept verbatim;
        // everything else is what the current parser emits.
        let mut kept: Vec<Event> = ledger
            .events()
            .iter()
            .filter(|e| e.channel == Channel::Typed || !parsed_files.contains(e.src_file()))
            .cloned()
            .collect();
        kept.extend(fresh.events().iter().cloned());
        ledger.replace(kept)?;
    } else {
        ledger.append(&lines)?;
    }
    ledger::write_json(&data.join("cursors.json"), &cursors)?;
    ledger::save_coverage(data, &coverage)?;
    Ok(report)
}

fn has_canary(chunk: &tail::Chunk) -> bool {
    chunk.lines.iter().any(|(_, l)| CANARY.iter().any(|m| l.windows(m.len()).any(|w| w == m.as_bytes())))
}

/// The first `"timestamp"` among the first lines of a transcript, for coverage.
fn first_timestamp(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(20) {
        let line = line.ok()?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()) {
            return Some(ts.to_string());
        }
    }
    None
}
