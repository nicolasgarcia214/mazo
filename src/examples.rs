use std::fs;
use std::path::Path;

use clap::ValueEnum;
use serde::Serialize;

use crate::adapters::{self, Transcripts};
use crate::ledger::{Actor, Agent, Event};
use crate::yaml;

const PROJECTS: usize = 3;
const MAX_CHARS: usize = 300;
const MIN_CHARS: usize = 20;

#[derive(Serialize)]
pub struct Examples {
    pub written: Vec<Invocation>,
    pub default_prompt: Option<String>,
    pub real: Vec<Real>,
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

fn invocation(agent: Agent, skill: &str) -> String {
    match agent {
        Agent::Claude => format!("/{skill}"),
        Agent::Codex => format!("${skill}"),
        Agent::Pi => format!("/skill:{skill}"),
    }
}

pub fn examples<'e>(
    agents: impl IntoIterator<Item = Agent>,
    dir: Option<&Path>,
    skill: &str,
    events: impl IntoIterator<Item = &'e Event>,
    curated: Vec<crate::audit::Example>,
    memo: &mut Transcripts,
) -> Examples {
    let mut agents: Vec<Agent> = agents.into_iter().collect();
    agents.sort();
    agents.dedup();
    let default_prompt = dir
        .and_then(|d| fs::read_to_string(d.join("agents/openai.yaml")).ok())
        .and_then(|text| yaml::load(&text).ok())
        .and_then(|m| {
            yaml::get(&m, "interface")
                .and_then(|i| yaml::get(i, "default_prompt"))
                .and_then(yaml::text)
                .map(str::to_string)
        });
    let written = agents.into_iter().map(|agent| Invocation { agent, text: invocation(agent, skill) }).collect();
    Examples { written, default_prompt, real: real(skill, events, memo), curated }
}

fn real<'e>(skill: &str, events: impl IntoIterator<Item = &'e Event>, memo: &mut Transcripts) -> Vec<Real> {
    let mut projects: Vec<(Option<&str>, Vec<&Event>)> = Vec::new();
    for e in events.into_iter().filter(|e| e.channel.strong() && e.actor == Actor::Main) {
        let cwd = e.cwd.as_deref();
        match projects.iter().position(|(c, _)| *c == cwd) {
            Some(i) => projects[i].1.push(e),
            None if projects.len() < PROJECTS => projects.push((cwd, vec![e])),
            None => {}
        }
    }
    let bare: Vec<String> = Agent::value_variants().iter().map(|&a| invocation(a, skill)).collect();
    projects
        .into_iter()
        .filter_map(|(_, list)| {
            list.into_iter()
                .map(|e| Some((e, adapters::for_agent(e.agent).prompt_at(memo, &e.src)?)))
                .take_while(Option::is_some)
                .flatten()
                .find(|(_, text)| text.chars().count() >= MIN_CHARS && !bare.iter().any(|b| b == text.trim()))
                .map(|(e, text)| Real {
                    ts: e.ts,
                    agent: e.agent,
                    cwd: e.cwd.clone(),
                    text: crate::render::inert(&text).chars().take(MAX_CHARS).collect(),
                })
        })
        .collect()
}
