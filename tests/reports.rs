//! SPEC §10.1 Fan-in, Held and Evals, plus the report rules of §4.1 (filters,
//! collapse, sess, authoring) on small fixture ledgers.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use common::{copy_tree, git, write};
use serde_json::Value;
use skima::config::{Config, Policy};
use skima::evals;
use skima::inventory::{self, Inventory};
use skima::ledger::{Actor, Agent, Channel, Coverage, Event, Origin};
use skima::report::{self, Input, Since, UsageArgs};

const NOW: &str = "2026-10-13T12:00:00Z";

fn now() -> jiff::Timestamp {
    NOW.parse().unwrap()
}

/// `days` days before NOW, as a ledger timestamp.
fn ago(days: i64) -> String {
    now().checked_sub(jiff::SignedDuration::from_hours(24 * days)).unwrap().to_string()
}

fn ev(id: &str, skill: &str, channel: Channel, session: &str, ts: &str) -> Event {
    Event {
        v: 1,
        id: id.into(),
        ts: ts.into(),
        agent: Agent::Claude,
        home: "claude".into(),
        agent_version: None,
        session: session.into(),
        actor: Actor::Main,
        skill: skill.into(),
        origin: Origin::Canonical,
        channel,
        path: None,
        cwd: Some("/work".into()),
        authoring: false,
        src: format!("/nonexistent/{id}.jsonl:1"),
    }
}

/// The fixture skills committed on 2026-01-01, plus `fresh`, which is never committed.
fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&common::fixture("reports/skills"), dir.path(), &|s| s);
    git(dir.path(), &["init", "-q"], NOW);
    git(dir.path(), &["add", "."], NOW);
    git(dir.path(), &["commit", "-q", "-m", "skills"], "2026-01-01T00:00:00Z");
    write(&dir.path().join("fresh/SKILL.md"), "---\nname: fresh\ndescription: New.\n---\n\nNew.\n");
    dir
}

struct World {
    cfg: Config,
    inv: Inventory,
    coverage: BTreeMap<String, Coverage>,
}

fn world(source: &Path) -> World {
    let home = |name: &str, agent| common::home(name, agent, &source.join(name));
    let cfg = Config {
        source: source.into(),
        loose: vec![],
        homes: vec![home("claude", Agent::Claude), home("codex", Agent::Codex)],
        policy: Policy {
            authoring_skills: ["grade", "retro", "writing-for-agents", "skill-creator"].map(String::from).into(),
            ..Policy::default()
        },
    };
    let skills = inventory::canonical_skills(source).unwrap();
    let inv = Inventory { source: source.into(), known: skills.clone(), skills, worktree: None };
    let coverage = BTreeMap::from([
        ("claude".to_string(), Coverage { first: Some("2026-09-10T08:00:00Z".into()), last_scan: Some(NOW.into()) }),
        ("codex".to_string(), Coverage { first: Some("2026-03-29T08:00:00Z".into()), last_scan: Some(NOW.into()) }),
    ]);
    World { cfg, inv, coverage }
}

impl World {
    fn input<'a>(&'a self, events: &'a [Event]) -> Input<'a> {
        Input { cfg: &self.cfg, inv: &self.inv, events, coverage: &self.coverage, now: now() }
    }
}

fn args() -> UsageArgs {
    UsageArgs { since: Since::Days(30), agent: None, channel: None, all: false, authoring: false }
}

/// The `--json` usage row of one skill.
fn row(out: &str, skill: &str) -> Value {
    let v: Value = serde_json::from_str(out).unwrap();
    v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["skill"] == skill)
        .cloned()
        .unwrap_or_else(|| panic!("no row {skill}"))
}

fn counts(r: &Value) -> [u64; 6] {
    ["sess", "slash", "tool", "typed", "read", "ref"].map(|k| r[k].as_u64().unwrap())
}

// ---------------------------------------------------------------- fan-in

/// One skill per §5.3 rule in `tests/fixtures/skills`: diagnose-bug cites
/// `the blast-radius skill`, review-audit-fixes the same broken across lines,
/// triage-finding `owned by grade`, unslop the `writing-for-agents/` path (not
/// `upgrade/` or `x-grade/`), and writing-for-agents `the goal-smart skill` in
/// a reference file. interrogate's description and its `owned by graders`, and
/// grade's self-mentions, yield nothing.
#[test]
fn fan_in_on_the_fixture_tree_matches_each_rule() {
    let src = common::fixture("skills");
    let g = inventory::fan_in(&src, &inventory::canonical_skills(&src).unwrap());
    let edges: Vec<String> = g.iter().flat_map(|(a, bs)| bs.iter().map(move |b| format!("{a}->{b}"))).collect();
    assert_eq!(
        edges,
        [
            "diagnose-bug->blast-radius",
            "review-audit-fixes->blast-radius",
            "triage-finding->grade",
            "unslop->writing-for-agents",
            "writing-for-agents->goal-smart",
        ]
    );
}

/// Only with `SKIMA_TEST_SKILLS`: invariants that hold on any skills repository.
#[test]
fn fan_in_on_a_real_repo_has_no_self_edges_and_only_skill_targets() {
    let Some(src) = common::test_skills_env() else {
        eprintln!("fan_in real repo: SKIPPED (SKIMA_TEST_SKILLS unset)");
        return;
    };
    let skills = inventory::canonical_skills(&src).unwrap();
    let g = inventory::fan_in(&src, &skills);
    assert_eq!(g.keys().cloned().collect::<BTreeSet<_>>(), skills);
    for (a, cited) in &g {
        assert!(!cited.contains(a), "self-edge {a}");
        assert!(cited.is_subset(&skills), "{a} cites a non-skill: {cited:?}");
    }
}

#[test]
fn fan_in_on_fixtures_ignores_frontmatter_and_name_suffixes() {
    let repo = fixture_repo();
    let skills = inventory::canonical_skills(repo.path()).unwrap();
    let g = inventory::fan_in(repo.path(), &skills);
    let edges: Vec<String> = g.iter().flat_map(|(a, bs)| bs.iter().map(move |b| format!("{a}->{b}"))).collect();
    assert_eq!(
        edges,
        [
            "goal-execute->goal-smart",
            "goal-plan->goal-smart",
            "goal-smart->goal-execute",
            "goal-smart->goal-plan",
            "unslop->fresh",
            "unslop->writing-for-agents",
            "writing-for-agents->wfa-helper",
        ]
    );
}

// ---------------------------------------------------------------- held

#[test]
fn unused_reports_clusters_held_new_and_weak_only() {
    let repo = fixture_repo();
    let w = world(repo.path());
    let events = vec![
        ev("u1", "unslop", Channel::Tool, "s1", &ago(10)),
        ev("p1", "goal-plan", Channel::Read, "s2", &ago(5)),
        // Outside the 60-day window: does not make goal-execute used.
        ev("x1", "goal-execute", Channel::Slash, "s0", &ago(90)),
    ];
    let input = w.input(&events);
    let v: Value = serde_json::from_str(&report::unused(&input, Since::Days(60), true, false)).unwrap();
    let by: BTreeMap<String, Value> =
        v["skills"].as_array().unwrap().iter().map(|r| (r["skill"].as_str().unwrap().to_string(), r.clone())).collect();
    let names = |xs: &Value| xs.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>();

    assert!(!by.contains_key("unslop"), "used skills are not listed");
    for (skill, cluster) in [
        ("goal-smart", vec!["goal-execute", "goal-plan"]),
        ("goal-plan", vec!["goal-smart"]),
        ("goal-execute", vec!["goal-smart"]),
    ] {
        assert_eq!(by[skill]["status"], "unused", "{skill}");
        assert_eq!(names(&by[skill]["cluster"]), cluster, "{skill}");
    }
    assert_eq!(by["goal-plan"]["weak_only"], true);
    assert_eq!(by["goal-smart"]["weak_only"], false);

    assert_eq!(by["writing-for-agents"]["status"], "held");
    assert_eq!(names(&by["writing-for-agents"]["holders"]), ["unslop"]);
    // Held is not transitive: wfa-helper's only citer is held, not used.
    assert_eq!(by["wfa-helper"]["status"], "unused");
    assert_eq!(names(&by["wfa-helper"]["cluster"]), ["writing-for-agents"]);
    // new wins over held: unslop cites the uncommitted fresh.
    assert_eq!(by["fresh"]["status"], "new");
    assert!(names(&by["fresh"]["holders"]).is_empty());

    let text = report::unused(&input, Since::Days(60), false, false);
    let unused_lines: Vec<&str> = text.lines().filter(|l| l.contains(" unused ")).collect();
    assert!(
        unused_lines.iter().any(|l| l.starts_with("goal-smart") && l.ends_with("cluster: goal-execute, goal-plan"))
    );
    assert!(!unused_lines.iter().any(|l| l.starts_with("writing-for-agents")), "{text}");
    assert!(text.contains("writing-for-agents held   held by unslop"), "{text}");
    assert!(text.ends_with("window: claude since 2026-09-10 · codex since 2026-08-14\n"), "{text}");
}

#[test]
fn a_skill_first_committed_inside_the_window_is_new() {
    let repo = fixture_repo();
    let w = world(repo.path());
    let input = w.input(&[]);
    let v: Value = serde_json::from_str(&report::unused(&input, Since::Days(400), true, false)).unwrap();
    let new: Vec<&str> = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["status"] == "new")
        .map(|r| r["skill"].as_str().unwrap())
        .collect();
    assert_eq!(new.len(), 7, "the 2026-01-01 commit is inside a 400-day window: {new:?}");
}

// ---------------------------------------------------------------- usage

#[test]
fn usage_collapses_and_applies_default_filters() {
    let repo = fixture_repo();
    let w = world(repo.path());
    let d1 = ago(1);
    let mut probe = ev("probe", "unslop", Channel::Tool, "P", &d1);
    probe.actor = Actor::Probe;
    let mut other = ev("other", "unslop", Channel::Slash, "O", &d1);
    other.origin = Origin::Other;
    let mut stored_authoring = ev("auth", "unslop", Channel::Read, "C", &d1);
    stored_authoring.authoring = true;
    let mut sub = ev("sub", "unslop", Channel::Tool, "B", &ago(2));
    sub.actor = Actor::Subagent;
    let mut codex_a = ev("cx1", "unslop", Channel::Read, "A", &d1);
    codex_a.agent = Agent::Codex;
    let mut codex_b = ev("cx2", "unslop", Channel::Read, "A", &d1);
    codex_b.agent = Agent::Codex;
    let events = vec![
        ev("t1", "unslop", Channel::Tool, "A", &d1),
        ev("t2", "unslop", Channel::Tool, "A", &d1), // same (agent, session, skill, channel)
        ev("s1", "unslop", Channel::Slash, "A", &d1),
        ev("r1", "unslop", Channel::Read, "A", &d1),
        ev("r2", "unslop", Channel::Read, "A", &d1), // a second chunk of the same file
        ev("f1", "unslop", Channel::Ref, "A", &d1),
        sub,
        codex_a,
        codex_b,
        probe,
        other,
        stored_authoring,
        // Report-time authoring: session D also loads `grade`.
        ev("g1", "grade", Channel::Tool, "D", &d1),
        ev("dr", "unslop", Channel::Read, "D", &d1),
        ev("dt", "unslop", Channel::Tool, "D", &d1), // strong events are never authoring
        ev("old", "unslop", Channel::Tool, "Z", &ago(40)),
        ev("ty", "unslop", Channel::Typed, "Y", "2026-02-24T10:00:00Z"),
    ];
    let input = w.input(&events);

    let out = report::usage(&input, &args(), true, false);
    let r = row(&out, "unslop");
    // sess: A, B, D. tool: A, B, D. read: claude A, codex A.
    assert_eq!(counts(&r), [3, 1, 3, 0, 2, 1]);
    assert_eq!(r["agents"], "CX");
    assert_eq!(r["last"], d1.as_str());
    let spark: Vec<u64> = r["30d"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect();
    assert_eq!(spark.len(), 30);
    assert_eq!(&spark[27..], [1, 3, 0], "two days ago: B/tool; yesterday: A/tool, A/slash, D/tool");
    assert_eq!(counts(&row(&out, "goal-plan")), [0; 6], "zero-use skills get a row");
    assert_eq!(row(&out, "goal-plan")["last"], Value::Null);

    let all = report::usage(&input, &UsageArgs { all: true, ..args() }, true, false);
    assert_eq!(counts(&row(&all, "unslop")), [5, 2, 4, 0, 2, 1], "--all adds the probe and the other origin");

    let authoring = report::usage(&input, &UsageArgs { authoring: true, ..args() }, true, false);
    assert_eq!(counts(&row(&authoring, "unslop")), [3, 1, 3, 0, 4, 1], "--authoring adds sessions C and D");

    let mut plain = world(repo.path());
    plain.cfg.policy.authoring_skills.clear();
    let out = report::usage(&plain.input(&events), &args(), true, false);
    assert_eq!(counts(&row(&out, "unslop")), [3, 1, 3, 0, 3, 1], "no authoring_skills: only the cwd rule tags C");

    let since_all = report::usage(&input, &UsageArgs { since: Since::All, ..args() }, true, false);
    assert_eq!(counts(&row(&since_all, "unslop")), [5, 1, 4, 1, 2, 1]);

    let codex = report::usage(&input, &UsageArgs { agent: Some(Agent::Codex), ..args() }, true, false);
    assert_eq!(counts(&row(&codex, "unslop")), [0, 0, 0, 0, 1, 0]);
    let slash = report::usage(&input, &UsageArgs { channel: Some(Channel::Slash), ..args() }, true, false);
    assert_eq!(counts(&row(&slash, "unslop")), [1, 1, 0, 0, 0, 0]);

    let text = report::usage(&input, &args(), false, false);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("skill") && lines[0].ends_with(&format!("30d{}grade", " ".repeat(28))), "{text}");
    assert!(lines[1].starts_with("unslop "), "highest sess first: {text}");
    assert!(lines[1].contains(" CX     1d ago "), "{text}");
    assert!(lines[1].ends_with(&format!("{}▄█▁     -", "▁".repeat(27))), "{text}");
    assert_eq!(
        *lines.last().unwrap(),
        "coverage: claude since 2026-09-10 (typed since 2026-02-24) · codex since 2026-03-29"
    );
    assert!(!text.contains('\x1b'), "no bold off a tty");
    assert!(report::usage(&input, &args(), false, true).starts_with("\x1b[1mskill"));
}

#[test]
fn usage_shows_the_latest_grade_and_staleness() {
    let repo = fixture_repo();
    write(&repo.path().join(".grades/unslop/g.md"), "---\ntarget: unslop\ndate: 2026-09-01\nscore: 9.1\n---\n");
    git(repo.path(), &["add", "."], NOW);
    git(repo.path(), &["commit", "-q", "-m", "grade"], "2026-09-01T00:00:00Z");
    let w = world(repo.path());
    let input = w.input(&[]);
    assert_eq!(row(&report::usage(&input, &args(), true, false), "unslop")["grade"]["stale"], false);
    write(&repo.path().join("unslop/SKILL.md"), "---\nname: unslop\ndescription: Edited.\n---\n");
    let text = report::usage(&input, &args(), false, false);
    assert!(text.lines().any(|l| l.starts_with("unslop") && l.ends_with(" 9.1*")), "{text}");
}

// ---------------------------------------------------------------- show

#[test]
fn show_lists_events_with_expired_locators_and_retro_counts() {
    let repo = fixture_repo();
    let transcript = repo.path().join("t.jsonl");
    write(&transcript, "{}\n");
    write(&repo.path().join(".retros/unslop/r.md"), "---\nskill: unslop\ndate: \"2026-10-05\"\nscore: 7\n---\n");
    let w = world(repo.path());
    let mut live = ev("live", "unslop", Channel::Tool, "A", &ago(1));
    live.src = format!("{}:1", transcript.display());
    let events = vec![
        live,
        ev("gone", "unslop", Channel::Slash, "B", &ago(3)),
        ev("before", "unslop", Channel::Tool, "C", &ago(10)),
        ev("weak", "unslop", Channel::Read, "D", &ago(3)),
    ];
    let input = w.input(&events);
    assert!(report::show(&input, "nope", Since::Days(30), 20, false, false).is_err());

    let v: Value =
        serde_json::from_str(&report::show(&input, "unslop", Since::Days(30), 2, true, false).unwrap()).unwrap();
    let shown = v["events"].as_array().unwrap();
    assert_eq!(shown.len(), 2);
    assert_eq!(v["total_events"], 4);
    assert_eq!(shown[0]["expired"], false, "newest first, its transcript exists");
    assert_eq!(shown[1]["expired"], true);
    assert_eq!(v["retros"]["count"], 1);
    assert_eq!(v["retros"]["latest"], "2026-10-05");
    assert_eq!(v["retros"]["strong_since"], 2, "A and B, after 2026-10-05; C was before");
    assert_eq!(v["grade"], Value::Null);
    let hist = v["histogram"].as_array().unwrap();
    assert_eq!(hist.len(), 3);
    assert_eq!(hist[1]["strong"], 1);
    assert_eq!(hist[1]["weak"], 1);

    let text = report::show(&input, "unslop", Since::Days(30), 20, false, false).unwrap();
    assert!(text.contains(".jsonl:1 expired"), "{text}");
    assert!(text.contains("retros: 1, latest 2026-10-05, 2 strong since"), "{text}");
}

// ---------------------------------------------------------------- evals

#[test]
fn evals_latest_date_and_staleness() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    git(src, &["init", "-q"], NOW);
    write(&src.join("s/SKILL.md"), "---\nname: s\ndescription: v1\n---\n");
    write(&src.join(".grades/GRADES.md"), "---\ntarget: s\ndate: 2027-01-01\nscore: 1\n---\n");
    write(&src.join(".grades/s/notes.md"), "# no frontmatter\n");
    write(&src.join(".grades/s/bad.md"), "---\ndate: 2027-01-01\ndate: 2027-01-02\n---\n");
    write(&src.join(".grades/s/a.md"), "---\ntarget: s\ndate: 2026-01-15\nscore: 7\n---\n");
    write(&src.join(".grades/s/b.md"), "---\ntarget: s\ndate: \"2026-02-01\"\nscore: 8.5\n---\n");
    write(&src.join(".retros/s/r0.md"), "---\nskill: s\ndate: 2026-03-05\n---\n");
    write(&src.join(".retros/s/r1.md"), "---\nskill: s\ndate: \"2026-03-05\"\n---\n");
    write(&src.join(".retros/s/r2.md"), "---\nskill: s\ndate: 2026-03-04\n---\n");
    git(src, &["add", "."], NOW);
    git(src, &["commit", "-q", "-m", "skill and grades"], "2026-02-01T00:00:00Z");

    let e = evals::load(src, "s");
    let g = e.grade.as_ref().unwrap();
    assert!(
        g.eval.file.ends_with("b.md"),
        "quoted 2026-02-01 beats unquoted 2026-01-15; GRADES.md, notes.md, bad.md skipped"
    );
    assert_eq!(g.eval.score.as_deref(), Some("8.5"));
    assert!(!g.stale, "a grade committed together with the skill edit is not stale");
    let retros: Vec<&str> = e.retros.iter().map(|r| r.file.file_name().unwrap().to_str().unwrap()).collect();
    assert_eq!(retros, ["r2.md", "r0.md", "r1.md"], "by date, ties by filename");

    write(&src.join("s/SKILL.md"), "---\nname: s\ndescription: v2\n---\n");
    git(src, &["commit", "-q", "-am", "edit skill"], "2026-03-01T00:00:00Z");
    assert!(evals::load(src, "s").grade.unwrap().stale, "a later skill commit makes it stale");

    write(&src.join(".grades/s/c.md"), "---\ntarget: s\ndate: 2026-04-01\nscore: 9\n---\n");
    let g = evals::load(src, "s").grade.unwrap();
    assert!(g.eval.file.ends_with("c.md") && !g.stale, "an uncommitted grade is not stale");

    write(&src.join("s/SKILL.md"), "---\nname: s\ndescription: v3\n---\n");
    assert!(evals::load(src, "s").grade.unwrap().stale, "uncommitted skill changes make any grade stale");
}
