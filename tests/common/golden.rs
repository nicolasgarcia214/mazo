//! The golden fixture: the report fixture given every optional card field,
//! `unslop`'s audit and the events every report renders.

use std::path::Path;

use mazo::ledger::{Actor, Agent, Channel, Event, Origin};

use super::{NOW, World, ago, event, git, grade, report_repo, report_world, skill_record, write};

/// `unslop` given every optional card field: two grades, one recording no
/// hash and one an old one, then uncommitted changes, two retros, a default
/// prompt, and a user prompt in the transcript `<skills>/t.jsonl` for a real
/// example. `writing-for-agents` gets a grade of its current content.
pub fn fill(src: &Path) {
    let evals = src.join(".evals");
    grade(&evals, "unslop", "2026-02-01", "6", None);
    grade(&evals, "unslop", "2026-03-01", "7", Some(&src.join("unslop")));
    write(&src.join("unslop/notes.md"), "edit\n");
    git(src, &["add", "."], NOW);
    git(src, &["commit", "-q", "-m", "tighten unslop"], "2026-04-01T00:00:00Z");
    grade(&evals, "writing-for-agents", "2026-05-01", "8", Some(&src.join("writing-for-agents")));
    write(&src.join("unslop/notes.md"), "edit, uncommitted\n");
    write(
        &evals.join("retros/unslop/a.md"),
        "---\nskill: unslop\ndate: 2026-04-02\nscore: 5\nverdict: too eager\n---\n",
    );
    write(
        &evals.join("retros/unslop/b.md"),
        "---\nskill: unslop\ndate: 2026-10-05\nscore: 8\nverdict: holds up\n---\n",
    );
    write(&src.join("unslop/agents/openai.yaml"), "interface:\n  default_prompt: \"Use $unslop on this draft.\"\n");
    let prompt =
        serde_json::json!({"type": "user", "message": {"role": "user", "content": "Tighten the release notes."}});
    write(&src.join("t.jsonl"), &format!("{prompt}\n"));
}

/// Every channel, both agents, a subagent, a probe, another origin, an
/// authoring session, typed events inside and before coverage, a real
/// example on `transcript`, and events outside the 30-day window and the
/// inventory.
pub fn events(transcript: &Path) -> Vec<Event> {
    let mut real = event("rx", "unslop", Channel::Tool, "E", &ago(7));
    real.cwd = Some("/notes".into());
    real.src = format!("{}:1", transcript.display());
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
        real,
        codex,
        probe,
        other,
        event("g1", "writing-for-agents", Channel::Slash, "D", &ago(6)),
        event("d1", "goal-smart", Channel::Read, "D", &ago(6)),
        event("gs", "goal-smart", Channel::Tool, "G", &ago(12)),
        event("ty", "goal-smart", Channel::Typed, "Y", "2026-02-24T10:00:00Z"),
        event("tu", "unslop", Channel::Typed, "T", &ago(8)),
        event("old", "goal-execute", Channel::Tool, "Z", &ago(40)),
        event("pr", "pruned", Channel::Tool, "R", &ago(90)),
    ]
}

/// The filled report repository, its world with `unslop`'s audit, and the events.
pub struct Golden {
    pub repo: tempfile::TempDir,
    pub world: World,
    pub events: Vec<Event>,
}

impl Golden {
    pub fn new() -> Golden {
        let repo = report_repo();
        fill(repo.path());
        let audits = tempfile::tempdir().unwrap();
        write(&audits.path().join("audits.jsonl"), &skill_record("Good on <short> texts", "2026-10-12T10:00:00Z"));
        let mut world = report_world(repo.path());
        world.audits = mazo::audit::latest(audits.path()).unwrap();
        let events = events(&repo.path().join("t.jsonl"));
        Golden { repo, world, events }
    }
}
