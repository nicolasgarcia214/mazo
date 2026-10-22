//! `mazo evals migrate` over a fixture git repository.

mod common;

use std::fs;
use std::path::Path;

use common::{Mazo, stderr, stdout};
use mazo::store::content_hash;

const DATE: &str = "2026-01-01T00:00:00Z";

fn grade(date: &str) -> String {
    format!("---\ndate: {date}\nscore: 8\n---\n\n# Grade\n")
}

/// The `content_hash` in `file`'s frontmatter.
fn hash_of(file: &Path) -> Option<String> {
    let text = fs::read_to_string(file).unwrap();
    text.lines().find_map(|l| l.strip_prefix("content_hash: ")).map(str::to_string)
}

/// AC1–AC4: grades committed before a skill change carry the hash of the skill
/// at that commit, which equals the current hash of a skill unchanged since,
/// eol-converted on checkout or not, and differs from that of a changed one. An uncommitted grade and one whose skill
/// directory never existed move without a hash and say why. A destination that
/// exists is never overwritten, its source stays and is listed on every run,
/// and a second run moves nothing and says so.
#[test]
fn migrate_moves_grades_and_retros_with_the_hash_at_their_commit() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let source = root.join("repo/skills");
    let git = |args: &[&str]| common::git(&source, args, DATE);
    common::write(&source.join("mazo.toml"), "source = \".\"\n");
    common::git(&root.join("repo"), &["init", "-q"], DATE);
    for skill in ["steady", "changed"] {
        common::write(
            &source.join(format!("{skill}/SKILL.md")),
            &format!("---\nname: {skill}\ndescription: D.\n---\n"),
        );
        common::write(&source.join(format!("{skill}/references/notes.md")), "notes\n");
    }
    // Checked out with CRLF, stored with LF: the historical copy must be the checkout's bytes.
    common::write(&source.join("crlf/.gitattributes"), "* text eol=crlf\r\n");
    common::write(&source.join("crlf/SKILL.md"), "---\r\nname: crlf\r\ndescription: D.\r\n---\r\n");
    common::write(&source.join(".grades/crlf/2026-01-01.md"), &grade("2026-01-01"));
    common::write(&source.join(".grades/steady/2026-01-01.md"), &grade("2026-01-01"));
    common::write(&source.join(".grades/changed/2026-01-01.md"), &grade("2026-01-01"));
    common::write(&source.join(".grades/gone/2026-01-01.md"), &grade("2026-01-01"));
    common::write(&source.join(".grades/changed/2026-03-01.md"), &grade("2026-03-01"));
    common::write(&source.join(".retros/steady/2026-01-02.md"), "---\ndate: 2026-01-02\nverdict: ok\n---\n");
    git(&["add", "-A", "."]);
    git(&["commit", "-q", "-m", "grades"]);
    let before = content_hash(&source.join("changed")).unwrap();
    common::write(&source.join("changed/SKILL.md"), "---\nname: changed\ndescription: Changed.\n---\n");
    git(&["commit", "-q", "-am", "change"]);
    common::write(&source.join(".grades/steady/2026-02-01.md"), &grade("2026-02-01"));
    let evals = root.join("data/mazo/evals");
    let kept = evals.join("grades/changed/2026-03-01.md");
    common::write(&kept, "mine\n");

    let mazo = Mazo::new(&root, &root.join("home"), &source.join("mazo.toml"), &root.join("data"));
    let out = mazo.output(&["evals", "migrate"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}{}", stderr(&out));

    let steady = content_hash(&source.join("steady")).unwrap();
    assert_eq!(hash_of(&evals.join("grades/steady/2026-01-01.md")), Some(steady.clone()), "{text}");
    assert_eq!(hash_of(&evals.join("retros/steady/2026-01-02.md")), Some(steady), "{text}");
    let crlf = content_hash(&source.join("crlf")).ok();
    assert_eq!(hash_of(&evals.join("grades/crlf/2026-01-01.md")), crlf, "{text}");
    let changed = hash_of(&evals.join("grades/changed/2026-01-01.md"));
    assert_eq!(changed, Some(before), "{text}");
    assert_ne!(changed, content_hash(&source.join("changed")).ok());
    let body = fs::read_to_string(evals.join("grades/steady/2026-01-01.md")).unwrap();
    assert!(body.ends_with("score: 8\n---\n\n# Grade\n"), "{body}");

    for (file, why) in [("steady/2026-02-01.md", "uncommitted"), ("gone/2026-01-01.md", "no gone directory at commit")]
    {
        let dest = evals.join("grades").join(file);
        assert_eq!(hash_of(&dest), None);
        let line = format!(
            "moved {} -> {} without a content_hash: {why}",
            source.join(".grades").join(file).display(),
            dest.display()
        );
        assert!(text.contains(&line), "{line}\n{text}");
    }
    let left = format!("left {}: {} exists\n", source.join(".grades/changed/2026-03-01.md").display(), kept.display());
    assert!(text.contains(&left), "{text}");
    assert!(text.ends_with("moved 6 files\n"), "{text}");
    assert_eq!(fs::read_to_string(&kept).unwrap(), "mine\n");
    assert!(source.join(".grades/changed/2026-03-01.md").is_file());
    assert!(!source.join(".retros").exists() && !source.join(".grades/steady").exists());

    let again = mazo.output(&["evals", "migrate"]);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(stdout(&again), format!("{left}moved nothing\n"));
    assert_eq!(fs::read_to_string(&kept).unwrap(), "mine\n");
}

/// AC5: the help says the command is one-time and removed in the next release.
#[test]
fn migrate_help_says_it_is_temporary() {
    let temp = tempfile::tempdir().unwrap();
    let mazo = Mazo::new(temp.path(), temp.path(), &temp.path().join("mazo.toml"), temp.path());
    for args in [&["evals", "--help"][..], &["evals", "migrate", "--help"]] {
        let help = stdout(&mazo.output(args));
        assert!(help.contains("One-time") && help.contains("removed in the next release"), "{help}");
    }
}
