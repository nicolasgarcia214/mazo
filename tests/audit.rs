//! `skima audit` selection, judge input and `--dry-run` (SPEC §6.4, §10.8).

mod common;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use skima::adapters;
use skima::audit::{self, Candidate, SkillText};
use skima::ledger::{Actor, Agent, Channel, Event, Origin};

/// Line 5 of the Claude fixture is a strong `unslop` load; its first record is
/// at 2026-10-14T04:00:01Z.
const CLAUDE_LOAD: u64 = 5;

fn event(agent: Agent, session: &str, ts: &str, actor: Actor, channel: Channel, cwd: &str, src: String) -> Event {
    Event {
        v: 1,
        id: format!("{session}:{ts}:{channel:?}"),
        ts: ts.into(),
        agent,
        home: skima::ledger::name(agent),
        agent_version: None,
        session: session.into(),
        actor,
        skill: "unslop".into(),
        origin: Origin::Canonical,
        channel,
        path: None,
        cwd: Some(cwd.into()),
        authoring: false,
        src,
    }
}

/// A copy of the Claude fixture at `<dir>/<session>.jsonl`, and its load's `src`.
fn transcript(dir: &Path, session: &str) -> String {
    let path = dir.join(format!("{session}.jsonl"));
    fs::copy(common::fixture("claude/condense.jsonl"), &path).unwrap();
    format!("{}:{CLAUDE_LOAD}", path.display())
}

/// A main-actor Claude tool load of `unslop` in its own transcript.
fn session(dir: &Path, session: &str, ts: &str, cwd: &str) -> Event {
    event(Agent::Claude, session, ts, Actor::Main, Channel::Tool, cwd, transcript(dir, session))
}

fn sessions(c: &[Candidate]) -> Vec<&str> {
    c.iter().map(|c| c.event.session.as_str()).collect()
}

#[test]
fn selection_is_newest_first_round_robin_across_projects_up_to_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let events = [
        session(d, "a3", "2026-10-01T06:00:00Z", "/p/a"),
        session(d, "b2", "2026-10-01T05:00:00Z", "/p/b"),
        session(d, "a1", "2026-10-01T10:00:00Z", "/p/a"),
        session(d, "c1", "2026-10-01T07:00:00Z", "/p/c"),
        session(d, "a2", "2026-10-01T08:00:00Z", "/p/a"),
        session(d, "b1", "2026-10-01T09:00:00Z", "/p/b"),
    ];
    let none = HashSet::new();
    assert_eq!(sessions(&audit::select(&events, "unslop", &none, Some(4))), ["a1", "b1", "c1", "a2"]);
    assert_eq!(sessions(&audit::select(&events, "unslop", &none, None)), ["a1", "b1", "c1", "a2", "b2", "a3"]);
    assert_eq!(sessions(&audit::select(&events, "unslop", &none, Some(20))).len(), 6);
    assert!(audit::select(&events, "grade", &none, None).is_empty());
}

#[test]
fn selection_takes_main_and_headless_and_skips_audited_weak_probe_and_expired_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let p = "/p/a";
    let load = |s: &str, ts: &str, actor: Actor, channel: Channel| {
        event(Agent::Claude, s, ts, actor, channel, p, transcript(d, s))
    };
    let mut events = vec![
        load("main", "2026-10-01T01:00:00Z", Actor::Main, Channel::Tool),
        load("headless", "2026-10-01T02:00:00Z", Actor::Headless, Channel::Slash),
        load("subagent", "2026-10-01T03:00:00Z", Actor::Subagent, Channel::Tool),
        load("probe", "2026-10-01T04:00:00Z", Actor::Probe, Channel::Tool),
        load("weak", "2026-10-01T05:00:00Z", Actor::Main, Channel::Read),
        load("weak", "2026-10-01T05:01:00Z", Actor::Main, Channel::Ref),
        load("audited", "2026-10-01T06:00:00Z", Actor::Main, Channel::Tool),
    ];
    let expired = d.join("gone.jsonl");
    events.push(event(
        Agent::Claude,
        "expired",
        "2026-10-01T07:00:00Z",
        Actor::Main,
        Channel::Tool,
        p,
        format!("{}:{CLAUDE_LOAD}", expired.display()),
    ));
    // A typed load names `history.jsonl`, which holds no window; the session's
    // later tool load in its own transcript is condensed instead.
    let history = d.join("history.jsonl");
    common::write(&history, "{\"display\":\"/unslop\"}\n");
    let typed = format!("{}:1", history.display());
    events.push(event(Agent::Claude, "typed", "2026-10-01T00:00:00Z", Actor::Main, Channel::Typed, p, typed));
    events.push(load("typed", "2026-10-01T00:00:05Z", Actor::Main, Channel::Tool));

    let data = d.join("data");
    common::write(
        &data.join("audits.jsonl"),
        concat!(
            "{\"v\":1,\"kind\":\"session\",\"skill\":\"unslop\",\"session\":\"audited\"}\n",
            "{\"v\":1,\"kind\":\"session\",\"skill\":\"grade\",\"session\":\"main\"}\n",
            "{\"v\":1,\"kind\":\"skill\",\"skill\":\"unslop\",\"sessions\":[\"headless\"]}\n",
        ),
    );
    let audited = audit::audited(&data, "unslop").unwrap();
    assert_eq!(audited, HashSet::from(["audited".to_string()]));

    let got = audit::select(&events, "unslop", &audited, None);
    assert_eq!(sessions(&got), ["headless", "main", "typed"]);
    assert_eq!(got[2].event.channel, Channel::Tool);
    // The expired session is skipped before the limit is counted.
    assert_eq!(sessions(&audit::select(&events, "unslop", &audited, Some(2))), ["headless", "main"]);
    // No file means nothing audited yet.
    assert!(audit::audited(&d.join("empty"), "unslop").unwrap().is_empty());
}

/// The Claude, Codex and Pi condense fixtures, each with a strong `unslop` load.
fn three_agents(dir: &Path) -> Vec<Event> {
    let codex = format!("{}:7", common::fixture("codex/condense.jsonl").display());
    let pi = format!("{}:4", common::fixture("pi/condense.jsonl").display());
    vec![
        session(dir, "c", "2026-10-14T04:00:05Z", "/p/claude"),
        event(Agent::Codex, "x", "2026-10-14T04:00:07Z", Actor::Main, Channel::Tool, "/p/codex", codex),
        event(Agent::Pi, "p", "2026-10-14T04:00:03Z", Actor::Main, Channel::Slash, "/p/pi", pi),
    ]
}

#[test]
fn every_agent_is_a_candidate_whatever_the_judge() {
    let dir = tempfile::tempdir().unwrap();
    let got = audit::select(&three_agents(dir.path()), "unslop", &HashSet::new(), None);
    let agents: Vec<Agent> = got.iter().map(|c| c.event.agent).collect();
    assert_eq!(agents, [Agent::Codex, Agent::Claude, Agent::Pi]);
    for c in &got {
        let want = adapters::for_agent(c.event.agent).condense(&c.event.src, audit::MAX_CHARS).unwrap();
        assert_eq!(c.condensed, want);
    }
}

/// A source with `unslop/SKILL.md` committed as `v1` on 2026-10-01 and `v2` on
/// 2026-10-20, and `v3` uncommitted.
fn repo(root: &Path) -> PathBuf {
    let source = root.join("skills");
    let skill = source.join("unslop/SKILL.md");
    common::git(root, &["init", "-q"], "2026-10-01T00:00:00Z");
    for (text, date) in [("v1\n", "2026-10-01T00:00:00Z"), ("v2\n", "2026-10-20T00:00:00Z")] {
        common::write(&skill, text);
        common::git(root, &["add", "-A"], date);
        common::git(root, &["commit", "-q", "-m", text.trim()], date);
    }
    common::write(&skill, "v3\n");
    source
}

fn ts(s: &str) -> Option<jiff::Timestamp> {
    Some(s.parse().unwrap())
}

#[test]
fn the_skill_is_read_as_of_the_last_commit_before_the_session_started() {
    let dir = tempfile::tempdir().unwrap();
    let source = repo(dir.path());
    let text = |t| audit::skill_at(&source, "unslop", t).unwrap().text;
    assert_eq!(text(ts("2026-10-10T00:00:00Z")), "v1\n");
    assert_eq!(text(ts("2026-10-21T00:00:00Z")), "v2\n");
    let before = audit::skill_at(&source, "unslop", ts("2026-09-01T00:00:00Z")).unwrap();
    assert_eq!(before, SkillText { commit: None, text: "v3\n".into() }, "no earlier commit");

    let bare = dir.path().join("bare");
    common::write(&bare.join("unslop/SKILL.md"), "plain\n");
    let plain = audit::skill_at(&bare, "unslop", ts("2026-10-10T00:00:00Z")).unwrap();
    assert_eq!(plain, SkillText { commit: None, text: "plain\n".into() }, "no repository");

    // The judge input: the transcript starts on 2026-10-14, between v1 and v2.
    let c = audit::select(&three_agents(dir.path()), "unslop", &HashSet::new(), None)
        .into_iter()
        .find(|c| c.event.agent == Agent::Claude)
        .unwrap();
    assert_eq!(c.started(), ts("2026-10-14T04:00:01Z"));
    let (skill, input) = audit::input(&source, &c).unwrap();
    assert!(skill.commit.is_some());
    assert_eq!(skill.text, "v1\n");
    assert!(input.starts_with("<skill name=\"unslop\">\nv1\n</skill>\n"), "{input}");
    assert!(input.contains(&c.condensed));
}

/// A private home with a skills source, a ledger of three agents' sessions, no
/// agent homes, and a stub `claude` on `PATH` that leaves a marker if started.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new(judge: &str) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        common::write(&root.join("skills/unslop/SKILL.md"), "---\nname: unslop\ndescription: X.\n---\n");
        common::write(&root.join("skills/skima.toml"), &format!("source = \".\"\n{judge}"));
        let ledger: String = three_agents(&root).iter().map(|e| serde_json::to_string(e).unwrap() + "\n").collect();
        common::write(&root.join("data/skima/events.jsonl"), &ledger);
        let stub = root.join("bin/claude");
        common::write(&stub, &format!("#!/bin/sh\ntouch {}\n", root.join("started").display()));
        Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
        Fixture { _temp: temp, root }
    }

    fn skima(&self, args: &[&str]) -> Output {
        let path = format!("{}:{}", self.root.join("bin").display(), std::env::var("PATH").unwrap());
        let out = Command::new(env!("CARGO_BIN_EXE_skima"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("PATH", path)
            .env("SKIMA_CONFIG", self.root.join("skills/skima.toml"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .output()
            .unwrap();
        assert!(!self.root.join("started").exists(), "the judge was started");
        out
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn dry_run_lists_sessions_the_judge_command_and_the_estimate_and_starts_nothing() {
    let fx = Fixture::new("[judge]\nmodel = \"sonnet\"\n");
    let candidates = audit::select(&three_agents(&fx.root), "unslop", &HashSet::new(), None);
    let chars: Vec<usize> = candidates.iter().map(|c| c.condensed.chars().count()).collect();
    let total: usize = chars.iter().sum();

    let out = fx.skima(&["audit", "unslop", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with("unslop: 3 sessions to judge with the claude judge\n"), "{text}");
    for (c, n) in candidates.iter().zip(&chars) {
        let agent = skima::ledger::name(c.event.agent);
        let cwd = c.event.cwd.as_deref().unwrap();
        let row = text.lines().find(|l| l.contains(&c.event.session) && l.contains(cwd)).unwrap();
        assert!(row.contains(&format!(" {agent} ")) && row.contains(&format!(" {n} ")), "{row}");
    }
    let judge = "judge: claude -p --safe-mode --tools \"\" --system-prompt \"$(cat <prompt>)\" --output-format json \
                 --json-schema <schema> --no-session-persistence --max-budget-usd <cap> --model sonnet < <input>\n";
    assert!(text.contains(judge), "{text}");
    assert!(text.ends_with(&format!("input: {total} condensed characters, about {} tokens\n", total / 4)), "{text}");

    let one = stdout(&fx.skima(&["audit", "unslop", "--dry-run", "--limit", "1"]));
    assert!(one.starts_with("unslop: 1 session to judge with"), "{one}");
    let all = stdout(&fx.skima(&["audit", "unslop", "--dry-run", "--all"]));
    assert!(all.starts_with("unslop: 3 sessions to judge"), "{all}");
}

#[test]
fn audit_usage_errors_exit_2_and_start_nothing() {
    let fx = Fixture::new("");
    let both = fx.skima(&["audit", "unslop", "--dry-run", "--limit", "3", "--all"]);
    assert_eq!(both.status.code(), Some(2), "--limit and --all conflict");
    let run = fx.skima(&["audit", "unslop"]);
    assert_eq!(run.status.code(), Some(2));
    assert!(stderr(&run).contains("--dry-run"), "{}", stderr(&run));
    let unknown = fx.skima(&["audit", "nope", "--dry-run"]);
    assert_eq!(unknown.status.code(), Some(2));
    for agent in ["codex", "pi"] {
        let fx = Fixture::new(&format!("[judge]\nagent = \"{agent}\"\n"));
        let out = fx.skima(&["audit", "unslop", "--dry-run"]);
        assert_eq!(out.status.code(), Some(2), "{agent}");
        assert!(stderr(&out).contains(&format!("the {agent} judge is not available yet")), "{}", stderr(&out));
    }
    let typo = Fixture::new("[judge]\nmodle = \"sonnet\"\n").skima(&["audit", "unslop", "--dry-run"]);
    assert!(stderr(&typo).contains("unknown field `modle`"), "{}", stderr(&typo));
    assert_ne!(typo.status.code(), Some(0));
}
