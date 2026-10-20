//! Golden files: every report rendered from the report fixture at NOW in
//! UTC, compared byte for byte with `tests/golden/`. `UPDATE_GOLDEN=1`
//! rewrites the files instead, so a renderer change shows up as their diff.

mod common;

use std::fs;
use std::path::Path;

use common::{NOW, ago, event, git, report_repo, report_world, skill_record, write};
use skima::html;
use skima::ledger::{Actor, Agent, Channel, Event, Origin};
use skima::report::{self, Since, UsageArgs};

/// `unslop` given every optional card field: two grades, the later one
/// before an edit and uncommitted changes, two retros, a default prompt, and
/// a user prompt in the transcript `<skills>/t.jsonl` for a real example.
fn fill_unslop(src: &Path) {
    for (date, score) in [("2026-02-01", "6"), ("2026-03-01", "7")] {
        write(&src.join(format!(".grades/unslop/{date}.md")), &format!("---\ndate: {date}\nscore: {score}\n---\n"));
        git(src, &["add", "."], NOW);
        git(src, &["commit", "-q", "-m", &format!("grade {date}")], &format!("{date}T00:00:00Z"));
    }
    write(&src.join("unslop/notes.md"), "edit\n");
    git(src, &["add", "."], NOW);
    git(src, &["commit", "-q", "-m", "tighten unslop"], "2026-04-01T00:00:00Z");
    write(&src.join("unslop/notes.md"), "edit, uncommitted\n");
    write(
        &src.join(".retros/unslop/a.md"),
        "---\nskill: unslop\ndate: 2026-04-02\nscore: 5\nverdict: too eager\n---\n",
    );
    write(&src.join(".retros/unslop/b.md"), "---\nskill: unslop\ndate: 2026-10-05\nscore: 8\nverdict: holds up\n---\n");
    write(&src.join("unslop/agents/openai.yaml"), "interface:\n  default_prompt: \"Use $unslop on this draft.\"\n");
    let prompt =
        serde_json::json!({"type": "user", "message": {"role": "user", "content": "Tighten the release notes."}});
    write(&src.join("t.jsonl"), &format!("{prompt}\n"));
}

/// Every channel, both agents, a subagent, a probe, another origin, an
/// authoring session, typed events inside and before coverage, a real
/// example on `transcript`, and events outside the 30-day window and the
/// inventory.
fn events(transcript: &Path) -> Vec<Event> {
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

/// `out` with the fixture's path, and its `~` form, replaced by `placeholder`.
fn scrub(out: String, source: &Path, placeholder: &str) -> String {
    let path = source.display().to_string();
    let out = out.replace(&path, placeholder).replace(&report::tilde(&path), placeholder);
    // The tempdir's name holds no character the HTML escapes, so nothing
    // escaped is left behind; a leftover would make the files machine-specific.
    let name = source.file_name().unwrap().to_str().unwrap();
    assert!(!out.contains(name), "the fixture path is left in: {out}");
    out
}

#[test]
fn reports_match_their_golden_files() {
    let repo = report_repo();
    fill_unslop(repo.path());
    let audits = tempfile::tempdir().unwrap();
    write(&audits.path().join("audits.jsonl"), &skill_record("Good on <short> texts", "2026-10-12T10:00:00Z"));
    let mut w = report_world(repo.path());
    w.audits = skima::audit::latest(audits.path()).unwrap();
    let events = events(&repo.path().join("t.jsonl"));
    let input = w.input(&events);
    let usage = UsageArgs { since: Since::Days(30), agent: None, channel: None, all: true, authoring: false };
    let mut outputs = vec![];
    for (ext, json) in [("json", true), ("txt", false)] {
        outputs.push((format!("usage-all.{ext}"), report::usage(&input, &usage, json, false)));
        outputs.push((format!("unused.{ext}"), report::unused(&input, Since::Days(30), json, false)));
        for skill in ["goal-smart", "unslop", "writing-for-agents"] {
            let card = report::show(&input, skill, report::SHOW_SINCE, 20, json, false).unwrap();
            outputs.push((format!("show-{skill}.{ext}"), card));
        }
    }
    outputs.push(("report.html".into(), html::render(&input, Since::Days(30)).unwrap()));

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let update = std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == "1");
    let mut stale = vec![];
    for (file, out) in outputs {
        // The page shows the placeholder escaped, so it renders as written.
        let placeholder = if file.ends_with(".html") { "&lt;skills&gt;" } else { "<skills>" };
        let out = scrub(out, repo.path(), placeholder);
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
