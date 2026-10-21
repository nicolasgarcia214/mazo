//! Lifecycle commands end to end: `new`, `retire`, `prune`.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use common::sync as harness;
use common::{Mazo, stderr, stdout};
use mazo::ledger::{Channel, Event};

const DATE: &str = "2026-01-01T00:00:00Z";
const LEDGER: &str = "{\"id\":\"x\"}\n";

/// A skills source one level below the repository root, so `git -C <source>`
/// resolution is exercised, with a private `HOME`, data directory and ledger.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new(git: bool) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join("repo/skills");
        common::write(&source.join("mazo.toml"), "source = \".\"\n");
        common::write(&root.join("data/mazo/events.jsonl"), LEDGER);
        if git {
            common::git(&root.join("repo"), &["init", "-q"], DATE);
        }
        Fixture { _temp: temp, root, source }
    }

    fn skill(&self, rel: &str) -> PathBuf {
        let dir = self.source.join(rel);
        common::write(&dir.join("SKILL.md"), "---\nname: x\ndescription: X.\n---\n\n# X\n");
        common::write(&dir.join("references/notes.md"), "notes\n");
        dir
    }

    fn git(&self, args: &[&str]) {
        common::git(&self.source, args, DATE);
    }

    fn commit(&self) {
        self.git(&["add", "-A", "."]);
        self.git(&["commit", "-q", "-m", "c"]);
    }

    /// Runs mazo with stdin not a terminal, and checks the ledger is untouched.
    fn prune(&self, args: &[&str]) -> Output {
        let run =
            Mazo::new(&self.root, &self.root.join("home"), &self.source.join("mazo.toml"), &self.root.join("data"));
        let out = run.command(&[], &[&["prune"], args].concat()).stdin(Stdio::null()).output().unwrap();
        assert_eq!(fs::read_to_string(self.root.join("data/mazo/events.jsonl")).unwrap(), LEDGER);
        out
    }
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn archived(fx: &Fixture) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(fx.source.join(".archive"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn assert_code(out: &Output, want: i32) {
    assert_eq!(code(out), want, "stdout: {}\nstderr: {}", stdout(out), stderr(out));
}

#[test]
fn without_a_tty_and_without_yes_prune_exits_2_and_deletes_nothing() {
    let fx = Fixture::new(false);
    fx.skill(".archive/a");
    fx.skill(".archive/b");
    for args in [&[][..], &["a"], &["a", "--force"]] {
        assert_code(&fx.prune(args), 2);
        assert_eq!(archived(&fx), ["a", "b"]);
    }
}

#[test]
fn prune_with_a_name_deletes_one_and_without_deletes_all() {
    let fx = Fixture::new(false);
    for n in ["a", "b", "c"] {
        fx.skill(&format!(".archive/{n}"));
    }
    fx.skill("live");
    assert_code(&fx.prune(&["b", "--yes"]), 0);
    assert_eq!(archived(&fx), ["a", "c"]);
    assert_code(&fx.prune(&["--yes"]), 0);
    assert!(archived(&fx).is_empty());
    assert!(fx.source.join("live/SKILL.md").is_file());
}

#[test]
fn an_unknown_or_nested_name_is_a_usage_error() {
    let fx = Fixture::new(false);
    fx.skill(".archive/a");
    fx.skill("live");
    for name in ["nope", "../live", "a/references", ".", ""] {
        assert_code(&fx.prune(&[name, "--yes"]), 2);
    }
    assert_eq!(archived(&fx), ["a"]);
    assert!(fx.source.join("live/SKILL.md").is_file());
}

#[test]
fn work_in_no_head_path_is_refused_and_force_deletes_it() {
    let fx = Fixture::new(true);
    let edited = fx.skill(".archive/edited");
    let added = fx.skill(".archive/added");
    fx.skill(".archive/clean");
    fx.commit();
    common::write(&edited.join("references/notes.md"), "changed\n");
    common::write(&added.join("new.md"), "new\n");

    for name in ["edited", "added"] {
        let out = fx.prune(&[name, "--yes"]);
        assert_code(&out, 1);
        assert!(stderr(&out).contains(&format!(".archive/{name}/")), "{}", stderr(&out));
    }
    // Pruning everything refuses as a whole: the clean entry survives too.
    assert_code(&fx.prune(&["--yes"]), 1);
    assert_eq!(archived(&fx), ["added", "clean", "edited"]);

    assert_code(&fx.prune(&["clean", "--yes"]), 0);
    assert_code(&fx.prune(&["edited", "--yes", "--force"]), 0);
    assert_eq!(archived(&fx), ["added"]);
    assert_code(&fx.prune(&["--yes", "--force"]), 0);
    assert!(archived(&fx).is_empty());
}

#[test]
fn a_skill_moved_by_git_mv_and_not_committed_is_deleted_without_force() {
    let fx = Fixture::new(true);
    fx.skill("old");
    fx.commit();
    fs::create_dir(fx.source.join(".archive")).unwrap();
    fx.git(&["mv", "old", ".archive/old"]);
    assert_code(&fx.prune(&["old", "--yes"]), 0);
    assert!(archived(&fx).is_empty());
}

#[test]
fn a_gitignore_covering_the_archive_does_not_hide_an_edit() {
    let fx = Fixture::new(true);
    common::write(&fx.source.join(".gitignore"), ".archive/\n");
    fx.skill("old");
    fx.commit();
    fs::create_dir(fx.source.join(".archive")).unwrap();
    fs::rename(fx.source.join("old"), fx.source.join(".archive/old")).unwrap();
    common::write(&fx.source.join(".archive/old/SKILL.md"), "edited\n");
    let out = fx.prune(&["old", "--yes"]);
    assert_code(&out, 1);
    assert!(stderr(&out).contains(".archive/old/SKILL.md"), "{}", stderr(&out));
    assert_eq!(archived(&fx), ["old"]);
}

/// An archived skill holding a committed valid link and a committed dangling link.
fn linked(fx: &Fixture) -> PathBuf {
    let dir = fx.skill(".archive/linked");
    std::os::unix::fs::symlink("../SKILL.md", dir.join("references/up")).unwrap();
    std::os::unix::fs::symlink("nowhere", dir.join("dangling")).unwrap();
    fx.commit();
    dir
}

#[test]
fn committed_valid_and_dangling_links_are_deleted_without_force() {
    let fx = Fixture::new(true);
    linked(&fx);
    assert_code(&fx.prune(&["linked", "--yes"]), 0);
    assert!(archived(&fx).is_empty());
}

#[test]
fn a_retargeted_link_is_refused_and_named() {
    let fx = Fixture::new(true);
    let dir = linked(&fx);
    fs::remove_file(dir.join("references/up")).unwrap();
    std::os::unix::fs::symlink("notes.md", dir.join("references/up")).unwrap();
    let out = fx.prune(&["linked", "--yes"]);
    assert_code(&out, 1);
    assert!(stderr(&out).contains(".archive/linked/references/up"), "{}", stderr(&out));
    assert_eq!(archived(&fx), ["linked"]);
}

#[test]
fn a_link_matching_a_committed_regular_file_is_refused() {
    let fx = Fixture::new(true);
    let dir = fx.skill(".archive/typed");
    common::write(&dir.join("references/up"), "../SKILL.md");
    fx.commit();
    fs::remove_file(dir.join("references/up")).unwrap();
    std::os::unix::fs::symlink("../SKILL.md", dir.join("references/up")).unwrap();
    let out = fx.prune(&["typed", "--yes"]);
    assert_code(&out, 1);
    assert!(stderr(&out).contains(".archive/typed/references/up"), "{}", stderr(&out));
    assert_eq!(archived(&fx), ["typed"]);
}

#[test]
fn an_archived_skill_in_a_repository_without_commits_is_refused() {
    let fx = Fixture::new(true);
    fx.skill(".archive/a");
    assert_code(&fx.prune(&["--yes"]), 1);
    assert_eq!(archived(&fx), ["a"]);
}

const ALL_RULES: &str =
    "[policy]\nno_disable_model_invocation = true\ncodex_manifest = true\nclaude_name_only = true\n";

/// A source with no skill yet, two Claude homes and a Codex home, each with an
/// empty settings file, under a private `HOME`.
struct Homes {
    _temp: tempfile::TempDir,
    home: PathBuf,
    source: PathBuf,
}

impl Homes {
    fn new(policy: &str) -> Homes {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let (home, source) = (root.join("home"), root.join("skills"));
        let homes = "[[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"~/.claude\"\n\n\
                     [[home]]\nname = \"work\"\nagent = \"claude\"\npath = \"~/.claude-work\"\n\n\
                     [[home]]\nname = \"codex\"\nagent = \"codex\"\npath = \"~/.codex\"\n";
        common::write(&source.join("mazo.toml"), &format!("source = \".\"\n{policy}\n{homes}"));
        for h in HOMES {
            common::write(&home.join(h).join("settings.json"), "{}\n");
        }
        Homes { _temp: temp, home, source }
    }

    fn mazo(&self, args: &[&str]) -> Output {
        Mazo::new(&self.home, &self.home, &self.source.join("mazo.toml"), &self.home.join("data")).output(args)
    }

    /// The files under `<source>/<name>`, relative to it.
    fn files(&self, name: &str) -> Vec<String> {
        let dir = self.source.join(name);
        let mut out = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    out.push(e.path().strip_prefix(&dir).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        out.sort();
        out
    }

    /// No home links the skill.
    fn unlinked(&self, name: &str) -> bool {
        HOMES.iter().all(|h| self.home.join(h).join("skills").join(name).symlink_metadata().is_err())
    }
}

const HOMES: [&str; 3] = [".claude", ".claude-work", ".codex"];

#[test]
fn new_with_every_rule_on_writes_a_skill_that_syncs_to_a_clean_status() {
    let hs = Homes::new(ALL_RULES);
    let out = hs.mazo(&["new", "fresh-skill", "--description", "Does a fresh thing."]);
    assert_code(&out, 0);
    assert_eq!(hs.files("fresh-skill"), ["SKILL.md", "agents/openai.yaml"]);
    let manifest = fs::read_to_string(hs.source.join("fresh-skill/agents/openai.yaml")).unwrap();
    assert!(manifest.contains("default_prompt: \"Use $fresh-skill."), "{manifest}");

    // The per-skill rules, every configurable one on, pass on what was written.
    let policy = mazo::config::Policy {
        no_disable_model_invocation: true,
        codex_manifest: true,
        claude_name_only: true,
        authoring_skills: vec![],
    };
    let cfg = mazo::config::Config { source: hs.source.clone(), loose: vec![], homes: vec![], policy, judge: None };
    let inv = mazo::inventory::Inventory::load(&hs.source).unwrap();
    assert_eq!(mazo::policy::per_skill(&cfg, &inv), Vec::<String>::new());

    // One name-only line per Claude home, none for Codex.
    let stdout = stdout(&out);
    let lines: Vec<(&str, &str)> = stdout.lines().filter_map(|l| l.split_once(": add to skillOverrides: ")).collect();
    let settings = |h: &str| hs.home.join(h).join("settings.json").to_string_lossy().into_owned();
    let paths: Vec<&str> = lines.iter().map(|(p, _)| *p).collect();
    assert_eq!(paths, [settings(".claude"), settings(".claude-work")], "{stdout}");
    assert!(hs.unlinked("fresh-skill"), "new ran sync");

    for (path, entry) in lines {
        assert_eq!(entry, "\"fresh-skill\": \"name-only\"");
        fs::write(path, format!("{{\"skillOverrides\": {{{entry}}}}}\n")).unwrap();
    }
    assert_code(&hs.mazo(&["sync"]), 0);
    assert_code(&hs.mazo(&["status"]), 0);
}

#[test]
fn new_with_every_rule_off_writes_only_skill_md() {
    let hs = Homes::new("");
    let out = hs.mazo(&["new", "plain", "--description", "A plain skill."]);
    assert_code(&out, 0);
    assert!(!stdout(&out).contains("name-only"));
    assert_eq!(hs.files("plain"), ["SKILL.md"]);
    // Not synced: status sees the missing links until sync runs.
    assert!(hs.unlinked("plain"), "new ran sync");
    assert_code(&hs.mazo(&["status"]), 1);
    assert_code(&hs.mazo(&["sync"]), 0);
    assert_code(&hs.mazo(&["status"]), 0);
}

#[test]
fn a_description_with_yaml_syntax_round_trips() {
    let hs = Homes::new("");
    let description = "Says \"hi\": a # b\\c, then\na tab\there, and \u{2028} é.";
    assert_code(&hs.mazo(&["new", "tricky", "--description", description]), 0);
    let text = fs::read_to_string(hs.source.join("tricky/SKILL.md")).unwrap();
    let meta = mazo::yaml::frontmatter(&text).unwrap();
    assert_eq!(mazo::yaml::get(&meta, "description").and_then(mazo::yaml::string), Some(description));
    assert!(text.ends_with("---\n\n# tricky\n"), "{text}");
    assert_code(&hs.mazo(&["sync"]), 0);
}

#[test]
fn new_rejects_a_missing_description_a_bad_name_and_an_existing_entry() {
    let hs = Homes::new(ALL_RULES);
    assert_code(&hs.mazo(&["new", "ok"]), 2);
    // Each rejection names the rule it broke.
    let rejected = |args: &[&str], why: &str| {
        let out = hs.mazo(args);
        assert_code(&out, 2);
        assert!(stderr(&out).contains(why), "{args:?}: {}", stderr(&out));
    };
    // clap takes `-a` for a flag.
    assert_code(&hs.mazo(&["new", "-a", "--description", "D."]), 2);
    for name in ["Bad", "a--b", "a_b", "../escape", "a/b", ""] {
        rejected(
            &["new", name, "--description", "D."],
            "must be lowercase letters and digits joined by single hyphens",
        );
    }
    rejected(&["new", &"a".repeat(65), "--description", "D."], "is over 64 characters");
    for description in ["", " \t "] {
        rejected(&["new", "ok", "--description", description], "must declare a nonempty description");
    }
    rejected(&["new", "ok", "--description", &"d".repeat(1025)], "description is over 1024 characters");
    let entries: Vec<_> = fs::read_dir(&hs.source).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(entries, ["mazo.toml"]);
    assert_code(&hs.mazo(&["new", "ok", "--description", &"d".repeat(1024)]), 0);

    // An existing skill, a plain file and a dangling link are each refused and left as they were.
    assert_code(&hs.mazo(&["new", "ok", "--description", "Other."]), 1);
    assert!(fs::read_to_string(hs.source.join("ok/SKILL.md")).unwrap().contains(&"d".repeat(1024)));
    fs::write(hs.source.join("file"), "x").unwrap();
    std::os::unix::fs::symlink("nowhere", hs.source.join("dangling")).unwrap();
    for name in ["file", "dangling"] {
        let out = hs.mazo(&["new", name, "--description", "D."]);
        assert_code(&out, 1);
        assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    }
    assert_eq!(fs::read_to_string(hs.source.join("file")).unwrap(), "x");
}

/// The sync harness (every rule on, four homes) with the Pi home under
/// `~/.pi/agent`, `diagnose-bug` citing `blast-radius`, synced.
fn synced() -> harness::Fixture {
    let mut fx = harness::Fixture::new();
    fx.set_env("PI_CODING_AGENT_DIR", None);
    fx.agent = fx.home.join(".pi/agent");
    fx.target = fx.agent.join("skills");
    common::write(
        &fx.source.join("diagnose-bug/SKILL.md"),
        "---\nname: diagnose-bug\ndescription: Test skill.\n---\n\nRun the blast-radius skill.\n",
    );
    fx.mazo(&["sync"]).assert_ok();
    fx
}

/// A ledger with one strong event per `(skill, days ago)`.
fn ledger(fx: &harness::Fixture, uses: &[(&str, i64)]) {
    let now = jiff::Timestamp::now();
    let lines: String = uses
        .iter()
        .map(|(skill, days)| {
            let ts = now.checked_sub(jiff::SignedDuration::from_hours(24 * days)).unwrap();
            let e = Event {
                cwd: None,
                src: "/nonexistent/t.jsonl:1".into(),
                ..common::event(
                    &format!("{skill}-{days}"),
                    skill,
                    Channel::Slash,
                    &format!("s-{skill}"),
                    &ts.to_string(),
                )
            };
            serde_json::to_string(&e).unwrap() + "\n"
        })
        .collect();
    common::write(&fx.data.join("mazo/events.jsonl"), &lines);
}

/// Every link under `$HOME` (all four homes) to `<source>/<name>`.
fn links_to(fx: &harness::Fixture, name: &str) -> Vec<PathBuf> {
    let want = format!("link {}", fx.source.join(name).display());
    harness::snapshot(&[&fx.home]).into_iter().filter(|(_, v)| *v == want).map(|(p, _)| p).collect()
}

/// `$HOME` (every home) and the source.
fn homes_and_source(fx: &harness::Fixture) -> harness::Snapshot {
    harness::snapshot(&[&fx.home, &fx.source])
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", stderr(&out));
    stdout(&out)
}

fn assert_retired(fx: &harness::Fixture, name: &str) {
    assert!(fx.source.join(".archive").join(name).join("SKILL.md").is_file());
    assert!(!fx.source.join(name).exists());
    assert!(links_to(fx, name).is_empty());
}

#[test]
fn retire_refuses_a_recent_or_held_skill_and_force_overrides_both() {
    let fx = synced();
    ledger(&fx, &[("grade", 3), ("diagnose-bug", 3), ("interrogate", 40)]);
    let before = homes_and_source(&fx);
    for (name, why) in [
        ("grade", "grade has a strong event in the last 30 days"),
        ("blast-radius", "blast-radius is held by diagnose-bug"),
    ] {
        let r = fx.mazo(&["retire", name]);
        assert_eq!(r.code(), 1, "{}", r.stdout());
        assert!(r.stderr().contains(why), "{}", r.stderr());
        assert!(r.stderr().contains("ERROR: retire refused"), "{}", r.stderr());
    }
    assert_eq!(homes_and_source(&fx), before);

    // Used only outside the window, and cited by nothing used.
    fx.mazo(&["retire", "interrogate"]).assert_ok();
    fx.mazo(&["retire", "grade", "--force"]).assert_ok();
    fx.mazo(&["retire", "blast-radius", "--force"]).assert_ok();
    for name in ["interrogate", "grade", "blast-radius"] {
        assert_retired(&fx, name);
    }
    fx.mazo(&["status"]).assert_ok();
}

#[test]
fn a_holder_used_only_before_the_30_day_window_does_not_hold() {
    let fx = synced();
    ledger(&fx, &[("diagnose-bug", 45)]);
    fx.mazo(&["retire", "blast-radius"]).assert_ok();
    assert_retired(&fx, "blast-radius");
}

#[test]
fn retire_refuses_a_skill_named_by_a_used_skill_and_force_overrides_it() {
    let fx = synced();
    common::write(
        &fx.source.join("triage-finding/SKILL.md"),
        "---\nname: triage-finding\ndescription: Test skill.\n---\n\n| Step | Skill |\n|---|---|\n| Ask | /interrogate |\n",
    );
    ledger(&fx, &[("triage-finding", 3)]);
    let before = homes_and_source(&fx);
    let r = fx.mazo(&["retire", "interrogate"]);
    assert_eq!(r.code(), 1, "{}", r.stdout());
    assert!(r.stderr().contains("interrogate is named by triage-finding"), "{}", r.stderr());
    assert!(r.stderr().contains("ERROR: retire refused"), "{}", r.stderr());
    assert_eq!(homes_and_source(&fx), before);

    fx.mazo(&["retire", "interrogate", "--force"]).assert_ok();
    assert_retired(&fx, "interrogate");
}

#[test]
fn a_conflict_or_policy_error_after_archiving_aborts_with_zero_writes() {
    let fx = synced();
    let obstructed = fx.target.join("grade");
    fs::remove_file(&obstructed).unwrap();
    common::write(&obstructed, "local");
    let before = homes_and_source(&fx);
    let r = fx.mazo(&["retire", "unslop"]);
    assert_eq!(r.code(), 1, "{}", r.stdout());
    assert!(r.stderr().contains("conflicts with a canonical entry"), "{}", r.stderr());
    assert_eq!(homes_and_source(&fx), before);
    fs::remove_file(&obstructed).unwrap();

    // The settings miss `grade`: a policy error in the post-archive plan.
    let mut settings: serde_json::Value = serde_json::from_str(&fx.valid_settings()).unwrap();
    settings["skillOverrides"].as_object_mut().unwrap().remove("grade");
    fs::write(fx.settings(".claude"), settings.to_string()).unwrap();
    let before = homes_and_source(&fx);
    let r = fx.mazo(&["retire", "unslop"]);
    assert_eq!(r.code(), 1, "{}", r.stdout());
    assert!(r.stderr().contains("skillOverrides must set every skill to name-only"), "{}", r.stderr());
    assert_eq!(homes_and_source(&fx), before);
    fx.write_settings(&fx.home);

    // A `links` entry into the skill is missing once it is archived.
    let toml = fx.source.join("mazo.toml");
    let text = fs::read_to_string(&toml).unwrap().replace("= \"pi-explicit-skills.ts\" }", "= \"unslop/SKILL.md\" }");
    fs::write(&toml, text).unwrap();
    fs::remove_file(fx.agent.join("extensions/pi-explicit-skills.ts")).unwrap();
    fx.mazo(&["sync"]).assert_ok();
    let before = homes_and_source(&fx);
    let r = fx.mazo(&["retire", "unslop"]);
    assert_eq!(r.code(), 1, "{}", r.stdout());
    assert!(
        r.stderr().contains(&format!("{} is missing", fx.source.join("unslop/SKILL.md").display())),
        "{}",
        r.stderr()
    );
    assert_eq!(homes_and_source(&fx), before);
}

#[test]
fn retire_stages_a_rename_prunes_the_links_and_never_commits() {
    let fx = synced();
    let repo = fx.source.parent().unwrap();
    common::git(repo, &["init", "-q"], DATE);
    common::git(repo, &["add", "-A", "."], DATE);
    common::git(repo, &["commit", "-q", "-m", "c"], DATE);
    assert!(!links_to(&fx, "unslop").is_empty());

    let r = fx.mazo(&["retire", "unslop"]);
    r.assert_ok();
    assert_retired(&fx, "unslop");
    let status = git_out(repo, &["status", "--porcelain"]);
    let renames: String = ["SKILL.md", "agents/openai.yaml"]
        .iter()
        .map(|f| format!("R  skills/unslop/{f} -> skills/.archive/unslop/{f}\n"))
        .collect();
    assert_eq!(status, renames);
    assert_eq!(git_out(repo, &["rev-list", "--count", "HEAD"]), "1\n");
    for dir in [".claude", ".claude-work"] {
        let lines = format!("{} can drop:\n  \"unslop\": \"name-only\"\n", fx.settings(dir).display());
        assert!(r.stdout().contains(&lines), "{}", r.stdout());
    }
    fx.mazo(&["status"]).assert_ok();
}

#[test]
fn an_untracked_skill_is_renamed_and_settings_lines_need_claude_name_only() {
    let fx = synced();
    let toml = fx.source.join("mazo.toml");
    let text =
        fs::read_to_string(&toml).unwrap().replace("claude_name_only            = true", "claude_name_only = false");
    fs::write(&toml, text).unwrap();
    let r = fx.mazo(&["retire", "unslop"]);
    r.assert_ok();
    assert_retired(&fx, "unslop");
    assert!(!r.stdout().contains("can drop"), "{}", r.stdout());
    fx.mazo(&["status"]).assert_ok();
}

#[test]
fn retire_of_a_non_canonical_name_is_a_usage_error() {
    let fx = synced();
    let before = homes_and_source(&fx);
    for name in ["nope", ".archive", "../skills"] {
        assert_eq!(fx.mazo(&["retire", name]).code(), 2);
    }
    assert_eq!(homes_and_source(&fx), before);
}
