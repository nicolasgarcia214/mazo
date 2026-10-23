//! Helpers shared by every integration test binary: fixture paths, the real
//! skills directory, `Home` builders, the fixed clock, events and the report
//! `World`, parsing a transcript the way a scan does, running the binary, a
//! stub judge's answers, and small filesystem and git utilities.

#![allow(dead_code)]

pub mod golden;
pub mod judge;
pub mod probe;
pub mod sync;

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use mazo::adapters::{Adapter, Parsed, SkillRoots};
use mazo::audit::SkillRecord;
use mazo::config::{Config, Home};
use mazo::inventory::Inventory;
use mazo::ledger::{Actor, Agent, Channel, Coverage, Event, Origin};
use mazo::report::Input;
use mazo::tail::{self, Chunk, Cursor, Prefilter};

/// The fixed "now" of report tests.
pub const NOW: &str = "2026-10-13T12:00:00Z";

pub fn now() -> jiff::Timestamp {
    NOW.parse().unwrap()
}

/// `days` days before NOW, as a ledger timestamp.
pub fn ago(days: i64) -> String {
    now().checked_sub(jiff::SignedDuration::from_hours(24 * days)).unwrap().to_string()
}

/// The date of timestamp `ts` in the system zone, as the binary prints it.
pub fn local_date(ts: &str) -> String {
    ts.parse::<jiff::Timestamp>().unwrap().to_zoned(jiff::tz::TimeZone::system()).date().to_string()
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
    /// `<source>/<name>`, covered from `first` to NOW, under the default
    /// policy, with grades and retros under `<source>/.evals`.
    pub fn new(source: &Path, homes: &[(&str, Agent, &str)]) -> World {
        let cfg = Config {
            source: Some(source.into()),
            loose: vec![],
            trusted: vec![],
            evals_dir: source.join(".evals"),
            context_window: mazo::config::CONTEXT_WINDOW,
            homes: homes.iter().map(|&(name, agent, _)| home(name, agent, &source.join(name))).collect(),
            policy: Default::default(),
            judge: None,
        };
        let coverage = homes
            .iter()
            .map(|&(name, _, first)| (name.into(), Coverage { first: Some(first.into()), last_scan: Some(NOW.into()) }))
            .collect();
        World { inv: Inventory::load(source).unwrap(), cfg, coverage, audits: BTreeMap::new() }
    }

    pub fn input<'a>(&'a self, events: &'a [Event]) -> Input<'a> {
        Input {
            cfg: &self.cfg,
            inv: &self.inv,
            events,
            coverage: &self.coverage,
            audits: &self.audits,
            now: now(),
            tz: jiff::tz::TimeZone::UTC,
        }
    }
}

/// The report fixture: the skills of `fixtures/reports/skills` committed on
/// 2026-01-01, plus `fresh`, which is never committed.
pub fn report_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&fixture("reports/skills"), dir.path(), &|s| s);
    git(dir.path(), &["init", "-q"], NOW);
    git(dir.path(), &["add", "."], NOW);
    git(dir.path(), &["commit", "-q", "-m", "skills"], "2026-01-01T00:00:00Z");
    write(&dir.path().join("fresh/SKILL.md"), "---\nname: fresh\ndescription: New.\n---\n\nNew.\n");
    dir
}

/// When the report fixture homes' coverage starts.
pub const CLAUDE_FIRST: &str = "2026-09-10T08:00:00Z";
pub const CODEX_FIRST: &str = "2026-03-29T08:00:00Z";

/// The report fixture homes `claude` and `codex`, with the authoring skills.
pub fn report_world(source: &Path) -> World {
    let mut w = World::new(source, &[("claude", Agent::Claude, CLAUDE_FIRST), ("codex", Agent::Codex, CODEX_FIRST)]);
    w.cfg.policy.authoring_skills = ["grade", "retro", "writing-for-agents", "skill-creator"].map(String::from).into();
    w
}

/// A store skill `vetted` under `data`, its lockfile entry vetted weak and
/// of low value.
pub fn vetted(data: &Path) {
    write(&data.join("store/vetted/SKILL.md"), "---\nname: vetted\n---\n");
    write(
        &data.join("mazo.lock"),
        "[[skill]]\nname = \"vetted\"\norigin = \"/x\"\nsubpath = \".\"\ncontent_hash = \"h\"\n\
         trusted = false\ninstalled_at = \"2026-01-01T00:00:00Z\"\nvetted_at = \"2026-01-01T00:00:00Z\"\n\
         [skill.verdict]\nquality = \"weak\"\nvalue = \"low value\"\nreasons = []\n",
    );
}

/// A skill record of `unslop` with `verdict`, written at `at`, with four
/// ranked improvements and a hostile curated example.
pub fn skill_record(verdict: &str, at: &str) -> String {
    let session = |s: &str| serde_json::json!([s]);
    let improvement = |i: usize| serde_json::json!({"change": format!("Change {i}."), "reason": format!("Reason {i}."), "sessions": session("s1")});
    let rate = |n: usize| serde_json::json!({"n": n, "of": 4, "sessions": &["s1", "s2", "s3", "s4"][..n]});
    let record = serde_json::json!({
        "v": 1, "kind": "skill", "skill": "unslop", "audited_at": at, "sessions": ["s1", "s2", "s3", "s4"],
        "judge": {"agent": "claude", "model": null, "cost_usd": 0.12},
        "prompt": "skill-v1",
        "rates": {"followed": rate(3), "achieved": rate(2), "abandoned_or_redone": rate(1)},
        "result": {
            "verdict": verdict, "patterns": [], "works": [], "failures": [],
            "improvements": (1..=4).map(improvement).collect::<Vec<_>>(),
            "versus_grades": "",
            "examples": [{"situation": "<b>note</b>", "asked": "Tidy.", "happened": "Done.", "tip": "Say the tone.", "session": "s1"}],
        },
    });
    format!("{record}\n")
}

/// The `mazo` binary run in `cwd` with a private `HOME`, `MAZO_CONFIG` and
/// `XDG_DATA_HOME`, and `<cwd>/bin` ahead of `PATH` for the stub agents, git
/// and openers a test writes there. `env` maps each variable to its value;
/// `None` removes it.
pub struct Mazo {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, Option<OsString>>,
}

impl Mazo {
    pub fn new(cwd: &Path, home: &Path, config: &Path, data: &Path) -> Mazo {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path =
            std::env::join_paths(std::iter::once(cwd.join("bin")).chain(std::env::split_paths(&inherited))).unwrap();
        let vars: [(&str, OsString); 4] =
            [("HOME", home.into()), ("MAZO_CONFIG", config.into()), ("XDG_DATA_HOME", data.into()), ("PATH", path)];
        Mazo { cwd: cwd.into(), env: vars.into_iter().map(|(k, v)| (k.into(), Some(v))).collect() }
    }

    /// Sets `key` for every later run, or removes it when `value` is `None`.
    pub fn set(&mut self, key: &str, value: Option<&OsStr>) -> &mut Mazo {
        self.env.insert(key.into(), value.map(OsString::from));
        self
    }

    /// `mazo args…` with `env` set on top of [`Mazo::env`] for this run only.
    pub fn command(&self, env: &[(&str, &OsStr)], args: &[&str]) -> Command {
        let mut cmd = self.wrap(env!("CARGO_BIN_EXE_mazo"));
        cmd.args(args).envs(env.iter().copied());
        cmd
    }

    /// `program` run as the binary is: in [`Mazo::cwd`] with [`Mazo::env`].
    pub fn wrap(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.cwd);
        for (k, v) in &self.env {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
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

/// A real skills repository, named by `MAZO_TEST_SKILLS`.
pub fn test_skills_env() -> Option<PathBuf> {
    std::env::var_os("MAZO_TEST_SKILLS").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The skills directory under test: `MAZO_TEST_SKILLS`, or the synthetic
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
        roots.source.iter().chain(&roots.home_skills).map(|p| p.to_string_lossy().into_owned()).collect();
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
    let chunk = Chunk { lines, oversized: 0, cursor: Cursor::default() };
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
/// The grade `<evals>/grades/<skill>/<date>.md` of `score`, recording the
/// current content hash of `dir` when given.
pub fn grade(evals: &Path, skill: &str, date: &str, score: &str, dir: Option<&Path>) {
    let hash = dir.map_or(String::new(), |d| format!("content_hash: {}\n", mazo::store::content_hash(d).unwrap()));
    write(&evals.join(format!("grades/{skill}/{date}.md")), &format!("---\ndate: {date}\nscore: {score}\n{hash}---\n"));
}

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

/// A grandparent → parent → child session chain of a Codex or Pi home on a
/// temporary machine, each session one load of its own skill, scanned into
/// `data` by the library scan.
pub struct Chain {
    pub tmp: tempfile::TempDir,
    pub world: World,
    pub data: PathBuf,
    /// Grandparent, parent and child session ids.
    pub ids: [String; 3],
    /// Grandparent, parent and child transcripts.
    pub files: [PathBuf; 3],
}

impl Chain {
    /// The chain in a fresh home; `written` picks which of the three transcripts exist.
    pub fn new(agent: Agent, written: [bool; 3]) -> Chain {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("skills");
        fs::create_dir_all(&source).unwrap();
        let world = World::new(&source, &[("home", agent, NOW)]);
        let dir = world.cfg.homes[0].transcripts().join("2026/10/10");
        let ids = ["1", "2", "3"].map(|n| format!("019a0000-0000-7000-8000-00000000000{n}"));
        let files = [0, 1, 2].map(|i| match agent {
            Agent::Codex => dir.join(format!("rollout-2026-10-10T00-00-0{i}-{}.jsonl", ids[i])),
            _ => dir.join(format!("2026-10-10T00-00-0{i}-000Z_{}.jsonl", ids[i])),
        });
        for i in 0..3 {
            if !written[i] {
                continue;
            }
            let ts = ago(3 - i as i64);
            let load = source.join(format!("s{i}/SKILL.md"));
            let lines = match agent {
                Agent::Codex => {
                    let mut meta = serde_json::json!({"id": ids[i], "cwd": "/work", "source": "cli"});
                    if i > 0 {
                        meta["forked_from_id"] = ids[i - 1].clone().into();
                    }
                    let args = serde_json::json!({"cmd": format!("cat {}", load.display())}).to_string();
                    [
                        serde_json::json!({"timestamp": ts, "type": "session_meta", "payload": meta}),
                        serde_json::json!({"timestamp": ts, "type": "response_item", "payload":
                            {"type": "function_call", "name": "exec_command", "arguments": args, "call_id": "c"}}),
                    ]
                }
                _ => {
                    let mut header =
                        serde_json::json!({"type": "session", "id": ids[i], "timestamp": ts, "cwd": "/work"});
                    if i > 0 {
                        header["parentSession"] = files[i - 1].to_str().unwrap().into();
                    }
                    let text = format!("<skill name=\"s{i}\" location=\"{}\">\n</skill>", load.display());
                    [
                        header,
                        serde_json::json!({"type": "message", "id": format!("e{i}"), "timestamp": ts,
                            "message": {"role": "user", "content": [{"type": "text", "text": text}]}}),
                    ]
                }
            };
            write(&files[i], &lines.map(|l| l.to_string() + "\n").concat());
        }
        Chain { data: tmp.path().join("data"), tmp, world, ids, files }
    }

    /// Scans, or reparses, and returns the ledger.
    pub fn scan(&self, reparse: bool) -> Vec<Event> {
        mazo::scan::scan(&self.world.cfg, &self.world.inv, &self.data, reparse).unwrap();
        mazo::ledger::Ledger::load(&self.data).unwrap().events().to_vec()
    }

    /// The `(id, session)` of every event read from the child's transcript.
    pub fn child(&self, events: &[Event]) -> Vec<(String, String)> {
        let file = self.files[2].to_string_lossy();
        events.iter().filter(|e| e.src_file() == file).map(|e| (e.id.clone(), e.session.clone())).collect()
    }

    pub fn roots_json(&self) -> PathBuf {
        self.data.join("roots.json")
    }

    /// `<data>/roots.json`, empty when it is missing.
    pub fn roots(&self) -> BTreeMap<String, String> {
        fs::read(self.roots_json()).map_or_else(|_| BTreeMap::new(), |b| serde_json::from_slice(&b).unwrap())
    }
}
