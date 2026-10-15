//! The scan driver (SPEC §6.2, F1): for each home, read new lines of every
//! transcript in scan order, classify, and append under the append rule.

use std::collections::{BTreeMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::Result;
use crate::adapters::{self, Adapter, SkillRoots};
use crate::classify::{Classifier, Roots};
use crate::config::Config;
use crate::inventory::Inventory;
use crate::ledger::{self, Agent, Channel, Event, Ledger};
use crate::tail::{self, Cursors, Prefilter};

#[derive(Debug, Default)]
pub struct HomeReport {
    /// Ledger lines this scan appended (or, on reparse, re-derived).
    pub new_events: Vec<Event>,
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

impl ScanReport {
    pub fn warn_io_errors(&self) {
        for (home, h) in &self.homes {
            for e in &h.io_errors {
                eprintln!("warning: {home}: {e}");
            }
        }
    }
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
    scan_with(cfg, inv, data, reparse, &adapters::for_agent)
}

/// [`scan`] with the adapter for each agent supplied by the caller, so a test
/// can stand in a changed parser rule for `scan --reparse` (SPEC §10.4).
pub fn scan_with<'a>(
    cfg: &Config,
    inv: &Inventory,
    data: &Path,
    reparse: bool,
    adapter_for: &dyn Fn(Agent) -> &'a dyn Adapter,
) -> Result<ScanReport> {
    let mut ledger = Ledger::open_locked(data)?;
    let mut cursors: Cursors = if reparse { Cursors::new() } else { ledger::read_json(&data.join("cursors.json"))? };
    let mut coverage = ledger::load_coverage(data)?;
    let roots = roots(cfg, inv);
    let source = cfg.source.to_string_lossy().into_owned();
    let home_skills: Vec<String> = roots.home_skills.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let filter = Prefilter::new(std::iter::once(source.as_str()).chain(home_skills.iter().map(String::as_str)));

    let mut progress = Progress { tty: std::io::stderr().is_terminal(), start: Instant::now(), shown: false };
    let mut report = ScanReport::default();
    let mut listed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut lines: Vec<Event> = Vec::new();
    let files: Vec<Vec<PathBuf>> = cfg.homes.iter().map(|h| adapter_for(h.agent).files(h)).collect();
    // Agents record their resolved cwd, so compare against the resolved data dir.
    let audit_runs = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf()).join("audit-runs");

    // Reparse builds a new ledger that starts with every typed event and every
    // event whose transcript this run does not parse, then appends the parse to
    // it under the append rule, as a fresh scan would.
    let mut fresh = Ledger::empty_like(&ledger);
    if reparse {
        let listed: HashSet<String> = files.iter().flatten().map(|p| p.to_string_lossy().into_owned()).collect();
        for e in ledger.events().iter().filter(|e| e.channel == Channel::Typed || !listed.contains(e.src_file())) {
            fresh.admit(e.clone());
        }
    }

    for (home, files) in cfg.homes.iter().zip(files) {
        let adapter = adapter_for(home.agent);
        let classifier = Classifier { inv, home, homes: &cfg.homes, audit_runs: &audit_runs };
        let mut roots_cache = Roots::default();
        let mut hr = HomeReport::default();
        let first_scan = !coverage.contains_key(&home.name);
        let need_first = coverage.get(&home.name).is_none_or(|c| c.first.is_none());
        let mut earliest: Option<String> = None;
        let total = files.len();

        for (i, path) in files.into_iter().enumerate() {
            progress.update(&home.name, i + 1, total);
            let key = path.to_string_lossy().into_owned();
            listed.insert(key.clone());
            let chunk = match tail::read(&path, cursors.get(&key), &filter) {
                Ok(c) => c,
                Err(e) => {
                    hr.io_errors.push(format!("{key}: {e}"));
                    if reparse {
                        // An unreadable transcript is not parsed: its events are kept.
                        for event in ledger.events().iter().filter(|ev| ev.src_file() == key) {
                            fresh.admit(event.clone());
                        }
                    }
                    continue;
                }
            };
            hr.files += 1;
            if need_first
                && let Some(ts) = first_timestamp(&path)
                && earliest.as_ref().is_none_or(|e| ts < *e)
            {
                earliest = Some(ts);
            }
            if chunk.lines.is_empty() {
                cursors.insert(key, chunk.cursor);
                continue;
            }
            let header = tail::first_line(&path).ok().flatten();
            let parsed = adapter.parse(&roots, &path, header.as_deref(), &chunk);
            hr.unparsed += parsed.unparsed.len();
            for s in parsed.unparsed {
                if hr.unparsed_samples.len() < 5 {
                    hr.unparsed_samples.push(s.chars().take(200).collect());
                }
            }
            if parsed.raws.is_empty() && !parsed.skipped && has_canary(&chunk) {
                hr.canary.push(key.clone());
            }
            for raw in parsed.raws {
                let Some(event) = classifier.event(&mut roots_cache, raw, &path) else { continue };
                let target = if reparse { &mut fresh } else { &mut ledger };
                if let Some(line) = target.admit(event) {
                    hr.new_events.push(line.clone());
                    lines.push(line);
                }
            }
            cursors.insert(key, chunk.cursor);
        }

        if home.agent == Agent::Claude && first_scan {
            for raw in adapters::claude::backfill(home, &inv.known) {
                let history = home.path.join("history.jsonl");
                let Some(event) = classifier.event(&mut roots_cache, raw, &history) else { continue };
                let target = if reparse { &mut fresh } else { &mut ledger };
                if let Some(line) = target.admit(event) {
                    hr.new_events.push(line.clone());
                    lines.push(line);
                }
            }
        }

        let entry = coverage.entry(home.name.clone()).or_default();
        if entry.first.is_none() {
            entry.first = earliest;
        }
        if hr.io_errors.is_empty() {
            entry.last_scan = Some(jiff::Timestamp::now().to_string());
        }
        report.homes.insert(home.name.clone(), hr);
    }

    if reparse {
        ledger.replace(fresh.events())?;
    } else {
        ledger.append(&lines)?;
    }
    // Transcripts Claude cleaned up keep no cursor.
    cursors.retain(|k, _| listed.contains(k));
    ledger::write_json(&data.join("cursors.json"), &cursors)?;
    ledger::save_coverage(data, &coverage)?;
    Ok(report)
}

/// Whether a scan shows the progress line (SPEC F12).
pub fn should_show_progress(is_tty: bool, elapsed: Duration) -> bool {
    is_tty && elapsed > Duration::from_secs(1)
}

/// The F12 progress line: one `\r`-rewritten stderr line, cleared when the scan ends.
struct Progress {
    tty: bool,
    start: Instant,
    shown: bool,
}

impl Progress {
    fn update(&mut self, home: &str, done: usize, total: usize) {
        if should_show_progress(self.tty, self.start.elapsed()) {
            eprint!("\r\x1b[2Kscanning {home} {done}/{total} files");
            self.shown = true;
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if self.shown {
            eprint!("\r\x1b[2K");
        }
    }
}

fn has_canary(chunk: &tail::Chunk) -> bool {
    chunk.lines.iter().any(|(_, l)| CANARY.iter().any(|m| l.windows(m.len()).any(|w| w == m.as_bytes())))
}

/// The earliest `"timestamp"` among the first lines of a transcript: when its
/// session started, for coverage and the audit. Records are written in time
/// order except for a Skill result and its meta record, whose timestamps are a
/// few milliseconds apart.
pub(crate) fn first_timestamp(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    BufReader::new(file)
        .lines()
        .take(20)
        .map_while(std::result::Result::ok)
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(&line).ok()?.get("timestamp")?.as_str().map(str::to_string)
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_shows_only_on_a_tty_after_one_second() {
        let second = Duration::from_secs(1);
        assert!(!should_show_progress(true, second));
        assert!(should_show_progress(true, second + Duration::from_nanos(1)));
        for elapsed in [Duration::ZERO, second, second + Duration::from_nanos(1), Duration::from_secs(60)] {
            assert!(!should_show_progress(false, elapsed));
        }
    }
}
