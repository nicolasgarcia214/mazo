//! Fan-in, held and evals, plus the report rules (filters,
//! collapse, sess, authoring) on small fixture ledgers.

#![allow(clippy::disallowed_methods)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use common::{
    CLAUDE_FIRST, CODEX_FIRST, NOW, ago, copy_tree, event, git, grade, report_repo, report_world, skill_record, write,
};
use mazo::inventory::{self, Inventory};
use mazo::ledger::{Actor, Agent, Channel, Event, Origin};
use mazo::report::{self, Since, UsageArgs};
use mazo::{evals, render};
use serde_json::Value;

/// The last line of the fixture's `usage`: only the marks its table shows.
const LEGEND: &str = "legend: agents C Claude, X Codex, P Pi; \
                      ● name and description in context: Claude on, every Codex skill, Pi by default";

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

/// One skill per citation rule in `tests/fixtures/skills`: diagnose-bug cites
/// `the blast-radius skill`, review-audit-fixes the same broken across lines,
/// triage-finding `owned by grade`, unslop the `writing-for-agents/` path (not
/// `upgrade/` or `x-grade/`), and writing-for-agents `the goal-smart skill` in
/// a reference file. interrogate's description and its `owned by graders`, and
/// grade's self-mentions, yield nothing.
#[test]
fn fan_in_on_the_fixture_tree_matches_each_rule() {
    let src = common::fixture("skills");
    let g = inventory::fan_in(&Inventory::load(&src).unwrap());
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

/// Only with `MAZO_TEST_SKILLS`: invariants that hold on any skills repository.
#[test]
fn fan_in_on_a_real_repo_has_no_self_edges_and_only_skill_targets() {
    let Some(src) = common::test_skills_env() else {
        eprintln!("fan_in real repo: SKIPPED (MAZO_TEST_SKILLS unset)");
        return;
    };
    let inv = Inventory::load(&src).unwrap();
    let skills = &inv.skills;
    let g = inventory::fan_in(&inv);
    assert_eq!(&g.keys().cloned().collect::<BTreeSet<_>>(), skills);
    for (a, cited) in &g {
        assert!(!cited.contains(a), "self-edge {a}");
        assert!(cited.is_subset(skills), "{a} cites a non-skill: {cited:?}");
    }
}

#[test]
fn fan_in_on_fixtures_ignores_frontmatter_and_name_suffixes() {
    let repo = report_repo();
    let g = inventory::fan_in(&Inventory::load(repo.path()).unwrap());
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
    let repo = report_repo();
    let w = report_world(repo.path());
    let events = vec![
        event("u1", "unslop", Channel::Tool, "s1", &ago(10)),
        event("p1", "goal-plan", Channel::Read, "s2", &ago(5)),
        // Outside the 60-day window: does not make goal-execute used.
        event("x1", "goal-execute", Channel::Slash, "s0", &ago(90)),
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
    let window = format!("window: claude since {} · codex since {}", &CLAUDE_FIRST[..10], &ago(60)[..10]);
    assert!(text.ends_with(&format!("{window}\n")), "unused shows no mark, so no legend: {text}");
}

#[test]
fn a_skill_first_committed_inside_the_window_is_new() {
    let repo = report_repo();
    let w = report_world(repo.path());
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

/// Store skills installed 10 and 90 days ago and one missing from the
/// lockfile, beside the report fixture's source skills.
#[test]
fn a_store_skill_is_new_from_its_install_date() {
    let repo = report_repo();
    let data = tempfile::tempdir().unwrap();
    let mut lock = String::new();
    for (name, installed) in [("late", Some(ago(10))), ("early", Some(ago(90))), ("unlisted", None)] {
        write(&data.path().join("store").join(name).join("SKILL.md"), &format!("---\nname: {name}\n---\n"));
        if let Some(at) = installed {
            lock += &format!(
                "[[skill]]\nname = \"{name}\"\norigin = \"/x\"\nsubpath = \".\"\n\
                 content_hash = \"h\"\ntrusted = true\ninstalled_at = \"{at}\"\n"
            );
        }
    }
    write(&data.path().join("mazo.lock"), &lock);
    let mut w = report_world(repo.path());
    w.inv = mazo::store::inventory(Some(repo.path()), data.path()).unwrap();
    let v: Value = serde_json::from_str(&report::unused(&w.input(&[]), Since::Days(60), true, false)).unwrap();
    let status: BTreeMap<&str, &str> = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["skill"].as_str().unwrap(), r["status"].as_str().unwrap()))
        .collect();
    assert_eq!(status["late"], "new");
    assert_eq!(status["early"], "unused");
    assert_eq!(status["unlisted"], "new", "an unknown install date is new");
    // Source skills keep their git dates: committed 2026-01-01, and never.
    assert_eq!(status["goal-smart"], "unused");
    assert_eq!(status["fresh"], "new");
}

// ---------------------------------------------------------------- named by

/// unslop names goal-plan in prose (`x-goal-plan/` is not a word); every
/// other whole-word name in the tree is already an edge, so none repeats.
#[test]
fn mentions_on_fixtures_are_whole_words_that_are_not_edges() {
    let repo = report_repo();
    let inv = Inventory::load(repo.path()).unwrap();
    let edges = inventory::fan_in(&inv);
    let m = inventory::mentions(&inv, &edges);
    let pairs: Vec<String> = m.iter().flat_map(|(a, bs)| bs.iter().map(move |b| format!("{a}->{b}"))).collect();
    assert_eq!(pairs, ["unslop->goal-plan"]);
    assert_eq!(m.keys().cloned().collect::<BTreeSet<_>>(), inv.skills);
}

/// `router` names wfa-helper in a routing table and is used; wfa-helper
/// stays unused, with the namer noted, and is never held by it.
#[test]
fn unused_notes_a_skill_named_by_a_used_skill_and_keeps_it_unused() {
    let repo = report_repo();
    write(
        &repo.path().join("router/SKILL.md"),
        "---\nname: router\ndescription: Routes.\n---\n\n| Need | Skill |\n|---|---|\n| Pi helpers | wfa-helper |\n",
    );
    git(repo.path(), &["add", "router"], NOW);
    git(repo.path(), &["commit", "-q", "-m", "router"], "2026-01-01T00:00:00Z");
    let w = report_world(repo.path());
    let events = vec![event("r1", "router", Channel::Slash, "s1", &ago(5))];
    let input = w.input(&events);

    let v: Value = serde_json::from_str(&report::unused(&input, Since::Days(60), true, false)).unwrap();
    let rows = v["skills"].as_array().unwrap();
    let keys = ["cluster", "holders", "named_by", "skill", "status", "weak_only"];
    for r in rows {
        assert_eq!(r.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(), keys, "{r}");
        assert!(r["named_by"].is_array(), "{r}");
    }
    let wfa = rows.iter().find(|r| r["skill"] == "wfa-helper").unwrap();
    assert_eq!(wfa["status"], "unused");
    assert_eq!(wfa["named_by"], serde_json::json!(["router"]));
    assert_eq!(wfa["holders"], serde_json::json!([]));
    // unslop names goal-plan but is not used in the window.
    let plan = rows.iter().find(|r| r["skill"] == "goal-plan").unwrap();
    assert_eq!(plan["named_by"], serde_json::json!([]));

    let text = report::unused(&input, Since::Days(60), false, false);
    let line = text.lines().find(|l| l.starts_with("wfa-helper")).unwrap();
    assert!(line.contains(" unused ") && line.contains("named by router"), "{text}");
}

/// On a terminal, `unused` paints its status words and the `named by` note
/// and `usage` only a stale grade's mark; `NO_COLOR=1` turns all of it off.
#[test]
fn status_tokens_are_coloured_on_a_tty_unless_no_color() {
    let repo = report_repo();
    write(
        &repo.path().join("router/SKILL.md"),
        "---\nname: router\ndescription: Routes.\n---\n\n| Need | Skill |\n|---|---|\n| Pi helpers | wfa-helper |\n",
    );
    grade(&repo.path().join(".evals"), "unslop", "2026-09-01", "9.1", Some(&repo.path().join("unslop")));
    git(repo.path(), &["add", "router"], NOW);
    git(repo.path(), &["commit", "-q", "-m", "router"], "2026-01-01T00:00:00Z");
    let skill_md = repo.path().join("unslop/SKILL.md");
    write(&skill_md, &(std::fs::read_to_string(&skill_md).unwrap() + "\nEdited.\n"));
    let w = report_world(repo.path());
    let events =
        vec![event("r1", "router", Channel::Slash, "s1", &ago(5)), event("u1", "unslop", Channel::Tool, "s2", &ago(5))];
    let input = w.input(&events);
    let (red, yellow, reset) = ("\x1b[31m", "\x1b[33m", "\x1b[0m");

    let on = render::colour(true, None);
    let text = report::unused(&input, Since::Days(60), false, on);
    let line = |skill: &str| text.lines().find(|l| l.starts_with(&format!("{skill} "))).unwrap();
    assert!(line("wfa-helper").contains(&format!("{red}unused{reset}")), "{text}");
    assert!(line("wfa-helper").contains(&format!("{yellow}named by router{reset}")), "{text}");
    assert!(line("writing-for-agents").contains(&format!("{yellow}held{reset}   held by unslop")), "{text}");
    assert!(line("fresh").contains(&format!("{yellow}new{reset}")), "{text}");
    assert!(line("goal-smart").ends_with(&format!("{reset} cluster: goal-execute, goal-plan")), "{text}");
    let usage = report::usage(&input, &args(), false, on);
    let unslop = usage.lines().find(|l| l.starts_with("unslop ")).unwrap();
    assert!(unslop.ends_with(&format!("9.1 {yellow}stale{reset} -")), "{usage}");
    assert_eq!(usage.lines().skip(1).map(|l| l.matches('\x1b').count()).sum::<usize>(), 2, "only the mark: {usage}");

    let off = render::colour(true, Some(std::ffi::OsStr::new("1")));
    assert!(!report::unused(&input, Since::Days(60), false, off).contains('\x1b'));
    assert!(!report::usage(&input, &args(), false, off).contains('\x1b'));
}

// ---------------------------------------------------------------- usage

/// 7d and 30d count strong sessions over local days, whatever the window,
/// under the usage filters; trend compares 30d with the 30 days before.
#[test]
fn usage_counts_sessions_over_7_and_30_local_days_and_their_trend() {
    let repo = report_repo();
    let w = report_world(repo.path());
    let mut codex = event("x", "unslop", Channel::Tool, "X", &ago(2));
    codex.agent = Agent::Codex;
    let mut probe = event("p", "unslop", Channel::Tool, "P", &ago(1));
    probe.actor = Actor::Probe;
    let events = vec![
        event("a", "unslop", Channel::Tool, "A", "2026-10-07T00:30:00Z"), // 6 days ago
        event("b1", "unslop", Channel::Slash, "B", "2026-10-06T23:59:00Z"), // 7 days ago
        event("b2", "unslop", Channel::Tool, "B", &ago(1)),
        event("c", "unslop", Channel::Tool, "C", &ago(29)),
        // Inside the 30d window by the hour, but 30 local days ago.
        event("e", "unslop", Channel::Tool, "E", "2026-09-13T13:00:00Z"),
        event("d", "unslop", Channel::Tool, "D", &ago(59)),
        event("r", "unslop", Channel::Read, "R", &ago(1)),
        codex,
        probe,
    ];
    let input = w.input(&events);
    let periods = |args: &UsageArgs| {
        let r = row(&report::usage(&input, args, true, false), "unslop");
        (
            r["sess"].as_u64().unwrap(),
            r["sessions_7d"].as_u64().unwrap(),
            r["sessions_30d"].as_u64().unwrap(),
            r["trend"].clone(),
        )
    };
    assert_eq!(periods(&args()), (5, 3, 4, Value::from("up")), "7d: A, B, X; 30d: and C; before: E, D");
    assert_eq!(periods(&UsageArgs { since: Since::Days(7), ..args() }), (3, 3, 4, Value::from("up")));
    assert_eq!(periods(&UsageArgs { since: Since::All, ..args() }), (6, 3, 4, Value::from("up")));
    assert_eq!(periods(&UsageArgs { all: true, ..args() }).1, 4, "--all adds the probe");
    let codex = UsageArgs { agent: Some(Agent::Codex), ..args() };
    assert_eq!(periods(&codex), (1, 1, 1, Value::from("new")));
    let read = UsageArgs { channel: Some(Channel::Read), ..args() };
    assert_eq!(periods(&read), (0, 0, 0, Value::Null), "reads are weak");
    let r = row(&report::usage(&input, &args(), true, false), "unslop");
    assert_eq!(r["30d"].as_array().unwrap().len(), 30, "the per-day counts stay");
}

#[test]
fn usage_collapses_and_applies_default_filters() {
    let repo = report_repo();
    let w = report_world(repo.path());
    let d1 = ago(1);
    let typed = "2026-02-24T10:00:00Z";
    let mut probe = event("probe", "unslop", Channel::Tool, "P", &d1);
    probe.actor = Actor::Probe;
    let mut other = event("other", "unslop", Channel::Slash, "O", &d1);
    other.origin = Origin::Other;
    let mut stored_authoring = event("auth", "unslop", Channel::Read, "C", &d1);
    stored_authoring.authoring = true;
    let mut sub = event("sub", "unslop", Channel::Tool, "B", &ago(2));
    sub.actor = Actor::Subagent;
    let mut codex_a = event("cx1", "unslop", Channel::Read, "A", &d1);
    codex_a.agent = Agent::Codex;
    let mut codex_b = event("cx2", "unslop", Channel::Read, "A", &d1);
    codex_b.agent = Agent::Codex;
    let events = vec![
        event("t1", "unslop", Channel::Tool, "A", &d1),
        event("t2", "unslop", Channel::Tool, "A", &d1), // same (agent, session, skill, channel)
        event("s1", "unslop", Channel::Slash, "A", &d1),
        event("r1", "unslop", Channel::Read, "A", &d1),
        event("r2", "unslop", Channel::Read, "A", &d1), // a second chunk of the same file
        event("f1", "unslop", Channel::Ref, "A", &d1),
        sub,
        codex_a,
        codex_b,
        probe,
        other,
        stored_authoring,
        // Report-time authoring: session D also loads `grade`.
        event("g1", "grade", Channel::Tool, "D", &d1),
        event("dr", "unslop", Channel::Read, "D", &d1),
        event("dt", "unslop", Channel::Tool, "D", &d1), // strong events are never authoring
        event("old", "unslop", Channel::Tool, "Z", &ago(40)),
        event("ty", "unslop", Channel::Typed, "Y", typed),
        // goal-execute's only strong event is older than the window.
        event("gx", "goal-execute", Channel::Tool, "Q", &ago(40)),
        // A pruned skill: in the ledger, not in the inventory.
        event("pr", "pruned", Channel::Tool, "R", &ago(90)),
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
    assert_eq!(row(&out, "goal-execute")["last"], Value::Null, "last is the last strong event in the window");
    assert!(!out.contains("\"pruned\""), "no row for a skill outside the inventory without --all");

    let all = report::usage(&input, &UsageArgs { all: true, ..args() }, true, false);
    assert_eq!(counts(&row(&all, "unslop")), [5, 2, 4, 0, 2, 1], "--all adds the probe and the other origin");
    assert_eq!(counts(&row(&all, "pruned")), [0; 6], "--all adds a row for a pruned skill, used outside the window");
    assert_eq!(row(&all, "pruned")["last"], Value::Null);

    let authoring = report::usage(&input, &UsageArgs { authoring: true, ..args() }, true, false);
    assert_eq!(counts(&row(&authoring, "unslop")), [3, 1, 3, 0, 4, 1], "--authoring adds sessions C and D");

    let mut plain = report_world(repo.path());
    plain.cfg.policy.authoring_skills.clear();
    let out = report::usage(&plain.input(&events), &args(), true, false);
    assert_eq!(counts(&row(&out, "unslop")), [3, 1, 3, 0, 3, 1], "no authoring_skills: only the cwd rule tags C");

    let since_all = report::usage(&input, &UsageArgs { since: Since::All, ..args() }, true, false);
    assert_eq!(counts(&row(&since_all, "unslop")), [5, 1, 4, 1, 2, 1]);
    assert_eq!(row(&since_all, "goal-execute")["last"], ago(40).as_str());

    let codex = report::usage(&input, &UsageArgs { agent: Some(Agent::Codex), ..args() }, true, false);
    assert_eq!(counts(&row(&codex, "unslop")), [0, 0, 0, 0, 1, 0]);
    let slash = report::usage(&input, &UsageArgs { channel: Some(Channel::Slash), ..args() }, true, false);
    assert_eq!(counts(&row(&slash, "unslop")), [1, 1, 0, 0, 0, 0]);

    let text = report::usage(&input, &args(), false, false);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with("skill")
            && lines[0].ends_with(" sess 7d 30d trend slash tool typed read ref agents ctx  last   grade audit"),
        "{text}"
    );
    assert!(lines[1].starts_with("unslop "), "highest sess first: {text}");
    assert!(lines[1].contains("  3  3   3 ↑ "), "Z 40 days ago is in the 30 before: {text}");
    assert!(lines[1].ends_with(" CX     C●X● 1d ago     - -"), "no strip: {text}");
    let execute = lines.iter().find(|l| l.starts_with("goal-execute ")).unwrap();
    assert!(execute.contains("  0  0   0 ↓ "), "{text}");
    let plan = lines.iter().find(|l| l.starts_with("goal-plan ")).unwrap();
    assert_eq!(plan.split_whitespace().nth(4), Some("0"), "blank trend, then slash: {text}");
    assert_eq!(row(&out, "goal-plan")["trend"], Value::Null);
    assert_eq!(
        lines[lines.len() - 2],
        format!(
            "coverage: claude since {} (typed since {}) · codex since {}",
            &CLAUDE_FIRST[..10],
            &typed[..10],
            &CODEX_FIRST[..10]
        )
    );
    assert_eq!(*lines.last().unwrap(), LEGEND);
    assert!(!text.contains('\x1b'), "no bold off a tty");
    assert!(report::usage(&input, &args(), false, true).starts_with("\x1b[1mskill"));
}

/// The audit column marks a vet security finding `!`, else an audit `✓`,
/// else a vet `vet`, else `-`; `--json` carries the latest audit's verdict,
/// else the vet's led by any security finding, and its source apart. The
/// card prints the vet verdict whenever there is one.
#[test]
fn usage_audit_marks_a_security_finding_else_an_audit_else_a_vet() {
    let repo = report_repo();
    let data = tempfile::tempdir().unwrap();
    common::vetted(data.path());
    let long = "Écrit très bien, même les longues notes de version.";
    let audits = skill_record("OLD", "2026-10-01T10:00:00Z") + &skill_record(long, "2026-10-12T10:00:00Z");
    write(&data.path().join("audits.jsonl"), &audits);
    let mut w = report_world(repo.path());
    w.inv = mazo::store::inventory(Some(repo.path()), data.path()).unwrap();
    w.audits = mazo::audit::latest(data.path()).unwrap();
    let input = w.input(&[]);

    let json = report::usage(&input, &args(), true, false);
    let source = |skill: &str| {
        let r = row(&json, skill);
        (r["verdict_source"].clone(), r["verdict"].clone())
    };
    assert_eq!(source("unslop"), (Value::from("audit"), Value::from(long)), "the latest audit");
    assert_eq!(source("vetted"), (Value::from("vet"), Value::from("quality weak, value low value")));
    assert_eq!(source("goal-plan"), (Value::Null, Value::Null));
    assert_eq!(row(&json, "unslop")["sess"], 0, "the existing fields stay");

    let text = report::usage(&input, &args(), false, false);
    let line = |skill: &str| text.lines().find(|l| l.starts_with(&format!("{skill} "))).unwrap().to_string();
    assert!(line("unslop").ends_with(" ✓"), "{text}");
    assert!(line("vetted").ends_with(" vet"), "{text}");
    assert!(line("goal-plan").ends_with(" -"), "{text}");

    // A security finding leads the vet verdict and marks the cell.
    let lock = data.path().join("mazo.lock");
    let text = std::fs::read_to_string(&lock).unwrap();
    write(&lock, &text.replace("[skill.verdict]\n", "[skill.verdict]\nsecurity = \"Sends files to a remote host.\"\n"));
    w.inv = mazo::store::inventory(Some(repo.path()), data.path()).unwrap();
    let finding = "security: Sends files to a remote host.; quality weak, value low value";
    let input = w.input(&[]);
    assert_eq!(row(&report::usage(&input, &args(), true, false), "vetted")["verdict"], finding);
    let text = report::usage(&input, &args(), false, true);
    assert!(text.lines().any(|l| l.starts_with("vetted ") && l.ends_with(" \x1b[31m!\x1b[0m")), "{text}");

    // An audit wins over the vet's verdict, not over its security mark, and the card prints both.
    write(
        &data.path().join("audits.jsonl"),
        &(audits + &skill_record("Fine.", NOW).replace("\"unslop\"", "\"vetted\"")),
    );
    w.audits = mazo::audit::latest(data.path()).unwrap();
    let input = w.input(&[]);
    let json = report::usage(&input, &args(), true, false);
    assert_eq!(row(&json, "vetted")["verdict_source"], "audit");
    assert_eq!(row(&json, "vetted")["verdict"], "Fine.");
    let text = report::usage(&input, &args(), false, false);
    assert!(text.lines().any(|l| l.starts_with("vetted ") && l.ends_with(" !")), "{text}");
    let card = report::show(&input, "vetted", Since::Days(30), 20, false, false).unwrap();
    assert!(card.contains(&format!("\nvet: {finding}\naudit: ")), "{card}");
    assert!(card.contains(": Fine.\n"), "{card}");
}

/// The `--json` usage row of `skill` in `scope`.
fn scoped(out: &str, skill: &str, scope: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(out).unwrap();
    v["rows"].as_array().unwrap().iter().find(|r| r["skill"] == skill && r["scope"] == scope).cloned()
}

#[test]
fn usage_lists_a_projects_skills_apart_from_the_global_ones() {
    let repo = report_repo();
    // A git project whose store holds `vetted`, vetted in its `mazo.lock`, and an `unslop` of its own.
    let project = tempfile::tempdir().unwrap();
    let p = project.path();
    git(p, &["init", "-q"], NOW);
    common::vetted(p);
    std::fs::create_dir(p.join(".mazo")).unwrap();
    std::fs::rename(p.join("store"), p.join(".mazo/skills")).unwrap();
    write(&p.join(".mazo/skills/unslop/SKILL.md"), "---\nname: unslop\n---\n");
    let at = |rel: &str| Some(p.join(rel).display().to_string());
    let other = |mut e: Event, path: Option<String>| {
        e.origin = Origin::Other;
        e.path = path;
        e
    };
    let mut events = vec![
        event("g", "unslop", Channel::Tool, "s1", &ago(1)),
        other(event("pu", "unslop", Channel::Tool, "s2", &ago(1)), at(".agents/skills/unslop/SKILL.md")),
        other(event("pr", "vetted", Channel::Read, "s3", &ago(2)), at(".claude/skills/vetted/SKILL.md")),
        other(event("ps", "vetted", Channel::Slash, "s4", &ago(2)), at(".mazo/skills/vetted/SKILL.md")),
        other(
            event("x", "vetted", Channel::Slash, "s5", &ago(2)),
            Some("/elsewhere/.claude/skills/vetted/SKILL.md".into()),
        ),
    ];
    // The same project reached through a symlink.
    let alias = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(p, alias.path().join("p")).unwrap();
    let through = alias.path().join("p/.mazo/skills/unslop/SKILL.md").display().to_string();
    events.push(other(event("pa", "unslop", Channel::Read, "s6", &ago(1)), Some(through)));
    // AC10: the global skill's grade is not the project skill's.
    grade(&repo.path().join(".evals"), "unslop", "2026-09-01", "9", None);
    let mut w = report_world(repo.path());
    let outside = report::usage(&w.input(&events), &args(), true, false);
    let outside_text = report::usage(&w.input(&events), &args(), false, false);
    w.inv.project = mazo::store::Project::at(p).unwrap();
    let input = w.input(&events);

    let json = report::usage(&input, &args(), true, false);
    let global = scoped(&json, "unslop", "global").unwrap();
    assert_eq!(counts(&global), [1, 0, 1, 0, 0, 0], "the project's load is not the global skill's: {global}");
    assert_eq!(global["grade"]["score"], "9");
    let own = scoped(&json, "unslop", "project").unwrap();
    assert_eq!(counts(&own), [1, 0, 1, 0, 1, 0], "the read through the symlink counts: {own}");
    assert_eq!((&own["grade"], &own["verdict"]), (&Value::Null, &Value::Null));
    let vetted = scoped(&json, "vetted", "project").unwrap();
    assert_eq!(counts(&vetted), [1, 1, 0, 0, 1, 0], "through either link or the store, not elsewhere: {vetted}");
    assert_eq!(
        (&vetted["verdict_source"], &vetted["verdict"]),
        (&"vet".into(), &"quality weak, value low value".into())
    );
    assert_eq!(scoped(&json, "vetted", "global"), None);
    let all = report::usage(&input, &UsageArgs { all: true, ..args() }, true, false);
    assert_eq!(scoped(&all, "vetted", "global"), None, "a name only the project has gets no global row");
    assert!(scoped(&all, "vetted", "project").is_some());
    let text = report::usage(&input, &args(), false, false);
    for skill in ["unslop (project) ", "vetted (project) ", "unslop "] {
        assert!(text.lines().any(|l| l.starts_with(skill)), "{skill}: {text}");
    }
    assert!(text.lines().any(|l| l.starts_with("vetted (project) ") && l.ends_with(" vet")));

    // Outside the project every row is global, and the text names no project.
    let rows = serde_json::from_str::<Value>(&outside).unwrap()["rows"].as_array().unwrap().clone();
    assert!(rows.iter().all(|r| r["scope"] == "global"), "{outside}");
    assert_eq!(counts(&row(&outside, "unslop")), [1, 0, 1, 0, 0, 0]);
    assert!(
        !outside_text.lines().any(|l| !l.starts_with("legend: ") && l.contains("(project)")),
        "the legend alone explains the mark: {outside_text}"
    );
}

/// The text and JSON grade of `skill` in a fresh report, which hashes the skill again.
fn grade_of(source: &Path, skill: &str) -> (String, Value) {
    let w = report_world(source);
    let input = w.input(&[]);
    let rows = report::usage_table(&input, &args()).rows;
    let r = rows.iter().find(|r| r.skill == skill).unwrap();
    (
        report::usage_cells(&input, r, false)[13].clone(),
        row(&report::usage(&input, &args(), true, false), skill)["grade"].clone(),
    )
}

/// AC6, AC11: the score alone while the grade's `content_hash` is the
/// skill's, then `stale` once the skill changes; a grade with no hash is stale.
#[test]
fn usage_grade_shows_the_score_then_stale_when_the_skill_changed() {
    let repo = report_repo();
    let src = repo.path();
    let evals = src.join(".evals");
    let summary = |g: &Value| (g["stale"].clone(), g["mark"].clone(), g["content_hash"].clone());
    let keys = |g: &Value| g.as_object().unwrap().keys().cloned().collect::<BTreeSet<String>>();

    assert_eq!(grade_of(src, "goal-plan").0, "-", "no grade");
    grade(&evals, "unslop", "2026-09-01", "9.1", None);
    let (cell, g) = grade_of(src, "unslop");
    assert_eq!((cell.as_str(), summary(&g)), ("9.1 stale", (true.into(), "stale".into(), Value::Null)), "no hash");
    assert_eq!(keys(&g), BTreeSet::from(["content_hash", "date", "file", "mark", "score", "stale"].map(String::from)));

    grade(&evals, "unslop", "2026-09-02", "9.1", Some(&src.join("unslop")));
    let hash = mazo::store::content_hash(&src.join("unslop")).unwrap();
    let (cell, g) = grade_of(src, "unslop");
    assert_eq!((cell.as_str(), summary(&g)), ("9.1", (false.into(), "".into(), hash.clone().into())), "current");

    write(&src.join("unslop/notes.md"), "edited\n");
    let (cell, g) = grade_of(src, "unslop");
    assert_eq!((cell.as_str(), summary(&g)), ("9.1 stale", (true.into(), "stale".into(), hash.into())), "edited");
}

/// AC2, AC3: a store skill's grade comes from `evals_dir`, as a source
/// skill's does, and `<source>/.grades` is not read.
#[test]
fn grades_come_from_evals_dir_for_source_and_store_skills_only() {
    let repo = report_repo();
    let src = repo.path();
    let data = tempfile::tempdir().unwrap();
    common::vetted(data.path());
    let evals = src.join(".evals");
    grade(&evals, "vetted", "2026-09-01", "8.6", Some(&data.path().join("store/vetted")));
    grade(&src.join(".grades"), "unslop", "2026-09-01", "9", Some(&src.join("unslop")));
    let mut w = report_world(src);
    w.inv = mazo::store::inventory(Some(src), data.path()).unwrap();
    let input = w.input(&[]);
    let usage = report::usage(&input, &args(), true, false);
    assert_eq!(row(&usage, "vetted")["grade"]["score"], "8.6");
    assert_eq!(row(&usage, "vetted")["grade"]["stale"], false);
    assert_eq!(row(&usage, "unslop")["grade"], Value::Null, "<source>/.grades is not read");

    let card: Value =
        serde_json::from_str(&report::show(&input, "vetted", Since::Days(30), 20, true, false).unwrap()).unwrap();
    assert_eq!(card["grades"][0]["score"], "8.6");
    let hash = mazo::store::content_hash(&data.path().join("store/vetted")).unwrap();
    assert_eq!(card["content_hash"], hash.as_str(), "the hash mazo.lock records for the same tree");
    let text = report::show(&input, "vetted", Since::Days(30), 20, false, false).unwrap();
    assert!(text.contains("grades:\ndate       score mark\n2026-09-01   8.6\n"), "{text}");
}

// ---------------------------------------------------------------- show

#[test]
fn show_lists_events_with_expired_locators_and_retro_counts() {
    let repo = report_repo();
    let transcript = repo.path().join("t.jsonl");
    write(&transcript, "{}\n");
    write(&repo.path().join(".evals/retros/unslop/r.md"), "---\nskill: unslop\ndate: \"2026-10-05\"\nscore: 7\n---\n");
    let w = report_world(repo.path());
    let mut live = event("live", "unslop", Channel::Tool, "A", &ago(1));
    live.src = format!("{}:1", transcript.display());
    let events = vec![
        live,
        event("gone", "unslop", Channel::Slash, "B", &ago(3)),
        event("before", "unslop", Channel::Tool, "C", &ago(10)),
        event("weak", "unslop", Channel::Read, "D", &ago(3)),
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
    assert_eq!(v["retros"].as_array().unwrap().len(), 1);
    assert_eq!(v["retros"][0]["date"], "2026-10-05");
    assert_eq!(v["strong_since_retro"], 2, "A and B, after 2026-10-05; C was before");
    assert_eq!(v["grades"], serde_json::json!([]));
    let hist = v["histogram"].as_array().unwrap();
    assert_eq!(hist.len(), 3);
    assert_eq!(hist[1]["strong"], 1);
    assert_eq!(hist[1]["weak"], 1);

    let text = report::show(&input, "unslop", Since::Days(30), 20, false, false).unwrap();
    assert!(text.contains("claude:gone.jsonl:1 expired"), "{text}");
    assert!(text.contains("retros: 2 strong since the latest\n"), "{text}");
}

/// D5: the card's chart counts distinct (agent, session) pairs with a strong
/// event per day over 30 days, per ISO week from Monday over 60, and per
/// calendar month over all time, the bucket of today last and partial. NOW
/// is Tuesday 2026-10-13, in ISO week 42.
#[test]
fn the_cards_chart_buckets_strong_sessions_by_day_week_and_month() {
    let repo = report_repo();
    let w = report_world(repo.path());
    let mut codex = event("x1", "unslop", Channel::Tool, "A", &ago(0));
    codex.agent = Agent::Codex;
    let events = vec![
        event("a1", "unslop", Channel::Tool, "A", &ago(0)),
        event("a2", "unslop", Channel::Slash, "A", &ago(0)), // the same pair
        codex,
        event("r1", "unslop", Channel::Read, "R", &ago(0)),   // weak
        event("c1", "unslop", Channel::Tool, "C", &ago(1)),   // Monday of week 42
        event("d1", "unslop", Channel::Tool, "D", &ago(2)),   // Sunday of week 41
        event("e1", "unslop", Channel::Tool, "E", &ago(45)),  // 2026-08-29, week 35
        event("f1", "unslop", Channel::Tool, "F", &ago(100)), // 2026-07-05
    ];
    let input = w.input(&events);
    let chart = |since| {
        let card = report::skill_card(&input, "unslop", &UsageArgs::new(since), report::SHOW_LIMIT);
        card.activity.into_iter().map(|b| (b.label, b.sessions, b.partial)).collect::<Vec<_>>()
    };

    let days = chart(Since::Days(30));
    assert_eq!(days.len(), 31, "from the window's first day, 09-13, to today: {days:?}");
    assert_eq!(days[0], ("13".into(), 0, false));
    assert_eq!(days[28..], [("11".into(), 1, false), ("12".into(), 1, false), ("13".into(), 2, true)]);
    assert_eq!(days.iter().map(|d| d.1).sum::<usize>(), 4);

    let weeks = chart(Since::Days(60));
    let labels: Vec<&str> = weeks.iter().map(|w| w.0.as_str()).collect();
    assert_eq!(labels, ["W33", "W34", "W35", "W36", "W37", "W38", "W39", "W40", "W41", "W42"]);
    let counts: Vec<usize> = weeks.iter().map(|w| w.1).collect();
    assert_eq!(counts, [0, 0, 1, 0, 0, 0, 0, 0, 1, 3]);
    assert_eq!(weeks.iter().map(|w| w.2).collect::<Vec<_>>(), [&[false; 9][..], &[true]].concat());

    let months = chart(Since::All);
    assert_eq!(
        months,
        [("Jul".into(), 1, false), ("Aug".into(), 1, false), ("Sep".into(), 0, false), ("Oct".into(), 4, true)]
    );
}

/// The card's sections after the events, in display order, with `$HOME` as
/// `~` and short locators in text and full paths in `--json`.
#[test]
fn show_prints_the_card() {
    let repo = report_repo();
    let src = repo.path();
    let evals = src.join(".evals");
    grade(&evals, "unslop", "2026-02-01", "6", None);
    grade(&evals, "unslop", "2026-03-01", "7", Some(&src.join("unslop")));
    write(&src.join("unslop/notes.md"), "edit\n");
    git(src, &["add", "."], NOW);
    git(src, &["commit", "-q", "-m", "tighten unslop"], "2026-04-01T00:00:00Z");
    write(&src.join("unslop/notes.md"), "edit, uncommitted\n");
    grade(&evals, "unslop", "2026-05-01", "9", Some(&src.join("unslop")));
    write(
        &evals.join("retros/unslop/a.md"),
        "---\nskill: unslop\ndate: 2026-04-02\nscore: 5\nverdict: too eager\n---\n",
    );
    write(
        &evals.join("retros/unslop/b.md"),
        "---\nskill: unslop\ndate: 2026-10-05\nscore: 8\nverdict: holds up\n---\n",
    );
    write(&src.join("citer/SKILL.md"), "---\nname: citer\ndescription: Cites.\n---\n\nUse the unslop skill.\n");

    let home = std::env::var("HOME").unwrap();
    let w = report_world(src);
    let at = |id, skill, channel, session, days, cwd: &str| {
        let mut e = event(id, skill, channel, session, &ago(days));
        e.cwd = Some(cwd.into());
        e.src = format!("{home}/t/{id}.jsonl:7");
        e
    };
    let mut codex = at("x", "unslop", Channel::Tool, "X", 2, "/srv/b");
    codex.agent = Agent::Codex;
    let events = vec![
        at("a", "unslop", Channel::Slash, "A", 1, &format!("{home}/work")),
        at("b", "unslop", Channel::Slash, "B", 3, &format!("{home}/work")),
        at("c", "unslop", Channel::Tool, "C", 4, "/srv/a"),
        at("d", "unslop", Channel::Read, "D", 4, "/srv/weak"),
        at("e", "unslop", Channel::Tool, "E", 5, "/srv/c"),
        codex,
        at("old", "unslop", Channel::Slash, "O", 100, "/srv/old"),
    ];
    let input = w.input(&events);

    let text = report::show(&input, "unslop", Since::Days(30), 20, false, false).unwrap();
    let tail = &text[text.find("description:").expect("description after the events")..];
    let path = match src.strip_prefix(&home) {
        Ok(rel) => format!("path: ~/{}/unslop\n", rel.display()),
        Err(_) => format!("path: {}/unslop\n", src.display()),
    };
    let expected = [
        "description: Removes slop from prose.\n".to_string(),
        path,
        "history: first 2026-01-01 skills, last 2026-04-01 tighten unslop, uncommitted changes\n".into(),
        "grades:\n".into(),
        "date       score mark\n".into(),
        "2026-05-01     9\n".into(),
        "2026-03-01     7 stale\n".into(),
        "2026-02-01     6 stale\n".into(),
        "retros: 5 strong since the latest\n".into(),
        "date       score verdict\n".into(),
        "2026-10-05     8 holds up\n".into(),
        "2026-04-02     5 too eager\n".into(),
        "cites: fresh (held), writing-for-agents (held)\n".into(),
        "cited by: citer\n".into(),
        "strong: claude slash 2, claude tool 2, codex tool 1\n".into(),
        format!("last use: {} (1d ago)\n", &ago(1)[..10]),
        "projects: ~/work 2, /srv/a 1, /srv/b 1\n".into(),
        "invoke: claude /unslop, codex $unslop\n".into(),
        "examples: none\n".into(),
        "audit: none\n".into(),
    ];
    assert_eq!(tail, expected.concat(), "{text}");
    assert!(text.contains("~/work") && !text.contains(&format!("{home}/work")), "{text}");
    assert!(text.contains(" claude:a.jsonl:7 expired\n"), "{text}");

    let v: Value =
        serde_json::from_str(&report::show(&input, "unslop", Since::Days(30), 20, true, false).unwrap()).unwrap();
    let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let all = [
        "skill",
        "manual",
        "since",
        "histogram",
        "events",
        "total_events",
        "description",
        "path",
        "origin",
        "content_hash",
        "history",
        "grades",
        "retros",
        "strong_since_retro",
        "cites",
        "cited_by",
        "usage",
        "examples",
        "audit",
        "vet",
        "ctx",
    ];
    assert_eq!(keys, BTreeSet::from(all));
    assert_eq!(v["path"], src.join("unslop").display().to_string());
    assert_eq!(v["events"][0]["src"], format!("{home}/t/a.jsonl:7"));
    assert_eq!(v["events"][0]["cwd"], format!("{home}/work"));
    assert_eq!(v["history"]["last"]["subject"], "tighten unslop");
    assert_eq!(v["history"]["uncommitted"], true);
    let hash = mazo::store::content_hash(&src.join("unslop")).unwrap();
    assert_eq!(v["content_hash"], hash.as_str());
    let grades: Vec<(&str, bool)> = v["grades"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| (g["date"].as_str().unwrap(), g["stale"].as_bool().unwrap()))
        .collect();
    assert_eq!(
        grades,
        [("2026-05-01", false), ("2026-03-01", true), ("2026-02-01", true)],
        "every grade, newest first"
    );
    assert_eq!(v["grades"][0]["content_hash"], hash.as_str());
    assert_eq!(v["retros"][1]["verdict"], "too eager");
    assert_eq!(v["cited_by"], serde_json::json!([{"skill": "citer", "held": false}]));
    assert_eq!(v["usage"]["last_use"], ago(1));
    assert_eq!(v["usage"]["projects"][0], serde_json::json!({"cwd": format!("{home}/work"), "sessions": 2}));
    assert_eq!(v["examples"]["real"], serde_json::json!([]), "every transcript expired");
    assert_eq!(v["audit"], Value::Null);
}

/// Not a repository: no history section, and the grade still reads.
#[test]
fn show_outside_a_git_worktree_omits_history() {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&common::fixture("reports/skills"), dir.path(), &|s| s);
    grade(&dir.path().join(".evals"), "unslop", "2026-05-01", "9", Some(&dir.path().join("unslop")));
    let w = report_world(dir.path());
    assert!(w.inv.history.is_none());
    let input = w.input(&[]);
    let text = report::show(&input, "unslop", Since::Days(30), 20, false, false).unwrap();
    assert!(!text.contains("history:"), "{text}");
    assert!(text.contains("grades:\ndate       score mark\n2026-05-01     9\n"), "{text}");
    let v: Value =
        serde_json::from_str(&report::show(&input, "unslop", Since::Days(30), 20, true, false).unwrap()).unwrap();
    assert_eq!(v["history"], Value::Null);
    assert_eq!(v["grades"][0]["stale"], false);
}

// ---------------------------------------------------------------- examples

/// Real examples, on Claude transcripts whose line `n` is
/// the user message `msgs[n - 1]`.
#[test]
fn show_lists_written_and_real_examples() {
    let repo = report_repo();
    let src = repo.path();
    write(&src.join("unslop/agents/openai.yaml"), "interface:\n  default_prompt: \"Use $unslop on this draft.\"\n");
    let long = "Tighten this paragraph. ".repeat(20);
    let msgs = [
        "/unslop",
        "yes",
        long.as_str(),
        "Tighten the release notes for the launch.",
        "A subagent asked for this one, not the user.",
        "A probe asked for this one, not the user.",
        "Older prompt from the same project, never shown.",
        "A fourth project is never read.",
        "/a-skill-with-a-long-name",
        "Run the long-named skill on this file.",
    ];
    let transcript = src.join("t.jsonl");
    let lines: String = msgs
        .iter()
        .map(|m| format!("{}\n", serde_json::json!({"type": "user", "message": {"role": "user", "content": m}})))
        .collect();
    write(&transcript, &lines);
    write(&src.join("a-skill-with-a-long-name/SKILL.md"), "---\nname: a-skill-with-a-long-name\n---\n");

    let w = report_world(src);
    let of = |skill: &str, id: &str, line: usize, days, cwd: &str| {
        let mut e = event(id, skill, Channel::Tool, id, &ago(days));
        e.cwd = Some(cwd.into());
        e.src = format!("{}:{line}", transcript.display());
        e
    };
    let at = |id: &str, line, days, cwd: &str| of("unslop", id, line, days, cwd);
    let mut sub = at("sub", 5, 0, "/b");
    sub.actor = Actor::Subagent;
    let mut probe = at("probe", 6, 0, "/p");
    probe.actor = Actor::Probe;
    let mut weak = at("weak", 4, 0, "/w");
    weak.channel = Channel::Read;
    let gone = event("gone", "unslop", Channel::Slash, "gone", &ago(5));
    let events = vec![
        sub,
        probe,
        weak,
        at("bare", 1, 1, "/a"),
        at("yes", 2, 2, "/a"),
        at("long", 3, 3, "/a"),
        at("notes", 4, 4, "/b"),
        Event { cwd: Some("/c".into()), ..gone },
        at("older", 7, 6, "/a"),
        at("fourth", 8, 7, "/d"),
        of("a-skill-with-a-long-name", "invoked", 9, 1, "/g"),
        of("a-skill-with-a-long-name", "asked", 10, 2, "/g"),
    ];
    let input = w.input(&events);

    let v: Value =
        serde_json::from_str(&report::show(&input, "unslop", Since::Days(30), 20, true, false).unwrap()).unwrap();
    let ex = &v["examples"];
    assert_eq!(
        ex["written"],
        serde_json::json!([{"agent": "claude", "text": "/unslop"}, {"agent": "codex", "text": "$unslop"}])
    );
    assert_eq!(ex["default_prompt"], "Use $unslop on this draft.");
    let real: Vec<(&str, &str)> = ex["real"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["cwd"].as_str().unwrap(), r["text"].as_str().unwrap()))
        .collect();
    let cut: String = long.chars().take(300).collect();
    // /a skips the bare invocation and "yes" for the cut long prompt; /c's
    // transcript expired; /d is the fourth project.
    assert_eq!(real, [("/a", cut.as_str()), ("/b", msgs[3])]);
    assert_eq!(ex["real"][0]["ts"], ago(3));

    let text = report::show(&input, "unslop", Since::Days(30), 20, false, false).unwrap();
    assert!(
        text.contains("invoke: claude /unslop, codex $unslop\ndefault prompt: Use $unslop on this draft.\n"),
        "{text}"
    );
    assert!(text.contains(&format!(" /a: {}\n", cut.trim_end())), "{text}");
    assert!(text.ends_with(&format!(" /b: {}\naudit: none\n", msgs[3])), "{text}");
    for m in &msgs[4..] {
        assert!(!text.contains(m), "{m}: {text}");
    }

    // A 25-character prompt that is only the invocation gives way too.
    let v: Value = serde_json::from_str(
        &report::show(&input, "a-skill-with-a-long-name", Since::Days(30), 20, true, false).unwrap(),
    )
    .unwrap();
    assert_eq!(v["examples"]["real"][0]["text"], msgs[9]);
    assert_eq!(v["examples"]["real"].as_array().unwrap().len(), 1);
}

/// No events, no manifest: the invocations alone.
#[test]
fn show_without_events_has_no_real_examples() {
    let repo = report_repo();
    let w = report_world(repo.path());
    let input = w.input(&[]);
    let text = report::show(&input, "unslop", Since::Days(30), 20, false, false).unwrap();
    assert!(text.ends_with("invoke: claude /unslop, codex $unslop\nexamples: none\naudit: none\n"), "{text}");
    let v: Value =
        serde_json::from_str(&report::show(&input, "unslop", Since::Days(30), 20, true, false).unwrap()).unwrap();
    assert_eq!(v["examples"]["default_prompt"], Value::Null);
    assert_eq!(v["examples"]["real"], serde_json::json!([]));
}

// ---------------------------------------------------------------- manual

/// A store skill the judge vetted: For and Try from its lockfile entry, not
/// its description, and Your use from transcripts, each prompt's first line
/// cut to 100 characters. The manual opens the card.
#[test]
fn show_opens_a_vetted_store_skill_with_its_manual() {
    let repo = report_repo();
    let data = tempfile::tempdir().unwrap();
    write(&data.path().join("store/vetted/SKILL.md"), "---\nname: vetted\ndescription: Not the For.\n---\n");
    write(
        &data.path().join("mazo.lock"),
        "[[skill]]\nname = \"vetted\"\norigin = \"/x\"\nsubpath = \".\"\ncontent_hash = \"h\"\n\
         trusted = false\ninstalled_at = \"2026-01-01T00:00:00Z\"\nvetted_at = \"2026-01-01T00:00:00Z\"\n\
         for = \"Tidies prose.\"\ntry = [\"Tidy a README.\", \"Tidy a commit body.\", \"Tidy a PR.\"]\n",
    );
    let long = "Tidy this paragraph. ".repeat(10);
    let msgs = ["Tidy the release notes for the launch.\nThen the changelog.", long.as_str()];
    let transcript = repo.path().join("t.jsonl");
    let lines: String = msgs
        .iter()
        .map(|m| format!("{}\n", serde_json::json!({"type": "user", "message": {"role": "user", "content": m}})))
        .collect();
    write(&transcript, &lines);
    let events: Vec<Event> = [("/a", 1), ("/b", 2)]
        .iter()
        .map(|&(cwd, line)| Event {
            cwd: Some(cwd.into()),
            src: format!("{}:{line}", transcript.display()),
            ..event(cwd, "vetted", Channel::Tool, cwd, &ago(line as i64))
        })
        .collect();
    let mut w = report_world(repo.path());
    w.inv = mazo::store::inventory(Some(repo.path()), data.path()).unwrap();
    let input = w.input(&events);

    let v: Value =
        serde_json::from_str(&report::show(&input, "vetted", Since::Days(30), 20, true, false).unwrap()).unwrap();
    let cut = format!("{}…", &long[..99]);
    assert_eq!(
        v["manual"],
        serde_json::json!({
            "for": "Tidies prose.",
            "your_use": ["Tidy the release notes for the launch.", cut],
            "try": ["Tidy a README.", "Tidy a commit body.", "Tidy a PR."],
        })
    );
    assert_eq!(v["description"], "Not the For.");

    let text = report::show(&input, "vetted", Since::Days(30), 20, false, false).unwrap();
    let manual = [
        "for:      Tidies prose.\n".to_string(),
        "your use: - Tidy the release notes for the launch.\n".into(),
        "          - Tidy this paragraph. Tidy this paragraph. Tidy this paragraph. Tidy\n".into(),
        "            this paragraph. Tidy this parag…\n".into(),
        "try:      - Tidy a README.\n".into(),
        "          - Tidy a commit body.\n".into(),
        "          - Tidy a PR.\n".into(),
        "\nvetted: since ".into(),
    ];
    assert!(text.starts_with(&manual.concat()), "{text}");
}

/// A source skill with no vet record and no events: For is its description,
/// and the empty Your use and Try are left out.
#[test]
fn show_gives_an_unvetted_source_skill_its_description_alone() {
    let repo = report_repo();
    let w = report_world(repo.path());
    let input = w.input(&[]);
    let v: Value =
        serde_json::from_str(&report::show(&input, "unslop", Since::Days(30), 20, true, false).unwrap()).unwrap();
    assert_eq!(v["manual"], serde_json::json!({"for": "Removes slop from prose.", "your_use": [], "try": []}));
    let text = report::show(&input, "unslop", Since::Days(30), 20, false, false).unwrap();
    assert!(text.starts_with("for:      Removes slop from prose.\n\nunslop: since "), "{text}");
}

/// A description cannot drive the terminal: its controls print as U+FFFD,
/// in For, in the card and in `--json`.
#[test]
fn show_prints_a_descriptions_terminal_controls_inert() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("evil/SKILL.md"), "---\nname: evil\ndescription: \"Hi\\e]52;c;eA==\\a\\e[2J.\"\n---\n");
    let w = report_world(dir.path());
    let input = w.input(&[]);
    let text = report::show(&input, "evil", Since::Days(30), 20, false, false).unwrap();
    assert!(!text.contains(['\x1b', '\x07']), "{text:?}");
    assert!(text.starts_with("for:      Hi\u{fffd}]52;c;eA==\u{fffd}\u{fffd}[2J.\n"), "{text:?}");
    assert!(text.contains("\ndescription: Hi\u{fffd}]52;c;eA==\u{fffd}\u{fffd}[2J.\n"), "{text:?}");
    let v: Value =
        serde_json::from_str(&report::show(&input, "evil", Since::Days(30), 20, true, false).unwrap()).unwrap();
    assert_eq!(v["description"], "Hi\u{fffd}]52;c;eA==\u{fffd}\u{fffd}[2J.");
}

/// For and Try come from the latest skill audit when it is newer than the
/// vet or there is no vet, else from the vet; a source skill with no audit
/// keeps its description.
#[test]
fn the_manual_takes_for_and_try_from_the_newer_of_the_audit_and_the_vet() {
    let repo = report_repo();
    let data = tempfile::tempdir().unwrap();
    write(&data.path().join("store/vetted/SKILL.md"), "---\nname: vetted\ndescription: Not the For.\n---\n");
    let lock = |vet: &str| {
        let entry = "[[skill]]\nname = \"vetted\"\norigin = \"/x\"\nsubpath = \".\"\ncontent_hash = \"h\"\n\
                     installed_at = \"2026-01-01T00:00:00Z\"\n";
        write(&data.path().join("mazo.lock"), &format!("{entry}{vet}"));
    };
    let vetted = "trusted = false\nvetted_at = \"2026-05-01T00:00:00Z\"\nfor = \"Vet for.\"\ntry = [\"Vet try.\"]\n";
    let audit = |skill: &str, at: &str| -> mazo::audit::SkillRecord {
        let mut r: Value = serde_json::from_str(&common::skill_record("V.", at)).unwrap();
        r["skill"] = skill.into();
        r["result"]["for"] = "Audit for.".into();
        r["result"]["try"] = serde_json::json!(["Audit try 1.", "Audit try 2."]);
        serde_json::from_value(r).unwrap()
    };
    let manual = |skill: &str, audits: &[mazo::audit::SkillRecord]| {
        let mut w = report_world(repo.path());
        w.inv = mazo::store::inventory(Some(repo.path()), data.path()).unwrap();
        w.audits = audits.iter().map(|r| (r.skill.clone(), r.clone())).collect();
        let input = w.input(&[]);
        let v: Value =
            serde_json::from_str(&report::show(&input, skill, Since::Days(30), 20, true, false).unwrap()).unwrap();
        let text = report::show(&input, skill, Since::Days(30), 20, false, false).unwrap();
        assert!(text.starts_with(&format!("for:      {}\n", v["manual"]["for"].as_str().unwrap())), "{text}");
        (v["manual"]["for"].clone(), v["manual"]["try"].clone())
    };
    let from_audit = (Value::from("Audit for."), serde_json::json!(["Audit try 1.", "Audit try 2."]));
    let from_vet = (Value::from("Vet for."), serde_json::json!(["Vet try."]));

    lock(vetted);
    assert_eq!(manual("vetted", &[audit("vetted", "2026-06-01T00:00:00Z")]), from_audit, "audit newer than vet");
    assert_eq!(manual("vetted", &[audit("vetted", "2026-04-01T00:00:00Z")]), from_vet, "vet newer than audit");
    assert_eq!(manual("vetted", &[]), from_vet, "no audit");
    lock("trusted = true\n");
    assert_eq!(manual("vetted", &[audit("vetted", "2026-04-01T00:00:00Z")]), from_audit, "no vet");
    assert_eq!(manual("unslop", &[audit("unslop", "2026-04-01T00:00:00Z")]), from_audit, "a source skill's audit");
    let description = (Value::from("Removes slop from prose."), serde_json::json!([]));
    assert_eq!(manual("unslop", &[]), description, "a source skill with no audit");
}

/// Every part at its longest, words and one unbroken run: the manual stays
/// within 15 lines of 80 columns, cutting with `…`.
#[test]
fn a_full_manual_fits_fifteen_lines_at_eighty_columns() {
    let manual = report::Manual {
        r#for: Some("Explains what this skill is for at length. ".repeat(7)[..300].into()),
        your_use: vec!["x".repeat(100), "Use it on this draft, please. ".repeat(4)[..100].into(), "x".repeat(100)],
        r#try: vec!["Try it on a long design document. ".repeat(6)[..200].into(), "y".repeat(200), "y".repeat(200)],
    };
    let text = manual.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 15, "{text}");
    assert!(lines.iter().all(|l| l.chars().count() <= 80), "{text}");
    assert_eq!(lines.iter().filter(|l| l.ends_with('…')).count(), 4, "For and every Try item are cut: {text}");
    assert!(lines[3].starts_with("your use: - xxx") && lines[9].starts_with("try:      - Try"), "{text}");
}

// ---------------------------------------------------------------- evals

#[test]
fn evals_latest_date_and_staleness() {
    let dir = tempfile::tempdir().unwrap();
    let (src, evals) = (dir.path().join("skills"), dir.path().join("evals"));
    write(&src.join("s/SKILL.md"), "---\nname: s\ndescription: v1\n---\n");
    let hash = mazo::store::content_hash(&src.join("s")).unwrap();
    write(&evals.join("grades/GRADES.md"), "---\ntarget: s\ndate: 2027-01-01\nscore: 1\n---\n");
    write(&evals.join("grades/s/notes.md"), "# no frontmatter\n");
    write(&evals.join("grades/s/bad.md"), "---\ndate: 2027-01-01\ndate: 2027-01-02\n---\n");
    write(&evals.join("grades/s/a.md"), "---\ntarget: s\ndate: 2026-01-15\nscore: 7\n---\n");
    write(&evals.join("grades/s/b.md"), &format!("---\ndate: \"2026-02-01\"\nscore: 8.5\ncontent_hash: {hash}\n---\n"));
    write(&evals.join("retros/s/r0.md"), "---\nskill: s\ndate: 2026-03-05\n---\n");
    write(&evals.join("retros/s/r1.md"), "---\nskill: s\ndate: \"2026-03-05\"\n---\n");
    write(&evals.join("retros/s/r2.md"), "---\nskill: s\ndate: 2026-03-04\n---\n");
    write(&evals.join("retros/s/r3.md"), "---\nskill: s\ndate: 2026-03-06T09:00:00Z\n---\n");
    write(&evals.join("retros/s/r4.md"), "---\nskill: s\ndate: 20260307\n---\n");
    write(&evals.join("retros/s/r5.md"), "---\nskill: s\ndate: 2026-02-30\n---\n");

    let load = |skill| evals::load(&evals, &Inventory::load(&src).unwrap(), skill);
    let e = load("s");
    assert_eq!(e.content_hash.as_deref(), Some(hash.as_str()));
    let g = e.grades.last().unwrap();
    assert!(
        g.eval.file.ends_with("b.md"),
        "quoted 2026-02-01 beats unquoted 2026-01-15; GRADES.md, notes.md, bad.md skipped"
    );
    assert_eq!(g.eval.score.as_deref(), Some("8.5"));
    assert!(!g.stale && g.mark().is_empty(), "the grade's content_hash is the skill's");
    assert!(e.grades[0].stale && e.grades[0].content_hash.is_none(), "a grade with no content_hash is stale");
    let retros: Vec<&str> = e.retros.iter().map(|r| r.file.file_name().unwrap().to_str().unwrap()).collect();
    assert_eq!(
        retros,
        ["r2.md", "r0.md", "r1.md", "r3.md"],
        "by date, ties by filename; r4.md and r5.md have no YYYY-MM-DD"
    );
    assert_eq!(e.retros[3].date, "2026-03-06", "a timestamp is read as a string, its first 10 characters the date");
    let skipped: Vec<&str> = e.skipped.iter().map(|s| s.rsplit_once('/').unwrap().1).collect();
    assert!(skipped[0].starts_with("bad.md: "), "{skipped:?}");
    let mut retro_skips = skipped[1..].to_vec();
    retro_skips.sort();
    // r5.md has the right shape, but no such day.
    assert_eq!(retro_skips, ["r4.md: no YYYY-MM-DD date", "r5.md: no YYYY-MM-DD date"]);

    write(&src.join("s/SKILL.md"), "---\nname: s\ndescription: v2\n---\n");
    let g = load("s").grades.pop().unwrap();
    assert!(g.stale && g.mark() == "stale", "a changed skill makes its grade stale");

    write(&evals.join("grades/gone/g.md"), "---\ndate: 2026-01-01\nscore: 9\n---\n");
    assert_eq!(load("gone"), evals::Evals::default(), "a name in no root");
}

/// `show --json`'s origin (D24): a store skill's lockfile entry when its
/// origin is on github.com, a source skill's `origin` remote on github.com
/// at HEAD, else null.
#[test]
fn show_gives_the_github_origin_of_store_and_source_skills() {
    let repo = report_repo();
    let data = tempfile::tempdir().unwrap();
    let entries = [
        ("pinned", "git@github.com:o/r.git", "skills/pinned", "ref = \"v1\"\ncommit = \"abc\"\n"),
        ("tip", "https://github.com/o/r", ".", "commit = \"def\"\n"),
        ("local", "/x", ".", ""),
        ("elsewhere", "https://gitlab.com/o/r.git", ".", "commit = \"abc\"\n"),
    ];
    let mut lock = String::new();
    for (name, origin, subpath, rest) in entries {
        write(&data.path().join(format!("store/{name}/SKILL.md")), &format!("---\nname: {name}\n---\n"));
        lock.push_str(&format!(
            "[[skill]]\nname = \"{name}\"\norigin = \"{origin}\"\nsubpath = \"{subpath}\"\n{rest}\
             content_hash = \"h\"\ntrusted = true\ninstalled_at = \"2026-01-01T00:00:00Z\"\n"
        ));
    }
    write(&data.path().join("mazo.lock"), &lock);
    let origin = |skill: &str| {
        let mut w = report_world(repo.path());
        w.inv = mazo::store::inventory(Some(repo.path()), data.path()).unwrap();
        let input = w.input(&[]);
        let v: Value =
            serde_json::from_str(&report::show(&input, skill, Since::Days(30), 20, true, false).unwrap()).unwrap();
        v["origin"].clone()
    };
    let store = |subpath: &str, r#ref: Value, commit: &str| serde_json::json!({"repo": "https://github.com/o/r.git", "subpath": subpath, "ref": r#ref, "commit": commit});
    assert_eq!(origin("pinned"), store("skills/pinned", "v1".into(), "abc"));
    assert_eq!(origin("tip"), store(".", Value::Null, "def"));
    assert_eq!(origin("local"), Value::Null, "a local install");
    assert_eq!(origin("elsewhere"), Value::Null, "a lock origin off github.com");

    assert_eq!(origin("unslop"), Value::Null, "no origin remote");
    let head = inventory::git(repo.path(), &["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let source =
        serde_json::json!({"repo": "https://github.com/o/s.git", "subpath": "unslop", "ref": null, "commit": head});
    git(repo.path(), &["remote", "add", "origin", "https://github.com/o/s"], NOW);
    assert_eq!(origin("unslop"), source, "an https remote");
    git(repo.path(), &["remote", "set-url", "origin", "git@github.com:o/s.git"], NOW);
    assert_eq!(origin("unslop"), source, "a git@github.com: remote");
    git(repo.path(), &["remote", "set-url", "origin", "git@gitlab.com:o/s.git"], NOW);
    assert_eq!(origin("unslop"), Value::Null, "a remote off github.com");
}

/// The usage guide's `mazo usage` sample is what the binary prints for its
/// scenario: three skills linked into one Claude home, two of them loaded.
#[test]
fn the_usage_guide_sample_is_current() {
    let source = tempfile::tempdir().unwrap();
    for skill in ["release-notes", "triage", "tidy-imports"] {
        write(&source.path().join(format!("{skill}/SKILL.md")), &format!("---\nname: {skill}\ndescription: D.\n---\n"));
    }
    let w = common::World::new(source.path(), &[("claude", Agent::Claude, "2026-09-14T08:00:00Z")]);
    let events = vec![
        event("a", "release-notes", Channel::Slash, "A", &ago(1)),
        event("b", "release-notes", Channel::Slash, "B", &ago(5)),
        event("c", "release-notes", Channel::Slash, "C", &ago(12)),
        event("d", "triage", Channel::Slash, "D", &ago(4)),
    ];
    let printed = format!("$ mazo usage\n{}", report::usage(&w.input(&events), &args(), false, false));
    let guide = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/usage-tracking.md")).unwrap();
    let sample = guide.split("```console\n").nth(1).and_then(|s| s.split("```").next()).unwrap();
    assert_eq!(sample, printed);
}
