//! SPEC §10.1 Claude fixtures. Each fixture is a real record from this machine,
//! trimmed to the fields the adapter reads, with prompt and tool text scrubbed.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use skima::adapters::claude::{self, Claude};
use skima::adapters::{ActorInputs, Adapter, Key, ParseCtx, Parsed, Raw, Session, SkillRoots};
use skima::classify::{Classifier, Roots};
use skima::config::{Config, Home};
use skima::inventory::Inventory;
use skima::ledger::{Actor, Agent, Channel, Ledger, Origin};
use skima::tail::{self, Cursor, Prefilter};

const HOME_SKILLS: &str = "/home/nicolas/.claude/skills";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude").join(name)
}

fn home(path: &Path) -> Home {
    Home { name: "claude".into(), agent: Agent::Claude, path: path.to_path_buf(), links: Default::default() }
}

fn roots() -> SkillRoots {
    SkillRoots {
        source: "/nonexistent/dotfiles/skills".into(),
        home_skills: vec![HOME_SKILLS.into()],
        known: BTreeSet::new(),
    }
}

fn filter() -> Prefilter {
    Prefilter::new([HOME_SKILLS])
}

/// Reads `path` through the real prefilter from `cursor` and parses the chunk.
fn parse_from(path: &Path, cursor: Option<&Cursor>) -> (Parsed, Cursor) {
    let chunk = tail::read(path, cursor, &filter()).unwrap();
    let h = home(Path::new("/nonexistent/.claude"));
    let roots = roots();
    let ctx = ParseCtx { home: &h, roots: &roots };
    (Claude.parse(&ctx, path, None, &chunk), chunk.cursor)
}

fn parse(name: &str) -> Vec<Raw> {
    let (p, _) = parse_from(&fixture(name), None);
    assert!(p.unparsed.is_empty(), "{:?}", p.unparsed);
    p.raws
}

fn session(id: &str, entrypoint: &str, subagent: bool, cwd: &str, version: &str) -> Session {
    Session {
        id: id.into(),
        parent: None,
        actor: ActorInputs { entrypoint: Some(entrypoint.into()), subagent, codex_exec: false },
        cwd: Some(cwd.into()),
        version: Some(version.into()),
    }
}

#[allow(clippy::too_many_arguments)]
fn raw(
    key: &str,
    ts: &str,
    skill: &str,
    channel: Channel,
    path: Option<&str>,
    line: u64,
    session: &Session,
    name_origin: bool,
) -> Raw {
    Raw {
        key: Key::Plain(key.into()),
        ts: ts.into(),
        skill: skill.into(),
        channel,
        path: path.map(Into::into),
        cwd: session.cwd.clone(),
        line,
        session: session.clone(),
        name_origin,
    }
}

const ARGUS: &str = "2e112b1e-0233-4375-99fd-cd21d2102ac1";

fn argus() -> Session {
    session(ARGUS, "cli", false, "/home/nicolas/projects/argus", "2.1.272")
}

fn unslop_pair() -> Raw {
    raw(
        "cc:toolu_01EKQ5jSt7RhgFyyw72kyqoY",
        "2026-09-15T06:15:33.104Z",
        "unslop",
        Channel::Tool,
        Some("/home/nicolas/.claude/skills/unslop/SKILL.md"),
        1,
        &argus(),
        true,
    )
}

#[test]
fn result_and_meta_merge_into_one_event_with_the_result_ts_and_the_meta_path() {
    assert_eq!(parse("pair.jsonl"), vec![unslop_pair()]);
}

#[test]
fn meta_with_source_tool_use_id_alone_is_a_tool_event() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.jsonl");
    let meta = std::fs::read_to_string(fixture("pair.jsonl")).unwrap().lines().nth(1).unwrap().to_string();
    std::fs::write(&p, meta + "\n").unwrap();
    let (parsed, _) = parse_from(&p, None);
    assert_eq!(
        parsed.raws,
        vec![raw(
            "cc:toolu_01EKQ5jSt7RhgFyyw72kyqoY",
            "2026-09-15T06:15:33.102Z",
            "unslop",
            Channel::Tool,
            Some("/home/nicolas/.claude/skills/unslop/SKILL.md"),
            1,
            &argus(),
            false,
        )]
    );
}

#[test]
fn slash_meta_without_source_tool_use_id_is_slash_and_the_command_record_is_ignored() {
    let slash = raw(
        "cc:d9ad44a4-3e61-4169-83e1-8e3b5655f276",
        "2026-09-15T16:18:27.431Z",
        "gitmoji-conventional-commit",
        Channel::Slash,
        Some("/home/nicolas/.claude/skills/gitmoji-conventional-commit/SKILL.md"),
        2,
        &argus(),
        false,
    );
    assert_eq!(parse("slash.jsonl"), vec![slash.clone()]);

    // A scan that stops between the `<command-name>` record and the meta record
    // loses nothing: the meta record classifies on its own.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.jsonl");
    let text = std::fs::read_to_string(fixture("slash.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    std::fs::write(&p, format!("{}\n", lines[0])).unwrap();
    let (first, cursor) = parse_from(&p, None);
    assert!(first.raws.is_empty());
    writeln!(std::fs::OpenOptions::new().append(true).open(&p).unwrap(), "{}", lines[1]).unwrap();
    let (second, _) = parse_from(&p, Some(&cursor));
    assert_eq!(second.raws, vec![slash]);
}

#[test]
fn subagent_slash_meta_matches_base_directory_in_block_1() {
    let s = session(
        "2b0a61b3-e8ed-44c0-81a3-6e94d229e0e4",
        "sdk-cli",
        true,
        "/tmp/claude-1000/-home-nicolas-dotfiles/29b0f25b-ce3a-4cf5-8081-e6bb2a2ff99d/scratchpad/claude-probe",
        "2.1.289",
    );
    let dir = "/tmp/claude-1000/-home-nicolas-dotfiles/29b0f25b-ce3a-4cf5-8081-e6bb2a2ff99d/scratchpad/claude-probe/.claude/skills/probe-nameonly";
    assert_eq!(
        parse("subagents/agent-ad81ed4c0f32313c2.jsonl"),
        vec![raw(
            "cc:5585cbb4-402e-4373-98e0-9a6d23f6b9e5",
            "2026-10-06T17:35:18.294Z",
            "probe-nameonly",
            Channel::Slash,
            Some(&format!("{dir}/SKILL.md")),
            1,
            &s,
            false,
        )]
    );
}

#[test]
fn skill_result_without_a_base_directory_meta_has_no_path() {
    // `workflow-authoring` is built in: its meta record carries no `Base directory`.
    let s = session("10da2fa2-c02b-442b-8173-eac8c0c16476", "cli", false, "/home/nicolas/dotfiles", "2.1.294");
    assert_eq!(
        parse("result_only.jsonl"),
        vec![raw(
            "cc:toolu_01TJBdbiPJWDEp5ZGWUmM35V",
            "2026-10-08T23:32:22.212Z",
            "workflow-authoring",
            Channel::Tool,
            None,
            1,
            &s,
            true,
        )]
    );
}

#[test]
fn subagent_meta_whose_result_has_no_tool_use_result_is_one_tool_event() {
    let cwd = "/tmp/claude-1000/-home-nicolas-dotfiles/29b0f25b-ce3a-4cf5-8081-e6bb2a2ff99d/scratchpad/probe";
    let s = session("3ea64990-464f-4f45-a9d0-91a25bf5a530", "sdk-cli", true, cwd, "2.1.289");
    assert_eq!(
        parse("subagents/agent-aacafe9c375ac817e.jsonl"),
        vec![raw(
            "cc:toolu_017VwgJ2qQctZPe8MJqjx5gS",
            "2026-10-05T23:18:27.937Z",
            "probe-named",
            Channel::Tool,
            Some(&format!("{cwd}/.claude/skills/probe-named/SKILL.md")),
            2,
            &s,
            false,
        )]
    );
}

#[test]
fn scan_boundary_between_result_and_meta_upgrades_to_the_fresh_merge() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.jsonl");
    let text = std::fs::read_to_string(fixture("pair.jsonl")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    std::fs::write(&p, format!("{}\n", lines[0])).unwrap();
    let (first, cursor) = parse_from(&p, None);
    assert_eq!(first.raws, vec![Raw { path: None, ..unslop_pair() }]);
    writeln!(std::fs::OpenOptions::new().append(true).open(&p).unwrap(), "{}", lines[1]).unwrap();
    let (second, _) = parse_from(&p, Some(&cursor));
    assert_eq!(second.raws.len(), 1);
    assert_eq!(second.raws[0].key, unslop_pair().key);
    assert_eq!(second.raws[0].line, 2);

    let inv =
        Inventory { skills: ["unslop".to_string()].into(), known: ["unslop".to_string()].into(), ..Default::default() };
    let h = home(dir.path());
    let homes = [h.clone()];
    let classifier = Classifier { inv: &inv, home: &h, homes: &homes };
    let event = |raw: Raw| classifier.event(&mut Roots::default(), raw, &p);
    let mut ledger = Ledger::load(dir.path()).unwrap();
    let stored = ledger.admit(event(first.raws[0].clone())).unwrap();
    let upgrade = ledger.admit(event(second.raws[0].clone())).unwrap();
    assert_eq!(upgrade, skima::ledger::Event { path: unslop_pair().path, ..stored });

    // A fresh scan of the whole file stores the same event.
    let (fresh, _) = parse_from(&p, None);
    assert_eq!(fresh.raws.len(), 1);
    assert_eq!(event(fresh.raws[0].clone()), upgrade);
}

#[test]
fn clear_command_yields_nothing() {
    assert_eq!(parse("clear.jsonl"), vec![]);
}

#[test]
fn markers_quoted_inside_tool_result_yield_nothing() {
    assert_eq!(parse("tool_result_quote.jsonl"), vec![]);
}

#[test]
fn skill_mention_and_invoked_skills_attachments_yield_nothing() {
    assert_eq!(parse("attachments.jsonl"), vec![]);
}

#[test]
fn refused_skill_calls_yield_nothing() {
    // `disable-model-invocation` and a `PreToolUse:Skill` hook refusal.
    assert_eq!(parse("refused.jsonl"), vec![]);
}

#[test]
fn subagent_file_read_carries_the_subagent_input_and_the_parent_session() {
    let s = session("29b0f25b-ce3a-4cf5-8081-e6bb2a2ff99d", "cli", true, "/home/nicolas/dotfiles", "2.1.291");
    assert_eq!(
        parse("subagents/agent-a5b86c28fed127eef.jsonl"),
        vec![raw(
            "cc:850b0039-5f37-4f9a-909f-cf9e7084bf15:0:0",
            "2026-10-06T20:22:49.640Z",
            "gitmoji-conventional-commit",
            Channel::Read,
            Some("/home/nicolas/.claude/skills/gitmoji-conventional-commit/SKILL.md"),
            1,
            &s,
            false,
        )]
    );
}

#[test]
fn bench_and_probe_entrypoints_pass_through_to_a_probe_actor() {
    let bench = parse("bench.jsonl");
    assert_eq!(bench.len(), 1);
    let b = &bench[0];
    // The plugin skill takes its name from `commandName`, its path from the meta record.
    assert_eq!((b.skill.as_str(), b.channel, b.line), ("anthropic-skills:pdf", Channel::Tool, 1));
    assert!(b.path.as_deref().unwrap().ends_with("/pdf/SKILL.md"));
    assert_eq!(b.session.actor.entrypoint.as_deref(), Some("bench"));
    let probe = parse("probe.jsonl");
    assert_eq!(probe.len(), 1);
    assert_eq!(probe[0].session.actor.entrypoint.as_deref(), Some("probe"));
    for r in [b, &probe[0]] {
        assert_eq!(skima::classify::actor(Agent::Claude, &r.session.actor), Actor::Probe);
    }
}

#[test]
fn bash_reading_two_skill_files_yields_two_events_with_distinct_ids() {
    let s = session("301fa4b5-1181-4988-a473-67dffb7eae85", "cli", false, "/home/nicolas/dotfiles/skills", "2.1.292");
    let r = |fi: u32, skill: &str| {
        raw(
            &format!("cc:4651fe5d-8f81-4419-9766-fe5c71b2acbb:0:{fi}"),
            "2026-10-07T16:14:44.766Z",
            skill,
            Channel::Read,
            Some(&format!("/home/nicolas/dotfiles/skills/{skill}/SKILL.md")),
            1,
            &s,
            false,
        )
    };
    // Two chunks of one file collapse; the second file gets its own index.
    assert_eq!(parse("bash_two.jsonl"), vec![r(0, "validate-findings-backlog"), r(1, "interrogate")]);
}

#[test]
fn unknown_command_naming_a_skill_file_is_counted_as_unparsed() {
    let (p, _) = parse_from(&fixture("unparsed.jsonl"), None);
    assert!(p.raws.is_empty());
    assert_eq!(p.unparsed, vec!["awk 'NR<5' /home/nicolas/.claude/skills/unslop/references/prose-tells.md"]);
}

const HISTORY_ARGUS_TS: &str = "2026-03-13T15:43:39.625Z";

#[test]
fn history_backfill_emits_typed_only_for_known_names_without_a_transcript() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(fixture("history.jsonl"), dir.path().join("history.jsonl")).unwrap();
    let projects = dir.path().join("projects/-home-nicolas-projects-argus");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(projects.join(format!("{ARGUS}.jsonl")), "").unwrap();
    let known: BTreeSet<String> =
        ["gitmoji-conventional-commit", "grade", "argus", "clear"].iter().map(|s| s.to_string()).collect();
    let typed = |line, sid: &str, ms: u64, ts: &str, skill: &str, cwd: &str| Raw {
        key: Key::Plain(format!("cch:{sid}:{ms}")),
        ts: ts.into(),
        skill: skill.into(),
        channel: Channel::Typed,
        path: None,
        cwd: Some(cwd.into()),
        line,
        session: Session { id: sid.into(), cwd: Some(cwd.into()), ..Session::default() },
        name_origin: true,
    };
    assert_eq!(
        claude::backfill(&home(dir.path()), &known),
        vec![
            // Line 1 (`/gitmoji-conventional-commit`) has a transcript on disk;
            // line 3 (`/argus:triage`) is a plugin command.
            typed(
                2,
                "af75c7ad-55c0-44f0-8836-492993dede34",
                1773764054452,
                "2026-03-17T16:14:14.452Z",
                "grade",
                "/home/nicolas/dotfiles/skills/argus",
            ),
            typed(
                4,
                "df86f445-5e82-45a4-87ad-1246ee26a0ec",
                1773416619625,
                HISTORY_ARGUS_TS,
                "argus",
                "/home/nicolas/bug_bounties_and_contests/folks-staking-contracts",
            ),
            // `/clear ` matches the anchor when `clear` is a known name.
            typed(
                5,
                "9ebf4dd4-f339-44a7-8e12-4ba723625987",
                1771891904403,
                "2026-02-24T00:11:44.403Z",
                "clear",
                "/home/nicolas/projects/ephemera",
            ),
        ]
    );
}

#[test]
fn history_backfill_runs_on_the_first_scan_only_with_typed_origin_from_canonical_names() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("skills");
    std::fs::create_dir_all(source.join("grade")).unwrap();
    std::fs::write(source.join("grade/SKILL.md"), "x").unwrap();
    let claude_home = dir.path().join("claude");
    std::fs::create_dir_all(claude_home.join("projects")).unwrap();
    std::fs::copy(fixture("history.jsonl"), claude_home.join("history.jsonl")).unwrap();
    let data = dir.path().join("data");
    let cfg = Config { source: source.clone(), loose: vec![], homes: vec![home(&claude_home)] };
    // `argus` is a known name from git history but not canonical today (E15).
    let inv = Inventory {
        source: source.clone(),
        skills: ["grade".to_string()].into(),
        known: ["grade".to_string(), "argus".to_string(), "gitmoji-conventional-commit".to_string()].into(),
        worktree: None,
    };
    let first = skima::scan::scan(&cfg, &inv, &data, false).unwrap();
    // No transcript exists here, so line 1 counts too.
    assert_eq!(first.homes["claude"].new_events.len(), 3);
    let ledger = Ledger::load(&data).unwrap();
    let got: Vec<(&str, Origin, &str)> =
        ledger.events().iter().map(|e| (e.skill.as_str(), e.origin, e.ts.as_str())).collect();
    assert_eq!(got[0].0, "gitmoji-conventional-commit");
    assert_eq!(got[0].1, Origin::Other);
    assert_eq!((got[1].0, got[1].1), ("grade", Origin::Canonical));
    assert_eq!(got[2], ("argus", Origin::Other, HISTORY_ARGUS_TS));
    assert!(
        ledger.events().iter().all(|e| e.channel == Channel::Typed
            && e.src.ends_with(".jsonl:1") == (e.skill == "gitmoji-conventional-commit"))
    );

    let mut h = std::fs::OpenOptions::new().append(true).open(claude_home.join("history.jsonl")).unwrap();
    writeln!(h, r#"{{"display":"/grade","pastedContents":{{}},"timestamp":1791921825029,"project":"/x","sessionId":"00000000-0000-0000-0000-000000000000"}}"#).unwrap();
    let second = skima::scan::scan(&cfg, &inv, &data, false).unwrap();
    assert_eq!(second.homes["claude"].new_events.len(), 0);
}
