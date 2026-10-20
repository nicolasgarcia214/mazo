//! Identity, actor, root session, origin and the stored authoring tag,
//! computed once for every adapter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::adapters::{Key, Parent, Raw};
use crate::config::Home;
use crate::inventory::Inventory;
use crate::ledger::{Actor, Agent, Event, Origin};

/// Walks session chains to their root, caching every lookup for one scan.
#[derive(Default)]
pub struct Roots {
    /// Codex rollout id → its file, built lazily from file names.
    codex_files: Option<HashMap<String, PathBuf>>,
    codex_parent: HashMap<String, Option<String>>,
    pi_header: HashMap<PathBuf, Option<(String, Option<PathBuf>)>>,
}

impl Roots {
    pub fn root(&mut self, home: &Home, id: &str, parent: Option<&Parent>) -> String {
        match parent {
            None => id.to_string(),
            Some(Parent::Codex(p)) => self.codex_root(home, p),
            Some(Parent::Pi(file)) => self.pi_root(file).unwrap_or_else(|| id.to_string()),
        }
    }

    fn codex_root(&mut self, home: &Home, start: &str) -> String {
        let mut id = start.to_string();
        for _ in 0..64 {
            let parent = match self.codex_parent.get(&id) {
                Some(p) => p.clone(),
                None => {
                    let p = self.codex_lookup(home, &id);
                    self.codex_parent.insert(id.clone(), p.clone());
                    p
                }
            };
            match parent {
                Some(p) => id = p,
                None => break,
            }
        }
        id
    }

    fn codex_lookup(&mut self, home: &Home, id: &str) -> Option<String> {
        let files = self.codex_files.get_or_insert_with(|| {
            crate::adapters::jsonl_files(&home.transcripts())
                .into_iter()
                .filter_map(|p| {
                    let stem = p.file_stem()?.to_str()?.to_string();
                    // rollout-<start time>-<id>: the id is the trailing UUID (36 chars).
                    let id = stem.get(stem.len().checked_sub(36)?..)?.to_string();
                    Some((id, p))
                })
                .collect()
        });
        let path = files.get(id)?;
        let header = crate::tail::first_line(path).ok()??;
        crate::adapters::codex::parent_of_header(&header)
    }

    fn pi_root(&mut self, file: &Path) -> Option<String> {
        let mut file = file.to_path_buf();
        let mut last_id = None;
        for _ in 0..64 {
            let entry = match self.pi_header.get(&file) {
                Some(h) => h.clone(),
                None => {
                    let h = crate::tail::first_line(&file)
                        .ok()
                        .flatten()
                        .and_then(|l| crate::adapters::pi::session_of_header(&l))
                        .map(|s| match s.parent {
                            Some(Parent::Pi(p)) => (s.id, Some(p)),
                            _ => (s.id, None),
                        });
                    self.pi_header.insert(file.clone(), h.clone());
                    h
                }
            };
            let Some((id, parent)) = entry else { break };
            last_id = Some(id);
            match parent {
                Some(p) => file = p,
                None => break,
            }
        }
        last_id
    }
}

pub struct Classifier<'a> {
    pub inv: &'a Inventory,
    pub home: &'a Home,
    pub homes: &'a [Home],
    /// `<data dir>/audit-runs`, where the judge runs.
    pub audit_runs: &'a Path,
}

impl Classifier<'_> {
    /// The event for `raw`, or `None` for a session whose cwd is inside
    /// `<data dir>/audit-runs/`: the judge's own runs are never usage.
    pub fn event(&self, roots: &mut Roots, raw: Raw, file: &Path) -> Option<Event> {
        if raw.session.cwd.as_deref().is_some_and(|c| Path::new(c).starts_with(self.audit_runs)) {
            return None;
        }
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
        Some(Event {
            v: 1,
            id,
            ts: raw.ts,
            agent: self.home.agent,
            home: self.home.name.clone(),
            agent_version: raw.session.version.clone(),
            session,
            actor: actor(self.home.agent, &raw.session.actor),
            skill: raw.skill,
            origin,
            channel: raw.channel,
            path: raw.path,
            cwd,
            authoring,
            src: format!("{}:{}", file.display(), raw.line),
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
        if std::fs::canonicalize(p).is_ok_and(|r| r.starts_with(&self.inv.source)) {
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
}
