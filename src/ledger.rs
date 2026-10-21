//! The append-only event ledger: schema, append rule, lock, coverage.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
    Pi,
}

impl Agent {
    pub fn letter(self) -> char {
        match self {
            Agent::Claude => 'C',
            Agent::Codex => 'X',
            Agent::Pi => 'P',
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Slash,
    Tool,
    Typed,
    Read,
    Ref,
}

impl Channel {
    /// Strong channels feed the headline `sess` column; weak ones never do.
    pub fn strong(self) -> bool {
        matches!(self, Channel::Slash | Channel::Tool | Channel::Typed)
    }
}

/// The serde name of a unit variant, as the ledger and `--json` write it.
pub fn name(v: impl Serialize) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Probe,
    Subagent,
    Headless,
    Main,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Canonical,
    Other,
}

/// Event schema v1. Field order is the serialised key order.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    pub v: u32,
    pub id: String,
    pub ts: String,
    pub agent: Agent,
    pub home: String,
    pub agent_version: Option<String>,
    pub session: String,
    pub actor: Actor,
    pub skill: String,
    pub origin: Origin,
    pub channel: Channel,
    pub path: Option<String>,
    pub cwd: Option<String>,
    pub authoring: bool,
    pub src: String,
}

impl Event {
    /// The transcript file named by `src` (`<path>:<line>`).
    pub fn src_file(&self) -> &str {
        self.src.rsplit_once(':').map_or(&self.src, |(f, _)| f)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Coverage {
    pub first: Option<String>,
    pub last_scan: Option<String>,
}

/// `${XDG_DATA_HOME:-~/.local/share}/skima`.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|x| !x.is_empty()) {
        return Ok(PathBuf::from(x).join("skima"));
    }
    Ok(PathBuf::from(crate::config::home_dir()?).join(".local/share/skima"))
}

/// The ledger, held under `scan.lock` while a scan or reparse writes it.
pub struct Ledger {
    dir: PathBuf,
    /// Last line per id, in first-appearance order of the id.
    events: Vec<Event>,
    index: HashMap<String, usize>,
    _lock: Option<File>,
}

impl Ledger {
    /// Read-only load: no lock, keeps the last line for each id.
    pub fn load(dir: &Path) -> Result<Ledger> {
        let mut ledger = Ledger { dir: dir.to_path_buf(), events: Vec::new(), index: HashMap::new(), _lock: None };
        let path = dir.join("events.jsonl");
        for (n, event) in jsonl::<Event>(&path)? {
            // A torn line (crash mid-append) is skipped; the next scan re-derives it.
            match event {
                Some(event) => ledger.keep(event),
                None => eprintln!("warning: {}:{n}: unreadable ledger line skipped", path.display()),
            }
        }
        Ok(ledger)
    }

    /// Takes `scan.lock` with a blocking flock, then loads the id set.
    pub fn open_locked(dir: &Path) -> Result<Ledger> {
        let lock = lock(dir)?;
        let mut ledger = Ledger::load(dir)?;
        ledger._lock = Some(lock);
        Ok(ledger)
    }

    /// An empty in-memory ledger for the same directory (reparse builds into it).
    pub fn empty_like(other: &Ledger) -> Ledger {
        Ledger { dir: other.dir.clone(), events: Vec::new(), index: HashMap::new(), _lock: None }
    }

    fn keep(&mut self, event: Event) {
        match self.index.get(&event.id) {
            Some(&i) => self.events[i] = event,
            None => {
                self.index.insert(event.id.clone(), self.events.len());
                self.events.push(event);
            }
        }
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn get(&self, id: &str) -> Option<&Event> {
        self.index.get(id).map(|&i| &self.events[i])
    }

    /// The append rule. Returns the line to append, if any: a new id, or a
    /// `path: null` event upgraded with a path, which copies the stored event and
    /// sets only `path`.
    pub fn admit(&mut self, event: Event) -> Option<Event> {
        let line = match self.get(&event.id) {
            None => event,
            Some(stored) if stored.path.is_none() && event.path.is_some() => {
                Event { path: event.path, ..stored.clone() }
            }
            Some(_) => return None,
        };
        self.keep(line.clone());
        Some(line)
    }

    /// Appends lines admitted by [`Ledger::admit`].
    pub fn append(&self, lines: &[Event]) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        append_lines(&self.dir.join("events.jsonl"), to_lines(lines))
    }

    /// Replaces the ledger file with `lines` (reparse only).
    pub fn replace(&self, lines: &[Event]) -> Result<()> {
        write_atomic(&self.dir.join("events.jsonl"), to_lines(lines).as_bytes())
    }
}

/// Takes `<dir>/scan.lock` with a blocking flock, held until the file is
/// dropped. Every writer of the data directory holds it.
pub fn lock(dir: &Path) -> Result<File> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("scan.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    lock.lock().map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(lock)
}

/// Appends whole lines to an append-only log. A crash mid-append leaves a torn
/// last line; appending to it would make the first new line unreadable too, so
/// the new lines start on their own.
pub(crate) fn append_lines(path: &Path, mut out: String) -> Result<()> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut f = OpenOptions::new().create(true).read(true).append(true).open(path).map_err(err)?;
    if f.metadata().map_err(err)?.len() > 0 {
        let mut last = [0u8];
        f.seek(SeekFrom::End(-1)).map_err(err)?;
        f.read_exact(&mut last).map_err(err)?;
        if last[0] != b'\n' {
            out.insert(0, '\n');
        }
    }
    f.write_all(out.as_bytes()).map_err(err)?;
    f.sync_data().map_err(|e| err(e).into())
}

pub fn load_coverage(dir: &Path) -> Result<BTreeMap<String, Coverage>> {
    read_json(&dir.join("coverage.json"))
}

pub fn save_coverage(dir: &Path, coverage: &BTreeMap<String, Coverage>) -> Result<()> {
    write_json(&dir.join("coverage.json"), coverage)
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()).into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("{}: {e}", path.display()).into()),
    }
}

fn to_lines(events: &[Event]) -> String {
    events.iter().map(|e| serde_json::to_string(e).expect("event serialises") + "\n").collect()
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_atomic(path, &serde_json::to_vec(value).expect("state serialises"))
}

/// Write to `<path>.new`, fsync it, then rename over `path`, so a crash
/// leaves either the old or the new file, never a truncated one.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let mut f = File::create(&tmp).map_err(|e| err(&tmp, e))?;
    f.write_all(bytes).map_err(|e| err(&tmp, e))?;
    f.sync_all().map_err(|e| err(&tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| err(path, e).into())
}

/// Each non-blank line of the JSONL file at `path`, numbered from 1, parsed as
/// `T` or `None` when it does not parse; empty when the file is missing. Lines
/// are split as bytes, so a torn write that cuts a UTF-8 character costs that
/// line, not the file.
pub fn jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<(usize, Option<T>)>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display()).into()),
    };
    let mut lines = Vec::new();
    for (n, line) in BufReader::new(file).split(b'\n').enumerate() {
        let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
        if !line.trim_ascii().is_empty() {
            lines.push((n + 1, serde_json::from_slice(&line).ok()));
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn event(id: &str, path: Option<&str>) -> Event {
        Event {
            v: 1,
            id: id.into(),
            ts: "2026-10-01T00:00:00Z".into(),
            agent: Agent::Claude,
            home: "claude".into(),
            agent_version: None,
            session: "s".into(),
            actor: Actor::Main,
            skill: "unslop".into(),
            origin: Origin::Canonical,
            channel: Channel::Tool,
            path: path.map(Into::into),
            cwd: None,
            authoring: false,
            src: "/t.jsonl:3".into(),
        }
    }

    #[test]
    fn append_rule_admits_new_ids_and_path_upgrades_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open_locked(dir.path()).unwrap();
        assert!(l.admit(event("a", None)).is_some());
        assert!(l.admit(event("a", None)).is_none());
        let mut later = event("a", Some("/x/SKILL.md"));
        later.src = "/t.jsonl:4".into();
        let up = l.admit(later).unwrap();
        assert_eq!(up.path.as_deref(), Some("/x/SKILL.md"));
        assert_eq!(up.src, "/t.jsonl:3", "an upgrade copies the stored event");
        assert!(l.admit(event("a", Some("/y/SKILL.md"))).is_none());
    }

    #[test]
    fn last_line_wins_and_torn_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let a = serde_json::to_string(&event("a", None)).unwrap();
        let b = serde_json::to_string(&event("a", Some("/p"))).unwrap();
        fs::write(dir.path().join("events.jsonl"), format!("{a}\n{b}\n{{\"v\":1,\"id\"")).unwrap();
        let l = Ledger::load(dir.path()).unwrap();
        assert_eq!(l.events().len(), 1);
        assert_eq!(l.events()[0].path.as_deref(), Some("/p"));
    }

    #[test]
    fn serialised_key_order_matches_schema() {
        let s = serde_json::to_string(&event("a", None)).unwrap();
        assert!(s.starts_with(r#"{"v":1,"id":"a","ts":"#));
        assert!(s.ends_with(r#""authoring":false,"src":"/t.jsonl:3"}"#));
    }
}
