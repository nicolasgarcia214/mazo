//! The 18 cases of `dotfiles/skills/tests/test_install.py`, ported to drive
//! `mazo sync`. Line numbers cite test_install.py.

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use common::sync::*;

/// :65
#[test]
fn pi_discovers_all_skills_and_is_idempotent() {
    let mut fx = Fixture::new();
    fx.add_skill("new-skill");
    fx.add_skill(".hidden-skill");
    fs::create_dir(fx.source.join("tests")).unwrap();
    fs::write(fx.source.join("GOAL_SKILLS_USAGE.md"), "Shared usage\n").unwrap();
    let mut expected: Vec<String> = fx.skills.clone();
    expected.extend(["new-skill".into(), "GOAL_SKILLS_USAGE.md".into()]);
    expected.sort();
    fs::create_dir(&fx.agent).unwrap();
    let settings = fx.agent.join("settings.json");
    fs::write(&settings, "{\"theme\":\"existing\",\"enableSkillCommands\":false}\n").unwrap();
    let before = fs::read(&settings).unwrap();
    let homes_before = snapshot(&[&fx.home]);
    fx.mazo(&["sync", "--agent", "pi"]).assert_ok();
    assert_eq!(names(&fx.target), expected);
    for name in &expected {
        assert_eq!(fs::read_link(fx.target.join(name)).unwrap(), fx.source.join(name));
    }
    assert_eq!(fs::read_link(fx.agent.join("extensions").join(PI_EXTENSION)).unwrap(), fx.source.join(PI_EXTENSION));
    // :81-82: the Claude and Codex homes are unchanged (a snapshot).
    assert_eq!(snapshot(&[&fx.home]), homes_before);
    let inodes: Vec<u64> = expected.iter().map(|n| ino(&fx.target.join(n))).collect();
    let second = fx.mazo(&["sync", "--agent", "pi"]);
    second.assert_ok();
    assert_eq!(second.stdout(), format!("in sync: 1 homes, {} skills\n", fx.skills.len() + 1));
    assert_eq!(inodes, expected.iter().map(|n| ino(&fx.target.join(n))).collect::<Vec<_>>());
    assert_eq!(fs::read(&settings).unwrap(), before);
}

/// :90
#[test]
fn frontmatter_must_omit_model_invocation_key() {
    let fx = Fixture::new();
    for name in ["goal-smart", "unslop"] {
        let path = fx.source.join(name).join("SKILL.md");
        let original = fs::read_to_string(&path).unwrap();
        for value in ["true", "false", "\"true\"", "yes", "null"] {
            fs::write(&path, original.replace("---\n\n#", &format!("disable-model-invocation: {value}\n---\n\n#")))
                .unwrap();
            fx.assert_rejected_before_writes();
        }
        fs::write(&path, original.replace("---\n\n#", "\n#")).unwrap();
        fx.assert_rejected_before_writes();
        fs::write(&path, original).unwrap();
    }
}

/// :102
#[test]
fn claude_profiles_must_list_explicit_skills_as_name_only() {
    let fx = Fixture::new();
    let assert_rejected = |needle: &str| {
        let before = fx.snapshot();
        let r = fx.mazo(&["sync"]);
        assert_ne!(r.code(), 0, "{}", r.stdout());
        assert!(r.stderr().contains(needle), "needle {needle:?} not in:\n{}", r.stderr());
        // :107 (a snapshot instead of an empty $HOME).
        assert_eq!(fx.snapshot(), before);
        assert!(!exists(&fx.agent));
    };
    let valid: serde_json::Value = serde_json::from_str(&fx.valid_settings()).unwrap();
    let mut missing = valid.clone();
    missing["skillOverrides"].as_object_mut().unwrap().remove("goal-smart");
    let mut wrong = valid.clone();
    wrong["skillOverrides"]["goal-smart"] = "user-invocable-only".into();
    let main = fx.settings(".claude");
    let work = fx.settings(".claude-work");
    for (path, content, needle) in [
        (&main, missing.to_string(), "\"goal-smart\": \"name-only\""),
        (&main, wrong.to_string(), "goal-smart"),
        (&main, r#"{"skillOverrides": []}"#.to_string(), "skillOverrides must be an object"),
        (&main, "{".to_string(), "settings.json"),
        // :118 `settings.work.json` is `.claude-work/settings.json`.
        (&work, missing.to_string(), ".claude-work/settings.json"),
    ] {
        fx.write_settings(&fx.home);
        fs::write(path, content).unwrap();
        assert_rejected(needle);
    }
    fx.write_settings(&fx.home);
    fs::remove_file(&main).unwrap();
    assert_rejected("no Claude settings profile");
    fx.mazo(&["sync", "--agent", "pi"]).assert_ok();
}

/// :130
#[test]
fn installing_through_a_directory_alias_still_links_canonical_source() {
    let mut fx = Fixture::new();
    let alias = fx.root.join("repo-alias");
    symlink(&fx.source, &alias).unwrap();
    fx.set_env("MAZO_CONFIG", Some(&alias.join("mazo.toml")));
    let elsewhere = fx.root.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fx.mazo_in(&elsewhere, &["sync", "--agent", "pi"]).assert_ok();
    for name in &fx.skills {
        assert_eq!(fs::read_link(fx.target.join(name)).unwrap(), fx.source.join(name));
    }
}

/// :141
#[test]
fn pi_policy_requires_codex_false_and_unique_keys() {
    let fx = Fixture::new();
    let path = fx.source.join("goal-smart/agents/openai.yaml");
    for policy in [
        "policy:\n  allow_implicit_invocation: true\n",
        "policy:\n  allow_implicit_invocation: \"false\"\n",
        "policy:\n  allow_implicit_invocation: no\n",
        "policy:\n  allow_implicit_invocation: False\n",
        "policy:\n  allow_implicit_invocation: !!bool false\n",
        "policy:\n  allow_implicit_invocation: off\n",
        "policy:\n  allow_implicit_invocation: 0\n",
        "policy:\n  allow_implicit_invocation: false\n  allow_implicit_invocation: true\n",
        "policy:\n  allow_implicit_invocation: false\npolicy:\n  allow_implicit_invocation: true\n",
        "policy: {}\n",
        "[]\n",
    ] {
        fs::write(&path, policy).unwrap();
        fx.assert_rejected_before_writes();
    }
    fs::remove_file(&path).unwrap();
    fx.assert_rejected_before_writes();
}

/// :159
#[test]
fn invalid_skill_metadata_fails_before_writes() {
    let fx = Fixture::new();
    let path = fx.source.join("goal-smart/SKILL.md");
    let original = fs::read_to_string(&path).unwrap();
    for content in [
        original.replace("name: goal-smart", "name: different-name"),
        original.replace("description: Test skill.", "description: ''"),
        original.replace("description: Test skill.", "description: 1"),
        original.replace("description: Test skill.", "description: true"),
        original.replace("description: Test skill.", "description: [Test skill.]"),
        original.replace("description: Test skill.", "description: Test skill.\nmetadata:\n  k: 1\n  'k': 2"),
    ] {
        fs::write(&path, content).unwrap();
        fx.assert_rejected_before_writes();
    }
}

/// :171
#[test]
fn conflicts_preserve_foreign_entries_and_prevent_partial_install() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.target).unwrap();
    let foreign = fx.root.join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep.txt"), "preserve me").unwrap();
    let conflict = fx.target.join("goal-smart");
    for kind in ["directory", "file", "absolute", "relative", "dangling", "relative-dangling", "relative-canonical"] {
        match kind {
            "directory" => {
                fs::create_dir(&conflict).unwrap();
                fs::write(conflict.join("keep.txt"), "preserve me").unwrap();
            }
            "file" => fs::write(&conflict, "preserve me").unwrap(),
            _ => {
                let destination = match kind {
                    "absolute" => s(&foreign),
                    "relative" => relpath(&foreign, &fx.target),
                    "dangling" => s(&fx.root.join("missing")),
                    "relative-dangling" => "../../missing".into(),
                    "relative-canonical" => relpath(&fx.source.join("goal-smart"), &fx.target),
                    _ => unreachable!(),
                };
                symlink(destination, &conflict).unwrap();
            }
        }
        let inode = ino(&conflict);
        let r = fx.mazo(&["sync", "--agent", "pi"]);
        assert_ne!(r.code(), 0, "{kind}");
        assert!(r.stderr().contains("preserved"), "{kind}: {}", r.stderr());
        assert_eq!(names(&fx.target), vec!["goal-smart".to_string()], "{kind}");
        assert_eq!(ino(&conflict), inode, "{kind}");
        assert_eq!(fs::read_to_string(foreign.join("keep.txt")).unwrap(), "preserve me");
        let meta = fs::symlink_metadata(&conflict).unwrap();
        if meta.is_dir() {
            assert_eq!(fs::read_to_string(conflict.join("keep.txt")).unwrap(), "preserve me");
            fs::remove_dir_all(&conflict).unwrap();
        } else {
            fs::remove_file(&conflict).unwrap();
        }
    }
}

/// :206
#[test]
fn prunes_only_managed_dangling_links() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.target).unwrap();
    symlink(fx.source.join("removed-goal"), fx.target.join("removed-goal")).unwrap();
    fs::create_dir(fx.agent.join("extensions")).unwrap();
    symlink(fx.source.join("removed.ts"), fx.agent.join("extensions/removed.ts")).unwrap();
    let foreign = [
        ("foreign-absolute", s(&fx.root.join("missing"))),
        ("foreign-relative", "../../missing".to_string()),
        ("foreign-prefix", format!("{}/../missing", s(&fx.source))),
        ("foreign-alias", s(&fx.source.join("different-missing-name"))),
    ];
    for (name, destination) in &foreign {
        symlink(destination, fx.target.join(name)).unwrap();
    }
    fs::create_dir(fx.target.join("user-skill")).unwrap();
    fx.mazo(&["sync", "--agent", "pi"]).assert_ok();
    assert!(!exists(&fx.target.join("removed-goal")));
    assert!(!exists(&fx.agent.join("extensions/removed.ts")));
    for (name, destination) in &foreign {
        assert_eq!(fs::read_link(fx.target.join(name)).unwrap(), PathBuf::from(destination));
    }
    assert!(fx.target.join("user-skill").is_dir());
}

/// :228
#[test]
fn missing_pi_extension_fails_before_writes() {
    let fx = Fixture::new();
    fs::remove_file(fx.source.join(PI_EXTENSION)).unwrap();
    fx.assert_rejected_before_writes();
}

/// :232
#[test]
fn target_obstruction_is_preserved() {
    let fx = Fixture::new();
    fs::write(&fx.agent, "keep").unwrap();
    let r = fx.mazo(&["sync", "--agent", "pi"]);
    assert_ne!(r.code(), 0);
    assert!(r.stderr().contains("not a directory"), "{}", r.stderr());
    assert_eq!(fs::read_to_string(&fx.agent).unwrap(), "keep");
}

/// :239
#[test]
fn every_mode_preflights_new_skill_policy_before_any_write() {
    let mut fx = Fixture::new();
    let extra = fx.add_skill("ordinary-skill");
    fs::remove_file(extra.join("agents/openai.yaml")).unwrap();
    for args in [&["sync"][..], &["sync", "--agent", "pi"]] {
        let before = fx.snapshot();
        let r = fx.mazo(args);
        assert_ne!(r.code(), 0);
        assert!(r.stderr().contains("agents/openai.yaml"), "{args:?}: {}", r.stderr());
        assert!(!exists(&fx.agent));
        // :247 (a snapshot instead of an empty $HOME).
        assert_eq!(fx.snapshot(), before, "{args:?}");
    }
}

/// :249
#[test]
fn all_harnesses_share_the_same_files_and_preserve_foreign_entries() {
    let fx = Fixture::new();
    let targets = [fx.home.join(".claude/skills"), fx.home.join(".codex/skills"), fx.target.clone()];
    for t in &targets {
        fs::create_dir_all(t.join("foreign-skill")).unwrap();
    }
    fx.mazo(&["sync"]).assert_ok();
    for name in &fx.skills {
        let source = fs::canonicalize(fx.source.join(name).join("SKILL.md")).unwrap();
        for t in &targets {
            assert!(fs::symlink_metadata(t.join(name)).unwrap().file_type().is_symlink());
            assert_eq!(fs::canonicalize(t.join(name).join("SKILL.md")).unwrap(), source);
            assert!(t.join("foreign-skill").is_dir());
        }
    }
    let inodes = |fx: &Fixture| -> Vec<Vec<u64>> {
        targets.iter().map(|t| fx.skills.iter().map(|n| ino(&t.join(n))).collect()).collect()
    };
    let before = inodes(&fx);
    let second = fx.mazo(&["sync"]);
    second.assert_ok();
    assert_eq!(second.stdout(), format!("in sync: 4 homes, {} skills\n", fx.skills.len()));
    assert_eq!(inodes(&fx), before);
}

/// :268
#[test]
fn conflict_in_any_harness_aborts_all_destinations() {
    let mut fx = Fixture::new();
    for (relative, name) in [
        (".claude/skills", "writing-for-agents"),
        (".codex/skills", "writing-for-agents"),
        ("pi/skills", "writing-for-agents"),
        ("pi/extensions", PI_EXTENSION),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(dir.path()).unwrap();
        // Valid settings in both Claude homes, so the run fails for the conflict.
        fx.write_settings(&home);
        let target = home.join(relative);
        fs::create_dir_all(&target).unwrap();
        let conflict = target.join(name);
        fs::write(&conflict, "preserve").unwrap();
        fx.set_env("HOME", Some(&home));
        fx.set_env("PI_CODING_AGENT_DIR", Some(&home.join("pi")));
        let before = snapshot(&[&home]);
        let r = fx.mazo(&["sync"]);
        assert_ne!(r.code(), 0, "{relative}");
        assert!(r.stderr().contains("conflicts with a canonical entry"), "{relative}: {}", r.stderr());
        assert_eq!(fs::read_to_string(&conflict).unwrap(), "preserve");
        // :283-284 (a snapshot instead of "the conflict is the only file, no links").
        assert_eq!(snapshot(&[&home]), before, "{relative}");
    }
}

/// :286
#[test]
fn invalid_cli_arguments_make_no_directories() {
    let fx = Fixture::new();
    for args in [&["sync", "--unknown"][..], &["sync", "--agent", "pi", "extra"]] {
        let r = fx.mazo(args);
        assert_ne!(r.code(), 0, "{args:?}");
        assert_eq!(r.code(), 2, "{args:?}: usage errors exit 2");
        assert!(r.stderr().contains("Usage:"), "{args:?}: {}", r.stderr());
        assert!(!exists(&fx.agent));
    }
}

fn assert_pi_synced(fx: &Fixture, value: &str) {
    let r = fx.mazo(&["sync", "--agent", "pi"]);
    assert_eq!(r.code(), 0, "{value:?}: {}", r.stderr());
    assert_eq!(names(&fx.target), fx.skills, "{value:?}");
}

/// :294
#[test]
fn agent_directory_expands_tilde_and_local_file_urls() {
    let mut fx = Fixture::new();
    let home_relative = relpath(&fx.agent, &fx.home);
    let agent_name = fx.agent.file_name().unwrap().to_str().unwrap().replace(' ', "%20");
    for value in [
        format!("~/{home_relative}"),
        format!("~//{home_relative}"),
        relpath(&fx.agent, &fx.root),
        as_uri(&fx.agent),
        as_uri(&fx.agent).replace("file:///", "file://LOCALHOST/") + "?ignored#ignored",
        format!("{}/unused/%2e%2e/{agent_name}", as_uri(fx.agent.parent().unwrap())),
    ] {
        fx.set_env("PI_CODING_AGENT_DIR", Some(Path::new(&value)));
        assert_pi_synced(&fx, &value);
        assert!(!exists(&fx.root.join("~")), "{value:?}");
    }
}

/// :309
#[test]
fn invalid_file_urls_are_rejected_before_writes() {
    let mut fx = Fixture::new();
    let uri = as_uri(&fx.agent);
    for value in [
        format!("file://remote.example{}", s(&fx.agent)),
        format!("{uri}%2Fchild"),
        format!("{uri}%"),
        format!("{uri}%FF"),
        format!("{uri}%00"),
        format!("file://localhost:80{}", s(&fx.agent)),
    ] {
        fx.set_env("PI_CODING_AGENT_DIR", Some(Path::new(&value)));
        let r = fx.mazo(&["sync", "--agent", "pi"]);
        assert_ne!(r.code(), 0, "{value:?}");
        assert!(r.stderr().contains("ERROR: invalid PI_CODING_AGENT_DIR"), "{value:?}: {}", r.stderr());
        assert!(!exists(&fx.agent), "{value:?}");
        assert!(!exists(&fx.root.join("file:")), "{value:?}");
    }
}

/// :324
#[test]
fn agent_directory_normalizes_before_following_symlinks() {
    let mut fx = Fixture::new();
    let nested = fx.root.join("other/nested");
    fs::create_dir_all(&nested).unwrap();
    let alias = fx.root.join("alias");
    symlink(&nested, &alias).unwrap();
    let agent_name = fx.agent.file_name().unwrap().to_str().unwrap().to_string();
    for value in [
        format!("{}/../{agent_name}", s(&alias)),
        format!("alias/../{agent_name}"),
        format!("{} ", as_uri(&fx.agent)),
        format!("{} \t\r\n", as_uri(&fx.agent)),
    ] {
        fx.set_env("PI_CODING_AGENT_DIR", Some(Path::new(&value)));
        assert_pi_synced(&fx, &value);
        assert!(!exists(&fx.root.join("other").join(&agent_name)), "{value:?}");
        assert!(!exists(Path::new(&format!("{} ", s(&fx.agent)))), "{value:?}");
    }
}

/// :341
#[test]
fn option_like_paths_and_url_empty_segments_use_exact_fresh_targets() {
    let mut fx = Fixture::new();
    let cases = [
        ("-agent".to_string(), fx.root.join("-agent")),
        (format!("{}/empty//../agent", as_uri(&fx.root)), fx.root.join("empty/agent")),
    ];
    for (value, expected) in cases {
        assert!(!exists(&expected), "{value:?}");
        fx.set_env("PI_CODING_AGENT_DIR", Some(Path::new(&value)));
        let r = fx.mazo(&["sync", "--agent", "pi"]);
        assert_eq!(r.code(), 0, "{value:?}: {}", r.stderr());
        assert_eq!(names(&expected.join("skills")), fx.skills, "{value:?}");
        for name in &fx.skills {
            assert_eq!(fs::read_link(expected.join("skills").join(name)).unwrap(), fx.source.join(name));
        }
        assert!(!exists(&fx.root.join("agent")), "{value:?}");
    }
}
