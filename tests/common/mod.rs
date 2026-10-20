//! Helpers shared by every integration test binary: fixture paths, the real
//! skills directory, `Home` builders, the fixed clock, events and the report
//! `World`, parsing a transcript the way a scan does, running the binary, and
//! small filesystem and git utilities.

#![allow(dead_code)]

pub mod sync;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use skima::adapters::{Adapter, Parsed, SkillRoots};
use skima::audit::SkillRecord;
use skima::config::{Config, Home};
use skima::inventory::Inventory;
use skima::ledger::{Actor, Agent, Channel, Coverage, Event, Origin};
use skima::report::Input;
use skima::tail::{self, Chunk, Cursor, Prefilter};

/// The fixed "now" of report tests.
pub const NOW: &str = "2026-10-13T12:00:00Z";

pub fn now() -> jiff::Timestamp {
    NOW.parse().unwrap()
}

/// `days` days before NOW, as a ledger timestamp.
pub fn ago(days: i64) -> String {
    now().checked_sub(jiff::SignedDuration::from_hours(24 * days)).unwrap().to_string()
}

/// A main-actor Claude event of `skill` from `/work`, read from a transcript
/// that does not exist.
pub fn event(id: &str, skill: &str, channel: Channel, session: &str, ts: &str) -> Event {
    Event {
        v: 1,
        id: id.into(),
        ts: ts.into(),
        agent: Agent::Claude,
        home: "claude".into(),
        agent_version: None,
        session: session.into(),
        actor: Actor::Main,
        skill: skill.into(),
        origin: Origin::Canonical,
        channel,
        path: None,
        cwd: Some("/work".into()),
        authoring: false,
        src: format!("/nonexistent/{id}.jsonl:1"),
    }
}

/// What a report reads besides the events.
pub struct World {
    pub cfg: Config,
    pub inv: Inventory,
    pub coverage: BTreeMap<String, Coverage>,
    pub audits: BTreeMap<String, SkillRecord>,
}

impl World {
    /// The inventory of `source` and one home per `(name, agent, first)` at
    /// `<source>/<name>`, covered from `first` to NOW, under the default policy.
    pub fn new(source: &Path, homes: &[(&str, Agent, &str)]) -> World {
        let cfg = Config {
            source: source.into(),
            loose: vec![],
            homes: homes.iter().map(|&(name, agent, _)| home(name, agent, &source.join(name))).collect(),
            policy: Default::default(),
            judge: Default::default(),
        };
        let coverage = homes
            .iter()
            .map(|&(name, _, first)| (name.into(), Coverage { first: Some(first.into()), last_scan: Some(NOW.into()) }))
            .collect();
        World { inv: Inventory::load(source).unwrap(), cfg, coverage, audits: BTreeMap::new() }
    }

    pub fn input<'a>(&'a self, events: &'a [Event]) -> Input<'a> {
        Input { cfg: &self.cfg, inv: &self.inv, events, coverage: &self.coverage, audits: &self.audits, now: now() }
    }
}

/// The `skima` binary run in `cwd` with a private `HOME`, `SKIMA_CONFIG` and
/// `XDG_DATA_HOME`, and `<cwd>/bin` ahead of `PATH` for the stub agents, git
/// and openers a test writes there. `env` maps each variable to its value;
/// `None` removes it.
pub struct Skima {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, Option<OsString>>,
}

impl Skima {
    pub fn new(cwd: &Path, home: &Path, config: &Path, data: &Path) -> Skima {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path =
            std::env::join_paths(std::iter::once(cwd.join("bin")).chain(std::env::split_paths(&inherited))).unwrap();
        let vars: [(&str, OsString); 4] =
            [("HOME", home.into()), ("SKIMA_CONFIG", config.into()), ("XDG_DATA_HOME", data.into()), ("PATH", path)];
        Skima { cwd: cwd.into(), env: vars.into_iter().map(|(k, v)| (k.into(), Some(v))).collect() }
    }

    /// Sets `key` for every later run, or removes it when `value` is `None`.
    pub fn set(&mut self, key: &str, value: Option<&OsStr>) -> &mut Skima {
        self.env.insert(key.into(), value.map(OsString::from));
        self
    }

    /// `skima args…` with `env` set on top of [`Skima::env`] for this run only.
    pub fn command(&self, env: &[(&str, &OsStr)], args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_skima"));
        cmd.args(args).current_dir(&self.cwd);
        for (k, v) in &self.env {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        cmd.envs(env.iter().copied());
        cmd
    }

    pub fn output(&self, args: &[&str]) -> Output {
        self.command(&[], args).output().unwrap()
    }
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `tests/fixtures/<rel>`.
pub fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel)
}

/// A real skills repository, named by `SKIMA_TEST_SKILLS`.
pub fn test_skills_env() -> Option<PathBuf> {
    std::env::var_os("SKIMA_TEST_SKILLS").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The skills directory under test: `SKIMA_TEST_SKILLS`, or the synthetic
/// `tests/fixtures/skills` tree.
pub fn real_skills() -> PathBuf {
    test_skills_env().unwrap_or_else(|| fixture("skills"))
}

pub fn home(name: &str, agent: Agent, path: &Path) -> Home {
    Home { name: name.into(), agent, path: path.to_path_buf(), links: Default::default() }
}

/// Reads `path` from `cursor` through the scan's prefilter (markers, the source
/// and every home skills dir in `roots`) and parses the chunk with its header.
pub fn parse_file(adapter: &dyn Adapter, roots: &SkillRoots, path: &Path, cursor: Option<&Cursor>) -> (Parsed, Cursor) {
    let extra: Vec<String> =
        std::iter::once(&roots.source).chain(&roots.home_skills).map(|p| p.to_string_lossy().into_owned()).collect();
    let chunk = tail::read(path, cursor, &Prefilter::new(extra.iter().map(String::as_str))).unwrap();
    let header = tail::first_line(path).unwrap();
    let parsed = adapter.parse(roots, path, header.as_deref(), &chunk);
    (parsed, chunk.cursor)
}

/// Parses every line of `path`, prefilter bypassed, so a no-event test reaches
/// the parser even when its records carry no marker.
pub fn parse_unfiltered(adapter: &dyn Adapter, roots: &SkillRoots, path: &Path) -> Parsed {
    let text = fs::read_to_string(path).unwrap();
    let lines: Vec<(u64, Vec<u8>)> = text.lines().zip(1..).map(|(l, n)| (n, l.as_bytes().to_vec())).collect();
    let chunk = Chunk { lines, cursor: Cursor { dev: 0, ino: 0, offset: 0, line: 0 } };
    let header = tail::first_line(path).unwrap();
    adapter.parse(roots, path, header.as_deref(), &chunk)
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
