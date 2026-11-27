use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

use jiff::Timestamp;

use super::frame;
use crate::Result;
use crate::adapters::{self, Transcripts};
use crate::inventory::git;
use crate::ledger::{Actor, Agent, Event};

pub const MAX_CHARS: usize = 60_000;

pub struct Candidate {
    pub event: Event,
    pub condensed: String,
    pub started: Timestamp,
}

pub fn select(events: &[Event], skill: &str, audited: &HashSet<String>, limit: Option<usize>) -> Vec<Candidate> {
    type Loads<'a> = Vec<(Timestamp, &'a Event)>;
    let mut sessions: BTreeMap<(Agent, &str), Loads> = BTreeMap::new();
    for e in events {
        if e.skill != skill || !e.channel.strong() || !matches!(e.actor, Actor::Main | Actor::Headless) {
            continue;
        }
        if audited.contains(&e.session) {
            continue;
        }
        sessions.entry((e.agent, &e.session)).or_default().push((e.ts, e));
    }
    let mut sessions: Vec<Loads> = sessions.into_values().collect();
    for loads in &mut sessions {
        loads.sort_by_key(|(t, _)| *t);
    }
    sessions.sort_by_key(|loads| std::cmp::Reverse(loads[0].0));
    let mut projects: Vec<(Option<&str>, VecDeque<Loads>)> = Vec::new();
    for loads in sessions {
        let cwd = loads.iter().find_map(|(_, e)| e.cwd.as_deref());
        match projects.iter_mut().find(|(c, _)| *c == cwd) {
            Some((_, queue)) => queue.push_back(loads),
            None => projects.push((cwd, VecDeque::from([loads]))),
        }
    }
    let mut memo = Transcripts::default();
    let mut next = |queue: &mut VecDeque<Loads>| {
        while let Some(loads) = queue.pop_front() {
            let found = loads.iter().find_map(|(_, e)| {
                let condensed = adapters::for_agent(e.agent).condense(&mut memo, &e.src, MAX_CHARS)?;
                let started = crate::scan::first_timestamp(Path::new(e.src_file())).unwrap_or(e.ts);
                Some(Candidate { event: (*e).clone(), condensed, started })
            });
            if let Some(c) = found {
                return Some((loads[0].0, c));
            }
        }
        None
    };
    let mut heads: Vec<((Timestamp, Candidate), VecDeque<Loads>)> =
        projects.into_iter().filter_map(|(_, mut queue)| Some((next(&mut queue)?, queue))).collect();
    heads.sort_by_key(|((t, _), _)| std::cmp::Reverse(*t));
    let limit = limit.unwrap_or(usize::MAX);
    let (mut out, mut queues): (Vec<Candidate>, Vec<_>) = heads.into_iter().map(|((_, c), q)| (c, q)).unzip();
    loop {
        let room = limit.saturating_sub(out.len());
        let round: Vec<Candidate> = queues.iter_mut().filter_map(|q| next(q).map(|(_, c)| c)).take(room).collect();
        if round.is_empty() {
            break;
        }
        out.extend(round);
    }
    out.truncate(limit);
    out
}

#[derive(Debug, PartialEq)]
pub struct SkillText {
    pub commit: Option<String>,
    pub text: String,
}

pub fn skill_at(source: &Path, skill: &str, started: Timestamp) -> Result<SkillText> {
    let dir = source.join(skill);
    let before = format!("--before={started}");
    let at = git(&dir, &["log", "-1", "--format=%H", &before, "--", "SKILL.md"]).and_then(|log| {
        let commit = Some(log.trim()).filter(|c| !c.is_empty())?;
        let text = git(&dir, &["show", &format!("{commit}:./SKILL.md")])?;
        Some(SkillText { commit: Some(commit.to_string()), text })
    });
    match at {
        Some(at) => Ok(at),
        None => {
            let path = dir.join("SKILL.md");
            let text = fs::read_to_string(&path).map_err(crate::at(&path))?;
            Ok(SkillText { commit: None, text })
        }
    }
}

pub fn input(source: &Path, c: &Candidate) -> Result<(SkillText, String)> {
    let skill = skill_at(source, &c.event.skill, c.started)?;
    let text = format!(
        "{}\n{}",
        frame(&format!("skill name={:?}", c.event.skill), &skill.text),
        frame("session", &c.condensed)
    );
    Ok((skill, text))
}
