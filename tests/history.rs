//! Cost and not a repository: one `git log` and one `git status` per
//! report give every skill's first and last commit, `+N` and `+wip`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Mazo, World, git, stderr, write};
use mazo::evals;
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
    write(&src.join(".grades/a/g.md"), "---\ndate: 2026-01-01\nscore: 7\n---\n");
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
fn first_last_since_and_wip_from_one_pass() {
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

    let grade = Path::new(".grades/a/g.md");
    assert_eq!(h.since("a", grade), 2, "two later commits touched a");
    assert_eq!(h.since("b", grade), 0);
    assert_eq!(h.since("a", Path::new(".grades/a/uncommitted.md")), 0);
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

const GRADE: &str = ".grades/s/g.md";

/// A repository with skills `s` and `t` committed on 2026-01-01.
fn merge_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q"], "2026-01-01T00:00:00Z");
    skill(tmp.path(), "s");
    skill(tmp.path(), "t");
    commit(tmp.path(), "init", "2026-01-01T00:00:00Z");
    tmp
}

/// Commits `path` with `text` on the current branch.
fn edit(src: &Path, path: &str, text: &str, date: &str) {
    write(&src.join(path), text);
    commit(src, path, date);
}

/// `(mazo's +N, git rev-list --count G..HEAD -- <skill>)` for each skill,
/// with whether the grade of `s` is stale.
fn parity(src: &Path, skills: &[&str]) -> (Vec<(usize, usize)>, bool) {
    let inv = Inventory::load(src).unwrap();
    let h = inv.history.as_ref().unwrap();
    let g = h.last(".grades").unwrap().hash.clone();
    let rev_list = |skill: &str| {
        let range = format!("{g}..HEAD");
        let out = Command::new("git").arg("-C").arg(src).args(["rev-list", "--count", &range, "--", skill]).output();
        let out = out.unwrap();
        String::from_utf8(out.stdout).unwrap().trim().parse::<usize>().unwrap()
    };
    let counts = skills.iter().map(|s| (h.since(s, Path::new(GRADE)), rev_list(s))).collect();
    (counts, evals::load(&inv, "s").grades.pop().unwrap().stale())
}

/// `+N` counts by ancestry. A side branch edits the skill before
/// the grade is committed on the main line, then merges: the edit is newer
/// than `G` by ancestry, not by date. `t`, edited in G itself, is not.
#[test]
fn since_counts_skill_commits_that_are_not_ancestors_of_the_grade() {
    let tmp = merge_repo();
    let src = tmp.path();
    git(src, &["checkout", "-q", "-b", "side"], "2026-01-02T00:00:00Z");
    edit(src, "s/SKILL.md", "side edit", "2026-01-02T00:00:00Z");
    git(src, &["checkout", "-q", "-"], "2026-01-03T00:00:00Z");
    write(&src.join("t/notes.md"), "with the grade");
    edit(src, GRADE, "---\ndate: 2026-01-03\nscore: 7\n---\n", "2026-01-03T00:00:00Z");
    git(src, &["merge", "-q", "--no-edit", "side"], "2026-01-04T00:00:00Z");
    assert_eq!(parity(src, &["s", "t"]), (vec![(1, 1), (0, 0)], true));
}

/// An evil merge edits the skill after the grade while merging a branch that
/// never touched it: the merge alone makes the grade stale.
#[test]
fn an_evil_merge_after_the_grade_counts() {
    let tmp = merge_repo();
    let src = tmp.path();
    edit(src, GRADE, "---\ndate: 2026-01-02\nscore: 7\n---\n", "2026-01-02T00:00:00Z");
    git(src, &["checkout", "-q", "-b", "side"], "2026-01-03T00:00:00Z");
    edit(src, "t/notes.md", "side", "2026-01-03T00:00:00Z");
    git(src, &["checkout", "-q", "-"], "2026-01-04T00:00:00Z");
    edit(src, "t/other.md", "main", "2026-01-04T00:00:00Z");
    git(src, &["merge", "-q", "--no-commit", "side"], "2026-01-05T00:00:00Z");
    write(&src.join("s/SKILL.md"), "edited in the merge");
    commit(src, "evil merge", "2026-01-05T00:00:00Z");
    assert_eq!(parity(src, &["s", "t"]), (vec![(1, 1), (3, 3)], true));
}

/// Both branches edit the skill after the grade and merge cleanly: the two
/// edits and the merge, which differs from each parent in the skill, count.
#[test]
fn both_branches_touching_the_skill_then_a_merge_count_three() {
    let tmp = merge_repo();
    let src = tmp.path();
    edit(src, GRADE, "---\ndate: 2026-01-02\nscore: 7\n---\n", "2026-01-02T00:00:00Z");
    git(src, &["checkout", "-q", "-b", "side"], "2026-01-03T00:00:00Z");
    edit(src, "s/a.md", "side", "2026-01-03T00:00:00Z");
    git(src, &["checkout", "-q", "-"], "2026-01-04T00:00:00Z");
    edit(src, "s/b.md", "main", "2026-01-04T00:00:00Z");
    git(src, &["merge", "-q", "--no-edit", "side"], "2026-01-05T00:00:00Z");
    assert_eq!(parity(src, &["s", "t"]), (vec![(3, 3), (0, 0)], true));
}

#[test]
fn outside_a_worktree_history_is_absent_and_no_grade_is_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path();
    skill(src, "s");
    write(&src.join(".grades/s/g.md"), "---\ndate: 2026-01-01\nscore: 7\n---\n");
    let w = World::new(src, &[]);
    assert!(w.inv.history.is_none() && w.inv.worktree.is_none());
    let g = evals::load(&w.inv, "s").grades.pop().unwrap();
    assert!(!g.stale());

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
/// skills: `rev-parse` for the worktree, then one `status` and one `log`.
#[test]
fn a_report_runs_one_log_and_one_status_whatever_the_skill_count() {
    for n in [1, 6] {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let src = root.join("repo/skills");
        git(&root, &["init", "-q", "repo"], "2026-01-01T00:00:00Z");
        for i in 0..n {
            skill(&src, &format!("s{i}"));
            write(&src.join(format!(".grades/s{i}/g.md")), "---\ndate: 2026-01-01\nscore: 7\n---\n");
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
            assert_eq!(subs, ["rev-parse", "status", "log"], "{n} skills, {args:?}: {calls:#?}");
            if args[0] == "usage" {
                let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
                for r in v["rows"].as_array().unwrap() {
                    assert_eq!(r["grade"]["stale"] == true, r["skill"] == "s0", "only s0 changed after its grade: {r}");
                }
            }
        }
    }
}
