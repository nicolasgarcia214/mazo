//! Lifecycle commands end to end (SPEC §10.8): `new`, `prune`.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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
        common::write(&source.join("skima.toml"), "source = \".\"\n");
        common::write(&root.join("data/skima/events.jsonl"), LEDGER);
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

    /// Runs skima with stdin not a terminal, and checks the ledger is untouched.
    fn prune(&self, args: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_skima"))
            .arg("prune")
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .env("HOME", self.root.join("home"))
            .env("SKIMA_CONFIG", self.source.join("skima.toml"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .output()
            .unwrap();
        assert_eq!(fs::read_to_string(self.root.join("data/skima/events.jsonl")).unwrap(), LEDGER);
        out
    }
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
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
    assert_eq!(code(out), want, "stdout: {}\nstderr: {}", String::from_utf8_lossy(&out.stdout), stderr(out));
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
        common::write(&source.join("skima.toml"), &format!("source = \".\"\n{policy}\n{homes}"));
        for h in HOMES {
            common::write(&home.join(h).join("settings.json"), "{}\n");
        }
        Homes { _temp: temp, home, source }
    }

    fn skima(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_skima"))
            .args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("SKIMA_CONFIG", self.source.join("skima.toml"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .output()
            .unwrap()
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
    let out = hs.skima(&["new", "fresh-skill", "--description", "Does a fresh thing."]);
    assert_code(&out, 0);
    assert_eq!(hs.files("fresh-skill"), ["SKILL.md", "agents/openai.yaml"]);
    let manifest = fs::read_to_string(hs.source.join("fresh-skill/agents/openai.yaml")).unwrap();
    assert!(manifest.contains("default_prompt: \"Use $fresh-skill."), "{manifest}");

    // The per-skill rules, every configurable one on, pass on what was written.
    let policy = skima::config::Policy {
        no_disable_model_invocation: true,
        codex_manifest: true,
        claude_name_only: true,
        authoring_skills: vec![],
    };
    let cfg = skima::config::Config { source: hs.source.clone(), loose: vec![], homes: vec![], policy };
    let inv = skima::inventory::Inventory::load(&hs.source).unwrap();
    assert_eq!(skima::policy::per_skill(&cfg, &inv), Vec::<String>::new());

    // One name-only line per Claude home, none for Codex.
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<(&str, &str)> = stdout.lines().filter_map(|l| l.split_once(": add to skillOverrides: ")).collect();
    let settings = |h: &str| hs.home.join(h).join("settings.json").to_string_lossy().into_owned();
    let paths: Vec<&str> = lines.iter().map(|(p, _)| *p).collect();
    assert_eq!(paths, [settings(".claude"), settings(".claude-work")], "{stdout}");
    assert!(hs.unlinked("fresh-skill"), "new ran sync");

    for (path, entry) in lines {
        assert_eq!(entry, "\"fresh-skill\": \"name-only\"");
        fs::write(path, format!("{{\"skillOverrides\": {{{entry}}}}}\n")).unwrap();
    }
    assert_code(&hs.skima(&["sync"]), 0);
    assert_code(&hs.skima(&["status"]), 0);
}

#[test]
fn new_with_every_rule_off_writes_only_skill_md() {
    let hs = Homes::new("");
    let out = hs.skima(&["new", "plain", "--description", "A plain skill."]);
    assert_code(&out, 0);
    assert!(!String::from_utf8_lossy(&out.stdout).contains("name-only"));
    assert_eq!(hs.files("plain"), ["SKILL.md"]);
    // Not synced: status sees the missing links until sync runs.
    assert!(hs.unlinked("plain"), "new ran sync");
    assert_code(&hs.skima(&["status"]), 1);
    assert_code(&hs.skima(&["sync"]), 0);
    assert_code(&hs.skima(&["status"]), 0);
}

#[test]
fn a_description_with_yaml_syntax_round_trips() {
    let hs = Homes::new("");
    let description = "Says \"hi\": a # b\\c, then\na tab\there, and \u{2028} é.";
    assert_code(&hs.skima(&["new", "tricky", "--description", description]), 0);
    let text = fs::read_to_string(hs.source.join("tricky/SKILL.md")).unwrap();
    let meta = skima::yaml::frontmatter(&text).unwrap();
    assert_eq!(skima::yaml::get(&meta, "description").and_then(skima::yaml::string), Some(description));
    assert!(text.ends_with("---\n\n# tricky\n"), "{text}");
    assert_code(&hs.skima(&["sync"]), 0);
}

#[test]
fn new_rejects_a_missing_description_a_bad_name_and_an_existing_entry() {
    let hs = Homes::new(ALL_RULES);
    assert_code(&hs.skima(&["new", "ok"]), 2);
    let long = "a".repeat(65);
    for name in ["Bad", "a--b", "-a", "a_b", "../escape", "a/b", "", long.as_str()] {
        assert_code(&hs.skima(&["new", name, "--description", "D."]), 2);
    }
    let too_long = "d".repeat(1025);
    for description in ["", " \t ", too_long.as_str()] {
        assert_code(&hs.skima(&["new", "ok", "--description", description]), 2);
    }
    let entries: Vec<_> = fs::read_dir(&hs.source).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(entries, ["skima.toml"]);
    assert_code(&hs.skima(&["new", "ok", "--description", &"d".repeat(1024)]), 0);

    // An existing skill, a plain file and a dangling link are each refused and left as they were.
    assert_code(&hs.skima(&["new", "ok", "--description", "Other."]), 1);
    assert!(fs::read_to_string(hs.source.join("ok/SKILL.md")).unwrap().contains(&"d".repeat(1024)));
    fs::write(hs.source.join("file"), "x").unwrap();
    std::os::unix::fs::symlink("nowhere", hs.source.join("dangling")).unwrap();
    for name in ["file", "dangling"] {
        let out = hs.skima(&["new", name, "--description", "D."]);
        assert_code(&out, 1);
        assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
    }
    assert_eq!(fs::read_to_string(hs.source.join("file")).unwrap(), "x");
}
