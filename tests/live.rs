#![allow(clippy::disallowed_methods)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

use common::probe::Probe;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(300);

fn live() -> bool {
    let on = std::env::var_os("MAZO_LIVE").is_some_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set MAZO_LIVE=1 to run the live tests");
    }
    on
}

struct Ran {
    ok: bool,
    log: String,
}

static LIVE: Mutex<()> = Mutex::new(());

fn run(mut cmd: Command, stdin: Option<(&str, &str)>) -> Ran {
    let _live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.process_group(0);
    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return Ran { ok: false, log: format!("{cmd:?}: {e}") },
    };
    let mut input = child.stdin.take();
    if let (Some(pipe), Some((text, _))) = (&mut input, stdin) {
        let _ = writeln!(pipe, "{text}");
    }
    let (tx, rx) = mpsc::channel();
    let readers: [(bool, Box<dyn Read + Send>); 2] =
        [(true, Box::new(child.stdout.take().unwrap())), (false, Box::new(child.stderr.take().unwrap()))];
    for (is_stdout, stream) in readers {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if tx.send((is_stdout, line)).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let deadline = Instant::now() + TIMEOUT;
    let (mut log, mut reached) = (String::new(), false);
    while let Ok((is_stdout, line)) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        log.push_str(&line);
        log.push('\n');
        let Some((_, until)) = stdin.filter(|_| is_stdout) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if v["type"] == until {
            reached = true;
            break;
        }
        if v["type"] == "response" && v["success"] == false {
            log.push_str("failed response; not waiting for the turn\n");
            break;
        }
    }
    drop(input);
    let grace = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < grace && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100))
            }
            _ => {
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                let _ = child.wait();
                break None;
            }
        }
    };
    let drained = Instant::now() + Duration::from_secs(1);
    while let Ok((_, line)) = rx.recv_timeout(drained.saturating_duration_since(Instant::now())) {
        log.push_str(&line);
        log.push('\n');
    }
    let ok = if stdin.is_some() { reached } else { status.is_some_and(|s| s.success()) };
    Ran { ok, log: format!("{cmd:?} exited {status:?}\n{log}") }
}

fn claude(probe: &Probe, prompt: &str) -> Ran {
    run(probe.agent("claude", &["-p", prompt, "--model", "haiku"]), None)
}

fn codex(probe: &Probe, prompt: &str) -> Ran {
    run(probe.agent("codex", &["exec", "--skip-git-repo-check", prompt]), None)
}

fn pi(probe: &Probe, prompt: &str) -> Ran {
    let ext = probe.extension.to_str().unwrap();
    let cmd = probe.agent("pi", &["--mode", "rpc", "-e", ext, "--provider", "probe", "--model", "probe"]);
    run(cmd, Some((&json!({"type": "prompt", "message": prompt}).to_string(), "agent_end")))
}

const GATING: [(&str, &str, &str); 3] =
    [("claude", "slash", "headless"), ("codex", "slash", "headless"), ("pi", "slash", "main")];

struct Gate {
    skill: String,
    runs: BTreeMap<&'static str, Ran>,
    transcripts: BTreeMap<&'static str, String>,
    first: BTreeMap<String, Vec<Value>>,
    shown: Vec<Value>,
    second: BTreeMap<String, Vec<Value>>,
}

fn gate_run() -> Gate {
    let probe = Probe::new();
    let skill = probe.skill.clone();
    let runs = BTreeMap::from([
        ("claude", claude(&probe, &format!("/{skill} ok"))),
        ("codex", codex(&probe, &format!("${skill} ok"))),
        ("pi", pi(&probe, &format!("/skill:{skill}"))),
    ]);
    let dirs = [
        ("claude", probe.claude.join("projects")),
        ("codex", probe.codex.join("sessions")),
        ("pi", probe.pi.join("sessions")),
    ];
    let transcripts = dirs.map(|(h, d)| (h, d.to_string_lossy().into_owned())).into();
    let first = probe.scan();
    let shown = probe.json(&["show", &skill])["events"].as_array().unwrap().clone();
    let second = probe.scan();
    Gate { skill, runs, transcripts, first, shown, second }
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Result<Gate, String>> = OnceLock::new();
    let gate = GATE.get_or_init(|| std::panic::catch_unwind(gate_run).map_err(|_| "the gating run panicked".into()));
    gate.as_ref().unwrap()
}

fn assert_gating(home: &str) {
    if !live() {
        return;
    }
    let g = gate();
    let ran = &g.runs[home];
    assert!(ran.ok, "{home} did not finish:\n{}", ran.log);
    let mut expected = BTreeSet::new();
    for (h, channel, actor) in GATING {
        let events = &g.first[h];
        assert_eq!(events.len(), 1, "{h}: first scan new_events {events:#?}\n{}", g.runs[h].log);
        let e = &events[0];
        let src = e["src"].as_str().unwrap();
        assert_eq!(
            (&e["skill"], &e["agent"], &e["channel"], &e["actor"]),
            (&json!(g.skill), &json!(h), &json!(channel), &json!(actor)),
            "{h}: {e:#}"
        );
        assert!(src.starts_with(&format!("{}/", g.transcripts[h])), "{h}: src {src} outside {}", g.transcripts[h]);
        expected.insert((h.to_string(), channel.to_string(), actor.to_string(), src.to_string()));
    }
    let shown: BTreeSet<(String, String, String, String)> = g
        .shown
        .iter()
        .map(|e| ["agent", "channel", "actor", "src"].map(|k| e[k].as_str().unwrap_or_default().to_string()).into())
        .collect();
    assert_eq!(g.shown.len(), 3, "show --json events: {:#}", json!(g.shown));
    assert_eq!(shown, expected);
    for (h, events) in &g.second {
        assert!(events.is_empty(), "{h}: second scan new_events {events:#?}");
    }
    assert_eq!(g.second.len(), 3, "{:?}", g.second.keys());
}

#[test]
fn claude_slash_load_is_one_headless_slash_event() {
    assert_gating("claude");
}

#[test]
fn codex_dollar_load_is_one_headless_slash_event() {
    assert_gating("codex");
}

#[test]
fn pi_skill_command_is_one_main_slash_event() {
    assert_gating("pi");
}

fn assert_recorded(home: &str, channel: &str, act: impl Fn(&Probe) -> Ran, acted: impl Fn(&Probe) -> bool) {
    if !live() {
        return;
    }
    for attempt in 1..=3 {
        let probe = Probe::new();
        let ran = act(&probe);
        if !acted(&probe) {
            assert!(ran.ok, "attempt {attempt}: the agent run failed:\n{}", ran.log);
            eprintln!("attempt {attempt}: the model never acted\n{}", ran.log);
            continue;
        }
        let events: Vec<Value> = probe
            .scan()
            .remove(home)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e["skill"] == probe.skill.as_str())
            .collect();
        let hit = |e: &Value| e["agent"] == home && e["channel"] == channel && e["actor"] == "headless";
        assert!(
            !events.is_empty() && events.iter().all(hit),
            "the model acted, so every probe event must be a headless {home} {channel} event: {}\n{}",
            json!(events),
            ran.log
        );
        return;
    }
    eprintln!("skipped: the model never acted in 3 runs");
}

#[test]
fn recorded_claude_skill_tool_call_is_a_headless_tool_event() {
    assert_recorded(
        "claude",
        "tool",
        |p| claude(p, &format!("Call the Skill tool with skill {}, then stop.", p.skill)),
        |p| {
            Probe::transcript_has(
                &p.claude.join("projects"),
                &["\"type\":\"tool_use\"", "\"name\":\"Skill\"", &p.skill],
            )
        },
    );
}

#[test]
fn recorded_codex_cat_is_a_headless_read_event() {
    let skill_md = |p: &Probe| p.codex.join("skills").join(&p.skill).join("SKILL.md").to_string_lossy().into_owned();
    assert_recorded(
        "codex",
        "read",
        |p| codex(p, &format!("Run: cat {}", skill_md(p))),
        |p| Probe::transcript_has(&p.codex.join("sessions"), &["\"response_item\"", "_call\"", &skill_md(p)]),
    );
}
