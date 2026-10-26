//! The binary's surface: help text and colour through pipes.

mod common;

use std::fs;

use common::{Mazo, stderr, stdout};

/// A source with one never-used skill and one Claude home never scanned, so
/// `unused` prints an `unused` row and `status` an `ERROR:` and a `warning:`.
fn fixture() -> (tempfile::TempDir, Mazo) {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let (home, source) = (root.join("home"), root.join("skills"));
    common::write(&source.join("idle/SKILL.md"), "---\nname: idle\ndescription: Idle.\n---\n");
    common::write(
        &source.join("mazo.toml"),
        "source = \".\"\n\n[policy]\nclaude_name_only = true\n\n\
         [[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"~/.claude\"\n",
    );
    common::write(&home.join(".claude/settings.json"), "{}\n");
    let mazo = Mazo::new(&home, &home, &source.join("mazo.toml"), &home.join("data"));
    (temp, mazo)
}

#[test]
fn piped_output_carries_no_escape_codes_without_no_color() {
    let (_temp, mut mazo) = fixture();
    mazo.set("NO_COLOR", None);
    let status = mazo.output(&["status"]);
    assert!(stderr(&status).contains("ERROR: ") && stderr(&status).contains("warning: "), "{}", stderr(&status));
    let unused = mazo.output(&["unused"]);
    assert!(stdout(&unused).contains("idle  unused"), "{}", stdout(&unused));
    for out in [status, unused] {
        assert!(!out.stdout.contains(&0x1b) && !out.stderr.contains(&0x1b), "{}{}", stdout(&out), stderr(&out));
    }
}

#[test]
fn since_help_names_its_syntax_and_default_window() {
    let (_temp, mazo) = fixture();
    let usage = stdout(&mazo.output(&["usage", "--help"]));
    assert!(usage.contains("<N>d or all") && usage.contains("[default: 30d]"), "{usage}");
    let unused = stdout(&mazo.output(&["unused", "--help"]));
    assert!(unused.contains("<N>d or all") && unused.contains("[default: 60d]"), "{unused}");
}

/// AC1, AC5: `evals dir` prints the two directories under the default
/// `<data dir>/evals`, then under a configured `evals_dir` relative to the
/// config, which `show` reads; an unknown skill exits 2.
#[test]
fn evals_dir_prints_the_grade_and_retro_directories() {
    let (temp, mazo) = fixture();
    let root = fs::canonicalize(temp.path()).unwrap();
    let out = mazo.output(&["evals", "dir", "idle"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let evals = root.join("home/data/mazo/evals");
    let expected = |e: &std::path::Path| {
        format!("grades\t{}\nretros\t{}\n", e.join("grades/idle").display(), e.join("retros/idle").display())
    };
    assert_eq!(stdout(&out), expected(&evals));

    let unknown = mazo.output(&["evals", "dir", "nope"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(stderr(&unknown).contains("unknown skill: nope"), "{}", stderr(&unknown));

    let config = root.join("skills/mazo.toml");
    fs::write(&config, format!("evals_dir = \"../evals\"\n{}", fs::read_to_string(&config).unwrap())).unwrap();
    assert_eq!(stdout(&mazo.output(&["evals", "dir", "idle"])), expected(&root.join("evals")));
    common::grade(&root.join("evals"), "idle", "2026-09-01", "8.6", Some(&root.join("skills/idle")));
    let show = mazo.output(&["show", "idle", "--json"]);
    let card: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(card["grades"][0]["score"], "8.6", "{}", stderr(&show));
    assert_eq!(card["grades"][0]["stale"], false);
    assert_eq!(card["content_hash"], card["grades"][0]["content_hash"]);
}

/// A read command parses the ledger once, so an unreadable line warns once,
/// and its scan drops the line, so the next command does not warn.
#[test]
fn an_unreadable_ledger_line_warns_once_and_the_scan_drops_it() {
    let (temp, mazo) = fixture();
    let ledger = fs::canonicalize(temp.path()).unwrap().join("home/data/mazo/events.jsonl");
    let kept =
        serde_json::to_string(&common::event("e1", "idle", mazo::ledger::Channel::Tool, "s", "2026-10-01T00:00:00Z"))
            .unwrap();
    common::write(&ledger, &format!("{kept}\nnot json\n"));
    let warning = "unreadable ledger line skipped";
    let first = stderr(&mazo.output(&["usage"]));
    assert_eq!(first.matches(warning).count(), 1, "{first}");
    assert_eq!(fs::read_to_string(&ledger).unwrap(), format!("{kept}\n"));
    let second = stderr(&mazo.output(&["unused"]));
    assert!(!second.contains(warning), "{second}");
}

#[test]
fn a_reader_that_closes_stdout_early_ends_the_command_with_0_and_no_panic() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let (temp, mazo) = fixture();
    let source = fs::canonicalize(temp.path()).unwrap().join("skills");
    // About 130 KiB of rows, past a 64 KiB pipe buffer, so the write after the close fails.
    for i in 0..1000 {
        let name = format!("idle-{i:04}-{}", "x".repeat(50));
        common::write(&source.join(&name).join("SKILL.md"), &format!("---\nname: {name}\ndescription: Idle.\n---\n"));
    }
    let mut child = mazo.command(&[], &["usage"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    assert!(line.starts_with("skill"), "{line}");
    let out = child.wait_with_output().unwrap();
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

/// Every subcommand `mazo --help` lists has a row in the command reference.
#[test]
fn the_command_reference_lists_every_subcommand() {
    let (_temp, mazo) = fixture();
    let help = stdout(&mazo.output(&["--help"]));
    let reference =
        fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/commands.md")).unwrap();
    let table = reference.split("# Commands\n\n").nth(1).and_then(|s| s.split("\n\n").next()).unwrap();
    let commands = help.split("Commands:\n").nth(1).and_then(|s| s.split("\n\n").next()).unwrap();
    let names: Vec<&str> =
        commands.lines().filter_map(|l| l.split_whitespace().next()).filter(|&n| n != "help").collect();
    assert!(names.contains(&"evals"), "{help}");
    for name in names {
        assert!(table.contains(&format!("| [`mazo {name}")), "no command reference row for `mazo {name}`");
    }
}
