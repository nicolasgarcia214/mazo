//! `skima audit` (SPEC §6.4, F18): stage-1 selection, the judge's input and
//! the judge command. `--dry-run` shows what a run would judge and calls nothing.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Deserialize;

use crate::Result;
use crate::adapters;
use crate::config::Judge;
use crate::inventory::git;
use crate::ledger::{Actor, Agent, Event, name};
use crate::render::{self, Align};

/// The condensed session's cap (SPEC §6.4 `max_chars`).
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

/// Sessions that already have a `session` record for `skill` in `<data>/audits.jsonl`.
pub fn audited(data: &Path, skill: &str) -> Result<HashSet<String>> {
    #[derive(Deserialize)]
    struct Record {
        kind: String,
        skill: String,
        session: Option<String>,
    }
    let path = data.join("audits.jsonl");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<Record>(l).ok())
        .filter(|r| r.kind == "session" && r.skill == skill)
        .filter_map(|r| r.session)
        .collect())
}

/// Stage-1 selection (SPEC §6.4, D29): root sessions of any agent with a strong
/// load of `skill` by actor `main` or `headless` and no session in `audited`,
/// newest first and taken round-robin across projects (cwd), up to `limit`
/// (`None` takes all). A session is condensed from its earliest strong load
/// whose transcript is still on disk; one with none left is skipped.
pub fn select(events: &[Event], skill: &str, audited: &HashSet<String>, limit: Option<usize>) -> Vec<Candidate> {
    let mut sessions: BTreeMap<(Agent, &str), Vec<(Timestamp, &Event)>> = BTreeMap::new();
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
    let mut sessions: Vec<Vec<(Timestamp, &Event)>> = sessions.into_values().collect();
    for loads in &mut sessions {
        loads.sort_by_key(|(t, _)| *t);
    }
    sessions.sort_by_key(|loads| std::cmp::Reverse(loads[0].0));
    // Round-robin: the n-th newest session of every project comes before any
    // project's (n+1)-th; projects in the order of their newest session.
    // `projects` holds each cwd with its sessions seen so far.
    let mut projects: Vec<(Option<&str>, usize)> = Vec::new();
    let mut rank: Vec<(usize, usize)> = Vec::new();
    for loads in &sessions {
        let cwd = loads.iter().find_map(|(_, e)| e.cwd.as_deref());
        let p = projects.iter().position(|(c, _)| *c == cwd).unwrap_or_else(|| {
            projects.push((cwd, 0));
            projects.len() - 1
        });
        rank.push((projects[p].1, p));
        projects[p].1 += 1;
    }
    let mut order: Vec<usize> = (0..sessions.len()).collect();
    order.sort_by_key(|&i| rank[i]);
    order
        .into_iter()
        .filter_map(|i| {
            sessions[i].iter().find_map(|(_, e)| {
                let condensed = adapters::for_agent(e.agent).condense(&e.src, MAX_CHARS)?;
                Some(Candidate { event: (*e).clone(), condensed })
            })
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect()
}

/// `SKILL.md` as the judge reads it, and the commit it came from.
#[derive(Debug, PartialEq)]
pub struct SkillText {
    pub commit: Option<String>,
    pub text: String,
}

/// `<source>/<skill>/SKILL.md` as of the last commit before `started` that
/// touched it, or the current file when there is no repository or no earlier
/// commit (SPEC §6.4, D30).
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
/// started, then the condensed session.
pub fn input(source: &Path, c: &Candidate) -> Result<(SkillText, String)> {
    let skill = skill_at(source, &c.event.skill, c.started())?;
    let text = format!(
        "<skill name=\"{}\">\n{}\n</skill>\n\n<session>\n{}\n</session>\n",
        c.event.skill,
        skill.text.trim_end(),
        c.condensed
    );
    Ok((skill, text))
}

/// The judge's argv (SPEC §6.4), `prompt`, `schema` and `cap` standing for the
/// call's own values. Only the Claude judge exists so far.
pub fn command(judge: &Judge, prompt: &str, schema: &str, cap: &str) -> Result<Vec<String>> {
    if judge.agent != Agent::Claude {
        return Err(format!("the {} judge is not available yet; set [judge] agent = \"claude\"", name(judge.agent)));
    }
    let mut argv: Vec<String> = [
        "claude",
        "-p",
        "--safe-mode",
        "--tools",
        "",
        "--system-prompt",
        prompt,
        "--output-format",
        "json",
        "--json-schema",
        schema,
        "--no-session-persistence",
        "--max-budget-usd",
        cap,
    ]
    .map(String::from)
    .into();
    if let Some(model) = &judge.model {
        argv.extend(["--model".to_string(), model.clone()]);
    }
    Ok(argv)
}

/// Input tokens estimated from condensed characters (SPEC §6.4, D36).
pub fn estimate_tokens(chars: usize) -> usize {
    chars / 4
}

/// `skima audit --dry-run`: each session the audit would judge with its agent,
/// project and condensed size, the judge command and the token estimate (D36).
pub fn dry_run(skill: &str, judge: &Judge, candidates: &[Candidate], tty: bool) -> Result<String> {
    let argv = command(judge, "$(cat <prompt>)", "<schema>", "<cap>")?;
    if candidates.is_empty() {
        return Ok(format!("{skill}: no session to judge\n"));
    }
    let tz = TimeZone::system();
    let chars = |c: &Candidate| c.condensed.chars().count();
    let rows: Vec<Vec<String>> = candidates
        .iter()
        .map(|c| {
            let ts = c
                .event
                .ts
                .parse::<Timestamp>()
                .map_or(c.event.ts.clone(), |t| t.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M").to_string());
            let cwd = c.event.cwd.clone().unwrap_or_else(|| "-".into());
            vec![ts, name(c.event.agent), cwd, chars(c).to_string(), c.event.session.clone()]
        })
        .collect();
    let header = [
        ("loaded", Align::Left),
        ("agent", Align::Left),
        ("project", Align::Left),
        ("chars", Align::Right),
        ("session", Align::Left),
    ];
    let quoted: Vec<String> = argv
        .iter()
        .map(|a| if a.is_empty() || a.contains([' ', '$']) { format!("\"{a}\"") } else { a.clone() })
        .collect();
    let total: usize = candidates.iter().map(chars).sum();
    let n = candidates.len();
    Ok(format!(
        "{skill}: {n} session{} to judge with the {} judge\n\n{}\njudge: {} < <input>\ninput: {total} condensed characters, about {} tokens\n",
        if n == 1 { "" } else { "s" },
        name(judge.agent),
        render::table(&header, &rows, tty),
        quoted.join(" "),
        estimate_tokens(total),
    ))
}
