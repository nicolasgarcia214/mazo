//! Golden files: every report rendered from the report fixture at NOW in
//! UTC, compared byte for byte with `tests/golden/`. `UPDATE_GOLDEN=1`
//! rewrites the files instead, so a renderer change shows up as their diff.

mod common;

use std::fs;
use std::path::Path;

use common::{ago, event, report_repo, report_world};
use skima::html;
use skima::ledger::{Actor, Agent, Channel, Event, Origin};
use skima::report::{self, Since, UsageArgs};

/// Every channel, both agents, a subagent, a probe, another origin, an
/// authoring session, a typed event before coverage, and events outside
/// the 30-day window and the inventory.
fn events() -> Vec<Event> {
    let mut sub = event("sub", "unslop", Channel::Tool, "B", &ago(2));
    sub.actor = Actor::Subagent;
    let mut codex = event("cx", "writing-for-agents", Channel::Read, "X", &ago(3));
    codex.agent = Agent::Codex;
    codex.home = "codex".into();
    let mut probe = event("probe", "unslop", Channel::Tool, "P", &ago(1));
    probe.actor = Actor::Probe;
    let mut other = event("other", "goal-plan", Channel::Slash, "O", &ago(4));
    other.origin = Origin::Other;
    vec![
        event("t1", "unslop", Channel::Tool, "A", &ago(1)),
        event("s1", "unslop", Channel::Slash, "A", &ago(1)),
        event("r1", "unslop", Channel::Read, "A", &ago(1)),
        event("f1", "unslop", Channel::Ref, "A", &ago(1)),
        sub,
        codex,
        probe,
        other,
        event("g1", "writing-for-agents", Channel::Slash, "D", &ago(6)),
        event("d1", "goal-smart", Channel::Read, "D", &ago(6)),
        event("gs", "goal-smart", Channel::Tool, "G", &ago(12)),
        event("ty", "goal-smart", Channel::Typed, "Y", "2026-02-24T10:00:00Z"),
        event("old", "goal-execute", Channel::Tool, "Z", &ago(40)),
        event("pr", "pruned", Channel::Tool, "R", &ago(90)),
    ]
}

/// `out` with the fixture's path, and its `~` form, replaced by `<skills>`.
fn scrub(out: String, source: &Path) -> String {
    let path = source.display().to_string();
    let out = out.replace(&path, "<skills>").replace(&report::tilde(&path), "<skills>");
    // The tempdir's name holds no character the HTML escapes, so nothing
    // escaped is left behind; a leftover would make the files machine-specific.
    let name = source.file_name().unwrap().to_str().unwrap();
    assert!(!out.contains(name), "the fixture path is left in: {out}");
    out
}

#[test]
fn reports_match_their_golden_files() {
    let repo = report_repo();
    let w = report_world(repo.path());
    let events = events();
    let input = w.input(&events);
    let usage = UsageArgs { since: Since::Days(30), agent: None, channel: None, all: true, authoring: false };
    let mut outputs = vec![
        ("usage-all.txt".to_string(), report::usage(&input, &usage, false, false)),
        ("unused.txt".into(), report::unused(&input, Since::Days(30), false, false)),
    ];
    for skill in ["goal-smart", "unslop", "writing-for-agents"] {
        for (ext, json) in [("json", true), ("txt", false)] {
            let card = report::show(&input, skill, report::SHOW_SINCE, 20, json, false).unwrap();
            outputs.push((format!("show-{skill}.{ext}"), card));
        }
    }
    outputs.push(("report.html".into(), html::render(&input, Since::Days(30)).unwrap()));

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let update = std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == "1");
    let mut stale = vec![];
    for (file, out) in outputs {
        let out = scrub(out, repo.path());
        let path = dir.join(&file);
        if update {
            fs::create_dir_all(&dir).unwrap();
            fs::write(&path, &out).unwrap();
        } else if fs::read_to_string(&path).ok().as_deref() != Some(out.as_str()) {
            stale.push(file);
        }
    }
    assert!(stale.is_empty(), "differ from tests/golden/ (UPDATE_GOLDEN=1 rewrites them): {stale:?}");
}
