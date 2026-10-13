//! skima's own permanent sync and status tests (SPEC §10.5), beyond the port
//! of test_install.py.

#[path = "fixtures/sync/harness.rs"]
mod harness;

use std::fs;

use harness::*;

/// The Pi home in its configured place, `~/.pi/agent`.
fn fixture() -> Fixture {
    let mut fx = Fixture::new();
    fx.set_env("PI_CODING_AGENT_DIR", None);
    fx.agent = fx.home.join(".pi/agent");
    fx.target = fx.agent.join("skills");
    fx
}

#[test]
fn conflict_in_the_last_home_leaves_zero_writes_in_every_home() {
    let fx = fixture();
    let last = fx.skills.last().unwrap();
    fs::create_dir_all(&fx.target).unwrap();
    fs::write(fx.target.join(last), "preserve").unwrap();
    let before = fx.snapshot();
    let r = fx.skima(&["sync"]);
    assert_eq!(r.code(), 1, "{}", r.stdout());
    assert!(
        r.stderr().contains(&format!("{} conflicts with a canonical entry", fx.target.join(last).display())),
        "{}",
        r.stderr()
    );
    assert_eq!(r.stdout(), "");
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn claude_work_is_linked_and_policy_checked() {
    let fx = fixture();
    let work = fx.home.join(".claude-work");
    fx.skima(&["sync", "--agent", "claude"]).assert_ok();
    for name in &fx.skills {
        assert_eq!(fs::read_link(work.join("skills").join(name)).unwrap(), fx.source.join(name));
    }

    let first = &fx.skills[0];
    let mut missing: serde_json::Value = serde_json::from_str(&fx.valid_settings()).unwrap();
    missing["skillOverrides"].as_object_mut().unwrap().remove(first);
    fs::write(work.join("settings.json"), missing.to_string()).unwrap();
    fs::remove_file(work.join("skills").join(first)).unwrap();
    let before = fx.snapshot();
    let r = fx.skima(&["sync", "--agent", "claude"]);
    assert_eq!(r.code(), 1);
    let needle = format!(
        "ERROR: {}: skillOverrides must set every skill to name-only:\n  \"{first}\": \"name-only\"",
        work.join("settings.json").display()
    );
    assert!(r.stderr().contains(&needle), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);

    fs::remove_file(work.join("settings.json")).unwrap();
    let r = fx.skima(&["sync"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains(&format!("ERROR: {}: no Claude settings profile", work.display())), "{}", r.stderr());
    assert_eq!(fx.skima(&["status", "--agent", "claude"]).code(), 1, "status runs the same check");
}

#[test]
fn sync_agent_pi_runs_per_skill_rules_and_skips_claude_settings() {
    let fx = fixture();
    fs::remove_dir_all(fx.home.join(".claude")).unwrap();
    fs::write(fx.settings(".claude-work"), r#"{"skillOverrides": []}"#).unwrap();
    let skill = fx.source.join(&fx.skills[0]);
    let manifest = skill.join("agents/openai.yaml");
    let policy = fs::read(&manifest).unwrap();
    fs::write(&manifest, "policy:\n  allow_implicit_invocation: yes\n").unwrap();

    let before = fx.snapshot();
    let r = fx.skima(&["sync", "--agent", "pi"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains(&format!("ERROR: {}:", skill.display())), "{}", r.stderr());
    assert!(r.stderr().contains("agents/openai.yaml"), "{}", r.stderr());
    assert!(!r.stderr().contains("settings"), "Claude settings are not checked: {}", r.stderr());
    assert_eq!(fx.snapshot(), before);

    fs::write(&manifest, policy).unwrap();
    fx.skima(&["sync", "--agent", "pi"]).assert_ok();
    assert_eq!(names(&fx.target), fx.skills);
    assert!(!fx.home.join(".claude").exists());
    assert!(!fx.home.join(".claude-work/skills").exists());
    assert!(!fx.home.join(".codex").exists());
}

fn write_coverage(fx: &Fixture, ages: &[(&str, i64)]) {
    let now = jiff::Timestamp::now();
    let map: serde_json::Map<String, serde_json::Value> = ages
        .iter()
        .map(|(home, days)| {
            let t = now.checked_sub(jiff::SignedDuration::from_hours(24 * days)).unwrap();
            (home.to_string(), serde_json::json!({ "first": null, "last_scan": t.to_string() }))
        })
        .collect();
    fs::create_dir_all(fx.data.join("skima")).unwrap();
    fs::write(fx.data.join("skima/coverage.json"), serde_json::Value::Object(map).to_string()).unwrap();
}

#[test]
fn status_warns_when_a_claude_home_nears_retention() {
    let fx = fixture();
    fx.skima(&["sync"]).assert_ok();

    write_coverage(&fx, &[("claude", 24), ("claude-work", 22)]);
    let r = fx.skima(&["status"]);
    r.assert_ok();
    assert_eq!(r.stdout(), "");
    assert!(r.stderr().contains("warning: claude: last scanned"), "{}", r.stderr());
    assert!(r.stderr().contains("after 30 days"), "{}", r.stderr());
    assert!(!r.stderr().contains("warning: claude-work"), "22 days is fresh: {}", r.stderr());

    write_coverage(&fx, &[("claude", 22)]);
    let r = fx.skima(&["status"]);
    r.assert_ok();
    assert!(!r.stderr().contains("warning: claude:"), "{}", r.stderr());
    assert!(r.stderr().contains("warning: claude-work: last scanned never"), "never scanned warns: {}", r.stderr());

    write_coverage(&fx, &[("claude", 22), ("claude-work", 1)]);
    let r = fx.skima(&["status"]);
    r.assert_ok();
    assert_eq!(r.stderr(), "", "clean and fresh");
}

#[test]
fn status_writes_nothing() {
    let fx = fixture();
    fs::create_dir_all(fx.home.join(".codex/skills")).unwrap();
    std::os::unix::fs::symlink(fx.source.join("gone"), fx.home.join(".codex/skills/gone")).unwrap();
    let before = fx.snapshot();
    let r = fx.skima(&["status"]);
    assert_eq!(r.code(), 1, "missing links are drift");
    assert!(r.stdout().contains(&format!("codex: missing {}", fx.skills[0])), "{}", r.stdout());
    assert!(r.stdout().contains("codex: dangling gone"), "{}", r.stdout());
    assert_eq!(fx.snapshot(), before);

    fs::remove_file(fx.home.join(".codex/skills/gone")).unwrap();
    fx.skima(&["sync"]).assert_ok();
    let before = fx.snapshot();
    fx.skima(&["status"]).assert_ok();
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn a_second_sync_prints_nothing() {
    let fx = fixture();
    let first = fx.skima(&["sync"]);
    first.assert_ok();
    assert!(first.stdout().contains("linked    "), "{}", first.stdout());
    let second = fx.skima(&["sync"]);
    second.assert_ok();
    assert_eq!(second.stdout(), "");
    assert_eq!(second.stderr(), "");
}

#[test]
fn dry_run_on_a_clean_tree_prints_nothing() {
    let fx = fixture();
    let before = fx.snapshot();
    let dirty = fx.skima(&["sync", "--dry-run"]);
    dirty.assert_ok();
    assert!(dirty.stdout().contains("would link"), "{}", dirty.stdout());
    assert_eq!(fx.snapshot(), before, "a dry run writes nothing");

    fx.skima(&["sync"]).assert_ok();
    let clean = fx.skima(&["sync", "--dry-run"]);
    clean.assert_ok();
    assert_eq!(clean.stdout(), "");
    assert_eq!(clean.stderr(), "");
}

// Regression tests for the code-review findings.

#[test]
fn retention_of_seven_days_or_less_always_warns() {
    let fx = fixture();
    fx.skima(&["sync"]).assert_ok();
    let mut settings: serde_json::Value = serde_json::from_str(&fx.valid_settings()).unwrap();
    settings["cleanupPeriodDays"] = 5.into();
    fs::write(fx.settings(".claude"), settings.to_string()).unwrap();
    write_coverage(&fx, &[("claude", 0), ("claude-work", 0)]);
    let r = fx.skima(&["status"]);
    r.assert_ok();
    assert!(r.stderr().contains("warning: claude: last scanned"), "{}", r.stderr());
    assert!(!r.stderr().contains("warning: claude-work"), "{}", r.stderr());
}

#[test]
fn two_homes_sharing_a_directory_sync_once() {
    let mut fx = fixture();
    let codex = fx.home.join(".codex");
    fx.set_env("PI_CODING_AGENT_DIR", Some(&codex));
    fx.skima(&["sync"]).assert_ok();
    assert_eq!(fx.skima(&["sync"]).assert_ok().stdout(), "");
}

#[test]
fn a_description_of_only_python_whitespace_is_rejected() {
    let fx = fixture();
    let skill = fx.source.join(&fx.skills[0]).join("SKILL.md");
    let name = &fx.skills[0];
    fs::write(&skill, format!("---\nname: {name}\ndescription: \"\\x1f\"\n---\n\n# Test\n")).unwrap();
    let r = fx.skima(&["sync", "--agent", "pi"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("nonempty description"), "{}", r.stderr());
}

#[test]
fn hidden_dangling_links_are_not_pruned() {
    let fx = fixture();
    fx.skima(&["sync"]).assert_ok();
    let hidden = fx.home.join(".claude/skills/.gone");
    std::os::unix::fs::symlink(fx.source.join(".gone"), &hidden).unwrap();
    fx.skima(&["sync"]).assert_ok();
    assert!(hidden.is_symlink(), "install.sh's `*` glob never pruned hidden names");
}

#[test]
fn show_of_an_unknown_skill_is_a_usage_error() {
    let fx = fixture();
    let r = fx.skima(&["show", "no-such-skill"]);
    assert_eq!(r.code(), 2);
    assert!(r.stderr().contains("unknown skill: no-such-skill"), "{}", r.stderr());
    assert!(!fx.data.join("skima/events.jsonl").exists(), "no scan before the usage error");
}
