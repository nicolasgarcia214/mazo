//! The card's examples: how to invoke a skill, and real prompts
//! read from transcripts at display time and never stored.

use std::path::Path;

use serde::Serialize;

use crate::adapters;
use crate::ledger::{Actor, Agent, Event};
use crate::yaml;

/// Real examples come from this many projects, one each.
const PROJECTS: usize = 3;
/// A real example is cut to this many characters.
const MAX_CHARS: usize = 300;
/// A shorter prompt ("yes", "go on") says nothing about the skill.
const MIN_CHARS: usize = 20;

#[derive(Serialize)]
pub struct Examples {
    /// The invocation on each configured agent.
    pub written: Vec<Invocation>,
    /// `interface.default_prompt` from `agents/openai.yaml`.
    pub default_prompt: Option<String>,
    /// Newest project first.
    pub real: Vec<Real>,
    /// Up to three from the latest audit.
    pub curated: Vec<crate::audit::Example>,
}

#[derive(Serialize)]
pub struct Invocation {
    pub agent: Agent,
    pub text: String,
}

#[derive(Serialize)]
pub struct Real {
    pub ts: jiff::Timestamp,
    pub agent: Agent,
    pub cwd: Option<String>,
    pub text: String,
}

/// `/name` on Claude, `$name` on Codex, `/skill:name` on Pi.
fn invocation(agent: Agent, skill: &str) -> String {
    match agent {
        Agent::Claude => format!("/{skill}"),
        Agent::Codex => format!("${skill}"),
        Agent::Pi => format!("/skill:{skill}"),
    }
}

/// The examples of `skill`, whose directory is `dir` if any, from its ledger
/// `events` newest first, with the latest audit's `curated` ones; only strong
/// `main` events are read.
pub fn examples<'e>(
    agents: impl IntoIterator<Item = Agent>,
    dir: Option<&Path>,
    skill: &str,
    events: impl IntoIterator<Item = &'e Event>,
    curated: Vec<crate::audit::Example>,
) -> Examples {
    let mut agents: Vec<Agent> = agents.into_iter().collect();
    agents.sort();
    agents.dedup();
    let default_prompt = dir
        .and_then(|d| std::fs::read_to_string(d.join("agents/openai.yaml")).ok())
        .and_then(|text| yaml::load(&text).ok())
        .and_then(|m| {
            yaml::get(&m, "interface")
                .and_then(|i| yaml::get(i, "default_prompt"))
                .and_then(yaml::text)
                .map(str::to_string)
        });
    let written = agents.into_iter().map(|agent| Invocation { agent, text: invocation(agent, skill) }).collect();
    Examples { written, default_prompt, real: real(skill, events), curated }
}

/// The most recent informative prompt per project, from the three most recent
/// projects. A prompt that is only the invocation or under 20 characters gives
/// way to the project's next most recent event; an expired transcript ends
/// the project's search.
fn real<'e>(skill: &str, events: impl IntoIterator<Item = &'e Event>) -> Vec<Real> {
    let mut projects: Vec<(Option<&str>, Vec<&Event>)> = Vec::new();
    for e in events.into_iter().filter(|e| e.channel.strong() && e.actor == Actor::Main) {
        let cwd = e.cwd.as_deref();
        match projects.iter().position(|(c, _)| *c == cwd) {
            Some(i) => projects[i].1.push(e),
            None if projects.len() < PROJECTS => projects.push((cwd, vec![e])),
            None => {}
        }
    }
    let bare = [Agent::Claude, Agent::Codex, Agent::Pi].map(|a| invocation(a, skill));
    projects
        .into_iter()
        .filter_map(|(_, list)| {
            list.into_iter()
                .map(|e| Some((e, adapters::for_agent(e.agent).prompt_at(&e.src)?)))
                .take_while(Option::is_some)
                .flatten()
                .find(|(_, text)| text.chars().count() >= MIN_CHARS && !bare.iter().any(|b| b == text.trim()))
                .map(|(e, text)| Real {
                    ts: e.ts,
                    agent: e.agent,
                    cwd: e.cwd.clone(),
                    text: text.chars().take(MAX_CHARS).collect(),
                })
        })
        .collect()
}
