//! Pi adapter fixtures. Each fixture is real session records,
//! trimmed to the lines that matter, with prose scrubbed.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mazo::adapters::{Adapter, Key, Parent, Parsed, SkillRoots, pi};
use mazo::classify::{self, Roots};
use mazo::config::Home;
use mazo::ledger::{Actor, Agent, Channel};
use mazo::tail;

const PI_SKILLS: &str = "/home/nicolas/.pi/agent/skills";
const SOURCE: &str = "/home/nicolas/dotfiles/skills";

fn fixture(name: &str) -> PathBuf {
    common::fixture("pi").join(name)
}

fn home(path: &Path) -> Home {
    common::home("pi", Agent::Pi, path)
}

fn roots() -> SkillRoots {
    let known = ["blast-radius", "goal-investigate", "goal-smart", "grade", "unslop", "writing-for-agents"];
    SkillRoots {
        source: Some(SOURCE.into()),
        home_skills: vec![PI_SKILLS.into()],
        known: known.iter().map(|s| s.to_string()).collect(),
    }
}

fn parse(path: &Path) -> Parsed {
    common::parse_file(&pi::Pi, &roots(), path, None).0
}

/// `(key, skill, channel, path, line)` of every raw event.
fn events(p: &Parsed) -> Vec<(String, String, Channel, String, u64)> {
    p.raws
        .iter()
        .map(|r| {
            let Key::Plain(k) = &r.key else { panic!("pi keys are plain") };
            (k.clone(), r.skill.clone(), r.channel, r.path.clone().unwrap(), r.line)
        })
        .collect()
}

fn ev(key: &str, skill: &str, channel: Channel, path: &str, line: u64) -> (String, String, Channel, String, u64) {
    (key.into(), skill.into(), channel, path.into(), line)
}

#[test]
fn skill_block_is_a_slash_load() {
    let p = parse(&fixture("slash.jsonl"));
    assert_eq!(
        events(&p),
        [ev(
            "pi:d7e9796d@2026-09-11T03:25:29.274Z",
            "goal-smart",
            Channel::Slash,
            "/home/nicolas/.pi/agent/skills/goal-smart/SKILL.md",
            2
        )]
    );
    let r = &p.raws[0];
    assert_eq!(r.ts, "2026-09-11T03:25:29.274Z");
    assert_eq!(r.session.id, "01a08e7c-3bde-7380-9061-4c05706c4214");
    assert_eq!(r.session.cwd.as_deref(), Some("/home/nicolas/dotfiles"));
    assert_eq!((r.session.parent.as_ref(), r.session.actor.subagent), (None, false));
    assert_eq!(r.session.version, None);
    assert!(!r.name_origin);
    assert!(!p.skipped && p.unparsed.is_empty());
}

#[test]
fn placeholder_block_in_a_tool_result_is_no_event() {
    let p = parse(&fixture("placeholder.jsonl"));
    assert!(p.raws.is_empty() && p.unparsed.is_empty() && !p.skipped);
}

#[test]
fn placeholders_fail_the_location_check_even_in_a_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("s.jsonl");
    let header = r#"{"type":"session","version":3,"id":"s","timestamp":"t","cwd":"/w"}"#;
    let user = |text: &str| {
        let msg = serde_json::json!({"type":"message","id":"e","parentId":null,"timestamp":"t",
            "message":{"role":"user","content":[{"type":"text","text":text}]}});
        msg.to_string()
    };
    let lines = [
        user(r#"<skill name="..." location="...">"#),
        user(r#"<skill name="{name}" location="{location}">"#),
        user(r#"<skill name="${skill.name}" location="${skill.filePath}">"#),
        user(r#"<skill name="x" location="x/SKILL.md">"#),
        user(r#"see <skill name="x" location="/s/x/SKILL.md">"#),
    ];
    std::fs::write(&f, format!("{header}\n{}\n", lines.join("\n"))).unwrap();
    assert!(parse(&f).raws.is_empty());
}

#[test]
fn read_tool_calls_resolve_against_the_header_cwd() {
    let p = parse(&fixture("read.jsonl"));
    let key = |i: u32| format!("pi:5f2d97ca@2026-09-11T03:25:57.990Z:{i}:0");
    let read = |i, skill| ev(&key(i), skill, Channel::Read, &format!("{SOURCE}/{skill}/SKILL.md"), 2);
    // Items 0-1 are thinking/text, 2 `publish.sh`, 3 the loose `skills/SKILLS_USAGE.md`,
    // 8 a bash call, 9 a doc outside every skill root.
    assert_eq!(
        events(&p),
        [read(4, "unslop"), read(5, "writing-for-agents"), read(6, "grade"), read(7, "blast-radius")]
    );
    // The bash call chains `date` (outside the grammar) with `find skills -name SKILL.md`.
    assert_eq!(p.unparsed.len(), 1);
    assert!(p.unparsed[0].starts_with("find skills -maxdepth 3 -type f | sort; "));
}

#[test]
fn noisy_bash_loop_is_unparsed_not_an_event() {
    let p = parse(&fixture("bash_loop.jsonl"));
    let ts = "427b7087@2026-09-07T04:34:07.893Z";
    let r = |i: u32, skill: &str, rest: &str, ch| {
        ev(&format!("pi:{ts}:{i}:0"), skill, ch, &format!("{PI_SKILLS}/{skill}/{rest}"), 2)
    };
    // Item 0 is the `for p in …; do wc -l "$p"; done` loop, item 5 `subagents_list`.
    assert_eq!(
        events(&p),
        [
            r(1, "goal-smart", "references/pi-goals.md", Channel::Ref),
            r(2, "goal-smart", "references/artifact-contract.md", Channel::Ref),
            r(3, "goal-smart", "references/multi-agent-policy.md", Channel::Ref),
            r(4, "goal-investigate", "SKILL.md", Channel::Read),
        ]
    );
    assert_eq!(p.unparsed.len(), 1);
    assert!(p.unparsed[0].starts_with("for p in /home/nicolas/.pi/agent/skills/goal-smart/references/{"));
}

#[test]
fn run_state_line_one_is_skipped() {
    // Line 1 is a herdr run-state record; line 2 would be a slash load in a session.
    let p = parse(&fixture("not_session.jsonl"));
    assert!(p.skipped && p.raws.is_empty());
}

/// Copies the fork pair into a temp Pi home's sessions dir, pointing the child's
/// `parentSession` at the copied parent as Pi would.
fn fork_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let real = "/home/nicolas/.pi/agent/sessions/--home-nicolas-zama-fhevm--";
    let slug = dir.path().join("sessions/--home-nicolas-zama-fhevm--");
    common::copy_tree(&fixture("fork"), &slug, &|s: String| s.replace(real, slug.to_str().unwrap()));
    dir
}

#[test]
fn files_lists_session_jsonl_only_in_basename_order() {
    let dir = fork_dir();
    // The copy also holds a `.jsonl.herdr-complete.json` run-state file and Markdown artifacts.
    let names: Vec<String> =
        pi::Pi.files(&home(dir.path())).iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect();
    assert_eq!(
        names,
        [
            "2026-09-24T15-53-01-067Z_01a0d41e-c38a-711d-84da-d0caa2649403.jsonl",
            "2026-09-24T15-54-52-685Z_b7296a7d765078e4.jsonl",
        ]
    );
}

#[test]
fn fork_copies_share_keys_and_the_child_roots_to_the_parent() {
    let dir = fork_dir();
    let files = pi::Pi.files(&home(dir.path()));
    let (parent, child) = (parse(&files[0]), parse(&files[1]));
    let ts = "d3accda9@2026-09-24T15:54:01.424Z";
    let expected = [
        ev(
            &format!("pi:{ts}:2:0"),
            "writing-for-agents",
            Channel::Read,
            &format!("{PI_SKILLS}/writing-for-agents/SKILL.md"),
            2,
        ),
        ev(&format!("pi:{ts}:3:0"), "unslop", Channel::Read, &format!("{PI_SKILLS}/unslop/SKILL.md"), 2),
        ev(
            "pi:0a4d3ba5@2026-09-24T15:54:05.000Z:0:0",
            "writing-for-agents",
            Channel::Ref,
            &format!("{PI_SKILLS}/writing-for-agents/references/pi-runtime.md"),
            3,
        ),
    ];
    assert_eq!(events(&parent), expected);
    assert_eq!(events(&child), expected);
    let keys: BTreeSet<_> = parent.raws.iter().chain(&child.raws).map(|r| format!("{:?}", r.key)).collect();
    assert_eq!(keys.len(), 3);

    let parent_id = "01a0d41e-c38a-711d-84da-d0caa2649403";
    let s = &child.raws[0].session;
    assert_eq!(s.id, "d97aa721-d793-439c-9039-1fba426c0f34");
    assert_eq!(s.parent, Some(Parent::Pi(files[0].clone())));
    assert_eq!(classify::actor(Agent::Pi, &s.actor), Actor::Subagent);
    assert_eq!(classify::actor(Agent::Pi, &parent.raws[0].session.actor), Actor::Main);
    let h = home(dir.path());
    assert_eq!(Roots::default().root(&h, &s.id, s.parent.as_ref()), parent_id);
    let header =
        |f: &std::path::Path| pi::session_of_header(&tail::first_line(f).unwrap().unwrap()).map(|h| (h.id, h.parent));
    assert_eq!(header(&files[1]), Some((s.id.clone(), Some(Parent::Pi(files[0].clone())))));
    assert_eq!(header(&files[0]), Some((parent_id.into(), None)));
}

#[test]
fn skill_block_prompt_is_the_invocation_and_its_arguments() {
    let src = format!("{}:2", fixture("slash.jsonl").display());
    assert_eq!(pi::Pi.prompt_at(&src).as_deref(), Some("/skill:goal-smart [scrubbed]"));
}

#[test]
fn bare_skill_block_prompt_is_the_message_before_it() {
    // No session on this machine has a bare `/skill:`, so this one is composed:
    // the `slash.jsonl` header, a plain message, then that file's block without arguments.
    let header = tail::first_line(&fixture("slash.jsonl")).unwrap().unwrap();
    let user = |id: &str, text: &str| {
        serde_json::json!({"type": "message", "id": id, "timestamp": "2026-09-11T03:25:29.274Z",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
        .to_string()
    };
    let block = "<skill name=\"goal-smart\" location=\"/home/nicolas/.pi/agent/skills/goal-smart/SKILL.md\">\n[scrubbed]\n</skill>";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let text = [String::from_utf8(header).unwrap(), user("a", "Plan the scrubbed work."), user("b", block)].join("\n");
    std::fs::write(&path, text).unwrap();
    assert_eq!(pi::Pi.prompt_at(&format!("{}:3", path.display())).as_deref(), Some("Plan the scrubbed work."));
}

#[test]
fn condense_keeps_the_window_from_the_prompt_to_another_skill_load() {
    // Line 4 is a `/skill:unslop` expansion, its own prompt; line 7 expands
    // `unslop` again and line 8 `grade`.
    let got = pi::Pi.condense(&format!("{}:4", fixture("condense.jsonl").display()), 60_000).unwrap();
    let want = [
        "L4: user: /skill:unslop tidy the prose".to_string(),
        "L5: assistant: Working on it.".into(),
        format!("L5: tool bash: {{\"command\":\"echo {}", "a".repeat(183)),
        format!("L6: result: {}", "r".repeat(500)),
        "L7: user: /skill:unslop check the tone".into(),
    ]
    .join("\n");
    assert_eq!(got, want);
    assert!(!got.contains("SKILL_BODY_TEXT") && !got.contains("_TAIL"));
}

#[test]
fn child_keeps_its_ids_and_root_after_the_middle_session_is_deleted() {
    let chain = common::Chain::new(Agent::Pi, [true; 3]);
    let before = chain.child(&chain.scan(false));
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].1, chain.ids[0]);
    std::fs::remove_file(&chain.files[1]).unwrap();
    assert_eq!(chain.child(&chain.scan(true)), before);
}
