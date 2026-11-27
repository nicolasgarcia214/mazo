use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::Result;
use crate::adapters::{self, Adapter, Raw, SkillRoots};
use crate::classify::{Classifier, Dropped, Roots};
use crate::config::Config;
use crate::inventory::Inventory;
use crate::ledger::{self, Agent, Channel, Event, Ledger};
use crate::render::Diag;
use crate::tail::{self, Cursors, Prefilter};

#[derive(Serialize, Debug, Default)]
pub struct HomeReport {
    pub new_events: Vec<Event>,
    pub files: usize,
    pub io_errors: Vec<String>,
    pub unparsed: usize,
    pub unparsed_samples: Vec<String>,
    pub canary: Vec<String>,
    pub malformed: usize,
    pub bad_timestamps: usize,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub homes: BTreeMap<String, HomeReport>,
    pub events: Vec<Event>,
}

impl ScanReport {
    pub fn warnings(&self) -> Vec<Diag> {
        let mut out = Vec::new();
        for (home, h) in &self.homes {
            out.extend(h.io_errors.iter().map(|e| Diag::warning(format!("{home}: {e}"))));
            if h.malformed > 0 {
                out.push(Diag::warning(format!(
                    "{home}: transcript lines that are not JSON: {}; a load in one is not counted",
                    h.malformed
                )));
            }
        }
        out
    }

    pub fn warn(&self) {
        for w in self.warnings() {
            eprintln!("{w}");
        }
    }
}

pub fn roots(cfg: &Config, inv: &Inventory) -> SkillRoots {
    SkillRoots {
        source: cfg.source.clone(),
        home_skills: cfg.homes.iter().map(|h| h.skills_dir()).collect(),
        known: inv.known.clone(),
    }
}

pub fn scan(cfg: &Config, inv: &Inventory, data: &Path, reparse: bool) -> Result<ScanReport> {
    scan_with(cfg, inv, data, reparse, &adapters::for_agent)
}

pub fn scan_with<'a>(
    cfg: &Config,
    inv: &Inventory,
    data: &Path,
    reparse: bool,
    adapter_for: &dyn Fn(Agent) -> &'a dyn Adapter,
) -> Result<ScanReport> {
    let mut ledger = Ledger::open_locked(data)?;
    let mut cursors: Cursors =
        if reparse { Cursors::new() } else { ledger::read_derived(&data.join("cursors.json"))?.unwrap_or_default() };
    let mut coverage = ledger::load_coverage(data)?;
    let roots = roots(cfg, inv);
    let dirs: Vec<String> =
        cfg.source.iter().chain(&roots.home_skills).map(|p| p.to_string_lossy().into_owned()).collect();
    let filter = Prefilter::new(dirs.iter().map(String::as_str));

    let mut progress = Progress { tty: std::io::stderr().is_terminal(), start: Instant::now(), shown: false };
    let mut report = ScanReport::default();
    let now = jiff::Timestamp::now();
    let files: Vec<Vec<PathBuf>> = cfg.homes.iter().map(|h| adapter_for(h.agent).files(h)).collect();
    let listed: HashSet<String> = files.iter().flatten().map(|p| p.to_string_lossy().into_owned()).collect();
    let audit_runs = fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf()).join("audit-runs");

    let stored = Roots::load(data)?;
    let rebuild = stored.is_none();
    let mut roots_cache = stored.unwrap_or_default();

    let mut fresh = Ledger::empty_like(&ledger);
    if reparse {
        for e in ledger.events().iter().filter(|e| e.channel == Channel::Typed || !listed.contains(e.src_file())) {
            fresh.admit(e.clone());
        }
    }

    for (home, files) in cfg.homes.iter().zip(files) {
        let adapter = adapter_for(home.agent);
        let classifier = Classifier { inv, home, homes: &cfg.homes, audit_runs: &audit_runs, now };
        let mut hr = HomeReport::default();
        let first_scan = !coverage.contains_key(&home.name) && !ledger.events().iter().any(|e| e.home == home.name);
        let need_first = coverage.get(&home.name).is_none_or(|c| c.first_at().is_none());
        let mut earliest: Option<jiff::Timestamp> = None;
        let total = files.len();

        for (i, path) in files.into_iter().enumerate() {
            progress.update(&home.name, i + 1, total);
            let key = path.to_string_lossy().into_owned();
            let unvisited = rebuild || cursors.get(&key).is_none_or(|c| c.line == 0);
            let mut chunk = match tail::read(&path, cursors.get(&key), &filter) {
                Ok(c) => c,
                Err(e) => {
                    hr.io_errors.push(crate::at(&path)(e));
                    if reparse {
                        for event in ledger.events().iter().filter(|ev| ev.src_file() == key) {
                            fresh.admit(event.clone());
                        }
                    }
                    continue;
                }
            };
            if unvisited {
                roots_cache.visit(home, &path);
            }
            hr.files += 1;
            hr.malformed += chunk.oversized;
            if need_first
                && let Some(ts) = first_timestamp(&path)
                && earliest.is_none_or(|e| ts < e)
            {
                earliest = Some(ts);
            }
            if chunk.lines.is_empty() {
                cursors.insert(key, chunk.cursor);
                continue;
            }
            let header = chunk.header.take().or_else(|| tail::first_line(&path).ok().flatten());
            let parsed = adapter.parse(&roots, &path, header.as_deref(), &chunk);
            hr.malformed += parsed.malformed;
            hr.unparsed += parsed.unparsed.len();
            for s in parsed.unparsed {
                if hr.unparsed_samples.len() < 5 {
                    hr.unparsed_samples.push(crate::render::inert(&s.chars().take(200).collect::<String>()));
                }
            }
            if parsed.raws.is_empty() && !parsed.skipped && has_canary(&chunk) {
                hr.canary.push(key.clone());
            }
            let target = if reparse { &mut fresh } else { &mut ledger };
            admit(&classifier, &mut roots_cache, parsed.raws, &path, target, &mut hr);
            cursors.insert(key, chunk.cursor);
        }

        let loaded = hr.new_events.iter().map(|e| e.ts).min();
        if home.agent == Agent::Claude && first_scan {
            let raws = adapters::claude::backfill(home, &inv.known);
            let target = if reparse { &mut fresh } else { &mut ledger };
            admit(&classifier, &mut roots_cache, raws, &home.path.join("history.jsonl"), target, &mut hr);
        }

        let entry = coverage.entry(home.name.clone()).or_default();
        entry.first = [entry.first_at(), earliest, loaded].into_iter().flatten().min().map(|t| t.to_string());
        if hr.io_errors.is_empty() {
            entry.last_scan = Some(jiff::Timestamp::now().to_string());
        }
        report.homes.insert(home.name.clone(), hr);
    }

    if reparse {
        ledger.replace(fresh.events())?;
    } else {
        ledger.append(cfg.homes.iter().flat_map(|h| &report.homes[&h.name].new_events))?;
    }
    roots_cache.save(data)?;
    cursors.retain(|k, _| listed.contains(k));
    ledger::write_json(&data.join("cursors.json"), &cursors)?;
    ledger::save_coverage(data, &coverage)?;
    report.events = if reparse { fresh } else { ledger }.into_events();
    Ok(report)
}

fn admit(
    classifier: &Classifier,
    cache: &mut Roots,
    raws: Vec<Raw>,
    path: &Path,
    target: &mut Ledger,
    hr: &mut HomeReport,
) {
    for raw in raws {
        let event = match classifier.event(cache, raw, path) {
            Ok(event) => event,
            Err(Dropped::Timestamp) => {
                hr.bad_timestamps += 1;
                continue;
            }
            Err(Dropped::AuditRun) => continue,
        };
        hr.new_events.extend(target.admit(event));
    }
}

fn should_show_progress(is_tty: bool, elapsed: Duration) -> bool {
    is_tty && elapsed > Duration::from_secs(1)
}

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
    chunk.lines.iter().any(|(_, l)| adapters::markers().any(|m| l.windows(m.len()).any(|w| w == m.as_bytes())))
}

pub(crate) fn first_timestamp(path: &Path) -> Option<jiff::Timestamp> {
    tail::head(path)
        .ok()?
        .take(20)
        .filter_map(|line| {
            serde_json::from_slice::<serde_json::Value>(&line).ok()?.get("timestamp")?.as_str()?.parse().ok()
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
