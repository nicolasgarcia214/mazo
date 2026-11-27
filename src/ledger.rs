use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::durable::{append_lines, jsonl, lock, write_atomic};
use crate::render::PathExt;

#[derive(
    Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, clap::ValueEnum,
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

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Slash,
    Tool,
    Typed,
    Read,
    Ref,
}

impl Channel {
    pub fn strong(self) -> bool {
        matches!(self, Channel::Slash | Channel::Tool | Channel::Typed)
    }
}

pub fn name(v: impl Serialize) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Probe,
    Subagent,
    Headless,
    Main,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Canonical,
    Other,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
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
    pub fn src_file(&self) -> &str {
        self.src.rsplit_once(':').map_or(&self.src, |(f, _)| f)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Coverage {
    pub first: Option<String>,
    pub last_scan: Option<String>,
}

impl Coverage {
    pub fn first_at(&self) -> Option<jiff::Timestamp> {
        self.first.as_deref()?.parse().ok()
    }
}

pub struct Ledger {
    dir: PathBuf,
    events: Vec<Event>,
    index: HashMap<String, usize>,
    unreadable: bool,
    _lock: Option<File>,
}

impl Ledger {
    pub fn load(dir: &Path) -> Result<Ledger> {
        let mut ledger = Ledger::empty(dir);
        let path = dir.join("events.jsonl");
        for (n, event) in jsonl::<Event>(&path)? {
            match event {
                Some(event) => ledger.keep(event),
                None => {
                    ledger.unreadable = true;
                    eprintln!("warning: {}:{n}: unreadable ledger line skipped", path.inert());
                }
            }
        }
        Ok(ledger)
    }

    pub fn open_locked(dir: &Path) -> Result<Ledger> {
        let lock = lock(dir)?;
        let mut ledger = Ledger::load(dir)?;
        ledger._lock = Some(lock);
        Ok(ledger)
    }

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

    pub fn append<'a>(&self, lines: impl IntoIterator<Item = &'a Event>) -> Result<()> {
        if self.unreadable {
            return self.replace(&self.events);
        }
        let text = to_lines(lines);
        if text.is_empty() {
            return Ok(());
        }
        append_lines(&self.dir.join("events.jsonl"), text)
    }

    pub fn replace(&self, lines: &[Event]) -> Result<()> {
        write_atomic(&self.dir.join("events.jsonl"), to_lines(lines).as_bytes(), 0o600)
    }
}

pub fn load_coverage(dir: &Path) -> Result<BTreeMap<String, Coverage>> {
    Ok(read_derived(&dir.join("coverage.json"))?.unwrap_or_default())
}

pub fn save_coverage(dir: &Path, coverage: &BTreeMap<String, Coverage>) -> Result<()> {
    write_json(&dir.join("coverage.json"), coverage)
}

pub(crate) fn read_derived<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    Ok(read(path)?.and_then(|bytes| {
        serde_json::from_slice(&bytes).map_err(|e| eprintln!("warning: {}: {e}; rebuilding it", path.inert())).ok()
    }))
}

fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::at(path)(e).into()),
    }
}

fn to_lines<'a>(events: impl IntoIterator<Item = &'a Event>) -> String {
    events.into_iter().map(|e| serde_json::to_string(e).expect("event serialises") + "\n").collect()
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_atomic(path, &serde_json::to_vec(value).expect("state serialises"), 0o600)
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
    fn serialised_key_order_matches_schema() {
        let s = serde_json::to_string(&event("a", None)).unwrap();
        assert!(s.starts_with(r#"{"v":1,"id":"a","ts":"#));
        assert!(s.ends_with(r#""authoring":false,"src":"/t.jsonl:3"}"#));
    }
}
