//! Fan-in, held and evals, plus the report rules (filters,
//! collapse, sess, authoring) on small fixture ledgers.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use common::{CLAUDE_FIRST, CODEX_FIRST, NOW, ago, copy_tree, event, git, report_repo, report_world, write};
use serde_json::Value;
use skima::inventory::{self, Inventory};
use skima::ledger::{Actor, Agent, Channel, Event, Origin};
use skima::report::{self, Since, UsageArgs};
use skima::{evals, render};

const LEGEND: &str = "legend: agents C Claude, X Codex, P Pi; · no events that day; \
                      +N commits since the grade; +wip uncommitted changes since the grade";

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
    let repo = report_repo();
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
    assert!(text.ends_with(&format!("{window}\n{LEGEND}\n")), "{text}");
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

// ---------------------------------------------------------------- named by

/// unslop names goal-plan in prose (`x-goal-plan/` is not a word); every
/// other whole-word name in the tree is already an edge, so none repeats.
#[test]
fn mentions_on_fixtures_are_whole_words_that_are_not_edges() {
    let repo = report_repo();
    let skills = inventory::canonical_skills(repo.path()).unwrap();
    let edges = inventory::fan_in(repo.path(), &skills);
    let m = inventory::mentions(repo.path(), &skills, &edges);
    let pairs: Vec<String> = m.iter().flat_map(|(a, bs)| bs.iter().map(move |b| format!("{a}->{b}"))).collect();
    assert_eq!(pairs, ["unslop->goal-plan"]);
    assert_eq!(m.keys().cloned().collect::<BTreeSet<_>>(), skills);
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
    write(&repo.path().join(".grades/unslop/g.md"), "---\ntarget: unslop\ndate: 2026-09-01\nscore: 9.1\n---\n");
    git(repo.path(), &["add", "router", ".grades"], NOW);
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
    assert!(unslop.ends_with(&format!("9.1{yellow}+wip{reset}")), "{usage}");
    assert_eq!(usage.lines().skip(1).map(|l| l.matches('\x1b').count()).sum::<usize>(), 2, "only the mark: {usage}");

    let off = render::colour(true, Some(std::ffi::OsStr::new("1")));
    assert!(!report::unused(&input, Since::Days(60), false, off).contains('\x1b'));
    assert!(!report::usage(&input, &args(), false, off).contains('\x1b'));
}

// ---------------------------------------------------------------- usage

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
    assert!(lines[0].starts_with("skill") && lines[0].ends_with(&format!("30d{}grade", " ".repeat(28))), "{text}");
    assert!(lines[1].starts_with("unslop "), "highest sess first: {text}");
    assert!(lines[1].contains(" CX     1d ago "), "{text}");
    assert!(lines[1].ends_with(&format!("{}▃█·     -", "·".repeat(27))), "a day with no event is a dot: {text}");
    let execute = lines.iter().find(|l| l.starts_with("goal-execute ")).unwrap();
    assert!(execute.ends_with(&format!(" 0        -      {}     -", "·".repeat(30))), "{text}");
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

/// The text and JSON grade of `skill` in a fresh report, which reads the history again.
fn grade_of(source: &Path, skill: &str) -> (String, Value) {
    let w = report_world(source);
    let input = w.input(&[]);
    let text = report::usage(&input, &args(), false, false);
    let line = text.lines().find(|l| l.starts_with(&format!("{skill} "))).unwrap();
    let cell = line.rsplit(' ').next().unwrap().to_string();
    (cell, row(&report::usage(&input, &args(), true, false), skill)["grade"].clone())
}

#[test]
fn usage_grade_shows_the_score_then_commits_or_wip_since() {
    let repo = report_repo();
    let src = repo.path();
    write(&src.join(".grades/unslop/g.md"), "---\ntarget: unslop\ndate: 2026-09-01\nscore: 9.1\n---\n");
    git(src, &["add", "."], NOW);
    git(src, &["commit", "-q", "-m", "grade"], "2026-09-01T00:00:00Z");
    let edit = |d: &str| write(&src.join("unslop/SKILL.md"), &format!("---\nname: unslop\ndescription: {d}.\n---\n"));
    let summary =
        |g: &Value| (g["stale"].as_bool().unwrap(), g["since"].as_u64().unwrap(), g["wip"].as_bool().unwrap());

    assert_eq!(grade_of(src, "goal-plan").0, "-", "no grade");
    let (cell, g) = grade_of(src, "unslop");
    assert_eq!((cell.as_str(), summary(&g)), ("9.1", (false, 0, false)));

    edit("One");
    let (cell, g) = grade_of(src, "unslop");
    assert_eq!((cell.as_str(), summary(&g)), ("9.1+wip", (true, 0, true)), "only uncommitted changes");

    git(src, &["commit", "-q", "-am", "one"], "2026-09-02T00:00:00Z");
    assert_eq!(grade_of(src, "unslop").0, "9.1+1");

    edit("Two");
    git(src, &["commit", "-q", "-am", "two"], "2026-09-03T00:00:00Z");
    edit("Three");
    let (cell, g) = grade_of(src, "unslop");
    assert_eq!((cell.as_str(), summary(&g)), ("9.1+2", (true, 2, true)), "commits win over uncommitted changes");
}

// ---------------------------------------------------------------- show

#[test]
fn show_lists_events_with_expired_locators_and_retro_counts() {
    let repo = report_repo();
    let transcript = repo.path().join("t.jsonl");
    write(&transcript, "{}\n");
    write(&repo.path().join(".retros/unslop/r.md"), "---\nskill: unslop\ndate: \"2026-10-05\"\nscore: 7\n---\n");
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

/// The card's sections after the events, in display order, with `$HOME` as
/// `~` and short locators in text and full paths in `--json`.
#[test]
fn show_prints_the_card() {
    let repo = report_repo();
    let src = repo.path();
    for (date, score) in [("2026-02-01", "6"), ("2026-03-01", "7")] {
        write(&src.join(format!(".grades/unslop/{date}.md")), &format!("---\ndate: {date}\nscore: {score}\n---\n"));
        git(src, &["add", "."], NOW);
        git(src, &["commit", "-q", "-m", &format!("grade {date}")], &format!("{date}T00:00:00Z"));
    }
    write(&src.join("unslop/notes.md"), "edit\n");
    git(src, &["add", "."], NOW);
    git(src, &["commit", "-q", "-m", "tighten unslop"], "2026-04-01T00:00:00Z");
    write(&src.join(".grades/unslop/2026-05-01.md"), "---\ndate: 2026-05-01\nscore: 9\n---\n");
    write(
        &src.join(".retros/unslop/a.md"),
        "---\nskill: unslop\ndate: 2026-04-02\nscore: 5\nverdict: too eager\n---\n",
    );
    write(&src.join(".retros/unslop/b.md"), "---\nskill: unslop\ndate: 2026-10-05\nscore: 8\nverdict: holds up\n---\n");
    write(&src.join("unslop/notes.md"), "edit, uncommitted\n");
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
        "date       score since\n".into(),
        "2026-05-01     9 +wip\n".into(),
        "2026-03-01     7 +1\n".into(),
        "2026-02-01     6 +1\n".into(),
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
        "since",
        "histogram",
        "events",
        "total_events",
        "description",
        "path",
        "history",
        "grades",
        "retros",
        "strong_since_retro",
        "cites",
        "cited_by",
        "usage",
        "examples",
        "audit",
    ];
    assert_eq!(keys, BTreeSet::from(all));
    assert_eq!(v["path"], src.join("unslop").display().to_string());
    assert_eq!(v["events"][0]["src"], format!("{home}/t/a.jsonl:7"));
    assert_eq!(v["events"][0]["cwd"], format!("{home}/work"));
    assert_eq!(v["history"]["last"]["subject"], "tighten unslop");
    assert_eq!(v["history"]["uncommitted"], true);
    let grades: Vec<(&str, u64)> = v["grades"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| (g["date"].as_str().unwrap(), g["since"].as_u64().unwrap()))
        .collect();
    assert_eq!(grades, [("2026-05-01", 0), ("2026-03-01", 1), ("2026-02-01", 1)], "every grade, newest first");
    assert_eq!(v["retros"][1]["verdict"], "too eager");
    assert_eq!(v["cited_by"], serde_json::json!([{"skill": "citer", "held": false}]));
    assert_eq!(v["usage"]["last_use"], ago(1));
    assert_eq!(v["usage"]["projects"][0], serde_json::json!({"cwd": format!("{home}/work"), "sessions": 2}));
    assert_eq!(v["examples"]["real"], serde_json::json!([]), "every transcript expired");
    assert_eq!(v["audit"], Value::Null);
}

/// Not a repository: no history section, no `+N`.
#[test]
fn show_outside_a_git_worktree_omits_history() {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&common::fixture("reports/skills"), dir.path(), &|s| s);
    write(&dir.path().join(".grades/unslop/g.md"), "---\ndate: 2026-05-01\nscore: 9\n---\n");
    let w = report_world(dir.path());
    assert!(w.inv.history.is_none());
    let input = w.input(&[]);
    let text = report::show(&input, "unslop", Since::Days(30), 20, false, false).unwrap();
    assert!(!text.contains("history:"), "{text}");
    assert!(text.contains("grades:\ndate       score since\n2026-05-01     9\n"), "{text}");
    let v: Value =
        serde_json::from_str(&report::show(&input, "unslop", Since::Days(30), 20, true, false).unwrap()).unwrap();
    assert_eq!(v["history"], Value::Null);
    assert_eq!(v["grades"][0]["since"], 0);
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
    write(&src.join(".retros/s/r3.md"), "---\nskill: s\ndate: 2026-03-06T09:00:00Z\n---\n");
    write(&src.join(".retros/s/r4.md"), "---\nskill: s\ndate: 20260307\n---\n");
    git(src, &["add", "."], NOW);
    git(src, &["commit", "-q", "-m", "skill and grades"], "2026-02-01T00:00:00Z");

    let load = || evals::load(&Inventory::load(src).unwrap(), "s");
    let e = load();
    let g = e.grades.last().unwrap();
    assert!(
        g.eval.file.ends_with("b.md"),
        "quoted 2026-02-01 beats unquoted 2026-01-15; GRADES.md, notes.md, bad.md skipped"
    );
    assert_eq!(g.eval.score.as_deref(), Some("8.5"));
    assert!(!g.stale(), "a grade committed together with the skill edit is not stale");
    let retros: Vec<&str> = e.retros.iter().map(|r| r.file.file_name().unwrap().to_str().unwrap()).collect();
    assert_eq!(retros, ["r2.md", "r0.md", "r1.md", "r3.md"], "by date, ties by filename; r4.md has no YYYY-MM-DD");
    assert_eq!(e.retros[3].date, "2026-03-06", "a timestamp is read as a string, its first 10 characters the date");

    write(&src.join("s/SKILL.md"), "---\nname: s\ndescription: v2\n---\n");
    git(src, &["commit", "-q", "-am", "edit skill"], "2026-03-01T00:00:00Z");
    assert!(load().grades.pop().unwrap().stale(), "a later skill commit makes it stale");

    write(&src.join(".grades/s/c.md"), "---\ntarget: s\ndate: 2026-04-01\nscore: 9\n---\n");
    let g = load().grades.pop().unwrap();
    assert!(g.eval.file.ends_with("c.md") && !g.stale(), "an uncommitted grade is not stale");

    write(&src.join("s/SKILL.md"), "---\nname: s\ndescription: v3\n---\n");
    assert!(load().grades.pop().unwrap().stale(), "uncommitted skill changes make any grade stale");
}
