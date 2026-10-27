//! Cost and not a repository: one `git log` and one `git status` per
//! report give every skill's first and last commit and its uncommitted changes.

#![allow(clippy::disallowed_methods)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Mazo, World, git, stderr, write};
use mazo::inventory::Inventory;
use mazo::report::{self, Since};

/// `git add -A` and commit everything under `repo` on `date`.
fn commit(repo: &Path, subject: &str, date: &str) {
    git(repo, &["add", "-A"], date);
    git(repo, &["commit", "-q", "-m", subject], date);
}

fn skill(dir: &Path, name: &str) {
    write(&dir.join(name).join("SKILL.md"), &format!("---\nname: {name}\ndescription: d\n---\n"));
}

/// `repo/skills` is the source; `repo/other` lies outside it.
fn known_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let src = repo.join("skills");
    git(tmp.path(), &["init", "-q", "repo"], "2026-01-01T00:00:00Z");
    skill(&src, "a");
    skill(&src, "b");
    commit(&repo, "add a and b", "2026-01-01T00:00:00Z");
    write(&src.join("a/notes.md"), "one");
    write(&repo.join("other/a/f"), "outside the source");
    commit(&repo, "edit a", "2026-02-01T00:00:00Z");
    skill(&src.join("claude"), "nested");
    commit(&repo, "add nested", "2026-03-01T00:00:00Z");
    write(&src.join("a/notes.md"), "two");
    commit(&repo, "edit a again", "2026-04-01T10:00:00+02:00");
    (tmp, src)
}

#[test]
fn first_last_and_wip_from_one_pass() {
    let (_tmp, src) = known_repo();
    let inv = Inventory::load(&src).unwrap();
    let h = inv.history.as_ref().expect("the source is in a git worktree");

    let first = h.first("a").unwrap();
    assert_eq!((first.date.as_str(), first.subject.as_str()), ("2026-01-01T00:00:00Z", "add a and b"));
    assert_eq!(first.hash.len(), 40);
    let last = h.last("a").unwrap();
    assert_eq!((last.date.as_str(), last.subject.as_str()), ("2026-04-01T10:00:00+02:00", "edit a again"));
    assert_eq!(h.last("b").unwrap().subject, "add a and b");
    assert!(h.first("other").is_none(), "paths outside the source are not the source's");
    assert!(h.first("fresh").is_none());

    assert!(inv.known.contains("nested"), "known names come from the same log");
    assert!(!h.wip("a") && !h.wip("b"));

    write(&src.join("b/SKILL.md"), "changed");
    write(&src.join("c/new.md"), "untracked");
    git(&src, &["mv", "claude", "renamed"], "2026-05-01T00:00:00Z");
    let h = Inventory::load(&src).unwrap().history.unwrap();
    assert!(h.wip("b"), "a modified file");
    assert!(h.wip("c"), "an untracked file");
    assert!(h.wip("claude") && h.wip("renamed"), "both sides of a staged rename");
    assert!(!h.wip("a"));
}

#[test]
fn outside_a_worktree_history_is_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path();
    skill(src, "s");
    let w = World::new(src, &[]);
    assert!(w.inv.history.is_none() && w.inv.worktree.is_none());

    let v: serde_json::Value =
        serde_json::from_str(&report::unused(&w.input(&[]), Since::Days(60), true, false)).unwrap();
    assert_eq!(v["skills"][0]["status"], "unused", "no new flag without history");
}

/// The first `git` on PATH that is not in `skip`.
fn real_git(skip: &Path) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    std::env::split_paths(&path).map(|d| d.join("git")).find(|g| g.is_file() && !g.starts_with(skip)).unwrap()
}

/// AC: a report starts the same few git processes whatever the number of
/// skills: `rev-parse` for the worktree, then one `status` and one `log`;
/// `show` adds one `config` for the origin remote, which this repository lacks.
#[test]
fn a_report_runs_one_log_and_one_status_whatever_the_skill_count() {
    for n in [1, 6] {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let src = root.join("repo/skills");
        git(&root, &["init", "-q", "repo"], "2026-01-01T00:00:00Z");
        for i in 0..n {
            let name = format!("s{i}");
            skill(&src, &name);
            let hash = mazo::store::content_hash(&src.join(&name)).unwrap();
            let grade = format!("---\ndate: 2026-01-01\nscore: 7\ncontent_hash: {hash}\n---\n");
            write(&root.join(format!("xdg/mazo/evals/grades/{name}/g.md")), &grade);
        }
        commit(&root.join("repo"), "skills", "2026-01-01T00:00:00Z");
        write(&src.join("s0/notes.md"), "later");
        commit(&root.join("repo"), "edit s0", "2026-02-01T00:00:00Z");
        fs::create_dir_all(root.join("claude/projects")).unwrap();
        let config = root.join("mazo.toml");
        fs::write(
            &config,
            "source = \"repo/skills\"\n\n[[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"claude\"\n",
        )
        .unwrap();
        let bin = root.join("bin");
        let log = root.join("git.log");
        write(
            &bin.join("git"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
                log.display(),
                real_git(&bin).display()
            ),
        );
        Command::new("chmod").arg("+x").arg(bin.join("git")).status().unwrap();
        let run = Mazo::new(&root, &root, &config, &root.join("xdg"));

        for args in [&["usage", "--json"][..], &["unused", "--json"], &["show", "s0", "--json"]] {
            let _ = fs::remove_file(&log);
            let out = run.output(args);
            assert!(out.status.success(), "{args:?}: {}", stderr(&out));
            let calls: Vec<String> = fs::read_to_string(&log).unwrap().lines().map(String::from).collect();
            let sub = |c: &String| c.split(' ').nth(3).unwrap_or("").to_string();
            let subs: Vec<String> = calls.iter().map(sub).collect();
            let want: &[&str] = if args[0] == "show" {
                &["rev-parse", "status", "log", "config"]
            } else {
                &["rev-parse", "status", "log"]
            };
            assert_eq!(subs, want, "{n} skills, {args:?}: {calls:#?}");
            if args[0] == "usage" {
                let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                for r in v["rows"].as_array().unwrap() {
                    assert_eq!(r["grade"]["stale"] == true, r["skill"] == "s0", "only s0 changed after its grade: {r}");
                }
            }
        }
    }
}
