use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};

use crate::Result;
use crate::adapters::{Key, Parent, Raw, Session};
use crate::config::Home;
use crate::inventory::Inventory;
use crate::ledger::{self, Actor, Agent, Event, Origin};

const ROOTS: &str = "roots.json";

#[derive(Default)]
pub struct Roots {
    stored: BTreeMap<String, String>,
    codex_files: HashMap<PathBuf, HashMap<String, PathBuf>>,
    headers: HashMap<PathBuf, Option<Session>>,
}

impl Roots {
    pub fn load(data: &Path) -> Result<Option<Roots>> {
        Ok(ledger::read_derived(&data.join(ROOTS))?.map(|stored| Roots { stored, ..Roots::default() }))
    }

    pub fn save(&self, data: &Path) -> Result<()> {
        ledger::write_json(&data.join(ROOTS), &self.stored)
    }

    pub fn visit(&mut self, home: &Home, file: &Path) {
        if let Some(s) = self.header(home.agent, file) {
            self.root(home, &s.id, s.parent.as_ref());
        }
    }

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
                None => return id,
                Some(None) => return self.store(passed, id),
                Some(Some(p)) => parent = p,
            }
            passed.push(id);
        }
        match parent {
            Parent::Codex(id) => id,
            Parent::Pi(_) => passed.pop().unwrap_or_default(),
        }
    }

    fn store(&mut self, passed: Vec<String>, root: String) -> String {
        for id in passed {
            self.stored.entry(id).or_insert_with(|| root.clone());
        }
        root
    }

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
    pub audit_runs: &'a Path,
    pub now: Timestamp,
}

const SKEW: SignedDuration = SignedDuration::from_hours(24);

#[derive(Debug, PartialEq)]
pub enum Dropped {
    AuditRun,
    Timestamp,
}

impl Classifier<'_> {
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
        let origin = if raw.name_origin {
            self.name_origin(&raw.skill, raw.channel)
        } else {
            self.path_origin(&raw.skill, raw.path.as_deref())
        };
        let cwd = raw.cwd.or(raw.session.cwd.clone());
        let authoring = !raw.channel.strong()
            && cwd.as_deref().is_some_and(|c| {
                let c = Path::new(c);
                self.inv.worktree.as_deref().is_some_and(|w| c.starts_with(w))
                    || self.homes.iter().any(|h| c.starts_with(h.skills_dir()))
            });
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
            src: crate::render::inert(&format!("{}:{}", file.to_string_lossy(), raw.line)),
        })
    }

    fn name_origin(&self, skill: &str, channel: crate::ledger::Channel) -> Origin {
        let names = if channel == crate::ledger::Channel::Typed { &self.inv.skills } else { &self.inv.known };
        if names.contains(skill) { Origin::Canonical } else { Origin::Other }
    }

    fn path_origin(&self, skill: &str, path: Option<&str>) -> Origin {
        let Some(path) = path else {
            return if self.inv.known.contains(skill) { Origin::Canonical } else { Origin::Other };
        };
        let p = Path::new(path);
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
