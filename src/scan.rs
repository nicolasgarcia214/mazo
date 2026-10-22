//! The scan driver: for each home, read new lines of every
//! transcript in scan order, classify, and append under the append rule.

use std::collections::{BTreeMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::IgnoredAny;

use crate::Result;
use crate::adapters::{self, Adapter, Raw, SkillRoots};
use crate::classify::{Classifier, Roots};
use crate::config::Config;
use crate::inventory::Inventory;
use crate::ledger::{self, Agent, Channel, Event, Ledger};
use crate::tail::{self, Cursors, Prefilter};

#[derive(Debug, Default, Serialize)]
pub struct HomeReport {
    /// Ledger lines this scan appended (or, on reparse, re-derived).
    pub new_events: Vec<Event>,
    pub files: usize,
    pub io_errors: Vec<String>,
    pub unparsed: usize,
    pub unparsed_samples: Vec<String>,
    /// Files with a skill marker that yielded no event: the parser canary.
    pub canary: Vec<String>,
    /// Lines this scan read that passed the prefilter, so might hold a load,
    /// but are not JSON: no event came from them, even in a file that yielded
    /// others.
    pub malformed: usize,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub homes: BTreeMap<String, HomeReport>,
}

impl ScanReport {
    /// Prints the read errors and the malformed lines of each home. A line is
    /// read once, so its warning comes once, or again on `--reparse`.
    pub fn warn(&self) {
        for (home, h) in &self.homes {
            for e in &h.io_errors {
                eprintln!("warning: {home}: {e}");
            }
            if h.malformed > 0 {
                eprintln!(
                    "warning: {home}: transcript lines that are not JSON: {}; a load in one is not counted",
                    h.malformed
                );
            }
        }
    }
}

/// Markers whose presence in a file without any event trips the canary.
const CANARY: [&str; 3] = ["Base directory for this skill", "<skill>", "<skill name="];

pub fn roots(cfg: &Config, inv: &Inventory) -> SkillRoots {
    SkillRoots {
        source: cfg.source.clone(),
        home_skills: cfg.homes.iter().map(|h| h.skills_dir()).collect(),
        known: inv.known.clone(),
    }
}

/// Incremental scan, or a full reparse when `reparse` is set.
pub fn scan(cfg: &Config, inv: &Inventory, data: &Path, reparse: bool) -> Result<ScanReport> {
    scan_with(cfg, inv, data, reparse, &adapters::for_agent)
}

/// [`scan`] with the adapter for each agent supplied by the caller, so a test
/// can stand in a changed parser rule for `scan --reparse`.
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
    let dirs: Vec<String> =
        cfg.source.iter().chain(&roots.home_skills).map(|p| p.to_string_lossy().into_owned()).collect();
    let filter = Prefilter::new(dirs.iter().map(String::as_str));

    let mut progress = Progress { tty: std::io::stderr().is_terminal(), start: Instant::now(), shown: false };
    let mut report = ScanReport::default();
    let mut lines: Vec<Event> = Vec::new();
    let files: Vec<Vec<PathBuf>> = cfg.homes.iter().map(|h| adapter_for(h.agent).files(h)).collect();
    let listed: HashSet<String> = files.iter().flatten().map(|p| p.to_string_lossy().into_owned()).collect();
    // Agents record their resolved cwd, so compare against the resolved data dir.
    let audit_runs = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf()).join("audit-runs");

    // A missing roots.json is rebuilt from every session on disk.
    let stored = Roots::load(data)?;
    let rebuild = stored.is_none();
    let mut roots_cache = stored.unwrap_or_default();

    // Reparse builds a new ledger that starts with every typed event and every
    // event whose transcript this run does not parse, then appends the parse to
    // it under the append rule, as a fresh scan would.
    let mut fresh = Ledger::empty_like(&ledger);
    if reparse {
        for e in ledger.events().iter().filter(|e| e.channel == Channel::Typed || !listed.contains(e.src_file())) {
            fresh.admit(e.clone());
        }
    }

    for (home, files) in cfg.homes.iter().zip(files) {
        let adapter = adapter_for(home.agent);
        let classifier = Classifier { inv, home, homes: &cfg.homes, audit_runs: &audit_runs };
        let mut hr = HomeReport::default();
        let first_scan = !coverage.contains_key(&home.name);
        let need_first = coverage.get(&home.name).is_none_or(|c| c.first.is_none());
        let mut earliest: Option<String> = None;
        let total = files.len();

        for (i, path) in files.into_iter().enumerate() {
            progress.update(&home.name, i + 1, total);
            let key = path.to_string_lossy().into_owned();
            // A session's root is stored once its header is first read, so it
            // survives a parent deleted before the session's first load.
            let unvisited = rebuild || cursors.get(&key).is_none_or(|c| c.line == 0);
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
            // After the read, so the header is at least what the read consumed.
            if unvisited {
                roots_cache.visit(home, &path);
            }
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
            if !parsed.skipped {
                hr.malformed +=
                    chunk.lines.iter().filter(|(_, l)| serde_json::from_slice::<IgnoredAny>(l).is_err()).count();
            }
            hr.unparsed += parsed.unparsed.len();
            for s in parsed.unparsed {
                if hr.unparsed_samples.len() < 5 {
                    hr.unparsed_samples.push(s.chars().take(200).collect());
                }
            }
            if parsed.raws.is_empty() && !parsed.skipped && has_canary(&chunk) {
                hr.canary.push(key.clone());
            }
            let target = if reparse { &mut fresh } else { &mut ledger };
            admit(&classifier, &mut roots_cache, parsed.raws, &path, target, &mut hr, &mut lines);
            cursors.insert(key, chunk.cursor);
        }

        if home.agent == Agent::Claude && first_scan {
            let raws = adapters::claude::backfill(home, &inv.known);
            let target = if reparse { &mut fresh } else { &mut ledger };
            admit(&classifier, &mut roots_cache, raws, &home.path.join("history.jsonl"), target, &mut hr, &mut lines);
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
    roots_cache.save(data)?;
    // Transcripts Claude cleaned up keep no cursor.
    cursors.retain(|k, _| listed.contains(k));
    ledger::write_json(&data.join("cursors.json"), &cursors)?;
    ledger::save_coverage(data, &coverage)?;
    Ok(report)
}

/// Classifies the raw events read from `path` and admits each to `target`
/// under the append rule, recording every line it admits in `hr` and `lines`.
fn admit(
    classifier: &Classifier,
    cache: &mut Roots,
    raws: Vec<Raw>,
    path: &Path,
    target: &mut Ledger,
    hr: &mut HomeReport,
    lines: &mut Vec<Event>,
) {
    for raw in raws {
        let Some(event) = classifier.event(cache, raw, path) else { continue };
        if let Some(line) = target.admit(event) {
            hr.new_events.push(line.clone());
            lines.push(line);
        }
    }
}

/// Whether a scan shows the progress line.
pub fn should_show_progress(is_tty: bool, elapsed: Duration) -> bool {
    is_tty && elapsed > Duration::from_secs(1)
}

/// The progress line: one `\r`-rewritten stderr line, cleared when the scan ends.
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
