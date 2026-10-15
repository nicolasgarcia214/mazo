//! SPEC §10.1 Codex fixtures and the §10.5 golden id. Each fixture is a set of
//! real rollout lines with their text scrubbed; `pipeline.jsonl` line 3,
//! `guardian.jsonl` line 2 and the `compacted` record in `excluded.jsonl` are
//! composed from real records, as each test notes.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use skima::adapters::codex::{self, Codex};
use skima::adapters::{Adapter, Key, Parent, Parsed, Raw, SkillRoots};
use skima::classify::Roots;
use skima::config::{Config, Home};
use skima::inventory::Inventory;
use skima::ledger::{Actor, Agent, Channel, Event, Ledger};
use skima::tail;

const SOURCE: &str = "/home/nicolas/dotfiles/skills";
const CODEX_SKILLS: &str = "/home/nicolas/.codex/skills";

fn fixtures() -> PathBuf {
    common::fixture("codex")
}

fn roots() -> SkillRoots {
    SkillRoots {
        source: SOURCE.into(),
        home_skills: vec![CODEX_SKILLS.into(), "/home/nicolas/.claude/skills".into()],
        known: ["resolve-issue", "triage-finding", "unslop", "writing-for-agents"].map(String::from).into(),
    }
}

fn home(path: &Path) -> Home {
    common::home("codex", Agent::Codex, path)
}

/// Parses a whole file from offset 0 with the scan's prefilter.
fn parse_path(path: &Path) -> Parsed {
    common::parse_file(&Codex, &roots(), path, None).0
}

fn parse(name: &str) -> Parsed {
    parse_path(&fixtures().join(name))
}

fn parse_text(text: &str) -> Parsed {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("rollout.jsonl");
    std::fs::write(&p, text).unwrap();
    parse_path(&p)
}

fn key(r: &Raw) -> (&str, &str) {
    match &r.key {
        Key::Codex { hash, index } => (hash, index),
        Key::Plain(_) => panic!("codex keys are Key::Codex"),
    }
}

/// The stored id, `cx:<root>:<hash>:<index>`, with the root walked over `sessions`.
fn full_id(home_dir: &Path, r: &Raw) -> String {
    let root = Roots::default().root(&home(home_dir), &r.session.id, r.session.parent.as_ref());
    let (hash, index) = key(r);
    format!("cx:{root}:{hash}:{index}")
}

fn summary(p: &Parsed) -> Vec<(u64, &str, Channel, &str)> {
    p.raws.iter().map(|r| (r.line, r.skill.as_str(), r.channel, r.path.as_deref().unwrap())).collect()
}

#[test]
fn name_block_without_payload_id_is_one_slash_event() {
    // CLI 0.145.0: the developer listing (line 3), the typed `$triage-finding`
    // (line 4) and its event_msg (line 5) are not events; line 2 is the
    // parent's copied header.
    let p = parse("name_block.jsonl");
    assert_eq!(
        summary(&p),
        [(6, "triage-finding", Channel::Slash, "/home/nicolas/dotfiles/skills/triage-finding/SKILL.md")]
    );
    let r = &p.raws[0];
    assert_eq!(key(r).1, "0");
    assert_eq!(r.ts, "2026-07-27T18:19:08.220Z");
    assert_eq!(r.cwd, None);
    assert!(!r.name_origin);
    assert_eq!(r.session.id, "019fa4cd-55e5-74a0-8ca6-f85824b9f349");
    assert_eq!(r.session.parent, Some(Parent::Codex("019fa4cb-582b-7fc0-93d5-4f0b3122ae42".into())));
    assert_eq!(r.session.version.as_deref(), Some("0.145.0"));
    assert!(p.unparsed.is_empty() && !p.skipped);
}

#[test]
fn exec_wrapper_with_two_exec_command_reads_gives_two_ids() {
    let p = parse("exec_two_reads.jsonl");
    assert_eq!(
        summary(&p),
        [
            (2, "writing-for-agents", Channel::Read, "/home/nicolas/.codex/skills/writing-for-agents/SKILL.md"),
            (2, "unslop", Channel::Read, "/home/nicolas/.codex/skills/unslop/SKILL.md"),
        ]
    );
    let (a, b) = (key(&p.raws[0]), key(&p.raws[1]));
    assert_eq!((a.1, b.1), ("0.0", "1.0"));
    assert_eq!(a.0, b.0, "one record, one payload hash");
    assert!(p.raws.iter().all(|r| r.cwd.as_deref() == Some("/home/nicolas/projects/demo")));
}

#[test]
fn subagent_rollout_loads_root_to_parent_with_subagent_actor() {
    let p = parse("subagent.jsonl");
    assert_eq!(summary(&p), [(2, "unslop", Channel::Read, "/home/nicolas/.codex/skills/unslop/SKILL.md")]);
    let s = &p.raws[0].session;
    assert!(s.actor.subagent);
    assert!(s.actor.codex_exec, "originator codex_exec; the actor rules rank subagent first");
    assert_eq!(s.cwd.as_deref(), Some("/home/nicolas/projects/demo/src"));
    let parent = "01a05e9c-0184-7800-80d2-6cda4a52db01";
    assert_eq!(s.parent, Some(Parent::Codex(parent.into())));
    assert!(full_id(Path::new("/nonexistent"), &p.raws[0]).starts_with(&format!("cx:{parent}:")));
}

#[test]
fn fork_header_on_line_one_wins_and_restamped_copies_collapse() {
    let home_dir = fixtures().join("fork");
    let day = home_dir.join("sessions/2026/03/30");
    let parent = parse_path(&day.join("rollout-2026-03-30T11-42-17-019d3f9f-e9d9-7c50-a727-71c34e2255b3.jsonl"));
    let child = parse_path(&day.join("rollout-2026-03-30T11-56-58-019d3fad-5b36-7610-81ab-fc1318eb0cce.jsonl"));
    let (p, c) = (&parent.raws[0], &child.raws[0]);
    assert_eq!((parent.raws.len(), child.raws.len()), (1, 1));
    // Line 2 of the child is the parent's session_meta; line 1 decides.
    assert_eq!(c.session.id, "019d3fad-5b36-7610-81ab-fc1318eb0cce");
    assert_eq!(c.session.parent, Some(Parent::Codex(p.session.id.clone())));
    assert_eq!(
        codex::parent_of_header(
            &tail::first_line(&day.join("rollout-2026-03-30T11-56-58-019d3fad-5b36-7610-81ab-fc1318eb0cce.jsonl"))
                .unwrap()
                .unwrap()
        )
        .as_deref(),
        Some("019d3f9f-e9d9-7c50-a727-71c34e2255b3")
    );
    // A relative `sed -n '1,260p' SKILL.md` run inside the skill.
    assert_eq!(summary(&child), [(4, "argus", Channel::Read, "/home/nicolas/dotfiles/skills/argus/SKILL.md")]);
    assert_ne!(p.ts, c.ts, "the child re-stamps the copy");
    let id = full_id(&home_dir, p);
    assert!(id.starts_with("cx:019d3f9f-e9d9-7c50-a727-71c34e2255b3:"));
    assert_eq!(id, full_id(&home_dir, c));
}

const FORK_PARENT: &str = "rollout-2026-03-30T11-42-17-019d3f9f-e9d9-7c50-a727-71c34e2255b3.jsonl";
const FORK_CHILD: &str = "rollout-2026-03-30T11-56-58-019d3fad-5b36-7610-81ab-fc1318eb0cce.jsonl";

/// A temporary machine for the real fork pair: a source holding `argus` (the
/// fixture's dotfiles paths rewritten to it) and one Codex home whose sessions
/// day directory starts empty.
struct ForkMachine {
    tmp: tempfile::TempDir,
    cfg: Config,
    inv: Inventory,
    day: PathBuf,
    /// The fork pair's lines with the dotfiles paths rewritten, by file name.
    lines: BTreeMap<&'static str, Vec<String>>,
}

impl ForkMachine {
    fn new() -> ForkMachine {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("dotfiles/skills");
        common::write(&source.join("argus/SKILL.md"), "x");
        let source = std::fs::canonicalize(source).unwrap();
        let codex_home = tmp.path().join("codex");
        let day = codex_home.join("sessions/2026/03/30");
        std::fs::create_dir_all(&day).unwrap();
        let fixture_day = fixtures().join("fork/sessions/2026/03/30");
        let lines = [FORK_PARENT, FORK_CHILD]
            .map(|f| {
                let text = std::fs::read_to_string(fixture_day.join(f)).unwrap();
                (f, text.replace(SOURCE, source.to_str().unwrap()).lines().map(String::from).collect())
            })
            .into();
        let skills: BTreeSet<String> = ["argus".to_string()].into();
        let inv = Inventory { source: source.clone(), known: skills.clone(), skills, worktree: None, history: None };
        let cfg = Config { source, loose: vec![], homes: vec![home(&codex_home)], policy: Default::default() };
        ForkMachine { tmp, cfg, inv, day, lines }
    }

    /// Appends lines `range` of fixture `file` to its rollout.
    fn append(&self, file: &str, range: std::ops::Range<usize>) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.day.join(file)).unwrap();
        for l in &self.lines[file][range] {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn scan(&self, data: &Path) -> Vec<Event> {
        skima::scan::scan(&self.cfg, &self.inv, data, false).unwrap();
        Ledger::load(data).unwrap().events().to_vec()
    }
}

#[test]
fn fork_pair_scanned_fresh_or_incrementally_stores_the_parent_copy() {
    let m = ForkMachine::new();
    let incremental = m.tmp.path().join("data-incremental");
    // The parent exists alone; the fork appears later and grows a line at a time.
    m.append(FORK_PARENT, 0..2);
    assert_eq!(m.scan(&incremental).len(), 1);
    m.append(FORK_CHILD, 0..3);
    m.scan(&incremental);
    m.append(FORK_CHILD, 3..4);
    let incremental = m.scan(&incremental);

    let fresh = m.scan(&m.tmp.path().join("data-fresh"));
    assert_eq!(fresh, incremental);
    assert_eq!(fresh.len(), 1, "the child's re-stamped copy collapses into the parent's id");
    let e = &fresh[0];
    assert!(e.id.starts_with("cx:019d3f9f-e9d9-7c50-a727-71c34e2255b3:"), "{}", e.id);
    assert_eq!(e.session, "019d3f9f-e9d9-7c50-a727-71c34e2255b3");
    assert_eq!(e.actor, Actor::Main);
    assert_eq!(e.src, format!("{}:2", m.day.join(FORK_PARENT).display()));
    assert_eq!(e.ts, "2026-03-30T16:42:36.643Z", "the parent's timestamp, not the child's re-stamp");
}

#[test]
fn split_sed_chunks_read_one_file_under_two_ids() {
    let p = parse("split_sed.jsonl");
    let path = "/home/nicolas/.codex/skills/resolve-issue/SKILL.md";
    assert_eq!(summary(&p), [(2, "resolve-issue", Channel::Read, path), (3, "resolve-issue", Channel::Read, path)]);
    assert_ne!(p.raws[0].key, p.raws[1].key);
}

#[test]
fn relative_read_resolves_against_workdir() {
    let p = parse("relative_read.jsonl");
    assert_eq!(
        summary(&p),
        [(2, "resolve-issue", Channel::Read, "/home/nicolas/.codex/skills/resolve-issue/SKILL.md")]
    );
    assert_eq!(p.raws[0].cwd.as_deref(), Some("/home/nicolas/.codex/skills/resolve-issue"));
}

#[test]
fn reader_first_pipeline_loads_only_through_pass_through_stages() {
    // Line 2 is real (`| nl -ba -v150`); line 3 is the same call with `| wc -l`,
    // because the corpus has no real reader piped into `wc` on a skill file.
    let p = parse("pipeline.jsonl");
    assert_eq!(summary(&p), [(2, "audit", Channel::Read, "/home/nicolas/projects/demo/skills/audit/SKILL.md")]);
    assert!(p.unparsed.is_empty());
}

#[test]
fn rg_over_skills_is_no_event() {
    let p = parse("rg.jsonl");
    assert!(p.raws.is_empty() && p.unparsed.is_empty());
}

#[test]
fn guardian_review_session_is_skipped() {
    // Line 2 is the relative-read call of `relative_read.jsonl`, an event anywhere else.
    let p = parse("guardian.jsonl");
    assert!(p.skipped);
    assert!(p.raws.is_empty() && p.unparsed.is_empty());
}

#[test]
fn path_echoed_in_output_record_is_no_event() {
    let p = parse("output_echo.jsonl");
    assert!(p.raws.is_empty() && p.unparsed.is_empty());
}

#[test]
fn duplicates_outputs_and_compacted_history_are_excluded() {
    // Line 2 is the exec read; line 3 its event_msg item_completed duplicate,
    // line 4 its *_output, line 5 a `compacted` record (from another rollout)
    // whose replacement_history holds the `$name` item of `name_block.jsonl`.
    let p = parse("excluded.jsonl");
    assert_eq!(summary(&p), [(2, "vbc-noimpl", Channel::Read, "/tmp/probe/.agents/skills/vbc-noimpl/SKILL.md")]);
    assert_eq!(key(&p.raws[0]).1, "0.0");
}

#[test]
fn unreadable_skill_commands_are_counted_unparsed() {
    let header = r#"{"timestamp":"t0","type":"session_meta","payload":{"id":"s1","cwd":"/w","originator":"codex-tui","cli_version":"0.1","source":"cli"}}"#;
    let shell = r#"{"timestamp":"t1","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"sed -n '1,9p' $HOME/.codex/skills/x/SKILL.md\"}","call_id":"c1"}}"#;
    let template = r#"{"timestamp":"t2","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c2","input":"for (const p of [\"/s/a/SKILL.md\"]) await tools.exec_command({cmd: `cat ${p}`});"}}"#;
    let quiet = r#"{"timestamp":"t3","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"git log -- skills/x/SKILL.md\"}","call_id":"c3"}}"#;
    let p = parse_text(&format!("{header}\n{shell}\n{template}\n{quiet}\n"));
    assert!(p.raws.is_empty());
    assert_eq!(p.unparsed.len(), 2);
    assert!(p.unparsed[0].contains("$HOME") && p.unparsed[1].contains("${p}"));
}

/// SPEC §10.5. The hash was computed once, outside skima, with
/// `hashlib.sha1(json.dumps(payload, sort_keys=True, separators=(",", ":"),
/// ensure_ascii=False).encode()).hexdigest()` in Python 3; the serialisations
/// agree on this payload (shortest round-trip floats, integers above 2^53
/// exact, code-point key order equal to UTF-8 byte order). `create_time` is a
/// real Codex value that serde_json misparses without its
/// `float_roundtrip` feature, so the id pins that feature.
#[test]
fn golden_codex_id() {
    let header = r#"{"timestamp":"t0","type":"session_meta","payload":{"id":"golden-session","cwd":"/w","originator":"codex-tui","cli_version":"0.1","source":"cli"}}"#;
    let payload = r#"{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cat /s/golden/SKILL.md\"}","call_id":"call_golden","zeta":{"b":[1,2.5,{"y":true,"x":null}],"a":"ü"},"ärger":"naïve ☃","Alpha":9007199254740993,"pi":3.14159,"neg":-0.5,"create_time":1791311493.9632053,"esc":"line\nquote\"tab\t/"}"#;
    let p = parse_text(&format!(
        "{header}\n{{\"timestamp\":\"2026-01-01T00:00:00Z\",\"type\":\"response_item\",\"payload\":{payload}}}\n"
    ));
    assert_eq!(p.raws.len(), 1);
    assert_eq!(
        full_id(Path::new("/nonexistent"), &p.raws[0]),
        "cx:golden-session:bd6143dc8f7e968e6badbfeceaadc6b06becd08d:0.0"
    );
    // The timestamp is not part of the id.
    let later = parse_text(&format!(
        "{header}\n{{\"timestamp\":\"2027-01-01T00:00:00Z\",\"type\":\"response_item\",\"payload\":{payload}}}\n"
    ));
    assert_eq!(later.raws[0].key, p.raws[0].key);
}

/// `prompt_at` on line `line` of a fixture (SPEC §4.7).
fn prompt(name: &str, line: u64) -> Option<String> {
    Codex.prompt_at(&format!("{}:{line}", fixtures().join(name).display()))
}

#[test]
fn name_block_prompt_is_the_user_turn() {
    // Line 6 is the `<skill>` injection, line 5 the `user_message` event.
    assert_eq!(prompt("name_block.jsonl", 6).as_deref(), Some("$triage-finding scrubbed"));
    // From 2026-08 the turn is an `item_completed` `UserMessage` (line 3).
    assert_eq!(prompt("prompt_item.jsonl", 4).as_deref(), Some("Use $vbc-noimpl now."));
}

#[test]
fn bare_name_prompt_is_the_turn_before_it() {
    // Line 5 is a bare `$gitmoji-conventional-commit`.
    assert_eq!(prompt("prompt_bare.jsonl", 6).as_deref(), Some("Validate the scrubbed solution with an agent."));
}
