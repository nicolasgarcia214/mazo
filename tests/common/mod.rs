//! Helpers shared by every integration test binary: fixture paths, the real
//! skills directory, `Home` builders, parsing a transcript the way a scan does,
//! and small filesystem and git utilities.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use skima::adapters::{Adapter, ParseCtx, Parsed, SkillRoots};
use skima::config::Home;
use skima::ledger::Agent;
use skima::tail::{self, Chunk, Cursor, Prefilter};

/// `tests/fixtures/<rel>`.
pub fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel)
}

/// The real skills directory: `SKIMA_TEST_SKILLS`, or `~/dotfiles/skills`.
pub fn real_skills() -> PathBuf {
    match std::env::var_os("SKIMA_TEST_SKILLS").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(std::env::var("HOME").expect("HOME")).join("dotfiles/skills"),
    }
}

pub fn home(name: &str, agent: Agent, path: &Path) -> Home {
    Home { name: name.into(), agent, path: path.to_path_buf(), links: Default::default() }
}

/// Reads `path` from `cursor` through the scan's prefilter (markers, the source
/// and every home skills dir in `roots`) and parses the chunk with its header.
pub fn parse_file(
    adapter: &dyn Adapter,
    home: &Home,
    roots: &SkillRoots,
    path: &Path,
    cursor: Option<&Cursor>,
) -> (Parsed, Cursor) {
    let extra: Vec<String> =
        std::iter::once(&roots.source).chain(&roots.home_skills).map(|p| p.to_string_lossy().into_owned()).collect();
    let chunk = tail::read(path, cursor, &Prefilter::new(extra.iter().map(String::as_str))).unwrap();
    let header = tail::first_line(path).unwrap();
    let parsed = adapter.parse(&ParseCtx { home, roots }, path, header.as_deref(), &chunk);
    (parsed, chunk.cursor)
}

/// Parses every line of `path`, prefilter bypassed, so a no-event test reaches
/// the parser even when its records carry no marker.
pub fn parse_unfiltered(adapter: &dyn Adapter, home: &Home, roots: &SkillRoots, path: &Path) -> Parsed {
    let text = fs::read_to_string(path).unwrap();
    let lines: Vec<(u64, Vec<u8>)> = text.lines().zip(1..).map(|(l, n)| (n, l.as_bytes().to_vec())).collect();
    let chunk = Chunk { lines, cursor: Cursor::default() };
    let header = tail::first_line(path).unwrap();
    adapter.parse(&ParseCtx { home, roots }, path, header.as_deref(), &chunk)
}

/// Copies a directory tree, passing every file's text through `edit`.
pub fn copy_tree(from: &Path, to: &Path, edit: &dyn Fn(String) -> String) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target, edit);
        } else {
            fs::write(&target, edit(fs::read_to_string(e.path()).unwrap())).unwrap();
        }
    }
}

/// Writes `text`, creating parent directories.
pub fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Runs `git -C dir args…` with a fixed identity and both dates set to `date`,
/// isolated from the user's git config.
pub fn git(dir: &Path, args: &[&str], date: &str) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}
