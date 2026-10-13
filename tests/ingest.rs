//! SPEC §10.4 ingest properties and the F1 criteria, run against temporary
//! Claude homes built from the real fixtures in `tests/fixtures/claude`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use skima::adapters::claude::Claude;
use skima::adapters::{ActorInputs, Adapter, Key, ParseCtx, Parsed, Raw, Session};
use skima::config::{Config, Home};
use skima::inventory::Inventory;
use skima::ledger::{self, Agent, Channel, Event, Ledger};
use skima::scan::{self, ScanReport};
use skima::tail::{Chunk, Cursors};
use tempfile::TempDir;

/// Every Claude transcript fixture: its path under the project directory and
/// its file under `tests/fixtures/claude`. `pair_copy.jsonl` repeats
/// `pair.jsonl` the way a Claude fork copies records with their ids (SPEC §6.2
/// Scan order).
const TRANSCRIPTS: [(&str, &str); 15] = [
    ("attachments.jsonl", "attachments.jsonl"),
    ("bash_two.jsonl", "bash_two.jsonl"),
    ("bench.jsonl", "bench.jsonl"),
    ("clear.jsonl", "clear.jsonl"),
    ("pair.jsonl", "pair.jsonl"),
    ("pair_copy.jsonl", "pair.jsonl"),
    ("probe.jsonl", "probe.jsonl"),
    ("refused.jsonl", "refused.jsonl"),
    ("result_only.jsonl", "result_only.jsonl"),
    ("slash.jsonl", "slash.jsonl"),
    ("tool_result_quote.jsonl", "tool_result_quote.jsonl"),
    ("unparsed.jsonl", "unparsed.jsonl"),
    (
        "29b0f25b-ce3a-4cf5-8081-e6bb2a2ff99d/subagents/agent-a5b86c28fed127eef.jsonl",
        "subagents/agent-a5b86c28fed127eef.jsonl",
    ),
    (
        "3ea64990-464f-4f45-a9d0-91a25bf5a530/subagents/agent-aacafe9c375ac817e.jsonl",
        "subagents/agent-aacafe9c375ac817e.jsonl",
    ),
    (
        "2b0a61b3-e8ed-44c0-81a3-6e94d229e0e4/subagents/agent-ad81ed4c0f32313c2.jsonl",
        "subagents/agent-ad81ed4c0f32313c2.jsonl",
    ),
];

/// The id the Claude adapter gives the Skill result/meta pair in `pair.jsonl`.
const PAIR_ID: &str = "cc:toolu_01EKQ5jSt7RhgFyyw72kyqoY";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude").join(name)
}

fn fixture_lines(name: &str) -> Vec<String> {
    fs::read_to_string(fixture(name)).unwrap().lines().map(str::to_string).collect()
}

/// A temporary machine: a source root, one or more Claude homes and a data dir.
struct Env {
    tmp: TempDir,
    cfg: Config,
    inv: Inventory,
    data: PathBuf,
}

impl Env {
    /// A Claude home named `claude` holding `files` (see [`TRANSCRIPTS`]) and `history.jsonl`, plus any extra empty homes.
    fn new(files: &[(&str, &str)], empty_homes: &[&str]) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("dotfiles/skills");
        for s in ["unslop", "gitmoji-conventional-commit", "grade"] {
            fs::create_dir_all(source.join(s)).unwrap();
            fs::write(source.join(s).join("SKILL.md"), "x").unwrap();
        }
        let source = fs::canonicalize(source).unwrap();
        let home = |name: &str| Home {
            name: name.into(),
            agent: Agent::Claude,
            path: tmp.path().join(name).join(".claude"),
            links: Default::default(),
        };
        let mut homes = vec![home("claude")];
        homes.extend(empty_homes.iter().map(|n| home(n)));
        fs::create_dir_all(&homes[0].path).unwrap();
        fs::copy(fixture("history.jsonl"), homes[0].path.join("history.jsonl")).unwrap();
        let skills: BTreeSet<String> = ["unslop", "gitmoji-conventional-commit", "grade"].map(String::from).into();
        let inv = Inventory { source: source.clone(), skills: skills.clone(), known: skills, worktree: None };
        let cfg = Config { source, loose: vec![], homes };
        let env = Env { data: tmp.path().join("data/skima"), tmp, cfg, inv };
        for (dest, src) in files {
            env.put(dest, &fs::read(fixture(src)).unwrap());
        }
        env
    }

    fn project(&self) -> PathBuf {
        self.cfg.homes[0].path.join("projects/-home-nicolas-projects-argus")
    }

    fn transcript(&self, name: &str) -> PathBuf {
        let p = skima::config::resolve(&self.project(), name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        p
    }

    fn put(&self, name: &str, bytes: &[u8]) {
        fs::write(self.transcript(name), bytes).unwrap();
    }

    fn append(&self, name: &str, bytes: &[u8]) {
        let mut f = fs::OpenOptions::new().create(true).append(true).open(self.transcript(name)).unwrap();
        f.write_all(bytes).unwrap();
    }

    fn scan(&self) -> ScanReport {
        self.scan_in(&self.data, false)
    }

    fn reparse(&self) -> ScanReport {
        self.scan_in(&self.data, true)
    }

    fn scan_in(&self, data: &Path, reparse: bool) -> ScanReport {
        scan::scan(&self.cfg, &self.inv, data, reparse).unwrap()
    }

    fn scan_rule(&self, reparse: bool, adapter: &dyn Adapter) -> ScanReport {
        scan::scan_with(&self.cfg, &self.inv, &self.data, reparse, &|_| adapter).unwrap()
    }

    fn cursors(&self) -> Cursors {
        serde_json::from_slice(&fs::read(self.data.join("cursors.json")).unwrap()).unwrap()
    }

    fn coverage(&self) -> BTreeMap<String, ledger::Coverage> {
        ledger::load_coverage(&self.data).unwrap()
    }
}

/// The raw lines of a ledger.
fn ledger_lines(data: &Path) -> Vec<String> {
    match fs::read_to_string(data.join("events.jsonl")) {
        Ok(s) => s.lines().map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// The ledger as readers see it (last line per id), sorted by id.
fn events(data: &Path) -> Vec<Event> {
    let mut v = Ledger::load(data).unwrap().events().to_vec();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

fn ids(data: &Path) -> BTreeSet<String> {
    events(data).into_iter().map(|e| e.id).collect()
}

/// The ledger line a reader keeps for `id`.
fn line_of(data: &Path, id: &str) -> Option<String> {
    ledger_lines(data).into_iter().rev().find(|l| serde_json::from_str::<Event>(l).unwrap().id == id)
}

fn new_events(r: &ScanReport) -> usize {
    r.homes.values().map(|h| h.new_events).sum()
}

#[test]
fn second_scan_and_cursorless_rescan_append_nothing() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let first = ledger_lines(&env.data);
    assert!(first.len() >= 9, "fixtures yield events: {first:#?}");
    assert_eq!(new_events(&env.scan()), 0);
    assert_eq!(ledger_lines(&env.data), first, "scan twice: +0 lines");
    fs::remove_file(env.data.join("cursors.json")).unwrap();
    assert_eq!(new_events(&env.scan()), 0);
    assert_eq!(ledger_lines(&env.data), first, "no cursors: +0 lines");
}

#[test]
fn appended_record_is_one_event_read_from_the_stored_offset() {
    let env = Env::new(&[("slash.jsonl", "slash.jsonl")], &[]);
    env.scan();
    let path = env.transcript("slash.jsonl");
    let key = path.to_string_lossy().into_owned();
    let before = env.cursors()[&key];
    assert_eq!((before.offset, before.line), (fs::metadata(&path).unwrap().len(), 2));
    let n = ledger_lines(&env.data).len();

    // Rewrite the meta record's uuid in place (same inode, same length). A scan
    // that re-read the prefix would emit it as a second slash event.
    let text = fs::read_to_string(&path).unwrap();
    let at = text.find("d9ad44a4-3e61").unwrap();
    let mut f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(at as u64)).unwrap();
    f.write_all(b"00000000-0000").unwrap();
    drop(f);

    // A new Skill result for a different tool_use id.
    let record = fixture_lines("result_only.jsonl")[0].replace("toolu_01TJBdbiPJWDEp5ZGWUmM35V", "toolu_appended");
    env.append("slash.jsonl", format!("{record}\n").as_bytes());
    assert_eq!(new_events(&env.scan()), 1);
    let lines = ledger_lines(&env.data);
    assert_eq!(lines.len(), n + 1);
    let added: Event = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(added.id, "cc:toolu_appended");
    assert_eq!(added.src, format!("{key}:3"), "line number continues from the cursor");
    let after = env.cursors()[&key];
    assert_eq!((after.offset, after.line), (fs::metadata(&path).unwrap().len(), 3));
}

#[test]
fn half_written_line_yields_exactly_one_event_once_finished() {
    let env = Env::new(&[], &[]);
    env.scan(); // the history backfill
    let typed = ledger_lines(&env.data).len();
    let lines = fixture_lines("slash.jsonl");
    let (head, tail) = lines[1].split_at(lines[1].len() / 2);
    env.put("slash.jsonl", format!("{}\n{head}", lines[0]).as_bytes());
    assert_eq!(new_events(&env.scan()), 0);
    let key = env.transcript("slash.jsonl").to_string_lossy().into_owned();
    assert_eq!(env.cursors()[&key].offset, lines[0].len() as u64 + 1, "cursor stops before the half line");
    env.append("slash.jsonl", format!("{tail}\n").as_bytes());
    assert_eq!(new_events(&env.scan()), 1);
    assert_eq!(new_events(&env.scan()), 0);
    let all = ledger_lines(&env.data);
    assert_eq!(all.len(), typed + 1);
    assert!(all[typed].ends_with(&format!("\"src\":\"{key}:2\"}}")));
}

#[test]
fn replaced_file_with_a_new_inode_adds_no_duplicates() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let before = ledger_lines(&env.data);
    let path = env.transcript("pair.jsonl");
    let key = path.to_string_lossy().into_owned();
    let old = env.cursors()[&key];
    let tmp = path.with_extension("tmp");
    fs::copy(&path, &tmp).unwrap();
    fs::rename(&tmp, &path).unwrap();
    assert_eq!(new_events(&env.scan()), 0);
    assert_eq!(ledger_lines(&env.data), before);
    let new = env.cursors()[&key];
    assert_ne!(new.ino, old.ino);
    assert_eq!((new.offset, new.line), (old.offset, old.line));
}

#[test]
fn deleted_transcript_events_survive_scan_and_reparse_byte_identical() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let all = ids(&env.data);
    let pair = line_of(&env.data, PAIR_ID).unwrap();
    let slash = ledger_lines(&env.data).into_iter().filter(|l| l.contains("slash.jsonl:")).collect::<Vec<_>>();
    assert_eq!(slash.len(), 1);
    fs::remove_file(env.transcript("pair.jsonl")).unwrap();
    fs::remove_file(env.transcript("pair_copy.jsonl")).unwrap();
    fs::remove_file(env.transcript("slash.jsonl")).unwrap();
    env.scan();
    assert_eq!(ids(&env.data), all);
    env.reparse();
    assert_eq!(ids(&env.data), all, "no events lost");
    assert_eq!(line_of(&env.data, PAIR_ID).unwrap(), pair);
    assert!(ledger_lines(&env.data).contains(&slash[0]));
}

/// A Claude fork keeps the record's id under another file. When the file that
/// stored the event is deleted, reparse still keeps the stored event, because
/// the copied events come first and the parse appends under the append rule.
#[test]
fn reparse_keeps_the_stored_event_when_a_surviving_file_shares_its_id() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let stored = line_of(&env.data, PAIR_ID).unwrap();
    assert!(stored.contains("/pair.jsonl:1"), "pair.jsonl sorts before pair_copy.jsonl: {stored}");
    fs::remove_file(env.transcript("pair.jsonl")).unwrap();
    env.reparse();
    assert_eq!(line_of(&env.data, PAIR_ID).unwrap(), stored);
    let lines = ledger_lines(&env.data);
    let unique: HashSet<String> = lines.iter().map(|l| serde_json::from_str::<Event>(l).unwrap().id).collect();
    assert_eq!(unique.len(), lines.len(), "reparse writes one line per id");
}

/// The Claude adapter with a rule change applied to its output.
struct Rule<F: Fn(&Path, &Chunk, &mut Parsed)>(F);

impl<F: Fn(&Path, &Chunk, &mut Parsed)> Adapter for Rule<F> {
    fn files(&self, home: &Home) -> Vec<PathBuf> {
        Claude.files(home)
    }
    fn parse(&self, ctx: &ParseCtx, path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed {
        let mut parsed = Claude.parse(ctx, path, header, chunk);
        (self.0)(path, chunk, &mut parsed);
        parsed
    }
}

#[test]
fn reparse_drops_a_new_false_positive_and_keeps_deleted_transcripts_verbatim() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let target = "cc:toolu_01TJBdbiPJWDEp5ZGWUmM35V"; // result_only.jsonl
    assert!(ids(&env.data).contains(target));
    let deleted: Vec<String> = ledger_lines(&env.data).into_iter().filter(|l| l.contains("/subagents/")).collect();
    assert_eq!(deleted.len(), 3);
    fs::remove_dir_all(env.transcript("29b0f25b-ce3a-4cf5-8081-e6bb2a2ff99d")).unwrap();
    fs::remove_dir_all(env.transcript("3ea64990-464f-4f45-a9d0-91a25bf5a530")).unwrap();
    fs::remove_dir_all(env.transcript("2b0a61b3-e8ed-44c0-81a3-6e94d229e0e4")).unwrap();

    // The new rule: a Skill result without a meta record is a false positive.
    let rule = Rule(|_: &Path, _: &Chunk, p: &mut Parsed| p.raws.retain(|r| r.skill != "workflow-authoring"));
    let before = ids(&env.data);
    env.scan_rule(false, &rule);
    assert_eq!(ids(&env.data), before, "an incremental scan never removes events");
    env.scan_rule(true, &rule);
    let after = ids(&env.data);
    assert!(!after.contains(target), "the false positive is gone");
    let mut expected = before.clone();
    expected.remove(target);
    assert_eq!(after, expected);
    let lines = ledger_lines(&env.data);
    for l in &deleted {
        assert!(lines.contains(l), "deleted transcript event not byte-identical: {l}");
    }
}

#[test]
fn reparse_adds_an_event_from_a_transcript_that_had_none() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let file = env.transcript("attachments.jsonl").to_string_lossy().into_owned();
    assert!(events(&env.data).iter().all(|e| e.src_file() != file));

    // The new rule: each skill of an `invoked_skills` attachment is a slash load.
    let rule = Rule(|path: &Path, chunk: &Chunk, p: &mut Parsed| {
        for (line, bytes) in &chunk.lines {
            let v: Value = serde_json::from_slice(bytes).unwrap();
            if v["attachment"]["type"] != "invoked_skills" {
                continue;
            }
            let s = |k: &str| v[k].as_str().map(str::to_string);
            let session = Session {
                id: s("sessionId").unwrap(),
                parent: None,
                actor: ActorInputs { entrypoint: s("entrypoint"), subagent: false, codex_exec: false },
                cwd: s("cwd"),
                version: s("version"),
            };
            assert!(path.ends_with("attachments.jsonl"));
            for (i, skill) in v["attachment"]["skills"].as_array().unwrap().iter().enumerate() {
                p.raws.push(Raw {
                    key: Key::Plain(format!("cc:{}:{i}", s("uuid").unwrap())),
                    ts: s("timestamp").unwrap(),
                    skill: skill["name"].as_str().unwrap().into(),
                    channel: Channel::Slash,
                    path: None,
                    cwd: session.cwd.clone(),
                    line: *line,
                    session: session.clone(),
                    name_origin: true,
                });
            }
        }
    });
    let before = ids(&env.data);
    assert_eq!(new_events(&env.scan_rule(false, &rule)), 0, "cursors sit past the record");
    env.scan_rule(true, &rule);
    let after = ids(&env.data);
    let added: Vec<_> = after.difference(&before).collect();
    let uuid = "cc:1420618c-9320-4d2e-9da7-cfa5c0b1005b";
    assert_eq!(added, [format!("{uuid}:0"), format!("{uuid}:1"), format!("{uuid}:2")].iter().collect::<Vec<_>>());
    assert!(before.is_subset(&after));
}

#[test]
fn reparse_leaves_cursors_before_a_half_written_last_line() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let lines = fixture_lines("slash.jsonl");
    let record = lines[1].replace("d9ad44a4-3e61-4169-83e1-8e3b5655f276", "11111111-3e61-4169-83e1-8e3b5655f276");
    let (head, tail) = record.split_at(record.len() / 3);
    env.append("slash.jsonl", head.as_bytes());
    let key = env.transcript("slash.jsonl").to_string_lossy().into_owned();
    let complete = fs::read(fixture("slash.jsonl")).unwrap().len() as u64;
    env.reparse();
    assert_eq!(env.cursors()[&key].offset, complete);
    assert_eq!(env.cursors()[&key].line, 2);
    env.append("slash.jsonl", format!("{tail}\n").as_bytes());
    assert_eq!(new_events(&env.scan()), 1, "the finished line is read, not skipped");
    assert!(ids(&env.data).contains("cc:11111111-3e61-4169-83e1-8e3b5655f276"));
}

#[test]
fn concurrent_scan_processes_write_no_duplicate_ids() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    let root = env.tmp.path();
    let config = root.join("dotfiles/skills/skima.toml");
    let home = &env.cfg.homes[0].path;
    fs::write(
        &config,
        format!("source = \".\"\n\n[[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"{}\"\n", home.display()),
    )
    .unwrap();
    for round in 0..8 {
        let xdg = root.join(format!("xdg{round}"));
        let run = || {
            Command::new(env!("CARGO_BIN_EXE_skima"))
                .arg("scan")
                .env("HOME", root)
                .env("XDG_DATA_HOME", &xdg)
                .env("SKIMA_CONFIG", &config)
                .spawn()
                .unwrap()
        };
        let (mut a, mut b) = (run(), run());
        assert!(a.wait().unwrap().success() && b.wait().unwrap().success());
        let data = xdg.join("skima");
        let lines = ledger_lines(&data);
        let unique: HashSet<String> = lines.iter().map(|l| serde_json::from_str::<Event>(l).unwrap().id).collect();
        assert!(!lines.is_empty());
        assert_eq!(unique.len(), lines.len(), "round {round}: duplicate ids");
    }
}

/// Every event but `typed`, sorted by id (F1).
fn comparable(data: &Path) -> Vec<Event> {
    events(data).into_iter().filter(|e| e.channel != Channel::Typed).collect()
}

#[test]
fn incremental_ledger_equals_a_fresh_scan() {
    let env = Env::new(&[], &[]);
    let files: Vec<(&str, Vec<String>)> = TRANSCRIPTS.iter().map(|(d, f)| (*d, fixture_lines(f))).collect();
    for (f, _) in &files {
        env.put(f, b"");
    }
    // Append the records round-robin, each in two halves, scanning after every write.
    let longest = files.iter().map(|(_, l)| l.len()).max().unwrap();
    for i in 0..longest {
        for (f, lines) in &files {
            let Some(line) = lines.get(i) else { continue };
            let (a, b) = line.split_at(line.len() / 2);
            env.append(f, a.as_bytes());
            env.scan();
            env.append(f, format!("{b}\n").as_bytes());
            env.scan();
        }
    }
    let fresh = env.tmp.path().join("fresh");
    env.scan_in(&fresh, false);
    for (dest, src) in TRANSCRIPTS {
        assert_eq!(fs::read(env.transcript(dest)).unwrap(), fs::read(fixture(src)).unwrap(), "{dest} rebuilt");
    }
    assert_eq!(comparable(&env.data), comparable(&fresh));
    assert!(!comparable(&fresh).is_empty());
}

#[test]
fn skill_pair_split_across_scans_ends_equal_to_a_fresh_scan() {
    let env = Env::new(&[], &[]);
    let lines = fixture_lines("pair.jsonl");
    env.put("pair.jsonl", format!("{}\n", lines[0]).as_bytes());
    env.scan();
    let first = Ledger::load(&env.data).unwrap().get(PAIR_ID).unwrap().clone();
    assert_eq!(first.path, None);
    env.append("pair.jsonl", format!("{}\n", lines[1]).as_bytes());
    env.scan();
    let fresh = env.tmp.path().join("fresh");
    env.scan_in(&fresh, false);
    let incremental = Ledger::load(&env.data).unwrap().get(PAIR_ID).unwrap().clone();
    assert_eq!(&incremental, Ledger::load(&fresh).unwrap().get(PAIR_ID).unwrap());
    assert_eq!(incremental.path.as_deref(), Some("/home/nicolas/.claude/skills/unslop/SKILL.md"));
    assert_eq!(ledger_lines(&env.data).iter().filter(|l| l.contains(PAIR_ID)).count(), 2, "one upgrade line, once");
    assert_eq!(new_events(&env.scan()), 0);
}

/// The earliest `timestamp` of any record in the fixture transcripts.
fn earliest() -> String {
    TRANSCRIPTS
        .iter()
        .flat_map(|(_, f)| fixture_lines(f))
        .filter_map(|l| serde_json::from_str::<Value>(&l).unwrap()["timestamp"].as_str().map(str::to_string))
        .min()
        .unwrap()
}

#[test]
fn coverage_records_first_and_advances_last_scan_only_without_io_errors() {
    let env = Env::new(&TRANSCRIPTS, &["empty"]);
    env.scan();
    let c = env.coverage();
    assert_eq!(c.keys().collect::<Vec<_>>(), ["claude", "empty"]);
    assert_eq!(c["claude"].first.as_deref(), Some(earliest().as_str()));
    assert_eq!(c["empty"].first, None);
    assert!(c["claude"].last_scan.is_some() && c["empty"].last_scan.is_some());

    let path = env.transcript("bench.jsonl");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&path).is_ok() {
        return; // running as root: permissions cannot cause an I/O error
    }
    let r = env.scan();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(r.homes["claude"].io_errors.len(), 1);
    let d = env.coverage();
    assert_eq!(d["claude"], c["claude"], "a home with an I/O error keeps its last_scan");
    assert!(d["empty"].last_scan > c["empty"].last_scan);
    env.scan();
    let e = env.coverage();
    assert!(e["claude"].last_scan > c["claude"].last_scan);
    assert_eq!(e["claude"].first, c["claude"].first, "first never moves");
}

#[test]
fn deleting_transcripts_never_removes_events() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let all = ids(&env.data);
    assert!(all.iter().any(|i| i.starts_with("cch:")), "history backfill ran");
    fs::remove_dir_all(env.cfg.homes[0].path.join("projects")).unwrap();
    env.scan();
    assert_eq!(ids(&env.data), all);
    env.reparse();
    assert_eq!(ids(&env.data), all);
}

#[test]
fn reparse_into_an_empty_data_dir_equals_a_first_scan() {
    let env = Env::new(&TRANSCRIPTS, &[]);
    env.scan();
    let other = env.tmp.path().join("reparsed");
    env.scan_in(&other, true);
    assert_eq!(events(&other), events(&env.data), "typed events included");
}

#[test]
fn a_torn_ledger_tail_does_not_swallow_the_next_append() {
    let env = Env::new(&[("slash.jsonl", "slash.jsonl")], &[]);
    fs::create_dir_all(&env.data).unwrap();
    fs::write(env.data.join("events.jsonl"), "{\"v\":1,\"id\"").unwrap();
    env.scan();
    let fresh = env.tmp.path().join("fresh");
    env.scan_in(&fresh, false);
    assert_eq!(ids(&env.data), ids(&fresh));
}
