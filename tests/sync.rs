//! The sync and status tests beyond the linking and policy cases of
//! `tests/sync_port.rs`.

mod common;

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;

use common::sync::*;

/// The Pi home in its configured place, `~/.pi/agent`.
fn fixture() -> Fixture {
    let mut fx = Fixture::new();
    fx.set_env("PI_CODING_AGENT_DIR", None);
    fx.agent = fx.home.join(".pi/agent");
    fx.target = fx.agent.join("skills");
    fx
}

#[test]
fn claude_work_is_linked_and_policy_checked() {
    let fx = fixture();
    let work = fx.home.join(".claude-work");
    fx.mazo(&["sync", "--agent", "claude"]).assert_ok();
    for name in &fx.skills {
        assert_eq!(fs::read_link(work.join("skills").join(name)).unwrap(), fx.source.join(name));
    }

    let first = &fx.skills[0];
    let mut missing: serde_json::Value = serde_json::from_str(&fx.valid_settings()).unwrap();
    missing["skillOverrides"].as_object_mut().unwrap().remove(first);
    fs::write(work.join("settings.json"), missing.to_string()).unwrap();
    fs::remove_file(work.join("skills").join(first)).unwrap();
    let before = fx.snapshot();
    let r = fx.mazo(&["sync", "--agent", "claude"]);
    assert_eq!(r.code(), 1);
    let needle = format!(
        "ERROR: {}: skillOverrides must set every skill to name-only:\n  \"{first}\": \"name-only\"",
        work.join("settings.json").display()
    );
    assert!(r.stderr().contains(&needle), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);
    let status: serde_json::Value = serde_json::from_str(&fx.mazo(&["status", "--json"]).stdout()).unwrap();
    let policy = status["policy"].as_array().unwrap();
    assert!(policy.iter().any(|p| p.as_str().unwrap().starts_with(&needle[7..])), "{status}");

    fs::remove_file(work.join("settings.json")).unwrap();
    let r = fx.mazo(&["sync"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains(&format!("ERROR: {}: no Claude settings profile", work.display())), "{}", r.stderr());
    assert_eq!(fx.mazo(&["status", "--agent", "claude"]).code(), 1, "status runs the same check");
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
    let r = fx.mazo(&["sync", "--agent", "pi"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains(&format!("ERROR: {}:", skill.display())), "{}", r.stderr());
    assert!(r.stderr().contains("agents/openai.yaml"), "{}", r.stderr());
    assert!(!r.stderr().contains("settings"), "Claude settings are not checked: {}", r.stderr());
    assert_eq!(fx.snapshot(), before);

    fs::write(&manifest, policy).unwrap();
    fx.mazo(&["sync", "--agent", "pi"]).assert_ok();
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
    common::write(&fx.data.join("mazo/coverage.json"), &serde_json::Value::Object(map).to_string());
}

#[test]
fn status_warns_when_a_claude_home_nears_retention() {
    let fx = fixture();
    fx.mazo(&["sync"]).assert_ok();

    write_coverage(&fx, &[("claude", 24), ("claude-work", 22)]);
    let r = fx.mazo(&["status"]);
    r.assert_ok();
    assert_eq!(r.stdout(), in_sync(&fx, &["claude", "claude-work", "codex", "pi"]));
    assert!(r.stderr().contains("warning: claude: last scanned"), "{}", r.stderr());
    assert!(r.stderr().contains("after 30 days"), "{}", r.stderr());
    assert!(!r.stderr().contains("warning: claude-work"), "22 days is fresh: {}", r.stderr());

    write_coverage(&fx, &[("claude", 22)]);
    let r = fx.mazo(&["status"]);
    r.assert_ok();
    assert!(!r.stderr().contains("warning: claude:"), "{}", r.stderr());
    assert!(r.stderr().contains("warning: claude-work: last scanned never"), "never scanned warns: {}", r.stderr());

    write_coverage(&fx, &[("claude", 22), ("claude-work", 1)]);
    let r = fx.mazo(&["status"]);
    r.assert_ok();
    assert_eq!(r.stderr(), "", "clean and fresh");
}

#[test]
fn status_writes_nothing() {
    let fx = fixture();
    fs::create_dir_all(fx.home.join(".codex/skills")).unwrap();
    std::os::unix::fs::symlink(fx.source.join("gone"), fx.home.join(".codex/skills/gone")).unwrap();
    let before = fx.snapshot();
    let r = fx.mazo(&["status"]);
    assert_eq!(r.code(), 1, "missing links are drift");
    assert!(r.stdout().contains(&format!("codex: {} is not linked (missing)", fx.skills[0])), "{}", r.stdout());
    assert!(r.stdout().contains("codex: gone link target is gone (dangling)"), "{}", r.stdout());
    assert_eq!(fx.snapshot(), before);

    fs::remove_file(fx.home.join(".codex/skills/gone")).unwrap();
    fx.mazo(&["sync"]).assert_ok();
    let before = fx.snapshot();
    fx.mazo(&["status"]).assert_ok();
    assert_eq!(fx.snapshot(), before);
}

/// `<home>: in sync (<n> skills)` for each of `homes`, in order.
fn in_sync(fx: &Fixture, homes: &[&str]) -> String {
    homes.iter().map(|h| format!("{h}: in sync ({} skills)\n", fx.skills.len())).collect()
}

#[test]
fn status_prints_one_plain_line_per_finding() {
    let fx = fixture();
    fx.mazo(&["sync"]).assert_ok();
    let codex = fx.home.join(".codex/skills");
    let [missing, wrong, obstructed] = [&fx.skills[0], &fx.skills[1], &fx.skills[2]];
    fs::remove_file(codex.join(missing)).unwrap();
    fs::remove_file(codex.join(wrong)).unwrap();
    std::os::unix::fs::symlink(&fx.home, codex.join(wrong)).unwrap();
    fs::remove_file(codex.join(obstructed)).unwrap();
    common::write(&codex.join(obstructed), "local");
    std::os::unix::fs::symlink(fx.source.join("gone"), codex.join("gone")).unwrap();
    fs::create_dir(fx.home.join(".claude/skills/synced")).unwrap();

    let r = fx.mazo(&["status"]);
    assert_eq!(r.code(), 1);
    let n = fx.skills.len();
    let expected = format!(
        "claude: in sync ({n} skills; 1 foreign left alone)\n\
         claude: synced is not from the source; left alone (foreign)\n{}\
         codex: {missing} is not linked (missing)\n\
         codex: {wrong} links somewhere else (wrong-link)\n\
         codex: {obstructed} is a real file, not a link (obstructed)\n\
         codex: gone link target is gone (dangling)\n{}",
        in_sync(&fx, &["claude-work"]),
        in_sync(&fx, &["pi"]),
    );
    assert_eq!(r.stdout(), expected);

    // A home whose only finding is foreign is in sync, and status exits 0.
    for name in [wrong.as_str(), obstructed, "gone"] {
        fs::remove_file(codex.join(name)).unwrap();
    }
    fx.mazo(&["sync"]).assert_ok();
    let r = fx.mazo(&["status"]);
    r.assert_ok();
    let expected = format!(
        "claude: in sync ({n} skills; 1 foreign left alone)\n\
         claude: synced is not from the source; left alone (foreign)\n{}",
        in_sync(&fx, &["claude-work", "codex", "pi"]),
    );
    assert_eq!(r.stdout(), expected);
}

#[test]
fn a_skill_rule_errors_on_stderr_while_homes_stay_in_sync() {
    let fx = fixture();
    fx.mazo(&["sync"]).assert_ok();
    let skill = fx.source.join(&fx.skills[0]);
    fs::write(skill.join("agents/openai.yaml"), "policy:\n  allow_implicit_invocation: true\n").unwrap();
    let r = fx.mazo(&["status"]);
    assert_eq!(r.code(), 1);
    assert_eq!(r.stdout(), in_sync(&fx, &["claude", "claude-work", "codex", "pi"]));
    assert!(r.stderr().starts_with(&format!("ERROR: {}: ", skill.display())), "{}", r.stderr());
    assert_eq!(fx.mazo(&["status", "--agent", "nope"]).code(), 2, "usage error");
}

#[test]
fn dry_run_on_a_clean_tree_prints_in_sync() {
    let fx = fixture();
    let before = fx.snapshot();
    let dirty = fx.mazo(&["sync", "--dry-run"]);
    dirty.assert_ok();
    assert!(dirty.stdout().contains("would link"), "{}", dirty.stdout());
    assert_eq!(fx.snapshot(), before, "a dry run writes nothing");

    fx.mazo(&["sync"]).assert_ok();
    let clean = fx.mazo(&["sync", "--dry-run"]);
    clean.assert_ok();
    assert_eq!(clean.stdout(), format!("in sync: 4 homes, {} skills\n", fx.skills.len()));
    assert_eq!(clean.stderr(), "");
    let pi = fx.mazo(&["sync", "--dry-run", "--agent", "pi"]);
    assert_eq!(pi.stdout(), format!("in sync: 1 home, {} skills\n", fx.skills.len()));
}

// Regression tests for the code-review findings.

#[test]
fn retention_of_seven_days_or_less_always_warns() {
    let fx = fixture();
    fx.mazo(&["sync"]).assert_ok();
    let mut settings: serde_json::Value = serde_json::from_str(&fx.valid_settings()).unwrap();
    settings["cleanupPeriodDays"] = 5.into();
    fs::write(fx.settings(".claude"), settings.to_string()).unwrap();
    write_coverage(&fx, &[("claude", 0), ("claude-work", 0)]);
    let r = fx.mazo(&["status"]);
    r.assert_ok();
    assert!(r.stderr().contains("warning: claude: last scanned"), "{}", r.stderr());
    assert!(!r.stderr().contains("warning: claude-work"), "{}", r.stderr());
}

#[test]
fn settings_local_json_wins_for_the_name_only_rule_and_retention_as_for_ctx() {
    let fx = fixture();
    fx.mazo(&["sync"]).assert_ok();
    let work = fx.home.join(".claude-work");
    let local = work.join("settings.local.json");
    fs::write(work.join("settings.json"), "{}").unwrap();
    let mut settings: serde_json::Value = serde_json::from_str(&fx.valid_settings()).unwrap();
    settings["cleanupPeriodDays"] = 60.into();
    fs::write(&local, settings.to_string()).unwrap();
    write_coverage(&fx, &[("claude", 0), ("claude-work", 40)]);
    let r = fx.mazo(&["status"]);
    r.assert_ok();
    assert_eq!(r.stderr(), "", "name-only and 60 days come from settings.local.json");
    let usage: serde_json::Value = serde_json::from_str(&fx.mazo(&["usage", "--all", "--json"]).stdout()).unwrap();
    let row = usage["rows"].as_array().unwrap().iter().find(|r| r["skill"] == fx.skills[0].as_str()).unwrap();
    assert_eq!(row["ctx"]["claude"], "name-only", "{row}");

    settings["skillOverrides"][&fx.skills[0]] = "on".into();
    fs::write(&local, settings.to_string()).unwrap();
    let r = fx.mazo(&["status"]);
    assert_eq!(r.code(), 1);
    let needle = format!(
        "ERROR: {}: skillOverrides must set every skill to name-only:\n  \"{}\": \"name-only\"\n",
        local.display(),
        fx.skills[0]
    );
    assert!(r.stderr().contains(&needle), "the file to edit is the one whose value wins: {}", r.stderr());
}

#[test]
fn a_description_of_only_python_whitespace_is_rejected() {
    let fx = fixture();
    let skill = fx.source.join(&fx.skills[0]).join("SKILL.md");
    let name = &fx.skills[0];
    fs::write(&skill, format!("---\nname: {name}\ndescription: \"\\x1f\"\n---\n\n# Test\n")).unwrap();
    let r = fx.mazo(&["sync", "--agent", "pi"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("nonempty description"), "{}", r.stderr());
}

#[test]
fn a_name_other_than_the_directory_is_rejected_by_name() {
    let fx = fixture();
    let name = &fx.skills[0];
    fs::write(fx.source.join(name).join("SKILL.md"), "---\nname: other\ndescription: D.\n---\n").unwrap();
    let r = fx.mazo(&["sync", "--agent", "pi"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains(&format!("SKILL.md must declare name: {name}")), "{}", r.stderr());
}

#[test]
fn hidden_dangling_links_are_not_pruned() {
    let fx = fixture();
    fx.mazo(&["sync"]).assert_ok();
    let hidden = fx.home.join(".claude/skills/.gone");
    std::os::unix::fs::symlink(fx.source.join(".gone"), &hidden).unwrap();
    fx.mazo(&["sync"]).assert_ok();
    assert!(hidden.is_symlink(), "sync never prunes hidden names");
}

#[test]
fn show_of_an_unknown_skill_is_a_usage_error() {
    let fx = fixture();
    let r = fx.mazo(&["show", "no-such-skill"]);
    assert_eq!(r.code(), 2);
    assert!(r.stderr().contains("unknown skill: no-such-skill"), "{}", r.stderr());
    assert!(!fx.data.join("mazo/events.jsonl").exists(), "no scan before the usage error");
}

#[test]
fn a_shared_directory_prunes_once() {
    let mut fx = fixture();
    let codex = fx.home.join(".codex");
    fx.set_env("PI_CODING_AGENT_DIR", Some(&codex));
    fx.mazo(&["sync"]).assert_ok();
    let gone = codex.join("skills/gone-skill");
    std::os::unix::fs::symlink(fx.source.join("gone-skill"), &gone).unwrap();
    let r = fx.mazo(&["sync"]);
    r.assert_ok();
    assert!(!gone.is_symlink());
    assert_eq!(r.stdout().lines().filter(|l| l.starts_with("pruned")).count(), 1, "{}", r.stdout());
}

#[test]
fn a_directory_shared_through_a_symlink_is_synced_once() {
    let mut fx = fixture();
    fs::create_dir_all(fx.home.join(".codex")).unwrap();
    let alias = fx.home.join("codex-alias");
    std::os::unix::fs::symlink(fx.home.join(".codex"), &alias).unwrap();
    fx.set_env("PI_CODING_AGENT_DIR", Some(&alias));
    let r = fx.mazo(&["sync"]);
    r.assert_ok();
    assert_eq!(fx.mazo(&["sync"]).assert_ok().stdout(), format!("in sync: 4 homes, {} skills\n", fx.skills.len()));
}

#[test]
fn a_links_entry_colliding_with_a_skill_aborts_before_writes() {
    let fx = fixture();
    let toml = fs::read_to_string(fx.source.join("mazo.toml")).unwrap();
    let name = &fx.skills[0];
    let other = &fx.skills[1];
    let toml = toml.replace("links = {", &format!("links = {{ \"skills/{name}\" = \"{other}\", "));
    fs::write(fx.source.join("mazo.toml"), toml).unwrap();
    let before = fx.snapshot();
    let r = fx.mazo(&["sync", "--agent", "pi"]);
    assert_eq!(r.code(), 1, "{}", r.stderr());
    assert!(r.stderr().contains("claimed by two sources"), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn sync_waits_for_the_data_lock_but_a_dry_run_does_not() {
    let fx = fixture();
    let held = mazo::ledger::lock(&fx.data.join("mazo")).unwrap();
    let dry = fx.mazo(&["sync", "--agent", "pi", "--dry-run"]);
    dry.assert_ok();
    assert!(dry.stdout().contains("would link"), "{}", dry.stdout());

    let mut sync = fx.command(&["sync", "--agent", "pi"]).stdout(std::process::Stdio::null()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(sync.try_wait().unwrap().is_none(), "sync ran while another process held the lock");
    assert!(!exists(&fx.target), "sync wrote before it had the lock");
    drop(held);
    assert!(sync.wait().unwrap().success());
    assert_eq!(names(&fx.target), fx.skills);
}

/// A skills dir that cannot be listed is an error, never a clean home.
#[test]
fn an_unlistable_skills_dir_fails_status() {
    let fx = fixture();
    fx.mazo(&["sync"]).assert_ok();
    let skills = fx.home.join(".claude/skills");
    fs::set_permissions(&skills, fs::Permissions::from_mode(0o311)).unwrap();
    let listable = fs::read_dir(&skills).is_ok(); // true when running as root
    let r = fx.mazo(&["status"]);
    fs::set_permissions(&skills, fs::Permissions::from_mode(0o755)).unwrap();
    if !listable {
        assert_eq!(r.code(), 1, "{}", r.stdout());
        assert!(r.stderr().contains(&format!("ERROR: {}: ", skills.display())), "{}", r.stderr());
        assert!(!r.stdout().starts_with("claude: in sync"), "{}", r.stdout());
    }
}

/// A foreign name that is not UTF-8 does not break `status --json`.
#[test]
fn status_json_reports_a_name_that_is_not_utf8() {
    let fx = fixture();
    fx.mazo(&["sync"]).assert_ok();
    fs::create_dir(fx.home.join(std::ffi::OsStr::from_bytes(b".claude/skills/bad\xff"))).unwrap();
    let r = fx.mazo(&["status", "--json"]);
    r.assert_ok();
    assert!(r.stdout().contains("bad\u{fffd}"), "{}", r.stdout());
}
