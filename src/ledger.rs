//! The append-only event ledger: schema, append rule, lock, coverage.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
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
    pub ts: jiff::Timestamp,
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

impl Coverage {
    /// `first` as a timestamp; one that does not parse counts as none, so a
    /// scan fills it again.
    pub fn first_at(&self) -> Option<jiff::Timestamp> {
        self.first.as_deref()?.parse().ok()
    }
}

/// The ledger, held under `scan.lock` while a scan or reparse writes it.
pub struct Ledger {
    dir: PathBuf,
    /// Last line per id, in first-appearance order of the id.
    events: Vec<Event>,
    index: HashMap<String, usize>,
    /// Whether a line of the file did not parse, so the next write rewrites it.
    unreadable: bool,
    _lock: Option<File>,
}

impl Ledger {
    /// Read-only load: no lock, keeps the last line for each id.
    pub fn load(dir: &Path) -> Result<Ledger> {
        let mut ledger = Ledger::empty(dir);
        let path = dir.join("events.jsonl");
        for (n, event) in jsonl::<Event>(&path)? {
            // A torn line (crash mid-append), or one from before timestamps were
            // checked whose `ts` does not parse, is skipped, and the next scan
            // drops it from the file; `scan --reparse` re-derives its event.
            match event {
                Some(event) => ledger.keep(event),
                None => {
                    ledger.unreadable = true;
                    eprintln!("warning: {}:{n}: unreadable ledger line skipped", path.display());
                }
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
        Ledger::empty(&other.dir)
    }

    fn empty(dir: &Path) -> Ledger {
        Ledger { dir: dir.to_path_buf(), events: Vec::new(), index: HashMap::new(), unreadable: false, _lock: None }
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

    pub fn into_events(self) -> Vec<Event> {
        self.events
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

    /// Appends lines admitted by [`Ledger::admit`], or rewrites the file
    /// without its unreadable lines when it has any.
    pub fn append(&self, lines: &[Event]) -> Result<()> {
        if self.unreadable {
            return self.replace(&self.events);
        }
        if lines.is_empty() {
            return Ok(());
        }
        append_lines(&self.dir.join("events.jsonl"), to_lines(lines))
    }

    /// Replaces the ledger file with `lines`.
    pub fn replace(&self, lines: &[Event]) -> Result<()> {
        write_atomic(&self.dir.join("events.jsonl"), to_lines(lines).as_bytes(), 0o666)
    }
}

/// Takes `<dir>/scan.lock` with a blocking flock, held until the file is
/// dropped. Every writer of the data directory holds it.
pub fn lock(dir: &Path) -> Result<File> {
    fs::create_dir_all(dir).map_err(crate::at(dir))?;
    let path = dir.join("scan.lock");
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(crate::at(&path))?;
    lock.lock().map_err(crate::at(&path))?;
    Ok(lock)
}

/// Appends whole lines to an append-only log. A crash mid-append leaves a torn
/// last line; appending to it would make the first new line unreadable too, so
/// the new lines start on their own.
pub(crate) fn append_lines(path: &Path, mut out: String) -> Result<()> {
    let err = crate::at(path);
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
    Ok(f.sync_data().map_err(err)?)
}

/// Rebuilt from the transcripts still on disk when the file does not parse,
/// so a home whose oldest transcripts are gone reports a later start.
pub fn load_coverage(dir: &Path) -> Result<BTreeMap<String, Coverage>> {
    Ok(read_derived(&dir.join("coverage.json"))?.unwrap_or_default())
}

pub fn save_coverage(dir: &Path, coverage: &BTreeMap<String, Coverage>) -> Result<()> {
    write_json(&dir.join("coverage.json"), coverage)
}

/// State a scan rebuilds from the transcripts, such as cursors, roots and coverage:
/// `None` when the file is missing or, with a warning, does not parse.
pub(crate) fn read_derived<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    Ok(read(path)?.and_then(|bytes| {
        serde_json::from_slice(&bytes).map_err(|e| eprintln!("warning: {}: {e}; rebuilding it", path.display())).ok()
    }))
}

/// The bytes of `path`, `None` when it is missing.
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::at(path)(e).into()),
    }
}

fn to_lines(events: &[Event]) -> String {
    events.iter().map(|e| serde_json::to_string(e).expect("event serialises") + "\n").collect()
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_atomic(path, &serde_json::to_vec(value).expect("state serialises"), 0o666)
}

/// Write to a fresh `<path>.new` created with `mode` (less the umask), fsync
/// it, rename it over `path`, then fsync the parent directory. A crash leaves
/// either the old or the new file, never a truncated one; once this returns
/// `Ok` the new file survives power loss. A symlink at either path is
/// replaced, never followed.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(crate::at(&tmp)(e).into()),
        _ => {}
    }
    let mut f = OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp).map_err(crate::at(&tmp))?;
    f.write_all(bytes).map_err(crate::at(&tmp))?;
    f.sync_all().map_err(crate::at(&tmp))?;
    fs::rename(&tmp, path).map_err(crate::at(path))?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Ok(fs::File::open(parent).and_then(|d| d.sync_all()).map_err(crate::at(parent))?)
}

/// Each non-blank line of the JSONL file at `path`, numbered from 1, parsed as
/// `T` or `None` when it does not parse; empty when the file is missing. Lines
/// are split as bytes, so a torn write that cuts a UTF-8 character costs that
/// line, not the file.
pub fn jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<(usize, Option<T>)>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(crate::at(path)(e).into()),
    };
    let mut lines = Vec::new();
    for (n, line) in BufReader::new(file).split(b'\n').enumerate() {
        let line = line.map_err(crate::at(path))?;
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
            ts: "2026-10-01T00:00:00Z".parse().unwrap(),
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
    fn write_atomic_replaces_a_file_in_a_fresh_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh").join("state.json");
        fs::create_dir(path.parent().unwrap()).unwrap();
        write_atomic(&path, b"old", 0o600).unwrap();
        write_atomic(&path, b"new", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(!path.with_extension("json.new").exists());
    }

    #[test]
    fn serialised_key_order_matches_schema() {
        let s = serde_json::to_string(&event("a", None)).unwrap();
        assert!(s.starts_with(r#"{"v":1,"id":"a","ts":"#));
        assert!(s.ends_with(r#""authoring":false,"src":"/t.jsonl:3"}"#));
    }
}
