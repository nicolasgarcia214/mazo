//! Identity, actor, root session, origin and the stored authoring tag,
//! computed once for every adapter.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};

use crate::Result;
use crate::adapters::{Key, Parent, Raw, Session};
use crate::config::Home;
use crate::inventory::Inventory;
use crate::ledger::{self, Actor, Agent, Event, Origin};

/// `<data dir>/roots.json`: Codex rollout id or Pi session id → its root session id.
const ROOTS: &str = "roots.json";

/// Walks session chains to their root. A root a walk reaches is stored and
/// used without walking from then on, so a session keeps it after a file in
/// its chain is deleted; header lookups are cached for one scan.
#[derive(Default)]
pub struct Roots {
    stored: BTreeMap<String, String>,
    /// Codex rollout id → its file, per transcripts dir, built lazily from file names.
    codex_files: HashMap<PathBuf, HashMap<String, PathBuf>>,
    headers: HashMap<PathBuf, Option<Session>>,
}

impl Roots {
    /// The roots stored in `data`; `None` when `roots.json` is missing or
    /// corrupt and must be rebuilt with [`Roots::visit`].
    pub fn load(data: &Path) -> Result<Option<Roots>> {
        Ok(ledger::read_derived(&data.join(ROOTS))?.map(|stored| Roots { stored, ..Roots::default() }))
    }

    /// Writes the stored roots to `data`; the caller holds the scan lock.
    pub fn save(&self, data: &Path) -> Result<()> {
        ledger::write_json(&data.join(ROOTS), &self.stored)
    }

    /// Walks the session in the transcript `file` of `home` to its root.
    pub fn visit(&mut self, home: &Home, file: &Path) {
        if let Some(s) = self.header(home.agent, file) {
            self.root(home, &s.id, s.parent.as_ref());
        }
    }

    /// The root of session `id`: its stored root, else the end of the walk up
    /// its parents. A walk that reaches a session with no parent, or a stored
    /// root, stores that root for every session it passed; one that reaches a
    /// missing file ends at that file's id and stores nothing.
    pub fn root(&mut self, home: &Home, id: &str, parent: Option<&Parent>) -> String {
        let Some(parent) = parent else { return id.to_string() };
        if let Some(root) = self.stored.get(id) {
            return root.clone();
        }
        let mut passed = vec![id.to_string()];
        let mut parent = parent.clone();
        for _ in 0..64 {
            let (id, header) = self.session(home, &parent);
            if let Some(root) = self.stored.get(&id).cloned() {
                return self.store(passed, root);
            }
            match header.map(|s| s.parent) {
                // A missing file ends the walk at its id and stores nothing.
                None => return id,
                Some(None) => return self.store(passed, id),
                Some(Some(p)) => parent = p,
            }
            passed.push(id);
        }
        // A walk cut off at 64 steps stores nothing: Codex ends at the next
        // unread parent's id, Pi at the last session whose header was read.
        match parent {
            Parent::Codex(id) => id,
            Parent::Pi(_) => passed.pop().unwrap_or_default(),
        }
    }

    /// Stores `root` for every session in `passed` that has none yet.
    fn store(&mut self, passed: Vec<String>, root: String) -> String {
        for id in passed {
            self.stored.entry(id).or_insert_with(|| root.clone());
        }
        root
    }

    /// The id of the session `parent` names, and its header while its file is
    /// on disk. A missing Pi file's id comes from its name, `<ts>_<id>.jsonl`.
    fn session(&mut self, home: &Home, parent: &Parent) -> (String, Option<Session>) {
        match parent {
            Parent::Codex(id) => {
                let header = self.codex_file(home, id).and_then(|f| self.header(Agent::Codex, &f));
                (id.clone(), header)
            }
            Parent::Pi(file) => match self.header(Agent::Pi, file) {
                Some(s) => (s.id.clone(), Some(s)),
                None => {
                    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
                    (stem.rsplit_once('_').map_or(&*stem, |(_, id)| id).to_string(), None)
                }
            },
        }
    }

    fn codex_file(&mut self, home: &Home, id: &str) -> Option<PathBuf> {
        let files = self.codex_files.entry(home.transcripts()).or_insert_with_key(|dir| {
            crate::adapters::jsonl_files(dir)
                .into_iter()
                .filter_map(|p| {
                    let stem = p.file_stem()?.to_str()?.to_string();
                    // rollout-<start time>-<id>: the id is the trailing UUID (36 chars).
                    let id = stem.get(stem.len().checked_sub(36)?..)?.to_string();
                    Some((id, p))
                })
                .collect()
        });
        files.get(id).cloned()
    }

    fn header(&mut self, agent: Agent, file: &Path) -> Option<Session> {
        let parse = match agent {
            Agent::Codex => crate::adapters::codex::session_of_header,
            Agent::Pi => crate::adapters::pi::session_of_header,
            Agent::Claude => return None,
        };
        self.headers.entry(file.to_path_buf()).or_insert_with(|| parse(&crate::tail::first_line(file).ok()??)).clone()
    }
}

pub struct Classifier<'a> {
    pub inv: &'a Inventory,
    pub home: &'a Home,
    pub homes: &'a [Home],
    /// `<data dir>/audit-runs`, where the judge runs.
    pub audit_runs: &'a Path,
    /// When the scan started: a record dated more than [`SKEW`] after it is corrupt.
    pub now: Timestamp,
}

/// How far ahead of the scan's clock a record may be dated, for transcripts
/// synced from a machine whose clock runs ahead.
const SKEW: SignedDuration = SignedDuration::from_hours(24);

/// Why [`Classifier::event`] makes no event of a raw one.
#[derive(Debug, PartialEq)]
pub enum Dropped {
    /// The session's cwd is inside `<data dir>/audit-runs/`: the judge's own
    /// runs are never usage.
    AuditRun,
    /// The record's timestamp does not parse or lies past `now` and [`SKEW`].
    Timestamp,
}

impl Classifier<'_> {
    /// The event for `raw`, or why there is none.
    pub fn event(&self, roots: &mut Roots, raw: Raw, file: &Path) -> std::result::Result<Event, Dropped> {
        if raw.session.cwd.as_deref().is_some_and(|c| Path::new(c).starts_with(self.audit_runs)) {
            return Err(Dropped::AuditRun);
        }
        let ts = raw
            .ts
            .parse::<Timestamp>()
            .ok()
            .filter(|t| t.duration_since(self.now) <= SKEW)
            .ok_or(Dropped::Timestamp)?;
        let session = roots.root(self.home, &raw.session.id, raw.session.parent.as_ref());
        let id = match &raw.key {
            Key::Plain(s) => s.clone(),
            Key::Codex { hash, index } => format!("cx:{session}:{hash}:{index}"),
        };
        // A Skill result keeps its name-based origin even after the meta record adds a
        // path, so a merged event equals a split-then-upgraded one.
        let origin = if raw.name_origin {
            self.name_origin(&raw.skill, raw.channel)
        } else {
            self.path_origin(&raw.skill, raw.path.as_deref())
        };
        let cwd = raw.cwd.or(raw.session.cwd.clone());
        // Only weak reads can be authoring; the stored part is the cwd rule.
        let authoring = !raw.channel.strong()
            && cwd.as_deref().is_some_and(|c| {
                let c = Path::new(c);
                self.inv.worktree.as_deref().is_some_and(|w| c.starts_with(w))
                    || self.homes.iter().any(|h| c.starts_with(h.skills_dir()))
            });
        // Transcript fields are anyone's text: stored inert, so no report,
        // card or TUI can print a control character from one.
        let inert = |s: Option<String>| s.as_deref().map(crate::render::inert);
        Ok(Event {
            v: 1,
            id,
            ts,
            agent: self.home.agent,
            home: self.home.name.clone(),
            agent_version: raw.session.version.clone(),
            session,
            actor: actor(self.home.agent, &raw.session.actor),
            skill: crate::render::inert(&raw.skill),
            origin,
            channel: raw.channel,
            path: inert(raw.path),
            cwd: inert(cwd),
            authoring,
            src: crate::render::inert(&format!("{}:{}", file.display(), raw.line)),
        })
    }

    /// Claude Skill results and typed history: the name alone decides. A Skill
    /// result needs a known name, so renamed or pruned skills keep their
    /// events; a typed line needs a skill canonical now, because history has
    /// no path to check and a name such as `argus` can be an old skill or a
    /// later plugin command.
    fn name_origin(&self, skill: &str, channel: crate::ledger::Channel) -> Origin {
        let names = if channel == crate::ledger::Channel::Typed { &self.inv.skills } else { &self.inv.known };
        if names.contains(skill) { Origin::Canonical } else { Origin::Other }
    }

    fn path_origin(&self, skill: &str, path: Option<&str>) -> Origin {
        let Some(path) = path else {
            return if self.inv.known.contains(skill) { Origin::Canonical } else { Origin::Other };
        };
        let p = Path::new(path);
        // Under any skill root, the store's included, whether reached through a home link or not.
        let rooted = |r: PathBuf| self.inv.roots.iter().any(|root| r.starts_with(root));
        if std::fs::canonicalize(p).is_ok_and(rooted) {
            return Origin::Canonical;
        }
        let in_home_skills = self.homes.iter().any(|h| {
            p.strip_prefix(h.skills_dir())
                .ok()
                .and_then(|rest| rest.components().next())
                .is_some_and(|c| c.as_os_str() == skill)
        });
        if in_home_skills && self.inv.known.contains(skill) { Origin::Canonical } else { Origin::Other }
    }
}

/// The actor, first matching rule wins.
pub fn actor(agent: Agent, a: &crate::adapters::ActorInputs) -> Actor {
    let ep = a.entrypoint.as_deref();
    if agent == Agent::Claude && ep.is_some_and(|e| e != "cli" && e != "sdk-cli") {
        Actor::Probe
    } else if a.subagent {
        Actor::Subagent
    } else if ep == Some("sdk-cli") || a.codex_exec {
        Actor::Headless
    } else {
        Actor::Main
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::ActorInputs;

    #[test]
    fn actor_rules_apply_in_order() {
        let a = |ep: Option<&str>, sub: bool, exec: bool| ActorInputs {
            entrypoint: ep.map(Into::into),
            subagent: sub,
            codex_exec: exec,
        };
        assert_eq!(actor(Agent::Claude, &a(Some("bench"), true, false)), Actor::Probe);
        assert_eq!(actor(Agent::Claude, &a(Some("cli"), true, false)), Actor::Subagent);
        assert_eq!(actor(Agent::Claude, &a(Some("sdk-cli"), false, false)), Actor::Headless);
        assert_eq!(actor(Agent::Codex, &a(None, false, true)), Actor::Headless);
        assert_eq!(actor(Agent::Pi, &a(None, false, false)), Actor::Main);
    }

    #[test]
    fn a_load_at_the_store_path_is_canonical_behind_a_symlinked_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = tmp.path().canonicalize().unwrap();
        let (source, data) = (tmp.join("source"), tmp.join("data"));
        std::fs::create_dir(tmp.join("real")).unwrap();
        std::os::unix::fs::symlink(tmp.join("real"), &data).unwrap();
        let store = data.join("store");
        for dir in [source.join("mine"), store.join("theirs"), tmp.join("elsewhere/theirs")] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "x").unwrap();
        }
        let inv = crate::store::inventory(Some(&source), &data).unwrap();
        let home =
            Home { name: "claude".into(), agent: Agent::Claude, path: tmp.join("home"), links: Default::default() };
        let homes = [home.clone()];
        let c = Classifier {
            inv: &inv,
            home: &home,
            homes: &homes,
            audit_runs: &tmp.join("audit-runs"),
            now: Timestamp::now(),
        };
        let origin = |skill: &str, dir: &Path| c.path_origin(skill, dir.join(skill).join("SKILL.md").to_str());
        assert_eq!(origin("mine", &source), Origin::Canonical);
        assert_eq!(origin("theirs", &store), Origin::Canonical);
        assert_eq!(origin("theirs", &tmp.join("elsewhere")), Origin::Other);
    }
}
