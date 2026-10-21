//! Stage-1 selection: the sessions an audit judges, condensed, and the
//! judge's input for each.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;

use jiff::Timestamp;

use crate::Result;
use crate::adapters;
use crate::inventory::git;
use crate::ledger::{Actor, Agent, Event};

/// The condensed session's cap.
pub const MAX_CHARS: usize = 60_000;

/// A session the audit judges: the strong load its window starts from, and
/// the condensed window.
pub struct Candidate {
    pub event: Event,
    pub condensed: String,
}

impl Candidate {
    /// When the session started: its transcript's first timestamp, else the load's.
    pub fn started(&self) -> Option<Timestamp> {
        crate::scan::first_timestamp(Path::new(self.event.src_file()))
            .and_then(|t| t.parse().ok())
            .or_else(|| self.event.ts.parse().ok())
    }
}

/// Stage-1 selection: root sessions of any agent with a strong
/// load of `skill` by actor `main` or `headless` and no session in `audited`,
/// newest first and taken round-robin across projects (cwd), up to `limit`
/// (`None` takes all). A session is condensed from its earliest strong load
/// whose transcript is still on disk; one with none left is skipped and takes
/// no turn.
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
        let Ok(t) = e.ts.parse::<Timestamp>() else { continue };
        sessions.entry((e.agent, &e.session)).or_default().push((t, e));
    }
    let mut sessions: Vec<Loads> = sessions.into_values().collect();
    for loads in &mut sessions {
        loads.sort_by_key(|(t, _)| *t);
    }
    sessions.sort_by_key(|loads| std::cmp::Reverse(loads[0].0));
    // Each project's sessions, newest first.
    let mut projects: Vec<(Option<&str>, VecDeque<Loads>)> = Vec::new();
    for loads in sessions {
        let cwd = loads.iter().find_map(|(_, e)| e.cwd.as_deref());
        match projects.iter_mut().find(|(c, _)| *c == cwd) {
            Some((_, queue)) => queue.push_back(loads),
            None => projects.push((cwd, VecDeque::from([loads]))),
        }
    }
    // A project's next session that can still be condensed, with its start.
    let next = |queue: &mut VecDeque<Loads>| {
        while let Some(loads) = queue.pop_front() {
            let found = loads.iter().find_map(|(_, e)| {
                let condensed = adapters::for_agent(e.agent).condense(&e.src, MAX_CHARS)?;
                Some(Candidate { event: (*e).clone(), condensed })
            });
            if let Some(c) = found {
                return Some((loads[0].0, c));
            }
        }
        None
    };
    // Round-robin: the n-th newest session of every project comes before any
    // project's (n+1)-th; projects in the order of their newest session.
    let mut heads: Vec<((Timestamp, Candidate), VecDeque<Loads>)> =
        projects.into_iter().filter_map(|(_, mut queue)| Some((next(&mut queue)?, queue))).collect();
    heads.sort_by_key(|((t, _), _)| std::cmp::Reverse(*t));
    let limit = limit.unwrap_or(usize::MAX);
    let (mut out, mut queues): (Vec<Candidate>, Vec<_>) = heads.into_iter().map(|((_, c), q)| (c, q)).unzip();
    loop {
        // Lazy, so no queue past the limit gives up a session.
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

/// `SKILL.md` as the judge reads it, and the commit it came from.
#[derive(Debug, PartialEq)]
pub struct SkillText {
    pub commit: Option<String>,
    pub text: String,
}

/// `<source>/<skill>/SKILL.md` as of the last commit before `started` that
/// touched it, or the current file when there is no repository or no earlier
/// commit.
pub fn skill_at(source: &Path, skill: &str, started: Option<Timestamp>) -> Result<SkillText> {
    let dir = source.join(skill);
    let at = started.and_then(|t| {
        let before = format!("--before={t}");
        let log = git(&dir, &["log", "-1", "--format=%H", &before, "--", "SKILL.md"])?;
        let commit = Some(log.trim()).filter(|c| !c.is_empty())?;
        let text = git(&dir, &["show", &format!("{commit}:./SKILL.md")])?;
        Some(SkillText { commit: Some(commit.to_string()), text })
    });
    match at {
        Some(at) => Ok(at),
        None => {
            let path = dir.join("SKILL.md");
            let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(SkillText { commit: None, text })
        }
    }
}

/// The judge's input for one session: the skill as it was when the session
/// started, then the condensed session. A closing tag inside either part is
/// written `<\/tag>`, so neither can end its own frame.
pub fn input(source: &Path, c: &Candidate) -> Result<(SkillText, String)> {
    let skill = skill_at(source, &c.event.skill, c.started())?;
    let text = format!(
        "<skill name=\"{}\">\n{}\n</skill>\n\n<session>\n{}\n</session>\n",
        c.event.skill,
        skill.text.trim_end().replace("</skill>", "<\\/skill>"),
        c.condensed.replace("</session>", "<\\/session>")
    );
    Ok((skill, text))
}
