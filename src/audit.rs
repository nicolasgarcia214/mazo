//! `skima audit` (SPEC §6.4, F18) stage 1: selection, the judge's input, one
//! Claude, Codex or Pi judge call per session, validation of its answer,
//! and the session records in `audits.jsonl`. `--dry-run` shows what a run
//! would judge and calls nothing.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io::{ErrorKind, IsTerminal, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::Result;
use crate::adapters;
use crate::config::{Home, Judge};
use crate::inventory::git;
use crate::ledger::{self, Actor, Agent, Event, name};
use crate::render::{self, Align};

/// The condensed session's cap (SPEC §6.4 `max_chars`).
pub const MAX_CHARS: usize = 60_000;

/// The stage-1 judge prompt, shipped in the binary, and the version records name.
pub const SESSION_PROMPT: &str = include_str!("../prompts/session.md");
pub const SESSION_PROMPT_VERSION: &str = "session-v1";

/// One cent in micro-dollars: no call starts with a smaller cap (SPEC §6.4, D34).
pub const CENT: u64 = 10_000;

const ASKED_CHARS: usize = 240;
const NOTES_CHARS: usize = 400;
const FINDING_CHARS: usize = 200;
const FINDINGS: usize = 5;
/// A run of this many characters shared with the session is a quote (D32).
const QUOTE_CHARS: usize = 40;
/// A whole user message of this many characters is a quote (D32).
const SHORT_MESSAGE: std::ops::RangeInclusive<usize> = 12..=39;

/// The JSON schema of the judge's answer, passed as `--json-schema`.
pub static SESSION_SCHEMA: LazyLock<String> = LazyLock::new(|| {
    let text = |max: usize| serde_json::json!({"type": "string", "maxLength": max});
    let findings = serde_json::json!({
        "type": "array",
        "maxItems": FINDINGS,
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["line", "text"],
            "properties": {"line": {"type": "integer", "minimum": 1}, "text": text(FINDING_CHARS)},
        },
    });
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["asked", "followed", "outcome", "redone", "strengths", "frictions", "notes"],
        "properties": {
            "asked": text(ASKED_CHARS),
            "followed": {"enum": ["yes", "partly", "no"]},
            "outcome": {"enum": ["achieved", "partial", "failed", "abandoned", "unclear"]},
            "redone": {"type": "boolean"},
            "strengths": findings,
            "frictions": findings,
            "notes": text(NOTES_CHARS),
        },
    })
    .to_string()
});

/// A session the audit judges: the strong load its window starts from, and
/// the condensed window.
pub struct Candidate {
    pub event: Event,
    pub condensed: String,
}

impl Candidate {
    /// When the session started: its transcript's first timestamp, else the load's.
    pub fn started(&self) -> Option<Timestamp> {
        crate::scan::first_timestamp(Path::new(self.event.src_file()))
            .and_then(|t| t.parse().ok())
            .or_else(|| self.event.ts.parse().ok())
    }
}

/// The fields of an `audits.jsonl` line that later runs read back.
#[derive(Deserialize)]
struct Recorded {
    kind: String,
    skill: String,
    session: Option<String>,
    condensed_chars: Option<u64>,
    judge: Option<RecordedJudge>,
}

#[derive(Deserialize)]
struct RecordedJudge {
    cost_usd: Option<Box<RawValue>>,
}

/// Every readable line of `<data>/audits.jsonl`; none when the file is missing.
fn recorded(data: &Path) -> Result<Vec<Recorded>> {
    let path = data.join("audits.jsonl");
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
}

/// Sessions that already have a `session` record for `skill` in `<data>/audits.jsonl`.
pub fn audited(data: &Path, skill: &str) -> Result<HashSet<String>> {
    Ok(recorded(data)?
        .into_iter()
        .filter(|r| r.kind == "session" && r.skill == skill)
        .filter_map(|r| r.session)
        .collect())
}

/// Stage-1 selection (SPEC §6.4, D29): root sessions of any agent with a strong
/// load of `skill` by actor `main` or `headless` and no session in `audited`,
/// newest first and taken round-robin across projects (cwd), up to `limit`
/// (`None` takes all). A session is condensed from its earliest strong load
/// whose transcript is still on disk; one with none left is skipped and takes
/// no turn.
pub fn select(events: &[Event], skill: &str, audited: &HashSet<String>, limit: Option<usize>) -> Vec<Candidate> {
    type Loads<'a> = Vec<(Timestamp, &'a Event)>;
    let mut sessions: BTreeMap<(Agent, &str), Loads> = BTreeMap::new();
    for e in events {
        if e.skill != skill || !e.channel.strong() || !matches!(e.actor, Actor::Main | Actor::Headless) {
            continue;
        }
        if audited.contains(&e.session) {
            continue;
        }
        let Ok(t) = e.ts.parse::<Timestamp>() else { continue };
        sessions.entry((e.agent, &e.session)).or_default().push((t, e));
    }
    let mut sessions: Vec<Loads> = sessions.into_values().collect();
    for loads in &mut sessions {
        loads.sort_by_key(|(t, _)| *t);
    }
    sessions.sort_by_key(|loads| std::cmp::Reverse(loads[0].0));
    // Each project's sessions, newest first.
    let mut projects: Vec<(Option<&str>, VecDeque<Loads>)> = Vec::new();
    for loads in sessions {
        let cwd = loads.iter().find_map(|(_, e)| e.cwd.as_deref());
        match projects.iter_mut().find(|(c, _)| *c == cwd) {
            Some((_, queue)) => queue.push_back(loads),
            None => projects.push((cwd, VecDeque::from([loads]))),
        }
    }
    // A project's next session that can still be condensed, with its start.
    let next = |queue: &mut VecDeque<Loads>| {
        while let Some(loads) = queue.pop_front() {
            let found = loads.iter().find_map(|(_, e)| {
                let condensed = adapters::for_agent(e.agent).condense(&e.src, MAX_CHARS)?;
                Some(Candidate { event: (*e).clone(), condensed })
            });
            if let Some(c) = found {
                return Some((loads[0].0, c));
            }
        }
        None
    };
    // Round-robin: the n-th newest session of every project comes before any
    // project's (n+1)-th; projects in the order of their newest session.
    let mut heads: Vec<((Timestamp, Candidate), VecDeque<Loads>)> =
        projects.into_iter().filter_map(|(_, mut queue)| Some((next(&mut queue)?, queue))).collect();
    heads.sort_by_key(|((t, _), _)| std::cmp::Reverse(*t));
    let limit = limit.unwrap_or(usize::MAX);
    let mut queues = Vec::new();
    let mut out = Vec::new();
    for ((_, c), queue) in heads {
        out.push(c);
        queues.push(queue);
    }
    while out.len() < limit {
        let before = out.len();
        for queue in &mut queues {
            if out.len() == limit {
                break;
            }
            out.extend(next(queue).map(|(_, c)| c));
        }
        if out.len() == before {
            break;
        }
    }
    out.truncate(limit);
    out
}

/// `SKILL.md` as the judge reads it, and the commit it came from.
#[derive(Debug, PartialEq)]
pub struct SkillText {
    pub commit: Option<String>,
    pub text: String,
}

/// `<source>/<skill>/SKILL.md` as of the last commit before `started` that
/// touched it, or the current file when there is no repository or no earlier
/// commit (SPEC §6.4, D30).
pub fn skill_at(source: &Path, skill: &str, started: Option<Timestamp>) -> Result<SkillText> {
    let dir = source.join(skill);
    let at = started.and_then(|t| {
        let before = format!("--before={t}");
        let log = git(&dir, &["log", "-1", "--format=%H", &before, "--", "SKILL.md"])?;
        let commit = Some(log.trim()).filter(|c| !c.is_empty())?;
        let text = git(&dir, &["show", &format!("{commit}:./SKILL.md")])?;
        Some(SkillText { commit: Some(commit.to_string()), text })
    });
    match at {
        Some(at) => Ok(at),
        None => {
            let path = dir.join("SKILL.md");
            let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(SkillText { commit: None, text })
        }
    }
}

/// The judge's input for one session: the skill as it was when the session
/// started, then the condensed session. A closing tag inside either part is
/// written `<\/tag>`, so neither can end its own frame.
pub fn input(source: &Path, c: &Candidate) -> Result<(SkillText, String)> {
    let skill = skill_at(source, &c.event.skill, c.started())?;
    let text = format!(
        "<skill name=\"{}\">\n{}\n</skill>\n\n<session>\n{}\n</session>\n",
        c.event.skill,
        skill.text.trim_end().replace("</skill>", "<\\/skill>"),
        c.condensed.replace("</session>", "<\\/session>")
    );
    Ok((skill, text))
}

/// Stand-ins in [`command`]'s argv for the files each call writes in its own
/// temporary directory: the answer schema, Codex's result and Pi's input.
pub const SCHEMA_FILE: &str = "<schema file>";
pub const RESULT_FILE: &str = "<result file>";
pub const INPUT_FILE: &str = "@<input file>";

/// The judge's argv (SPEC §6.4, D33) with the shipped prompt and schema; a
/// Claude call appends `--max-budget-usd <cap>`.
pub fn command(judge: &Judge) -> Vec<String> {
    let (argv, model): (&[&str], _) = match judge.agent {
        Agent::Claude => (
            &[
                "claude",
                "-p",
                "--safe-mode",
                "--tools",
                "",
                "--system-prompt",
                SESSION_PROMPT,
                "--output-format",
                "json",
                "--json-schema",
                &SESSION_SCHEMA,
                "--no-session-persistence",
            ],
            "--model",
        ),
        Agent::Codex => (
            &[
                "codex",
                "exec",
                "--ephemeral",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--output-schema",
                SCHEMA_FILE,
                "-o",
                RESULT_FILE,
                "--json",
            ],
            "-m",
        ),
        Agent::Pi => (
            &[
                "pi",
                "-p",
                "--no-session",
                "--no-tools",
                "--no-extensions",
                "--no-skills",
                "--no-context-files",
                "--system-prompt",
                SESSION_PROMPT,
                "--mode",
                "json",
            ],
            "--model",
        ),
    };
    let mut argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    if let Some(m) = &judge.model {
        argv.extend([model.to_string(), m.clone()]);
    }
    // Codex reads prompt and input from stdin; Pi reads the input file.
    match judge.agent {
        Agent::Claude => {}
        Agent::Codex => argv.push("-".into()),
        Agent::Pi => argv.push(INPUT_FILE.into()),
    }
    argv
}

/// A non-negative dollar amount from its decimal text (`0.031`, `1`, `3.1e-5`),
/// truncated to integer micro-dollars without passing through a float (D34).
pub fn parse_usd(text: &str) -> std::result::Result<u64, String> {
    let bad = || format!("not a dollar amount: {text}");
    let (mantissa, exp) = match text.split_once(['e', 'E']) {
        Some((m, e)) => (m, i64::from(e.parse::<i32>().map_err(|_| bad())?)),
        None => (text, 0),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{int}{frac}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let digits = digits.trim_start_matches('0');
    // The amount is `digits · 10^shift` micro-dollars.
    let shift = exp + 6 - frac.len() as i64;
    let kept = (digits.len() as i64 + shift.min(0)).max(0) as usize;
    let n: u64 = if kept == 0 { 0 } else { digits[..kept].parse().map_err(|_| bad())? };
    let scale = 10u64.checked_pow(shift.max(0).try_into().map_err(|_| bad())?).ok_or_else(bad)?;
    n.checked_mul(scale).ok_or_else(bad)
}

/// Micro-dollars as dollars rounded down to cents, the form caps are passed in.
pub fn cents(micros: u64) -> String {
    format!("{}.{:02}", micros / 1_000_000, micros % 1_000_000 / CENT)
}

/// Micro-dollars as an exact decimal (`0.031`), the form records store.
fn exact(micros: u64) -> String {
    let s = format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Followed {
    Yes,
    Partly,
    No,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Achieved,
    Partial,
    Failed,
    Abandoned,
    Unclear,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub line: u64,
    pub text: String,
}

/// The judge's answer for one session (SPEC §6.4, D31).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    pub asked: String,
    pub followed: Followed,
    pub outcome: Outcome,
    pub redone: bool,
    pub strengths: Vec<Finding>,
    pub frictions: Vec<Finding>,
    pub notes: String,
}

/// The condensed session's records: each `L<n>: ` line with the lines after
/// it that carry no number of their own.
fn records(condensed: &str) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = Vec::new();
    for l in condensed.lines() {
        let numbered = l.strip_prefix('L').and_then(|r| r.split_once(": ")).and_then(|(n, rest)| {
            let n = n.bytes().all(|b| b.is_ascii_digit()).then(|| n.parse::<u64>().ok())??;
            Some((n, rest))
        });
        match (numbered, out.last_mut()) {
            (Some((n, rest)), _) => out.push((n, rest.to_string())),
            (None, Some((_, text))) => {
                text.push('\n');
                text.push_str(l);
            }
            (None, None) => {}
        }
    }
    out
}

/// Lowercased, with every run of whitespace one space (D32).
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The first of `texts` that quotes the session (SPEC §6.4, D32): it shares
/// 40 consecutive characters with a user message, an assistant message or a
/// tool result, or holds a whole user message of 12 to 39 characters.
fn quoting<'a>(texts: impl Iterator<Item = &'a str>, records: &[(u64, String)]) -> Option<&'a str> {
    let sources: Vec<(bool, String)> = records
        .iter()
        .filter_map(|(_, r)| {
            let (user, text) = match r.split_once(": ")? {
                ("user", t) => (true, t),
                ("assistant" | "result", t) => (false, t),
                _ => return None,
            };
            Some((user, normalize(text)))
        })
        .collect();
    let windows = |s: &str| -> Vec<(usize, usize)> {
        let at: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
        at.windows(QUOTE_CHARS + 1).map(|w| (w[0], w[QUOTE_CHARS])).collect()
    };
    let shared: HashSet<&str> =
        sources.iter().flat_map(|(_, s)| windows(s).into_iter().map(|(a, b)| &s[a..b])).collect();
    let short: Vec<&str> = sources
        .iter()
        .filter(|(user, s)| *user && SHORT_MESSAGE.contains(&s.chars().count()))
        .map(|(_, s)| s.as_str())
        .collect();
    texts.into_iter().find(|t| {
        let t = normalize(t);
        windows(&t).into_iter().any(|(a, b)| shared.contains(&t[a..b])) || short.iter().any(|s| t.contains(s))
    })
}

/// The judge's answer checked against its schema, the lines of `condensed`
/// and the no-quote rule (SPEC §6.4, D32). The error says what failed.
pub fn validate(answer: Value, condensed: &str) -> std::result::Result<Verdict, String> {
    let v: Verdict = serde_json::from_value(answer).map_err(|e| format!("not the answer schema: {e}"))?;
    let chars = |s: &str| s.chars().count();
    let findings = || v.strengths.iter().chain(&v.frictions);
    if chars(&v.asked) > ASKED_CHARS || chars(&v.notes) > NOTES_CHARS {
        return Err(format!("asked is over {ASKED_CHARS} or notes over {NOTES_CHARS} characters"));
    }
    if v.strengths.len() > FINDINGS
        || v.frictions.len() > FINDINGS
        || findings().any(|f| chars(&f.text) > FINDING_CHARS)
    {
        return Err(format!("more than {FINDINGS} findings of a kind, or one over {FINDING_CHARS} characters"));
    }
    let records = records(condensed);
    if let Some(f) = findings().find(|f| !records.iter().any(|(n, _)| *n == f.line)) {
        return Err(format!("cites L{}, which is not in the session", f.line));
    }
    let texts = [v.asked.as_str(), v.notes.as_str()].into_iter().chain(findings().map(|f| f.text.as_str()));
    if quoting(texts, &records).is_some() {
        return Err("quotes the session".into());
    }
    Ok(v)
}

/// What one judge call returned: its reported cost, none from a judge that
/// reports none, and the answer or why there is none.
struct Reply {
    cost: Option<u64>,
    answer: std::result::Result<Value, String>,
}

/// Claude's `--output-format json` result; the cost stays decimal text.
#[derive(Deserialize)]
struct ClaudeOutput<'a> {
    #[serde(borrow)]
    total_cost_usd: Option<&'a RawValue>,
    #[serde(default)]
    is_error: bool,
    subtype: Option<String>,
    structured_output: Option<Value>,
}

/// One judge call (D33): [`command`]'s argv run in a fresh empty directory
/// under `<data>/audit-runs/`, with the files it names in a fresh temporary
/// directory, both removed after. Claude gets `--max-budget-usd <cap>` and the
/// input on stdin; Codex gets the prompt and the input on stdin and the scratch
/// `CODEX_HOME`; Pi reads the input file. A Claude call that ran but reported
/// no cost is counted at its cap, the most it could have spent short of one
/// turn's overshoot, so the budget still binds.
fn call(a: &Audit, cap: Option<u64>, input: &str) -> Result<Reply> {
    let unique = format!("{}-{}", std::process::id(), Timestamp::now().as_nanosecond());
    let runs = a.data.join("audit-runs");
    let dir = runs.join(&unique);
    fs::create_dir_all(&runs).and_then(|()| fs::create_dir(&dir)).map_err(|e| format!("{}: {e}", dir.display()))?;
    let scratch = if a.judge.agent == Agent::Codex { Some(codex_home(a)?) } else { None };
    let files = std::env::temp_dir().join(format!("skima-judge-{unique}"));
    let result = files.join("result.json");
    let output = DirBuilder::new().mode(0o700).create(&files).and_then(|()| {
        let mut args: Vec<OsString> = Vec::new();
        for arg in &a.argv[1..] {
            args.push(match arg.as_str() {
                SCHEMA_FILE => {
                    let path = files.join("schema.json");
                    fs::write(&path, &*SESSION_SCHEMA)?;
                    path.into()
                }
                RESULT_FILE => result.clone().into(),
                INPUT_FILE => {
                    let path = files.join("input.md");
                    fs::write(&path, input)?;
                    format!("@{}", path.display()).into()
                }
                _ => arg.into(),
            });
        }
        let stdin = match a.judge.agent {
            Agent::Claude => Some(input.to_string()),
            Agent::Codex => Some(format!("{SESSION_PROMPT}\n{input}")),
            Agent::Pi => None,
        };
        let mut cmd = Command::new(&a.argv[0]);
        cmd.args(args)
            .current_dir(&dir)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cap) = cap {
            cmd.args(["--max-budget-usd", &cents(cap)]);
        }
        if let Some(home) = &scratch {
            cmd.env("CODEX_HOME", home);
        }
        let mut child = cmd.spawn()?;
        let pipe = child.stdin.take();
        std::thread::scope(|s| {
            // A judge that exits without reading its input closes the pipe;
            // its output says why.
            if let (Some(mut pipe), Some(stdin)) = (pipe, stdin) {
                s.spawn(move || pipe.write_all(stdin.as_bytes()));
            }
            child.wait_with_output()
        })
    });
    let written = fs::read(&result);
    let removed = fs::remove_dir_all(&dir);
    let cleaned = fs::remove_dir_all(&files);
    let output = output.map_err(|e| format!("{}: {e}", a.argv[0]))?;
    removed.map_err(|e| format!("{}: {e}", dir.display()))?;
    cleaned.map_err(|e| format!("{}: {e}", files.display()))?;
    if scratch.is_some() {
        codex_home(a)?;
    }
    let failed = || {
        let stderr = String::from_utf8_lossy(&output.stderr);
        format!("({}): {}", output.status, stderr.lines().next().unwrap_or_default())
    };
    Ok(match a.judge.agent {
        Agent::Claude => {
            let cap = cap.unwrap_or_default();
            let Ok(out) = serde_json::from_slice::<ClaudeOutput>(&output.stdout) else {
                return Ok(Reply { cost: Some(cap), answer: Err(format!("unreadable judge output {}", failed())) });
            };
            let cost = out.total_cost_usd.map(|c| parse_usd(c.get())).transpose()?.unwrap_or(cap);
            let answer = match out.structured_output {
                Some(v) if !out.is_error => Ok(v),
                _ => Err(format!("no answer ({})", out.subtype.as_deref().unwrap_or("error"))),
            };
            Reply { cost: Some(cost), answer }
        }
        Agent::Codex => Reply {
            cost: None,
            answer: written
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .ok_or_else(|| format!("no answer in the -o file {}", failed())),
        },
        Agent::Pi => Reply { cost: None, answer: pi_answer(&output.stdout).map_err(|e| format!("{e} {}", failed())) },
    })
}

/// The user's Codex `auth.json`: in the configured Codex home, else `~/.codex`.
pub fn codex_auth(homes: &[Home]) -> Result<PathBuf> {
    let home = match homes.iter().find(|h| h.agent == Agent::Codex) {
        Some(h) => h.path.clone(),
        None => PathBuf::from(crate::config::home_dir()?).join(".codex"),
    };
    Ok(home.join("auth.json"))
}

/// `<data>/codex-judge-home` holding only a symlink to the user's `auth.json`
/// (D33), made afresh so nothing an earlier call left there survives.
fn codex_home(a: &Audit) -> Result<PathBuf> {
    let home = a.data.join("codex-judge-home");
    if !a.codex_auth.exists() {
        return Err(format!("{}: no Codex login for the Codex judge", a.codex_auth.display()));
    }
    match fs::remove_dir_all(&home) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(format!("{}: {e}", home.display())),
        _ => {}
    }
    fs::create_dir_all(&home)
        .and_then(|()| std::os::unix::fs::symlink(a.codex_auth, home.join("auth.json")))
        .map_err(|e| format!("{}: {e}", home.display()))?;
    Ok(home)
}

/// Pi's answer: the text of the last assistant `message_end` in its
/// `--mode json` stream, parsed as JSON once a code fence around it is dropped.
fn pi_answer(stdout: &[u8]) -> std::result::Result<Value, String> {
    let message = String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "message_end" && e["message"]["role"] == "assistant")
        .map(|mut e| e["message"].take())
        .ok_or("no assistant message")?;
    if message["stopReason"] != "stop" {
        let why = message["errorMessage"].as_str().or(message["stopReason"].as_str()).unwrap_or("error");
        return Err(format!("no answer ({why})"));
    }
    let text: String = message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect();
    let text = text.trim();
    let bare = text.strip_prefix("```json").or_else(|| text.strip_prefix("```")).and_then(|t| t.strip_suffix("```"));
    serde_json::from_str(bare.unwrap_or(text).trim()).map_err(|e| format!("the final message is not JSON: {e}"))
}

#[derive(Serialize)]
struct SessionRecord<'a> {
    v: u32,
    kind: &'static str,
    skill: &'a str,
    session: &'a str,
    agent: Agent,
    src: &'a str,
    session_ts: Option<String>,
    audited_at: String,
    skill_commit: Option<&'a str>,
    condensed_chars: usize,
    judge: JudgeRecord<'a>,
    prompt: &'static str,
    result: &'a Verdict,
}

#[derive(Serialize)]
struct JudgeRecord<'a> {
    agent: Agent,
    model: Option<&'a str>,
    cost_usd: Option<Box<RawValue>>,
}

/// One audit run's settings.
pub struct Audit<'a> {
    pub skill: &'a str,
    pub source: &'a Path,
    pub data: &'a Path,
    pub judge: &'a Judge,
    /// [`command`]'s argv.
    pub argv: &'a [String],
    /// `--max-budget-usd` in micro-dollars; the Claude judge's only.
    pub budget: Option<u64>,
    /// The user's Codex `auth.json`, which the Codex judge's home links to.
    pub codex_auth: &'a Path,
}

/// Stage 1 (SPEC §6.4): judges `candidates` one at a time; with a budget,
/// each call is capped at 75% of it minus what is spent, stopping once that is
/// under a cent.
/// Each valid answer is appended to `audits.jsonl` under `scan.lock` at once;
/// an invalid one is retried once, then dropped. Returns the run's summary.
pub fn run(a: &Audit, candidates: &[Candidate]) -> Result<String> {
    if candidates.is_empty() {
        return Ok(format!("{}: no session to judge\n", a.skill));
    }
    let pool = a.budget.map(|b| b / 4 * 3);
    let progress = std::io::stderr().is_terminal();
    let (mut spent, mut judged, mut dropped) = (0u64, 0usize, 0usize);
    let mut left: &[Candidate] = &[];
    'sessions: for (i, c) in candidates.iter().enumerate() {
        let (skill, input) = input(a.source, c)?;
        let mut cost = None;
        let mut why = String::new();
        for _ in 0..2 {
            let cap = pool.map(|p| p.saturating_sub(spent));
            if cap.is_some_and(|cap| cap < CENT) {
                left = &candidates[i..];
                break 'sessions;
            }
            if progress {
                let cap = cap.map(|cap| format!(" (cap ${})", cents(cap))).unwrap_or_default();
                eprintln!("judging {}/{} {}{cap}", i + 1, candidates.len(), c.event.session);
            }
            let reply = call(a, cap, &input)?;
            spent += reply.cost.unwrap_or_default();
            cost = reply.cost.map(|r| r + cost.unwrap_or_default());
            match reply.answer.and_then(|v| validate(v, &c.condensed)) {
                Ok(verdict) => {
                    let record = SessionRecord {
                        v: 1,
                        kind: "session",
                        skill: a.skill,
                        session: &c.event.session,
                        agent: c.event.agent,
                        src: &c.event.src,
                        session_ts: c.started().map(|t| t.to_string()),
                        audited_at: Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string(),
                        skill_commit: skill.commit.as_deref(),
                        condensed_chars: c.condensed.chars().count(),
                        judge: JudgeRecord {
                            agent: a.judge.agent,
                            model: a.judge.model.as_deref(),
                            cost_usd: cost.map(|c| RawValue::from_string(exact(c)).expect("a decimal is JSON")),
                        },
                        prompt: SESSION_PROMPT_VERSION,
                        result: &verdict,
                    };
                    let line = serde_json::to_string(&record).expect("record serialises") + "\n";
                    let _lock = ledger::lock(a.data)?;
                    ledger::append_lines(&a.data.join("audits.jsonl"), line)?;
                    judged += 1;
                    continue 'sessions;
                }
                Err(e) => why = e,
            }
        }
        eprintln!("warning: {}: judge answer dropped after one retry: {why}", c.event.session);
        dropped += 1;
    }
    let spend = a.budget.map(|b| format!(", ${} spent of ${}", cents(spent), cents(b))).unwrap_or_default();
    let mut out = format!("{}: {judged} judged, {dropped} dropped{spend}\n", a.skill);
    if !left.is_empty() {
        out.push_str(&format!("budget used up; left for the next run: {}\n", left.len()));
        for c in left {
            out.push_str(&format!("  {} {}\n", name(c.event.agent), c.event.session));
        }
    }
    Ok(out)
}

/// Input tokens estimated from condensed characters (SPEC §6.4, D36).
pub fn estimate_tokens(chars: usize) -> usize {
    chars / 4
}

/// The dollar cost of `chars` condensed characters at the rate earlier session
/// records paid per condensed character, with how many records set it (D36).
fn estimate_cost(data: &Path, chars: usize) -> Result<Option<(u64, usize)>> {
    let (mut cost, mut seen, mut n) = (0u128, 0u128, 0usize);
    for r in recorded(data)? {
        let paid = r.judge.and_then(|j| j.cost_usd).and_then(|c| parse_usd(c.get()).ok());
        if let (true, Some(paid), Some(len)) = (r.kind == "session", paid, r.condensed_chars) {
            cost += u128::from(paid);
            seen += u128::from(len);
            n += 1;
        }
    }
    if seen == 0 {
        return Ok(None);
    }
    Ok(Some(((cost * chars as u128 / seen) as u64, n)))
}

/// `skima audit --dry-run`: each session the audit would judge with its agent,
/// project and condensed size, the judge command, the token estimate and,
/// once session records exist, the dollar estimate (D36).
pub fn dry_run(a: &Audit, candidates: &[Candidate], tty: bool) -> Result<String> {
    let skill = a.skill;
    if candidates.is_empty() {
        return Ok(format!("{skill}: no session to judge\n"));
    }
    let tz = TimeZone::system();
    let chars = |c: &Candidate| c.condensed.chars().count();
    let rows: Vec<Vec<String>> = candidates
        .iter()
        .map(|c| {
            let ts = c
                .event
                .ts
                .parse::<Timestamp>()
                .map_or(c.event.ts.clone(), |t| t.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M").to_string());
            let cwd = c.event.cwd.clone().unwrap_or_else(|| "-".into());
            vec![ts, name(c.event.agent), cwd, chars(c).to_string(), c.event.session.clone()]
        })
        .collect();
    let header = [
        ("loaded", Align::Left),
        ("agent", Align::Left),
        ("project", Align::Left),
        ("chars", Align::Right),
        ("session", Align::Left),
    ];
    let quoted: Vec<String> = a
        .argv
        .iter()
        .map(|arg| match arg.as_str() {
            SESSION_PROMPT => "\"$(cat <prompt>)\"".into(),
            s if s == *SESSION_SCHEMA => "<schema>".into(),
            "" => "\"\"".into(),
            s => s.into(),
        })
        .collect();
    let judge = match a.judge.agent {
        Agent::Claude => format!("{} --max-budget-usd <cap> < <input>", quoted.join(" ")),
        Agent::Codex => {
            format!("CODEX_HOME={} {} < <prompt + input>", a.data.join("codex-judge-home").display(), quoted.join(" "))
        }
        Agent::Pi => quoted.join(" "),
    };
    let total: usize = candidates.iter().map(chars).sum();
    let n = candidates.len();
    let mut out = format!(
        "{skill}: {n} session{} to judge with the {} judge\n\n{}\njudge: {judge}\ninput: {total} condensed characters, about {} tokens\n",
        if n == 1 { "" } else { "s" },
        name(a.judge.agent),
        render::table(&header, &rows, tty),
        estimate_tokens(total),
    );
    if let (Some(_), Some((cost, records))) = (a.budget, estimate_cost(a.data, total)?) {
        out.push_str(&format!(
            "cost: about ${}, at the rate of {records} earlier session record{}\n",
            cents(cost),
            if records == 1 { "" } else { "s" }
        ));
    }
    Ok(out)
}
