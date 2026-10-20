//! The binary's surface: help text and colour through pipes.

mod common;

use std::fs;

use common::{Skima, stderr, stdout};

/// A source with one never-used skill and one Claude home never scanned, so
/// `unused` prints an `unused` row and `status` an `ERROR:` and a `warning:`.
fn fixture() -> (tempfile::TempDir, Skima) {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (home, source) = (root.join("home"), root.join("skills"));
    common::write(&source.join("idle/SKILL.md"), "---\nname: idle\ndescription: Idle.\n---\n");
    common::write(
        &source.join("skima.toml"),
        "source = \".\"\n\n[policy]\nclaude_name_only = true\n\n\
         [[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"~/.claude\"\n",
    );
    common::write(&home.join(".claude/settings.json"), "{}\n");
    let skima = Skima::new(&home, &home, &source.join("skima.toml"), &home.join("data"));
    (temp, skima)
}

#[test]
fn piped_output_carries_no_escape_codes_without_no_color() {
    let (_temp, mut skima) = fixture();
    skima.set("NO_COLOR", None);
    let status = skima.output(&["status"]);
    assert!(stderr(&status).contains("ERROR: ") && stderr(&status).contains("warning: "), "{}", stderr(&status));
    let unused = skima.output(&["unused"]);
    assert!(stdout(&unused).contains("idle  unused"), "{}", stdout(&unused));
    for out in [status, unused] {
        assert!(!out.stdout.contains(&0x1b) && !out.stderr.contains(&0x1b), "{}{}", stdout(&out), stderr(&out));
    }
}

#[test]
fn since_help_names_its_syntax_and_default_window() {
    let (_temp, skima) = fixture();
    let usage = stdout(&skima.output(&["usage", "--help"]));
    assert!(usage.contains("<N>d or all") && usage.contains("30 days"), "{usage}");
    let unused = stdout(&skima.output(&["unused", "--help"]));
    assert!(unused.contains("<N>d or all") && unused.contains("60 days"), "{unused}");
}
