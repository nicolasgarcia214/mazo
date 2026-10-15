//! Lifecycle commands end to end (SPEC §10.8): `prune`.

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
