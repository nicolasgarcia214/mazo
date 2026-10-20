//! `skima audit`. Stage 1: selection, the judge's input, one
//! Claude, Codex or Pi judge call per session, validation of its answer, and
//! the session records in `audits.jsonl`. Stage 2: one call over every
//! session record of the skill, the rates skima computes, and the skill
//! record the card shows. `--dry-run` shows what a run would judge and calls
//! nothing.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder};
use std::io::{ErrorKind, IsTerminal, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{LazyLock, mpsc};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

use crate::Result;
use crate::adapters;
use crate::config::{Billing, Home, Judge};
use crate::evals::Evals;
use crate::inventory::git;
use crate::ledger::{self, Actor, Agent, Event, name};
use crate::render::{self, Align};

/// The condensed session's cap.
pub const MAX_CHARS: usize = 60_000;

/// The stage-1 judge prompt, shipped in the binary, and the version records name.
pub const SESSION_PROMPT: &str = include_str!("../prompts/session.md");
pub const SESSION_PROMPT_VERSION: &str = "session-v1";

/// The stage-2 judge prompt, and the version skill records name.
pub const SKILL_PROMPT: &str = include_str!("../prompts/skill.md");
pub const SKILL_PROMPT_VERSION: &str = "skill-v1";

/// One cent in micro-dollars: no call starts with a smaller cap.
pub const CENT: u64 = 10_000;

const ASKED_CHARS: usize = 240;
const NOTES_CHARS: usize = 400;
const FINDING_CHARS: usize = 200;
const FINDINGS: usize = 5;
/// A run of this many characters shared with the session is a quote.
const QUOTE_CHARS: usize = 40;
/// A whole user message of this many characters is a quote.
const SHORT_MESSAGE: std::ops::RangeInclusive<usize> = 12..=39;
/// The most of a quote a rejection shows the judge.
const SHARED_CHARS: usize = 60;

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

/// Every line of `<data>/audits.jsonl` that parses as `T`; none when the file
/// is missing.
fn parsed<T: DeserializeOwned>(data: &Path) -> Result<Vec<T>> {
    Ok(ledger::jsonl(&data.join("audits.jsonl"))?.into_iter().filter_map(|(_, r)| r).collect())
}

/// Sessions that already have a `session` record for `skill` in `<data>/audits.jsonl`.
pub fn audited(data: &Path, skill: &str) -> Result<HashSet<String>> {
    Ok(parsed::<Recorded>(data)?
        .into_iter()
        .filter(|r| r.kind == "session" && r.skill == skill)
        .filter_map(|r| r.session)
        .collect())
}

/// Stage-1 selection: root sessions of any agent with a strong
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
/// commit.
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

/// A stage's shipped prompt and the JSON schema of its answer.
pub struct Stage {
    pub prompt: &'static str,
    pub schema: &'static LazyLock<String>,
}

/// Stage 1, one call per session, and stage 2, one call per skill.
pub static SESSION: Stage = Stage { prompt: SESSION_PROMPT, schema: &SESSION_SCHEMA };
pub static SKILL: Stage = Stage { prompt: SKILL_PROMPT, schema: &SKILL_SCHEMA };

/// Stand-ins in [`command`]'s argv for the files each call writes in its own
/// temporary directory: the answer schema, Codex's result and Pi's input.
pub const SCHEMA_FILE: &str = "<schema file>";
pub const RESULT_FILE: &str = "<result file>";
pub const INPUT_FILE: &str = "@<input file>";

/// The judge's argv with a stage's shipped prompt and
/// schema; a Claude call appends `--max-budget-usd <cap>`.
pub fn command(judge: &Judge, stage: &Stage) -> Vec<String> {
    let (argv, model): (&[&str], _) = match judge.agent {
        Agent::Claude => (
            &[
                "claude",
                "-p",
                "--safe-mode",
                "--tools",
                "",
                "--system-prompt",
                stage.prompt,
                "--output-format",
                "json",
                "--json-schema",
                stage.schema,
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
                stage.prompt,
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
/// truncated to integer micro-dollars without passing through a float.
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

/// The judge's answer for one session.
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

/// Lowercased, with every run of whitespace one space.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The field of the first of `texts` that quotes the session, and the text it
/// shares: 40 consecutive characters shared with a user message, an assistant
/// message or a tool result, run on to at most 60, or a whole user message of
/// 12 to 39 characters.
fn quoting<'a>(texts: impl Iterator<Item = (String, &'a str)>, records: &[(u64, String)]) -> Option<(String, String)> {
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
    texts.into_iter().find_map(|(field, t)| {
        let t = normalize(t);
        let at: Vec<usize> = t.char_indices().map(|(i, _)| i).chain([t.len()]).collect();
        let quote = (0..at.len().saturating_sub(QUOTE_CHARS))
            .find(|&i| shared.contains(&t[at[i]..at[i + QUOTE_CHARS]]))
            .and_then(|i| {
                (QUOTE_CHARS..=SHARED_CHARS)
                    .rev()
                    .filter_map(|n| Some(&t[at[i]..*at.get(i + n)?]))
                    .find(|run| sources.iter().any(|(_, s)| s.contains(run)))
            })
            .or_else(|| short.iter().copied().find(|s| t.contains(s)));
        Some((field, quote?.to_string()))
    })
}

/// The judge's answer checked against its schema, the lines of `condensed`
/// and the no-quote rule. The error says what failed.
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
    let mut texts = vec![("asked".to_string(), v.asked.as_str()), ("notes".to_string(), v.notes.as_str())];
    for (kind, list) in [("strengths", &v.strengths), ("frictions", &v.frictions)] {
        texts.extend(list.iter().enumerate().map(|(i, f)| (format!("{kind}[{i}]"), f.text.as_str())));
    }
    if let Some((field, text)) = quoting(texts.into_iter(), &records) {
        return Err(format!("{field} quotes the session: \"{text}\""));
    }
    Ok(v)
}

/// What one judge call returned: its reported cost, none from a judge that
/// reports none, and the answer or why there is none.
struct Reply {
    cost: Option<u64>,
    answer: std::result::Result<Value, NoAnswer>,
}

/// Why a judge call gave no answer.
enum NoAnswer {
    /// Over its cap, past its time, or an answer that does not parse: retried as it was.
    Retry(String),
    /// A judge that failed without an answer: a login, key or network failure,
    /// or a signal skima did not send. The run stops.
    Stop(String),
}

/// Claude's `--output-format json` result; the cost stays decimal text.
#[derive(Deserialize)]
struct ClaudeOutput<'a> {
    #[serde(borrow)]
    total_cost_usd: Option<&'a RawValue>,
    #[serde(default)]
    is_error: bool,
    subtype: Option<String>,
    terminal_reason: Option<String>,
    result: Option<String>,
    structured_output: Option<Value>,
}

/// The variables that bill an API key or a cloud account instead of a login:
/// Claude's key and token, its cloud routes, and the keys Codex and Pi may read.
const BILLED: [&str; 7] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
];

/// Removes [`BILLED`] from `cmd`'s environment under `subscription`.
fn scrub(cmd: &mut Command, billing: Billing) {
    if billing == Billing::Subscription {
        for var in BILLED {
            cmd.env_remove(var);
        }
    }
}

/// How long `claude auth status` may run before the login check fails.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);

/// Under `subscription`, the check a run makes once before its first judge
/// call, in the environment and the kind of fresh empty directory the calls
/// get: Claude's `auth status` must report a claude.ai or OAuth-token login,
/// and the user's Codex `auth.json` a ChatGPT login. Pi picks its provider
/// from its own settings, so it is not checked. The error says what would be
/// billed, or why the login could not be read.
pub fn login(a: &Audit) -> Result<()> {
    if a.judge.billing == Billing::Api {
        return Ok(());
    }
    match a.judge.agent {
        Agent::Claude => {
            let (_, dir) = run_dir(a.data)?;
            let checked = claude_login(OsStr::new("claude"), &dir, AUTH_TIMEOUT);
            fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            checked
        }
        Agent::Codex => {
            let path = a.codex_auth.display();
            let unread = |why: String| {
                format!(
                    "{path}: {why}; the Codex judge needs a ChatGPT login: run codex login, or set billing = \"api\" under [judge]"
                )
            };
            let bytes = fs::read(a.codex_auth).map_err(|e| unread(e.to_string()))?;
            let auth: Value = serde_json::from_slice(&bytes).map_err(|e| unread(format!("not JSON: {e}")))?;
            if auth["auth_mode"] == "chatgpt" {
                return Ok(());
            }
            Err(format!(
                "{path}: no ChatGPT login for the Codex judge; run codex login, or set billing = \"api\" under [judge]"
            ))
        }
        Agent::Pi => Ok(()),
    }
}

/// `program auth status` run in `dir` without [`BILLED`], killed after
/// `limit`: Ok for a claude.ai or OAuth-token login.
fn claude_login(program: &OsStr, dir: &Path, limit: Duration) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(["auth", "status"]).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    scrub(&mut cmd, Billing::Subscription);
    let failed = |e: std::io::Error| format!("claude auth status: {e}");
    let check = "check it with claude auth status, or set billing = \"api\" under [judge]";
    let Some(out) = wait(cmd.spawn().map_err(failed)?, None, limit).map_err(failed)? else {
        let secs = limit.as_secs();
        return Err(format!("claude auth status gave no answer within {secs} seconds; {check}"));
    };
    let status: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    match (out.status.success(), status["authMethod"].as_str()) {
        (true, Some("claude.ai" | "oauth_token")) => Ok(()),
        (_, Some(method)) if !matches!(method, "claude.ai" | "oauth_token") => Err(format!(
            "the Claude judge would bill {method}; log in with claude /login, or set billing = \"api\" under [judge]"
        )),
        _ => Err(format!("could not read the Claude judge's login from claude auth status ({}); {check}", out.status)),
    }
}

/// A fresh empty directory under `<data>/audit-runs/`, and its unique name.
fn run_dir(data: &Path) -> Result<(String, PathBuf)> {
    let unique = format!("{}-{}", std::process::id(), Timestamp::now().as_nanosecond());
    let runs = data.join("audit-runs");
    let dir = runs.join(&unique);
    fs::create_dir_all(&runs).and_then(|()| fs::create_dir(&dir)).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok((unique, dir))
}

/// One judge call: [`command`]'s argv run in a fresh empty directory
/// under `<data>/audit-runs/`, with the files it names in a fresh temporary
/// directory, both removed after. Claude gets `--max-budget-usd <cap>` and the
/// input on stdin; Codex gets the prompt and the input on stdin and the scratch
/// `CODEX_HOME`; Pi reads the input file. A Claude call that ran but reported
/// no cost is counted at its cap, the most it could have spent short of one
/// turn's overshoot, so the budget still binds. A judge that failed without an
/// answer is a [`NoAnswer::Stop`] with the first five lines of its stderr:
/// Claude's `api_error` or output that is no JSON, at the reported cost or $0,
/// and a Claude judge killed by a signal skima did not send, at its cap; a
/// Codex or Pi judge that exited nonzero with no answer.
fn call(a: &Audit, stage: &Stage, cap: Option<u64>, input: &str) -> Result<Reply> {
    let argv = command(a.judge, stage);
    let (unique, dir) = run_dir(a.data)?;
    let scratch = if a.judge.agent == Agent::Codex { Some(codex_home(a)?) } else { None };
    let files = std::env::temp_dir().join(format!("skima-judge-{unique}"));
    let result = files.join("result.json");
    let output = DirBuilder::new().mode(0o700).create(&files).and_then(|()| {
        let mut args: Vec<OsString> = Vec::new();
        for arg in &argv[1..] {
            args.push(match arg.as_str() {
                SCHEMA_FILE => {
                    let path = files.join("schema.json");
                    fs::write(&path, stage.schema.as_str())?;
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
            Agent::Codex => Some(format!("{}\n{input}", stage.prompt)),
            Agent::Pi => None,
        };
        let mut cmd = Command::new(&argv[0]);
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
        scrub(&mut cmd, a.judge.billing);
        wait(cmd.spawn()?, stdin, JUDGE_TIMEOUT)
    });
    let written = fs::read(&result);
    let removed = fs::remove_dir_all(&dir);
    let cleaned = fs::remove_dir_all(&files);
    let output = output.map_err(|e| format!("{}: {e}", argv[0]))?;
    removed.map_err(|e| format!("{}: {e}", dir.display()))?;
    cleaned.map_err(|e| format!("{}: {e}", files.display()))?;
    if scratch.is_some() {
        codex_home(a)?;
    }
    let Some(output) = output else {
        // A killed Claude judge is counted at its cap, as one reporting no cost.
        let cost = (a.judge.agent == Agent::Claude).then(|| cap.unwrap_or_default());
        let minutes = JUDGE_TIMEOUT.as_secs() / 60;
        let answer = Err(NoAnswer::Retry(format!("no answer within {minutes} minutes; the judge was killed")));
        return Ok(Reply { cost, answer });
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let failed = || format!("({}): {}", output.status, stderr.lines().next().unwrap_or_default());
    let stop = |result: Option<&str>| {
        let result = result.map(|r| format!(": {r}")).unwrap_or_default();
        let lines: String = stderr
            .lines()
            .take(STDERR_LINES)
            .map(|l| format!("\n  {}", l.chars().take(STDERR_CHARS).collect::<String>()))
            .collect();
        NoAnswer::Stop(format!("the judge failed without an answer ({}){result}{lines}", output.status))
    };
    // No answer stops the run after a nonzero exit and is retried after a zero one.
    let exited = |answer: std::result::Result<Value, String>| {
        answer.map_err(|e| {
            if output.status.success() { NoAnswer::Retry(format!("{e} {}", failed())) } else { stop(Some(&e)) }
        })
    };
    Ok(match a.judge.agent {
        Agent::Claude => {
            let cap = cap.unwrap_or_default();
            if output.status.signal().is_some() {
                return Ok(Reply { cost: Some(cap), answer: Err(stop(None)) });
            }
            let Ok(out) = serde_json::from_slice::<ClaudeOutput>(&output.stdout) else {
                return Ok(Reply { cost: Some(0), answer: Err(stop(None)) });
            };
            let cost = out.total_cost_usd.map(|c| parse_usd(c.get())).transpose()?;
            if out.is_error && out.terminal_reason.as_deref() == Some("api_error") {
                return Ok(Reply { cost: Some(cost.unwrap_or(0)), answer: Err(stop(out.result.as_deref())) });
            }
            let answer = match out.structured_output {
                Some(v) if !out.is_error => Ok(v),
                _ => Err(NoAnswer::Retry(format!("no answer ({})", out.subtype.as_deref().unwrap_or("error")))),
            };
            Reply { cost: Some(cost.unwrap_or(cap)), answer }
        }
        Agent::Codex => {
            let written = written.ok().and_then(|b| serde_json::from_slice(&b).ok());
            Reply { cost: None, answer: exited(written.ok_or_else(|| "no answer in the -o file".into())) }
        }
        Agent::Pi => Reply { cost: None, answer: exited(pi_answer(&output.stdout)) },
    })
}

/// How long one judge call may run before it is killed and counted as invalid.
const JUDGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The most stderr lines a stopped judge prints, and the most of each.
const STDERR_LINES: usize = 5;
const STDERR_CHARS: usize = 200;

/// How long a judge past its limit has to exit on SIGTERM before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(5);

/// `child`'s output once it exits, after writing `stdin` to it; `None` when it
/// was still running after `limit` and was killed: SIGTERM first, which
/// launchers such as Codex's npm shim forward to the process doing the work
/// and SIGKILL would orphan, then SIGKILL after [`KILL_GRACE`]. The pipes are read on
/// detached threads given a second after the exit, so a grandchild left
/// holding one costs its output, not a hang.
fn wait(mut child: Child, stdin: Option<String>, limit: Duration) -> std::io::Result<Option<Output>> {
    // A judge that exits without reading its input closes the pipe; its
    // output says why.
    if let (Some(mut pipe), Some(stdin)) = (child.stdin.take(), stdin) {
        std::thread::spawn(move || pipe.write_all(stdin.as_bytes()));
    }
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        if let Some(mut pipe) = pipe {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                let _ = tx.send(bytes);
            });
        }
        rx
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            // SAFETY: kill(2) on our own child, unreaped, so its pid is not reused.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
            let grace = Instant::now() + KILL_GRACE;
            while child.try_wait()?.is_none() && Instant::now() < grace {
                std::thread::sleep(Duration::from_millis(10));
            }
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let read = |rx: mpsc::Receiver<Vec<u8>>| rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    Ok(Some(Output { status, stdout: read(stdout), stderr: read(stderr) }))
}

/// The input of a retry: the first call's, and why its answer was rejected.
/// A call that gave no answer is retried as it was.
fn retry<'i>(input: &'i str, why: Option<&str>) -> Cow<'i, str> {
    match why {
        None => Cow::Borrowed(input),
        Some(why) => {
            let note = format!("Your previous answer was rejected: {why}. Answer again with that fixed.");
            Cow::Owned(format!("{input}\n{}", frame("rejected-answer", &note)))
        }
    }
}

/// The user's Codex `auth.json`: in the configured Codex home, else `~/.codex`.
pub fn codex_auth(homes: &[Home]) -> Result<PathBuf> {
    Ok(agent_home(homes, Agent::Codex, ".codex")?.join("auth.json"))
}

/// The user's Pi `settings.json`: in the configured Pi home, else `~/.pi/agent`.
pub fn pi_settings(homes: &[Home]) -> Result<PathBuf> {
    Ok(agent_home(homes, Agent::Pi, ".pi/agent")?.join("settings.json"))
}

/// The configured home of `agent`, else `~/<default>`.
fn agent_home(homes: &[Home], agent: Agent, default: &str) -> Result<PathBuf> {
    Ok(match homes.iter().find(|h| h.agent == agent) {
        Some(h) => h.path.clone(),
        None => PathBuf::from(crate::config::home_dir()?).join(default),
    })
}

/// `<data>/codex-judge-home` holding only a symlink to the user's `auth.json`,
/// made afresh so nothing an earlier call left there survives.
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
    judge: JudgeRecord,
    prompt: &'static str,
    result: &'a Verdict,
}

/// Who judged a record, and the exact reported cost of its calls.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JudgeRecord {
    pub agent: Agent,
    pub model: Option<String>,
    /// None from a judge that reports no cost (Codex, Pi).
    pub cost_usd: Option<Box<RawValue>>,
    /// None in records written before skima recorded it: unknown.
    #[serde(default)]
    pub billing: Option<Billing>,
}

impl JudgeRecord {
    fn new(judge: &Judge, cost: Option<u64>) -> JudgeRecord {
        JudgeRecord {
            agent: judge.agent,
            model: judge.model.clone(),
            cost_usd: cost.map(|c| RawValue::from_string(exact(c)).expect("a decimal is JSON")),
            billing: Some(judge.billing),
        }
    }
}

/// Now, as records write `audited_at`.
fn stamp() -> String {
    Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Appends `record` to `audits.jsonl` under `scan.lock`.
fn append(data: &Path, record: &impl Serialize) -> Result<()> {
    let line = serde_json::to_string(record).expect("record serialises") + "\n";
    let _lock = ledger::lock(data)?;
    ledger::append_lines(&data.join("audits.jsonl"), line)
}

/// One audit run's settings.
pub struct Audit<'a> {
    pub skill: &'a str,
    pub source: &'a Path,
    pub data: &'a Path,
    pub judge: &'a Judge,
    /// `--max-budget-usd` in micro-dollars; the Claude judge's only.
    pub budget: Option<u64>,
    /// The user's Codex `auth.json`, which the Codex judge's home links to.
    pub codex_auth: &'a Path,
    /// The user's Pi `settings.json`, which names the Pi judge's default model.
    pub pi_settings: &'a Path,
    /// The skill's grades and retros, for stage 2.
    pub evals: &'a Evals,
    /// The card's usage section, for stage 2.
    pub usage: &'a Value,
}

impl Audit<'_> {
    /// A dollar figure; under `subscription` it is Claude's client-side
    /// estimate, not a charge, and says so.
    fn usd(&self, micros: u64) -> String {
        let label = if self.judge.billing == Billing::Subscription { " API-equivalent" } else { "" };
        format!("${}{label}", cents(micros))
    }
}

/// What a run did: stage 1's counts and spend, the sessions the budget or a
/// stopped judge left for the next run, and stage 2's result.
pub struct Run<'c> {
    pub candidates: usize,
    pub judged: usize,
    pub dropped: usize,
    /// Micro-dollars, both stages; 0 with a judge that reports no cost.
    pub spent: u64,
    pub left: &'c [Candidate],
    /// A stage-1 judge failed without an answer: `left` starts at its session.
    pub stopped: bool,
    pub report: Synthesis,
}

/// Stage 2's result.
pub enum Synthesis {
    /// No new session record and no `--resynth`, or no session record at all.
    NotDue,
    Written(Box<SkillRecord>),
    /// Invalid twice; why the last answer failed.
    Dropped(String),
    /// Under a cent of the budget was left; never without a budget.
    Skipped,
    /// The judge failed without an answer; why.
    Stopped(String),
}

/// How judging one session, or the skill, ended.
enum Ruling<T> {
    /// A valid answer, and the reported cost of its calls.
    Valid(T, Option<u64>),
    /// Invalid twice; why the last answer failed.
    Dropped(String),
    /// Under a cent of the cap left before a call; why an earlier answer failed.
    Unfunded(Option<String>),
    /// The judge failed without an answer; why.
    Stopped(String),
}

/// The call-and-validate step both stages share: `stage`'s judge on `input`,
/// its answer checked by `check`, an invalid one retried once with why it was
/// rejected. Each call is capped at `limit` minus `spent`, adds its cost to
/// `spent`, and starts only on a cap of a cent or more; `progress` names it
/// while stderr is a terminal.
fn judge<T>(
    a: &Audit,
    stage: &Stage,
    limit: Option<u64>,
    spent: &mut u64,
    input: &str,
    progress: &str,
    check: impl Fn(Value) -> std::result::Result<T, String>,
) -> Result<Ruling<T>> {
    let mut cost = None;
    let (mut why, mut rejected) = (None, None);
    for _ in 0..2 {
        let cap = limit.map(|l| l.saturating_sub(*spent));
        if cap.is_some_and(|cap| cap < CENT) {
            return Ok(Ruling::Unfunded(why));
        }
        if std::io::stderr().is_terminal() {
            let cap = cap.map(|cap| format!(" (cap {})", a.usd(cap))).unwrap_or_default();
            eprintln!("{progress}{cap}");
        }
        let reply = call(a, stage, cap, &retry(input, rejected.as_deref()))?;
        *spent += reply.cost.unwrap_or_default();
        cost = reply.cost.map(|r| r + cost.unwrap_or_default());
        match reply.answer.map(&check) {
            Ok(Ok(answer)) => return Ok(Ruling::Valid(answer, cost)),
            Ok(Err(e)) => (why, rejected) = (Some(e.clone()), Some(e)),
            Err(NoAnswer::Retry(e)) => (why, rejected) = (Some(e), None),
            Err(NoAnswer::Stop(e)) => return Ok(Ruling::Stopped(e)),
        }
    }
    Ok(Ruling::Dropped(why.unwrap_or_default()))
}

/// Whether [`run`] would start a judge call: a candidate to judge, or
/// `resynth` with a session record to report on.
pub fn calls(a: &Audit, candidates: &[Candidate], resynth: bool) -> Result<bool> {
    Ok(!candidates.is_empty() || resynth && !session_records(a)?.is_empty())
}

/// Runs stage 1 on `candidates`, then stage 2 when stage 1 wrote a session
/// record or `resynth` is set and no judge stopped the run.
pub fn run<'c>(a: &Audit, candidates: &'c [Candidate], resynth: bool) -> Result<Run<'c>> {
    let mut run = judge_sessions(a, candidates)?;
    if !run.stopped && (run.judged > 0 || resynth) {
        run.report = synthesize(a, &mut run.spent)?;
    }
    Ok(run)
}

/// Stage 1: judges `candidates` one at a time; with a budget,
/// each call is capped at 75% of it minus what is spent, stopping once that is
/// under a cent.
/// Each valid answer is appended to `audits.jsonl` under `scan.lock` at once;
/// an invalid one is retried once, then dropped. A judge that failed without
/// an answer stops the run.
fn judge_sessions<'c>(a: &Audit, candidates: &'c [Candidate]) -> Result<Run<'c>> {
    let pool = a.budget.map(|b| b / 4 * 3);
    let mut run = Run {
        candidates: candidates.len(),
        judged: 0,
        dropped: 0,
        spent: 0,
        left: &[],
        stopped: false,
        report: Synthesis::NotDue,
    };
    for (i, c) in candidates.iter().enumerate() {
        let (skill, input) = input(a.source, c)?;
        let progress = format!("judging {}/{} {}", i + 1, candidates.len(), c.event.session);
        match judge(a, &SESSION, pool, &mut run.spent, &input, &progress, |v| validate(v, &c.condensed))? {
            Ruling::Valid(verdict, cost) => {
                let record = SessionRecord {
                    v: 1,
                    kind: "session",
                    skill: a.skill,
                    session: &c.event.session,
                    agent: c.event.agent,
                    src: &c.event.src,
                    session_ts: c.started().map(|t| t.to_string()),
                    audited_at: stamp(),
                    skill_commit: skill.commit.as_deref(),
                    condensed_chars: c.condensed.chars().count(),
                    judge: JudgeRecord::new(a.judge, cost),
                    prompt: SESSION_PROMPT_VERSION,
                    result: &verdict,
                };
                append(a.data, &record)?;
                run.judged += 1;
            }
            Ruling::Dropped(why) => {
                eprintln!("warning: {}: judge answer dropped after one retry: {why}", c.event.session);
                run.dropped += 1;
            }
            Ruling::Unfunded(_) => {
                run.left = &candidates[i..];
                break;
            }
            Ruling::Stopped(why) => {
                eprintln!("ERROR: {}: {why}", c.event.session);
                hint(a);
                run.left = &candidates[i..];
                run.stopped = true;
                break;
            }
        }
    }
    Ok(run)
}

/// Under `subscription`, names `billing = "api"` after a judge failure.
fn hint(a: &Audit) {
    if a.judge.billing == Billing::Subscription {
        eprintln!("hint: to judge with an API key instead, set billing = \"api\" under [judge]");
    }
}

impl Run<'_> {
    /// False, and the run exits 1, when stage 1 dropped every
    /// session it judged, a judge stopped the run, or stage 2 was due and
    /// wrote no record.
    pub fn ok(&self) -> bool {
        !self.stopped
            && (self.judged > 0 || self.dropped == 0)
            && matches!(self.report, Synthesis::NotDue | Synthesis::Written(_))
    }

    /// The run's summary as text.
    pub fn text(&self, a: &Audit) -> String {
        let skill = a.skill;
        let mut out = if self.candidates == 0 {
            format!("{skill}: no session to judge\n")
        } else {
            let spend = a.budget.map(|b| format!(", {} spent of {}", a.usd(self.spent), a.usd(b))).unwrap_or_default();
            format!("{skill}: {} judged, {} dropped{spend}\n", self.judged, self.dropped)
        };
        if !self.left.is_empty() {
            let why = if self.stopped { "the judge failed" } else { "budget used up" };
            out.push_str(&format!("{why}; left for the next run: {}\n", self.left.len()));
            for c in self.left {
                out.push_str(&format!("  {} {}\n", name(c.event.agent), c.event.session));
            }
        }
        match &self.report {
            Synthesis::NotDue => {}
            Synthesis::Written(r) => {
                out.push_str(&format!("{skill}: skill report over {} sessions: {}\n", r.sessions.len(), r.result.verdict));
                for (label, rate) in r.rates.labeled() {
                    out.push_str(&format!("  {label} {}: {}\n", rate.of(), rate.sessions.join(", ")));
                }
            }
            Synthesis::Dropped(why) => out.push_str(&format!("{skill}: skill report dropped after one retry: {why}\n")),
            Synthesis::Stopped(why) => {
                let why = why.lines().next().unwrap_or_default();
                out.push_str(&format!("{skill}: skill report stopped: {why}\n"));
            }
            Synthesis::Skipped => out.push_str(&format!(
                "{skill}: skill report skipped: {} of the budget is left, under $0.01; run `skima audit {skill} --resynth`\n",
                a.usd(a.budget.unwrap_or_default().saturating_sub(self.spent))
            )),
        }
        out
    }

    /// `audit --json`: the new skill record, the session records written
    /// and dropped, the spend (null with a judge that reports no cost), and the
    /// sessions left for the next run.
    pub fn json(&self, a: &Audit) -> String {
        let record = match &self.report {
            Synthesis::Written(r) => serde_json::to_value(r).expect("record serialises"),
            _ => Value::Null,
        };
        let spent: Option<Box<RawValue>> =
            a.budget.map(|_| RawValue::from_string(exact(self.spent)).expect("a decimal is JSON"));
        let left: Vec<&str> = self.left.iter().map(|c| c.event.session.as_str()).collect();
        let out = json!({"record": record, "written": self.judged, "dropped": self.dropped, "spent_usd": spent, "left": left});
        format!("{out}\n")
    }
}

const VERDICT_CHARS: usize = 400;
const REPORT_CHARS: usize = 300;
const REPORT_ITEMS: usize = 5;
const REPORT_EXAMPLES: usize = 3;
/// Stage 2's input cuts.
const COMMITS: &str = "-30";
const GRADE_BODY_CHARS: usize = 6_000;
const RETRO_BODIES: usize = 3;
const RETRO_BODY_CHARS: usize = 4_000;

/// The JSON schema of the stage-2 answer, passed as `--json-schema`.
pub static SKILL_SCHEMA: LazyLock<String> = LazyLock::new(|| {
    let text = |max: usize| json!({"type": "string", "maxLength": max});
    let sessions = json!({"type": "array", "minItems": 1, "items": {"type": "string"}});
    let object = |fields: Vec<(&str, Value)>| {
        let required: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
        let properties: serde_json::Map<String, Value> = fields.into_iter().map(|(k, v)| (k.into(), v)).collect();
        json!({"type": "object", "additionalProperties": false, "required": required, "properties": properties})
    };
    let list = |items: Value, max: usize| json!({"type": "array", "maxItems": max, "items": items});
    let cited = object(vec![("text", text(REPORT_CHARS)), ("sessions", sessions.clone())]);
    object(vec![
        ("verdict", text(VERDICT_CHARS)),
        ("patterns", list(text(REPORT_CHARS), REPORT_ITEMS)),
        ("works", list(cited.clone(), REPORT_ITEMS)),
        ("failures", list(cited, REPORT_ITEMS)),
        (
            "improvements",
            list(
                object(vec![("change", text(REPORT_CHARS)), ("reason", text(REPORT_CHARS)), ("sessions", sessions)]),
                REPORT_ITEMS,
            ),
        ),
        ("versus_grades", text(VERDICT_CHARS)),
        (
            "examples",
            list(
                object(vec![
                    ("situation", text(REPORT_CHARS)),
                    ("asked", text(REPORT_CHARS)),
                    ("happened", text(REPORT_CHARS)),
                    ("tip", text(REPORT_CHARS)),
                    ("session", json!({"type": "string"})),
                ]),
                REPORT_EXAMPLES,
            ),
        ),
    ])
    .to_string()
});

/// A finding of the skill report and the sessions behind it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Cited {
    pub text: String,
    pub sessions: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Improvement {
    pub change: String,
    pub reason: String,
    pub sessions: Vec<String>,
}

/// A curated example: paraphrased, citing one session.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Example {
    pub situation: String,
    pub asked: String,
    pub happened: String,
    pub tip: String,
    pub session: String,
}

/// The stage-2 judge's answer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SkillReport {
    pub verdict: String,
    pub patterns: Vec<String>,
    pub works: Vec<Cited>,
    pub failures: Vec<Cited>,
    /// Ranked, most important first.
    pub improvements: Vec<Improvement>,
    pub versus_grades: String,
    pub examples: Vec<Example>,
}

/// `n of N`, and the sessions counted in `n`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Rate {
    pub n: usize,
    pub of: usize,
    pub sessions: Vec<String>,
}

impl Rate {
    pub fn of(&self) -> String {
        format!("{} of {}", self.n, self.of)
    }
}

/// The rates skima computes from session records, never the judge.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Rates {
    pub followed: Rate,
    pub achieved: Rate,
    pub abandoned_or_redone: Rate,
}

impl Rates {
    fn new(judged: &[Judged]) -> Rates {
        let rate = |hit: fn(&Verdict) -> bool| {
            let sessions: Vec<String> = judged.iter().filter(|j| hit(&j.result)).map(|j| j.session.clone()).collect();
            Rate { n: sessions.len(), of: judged.len(), sessions }
        };
        Rates {
            followed: rate(|v| v.followed == Followed::Yes),
            achieved: rate(|v| v.outcome == Outcome::Achieved),
            abandoned_or_redone: rate(|v| v.outcome == Outcome::Abandoned || v.redone),
        }
    }

    /// Each rate with the words it is printed with.
    pub fn labeled(&self) -> [(&'static str, &Rate); 3] {
        [("followed", &self.followed), ("achieved", &self.achieved), ("abandoned or redone", &self.abandoned_or_redone)]
    }
}

/// A skill record of `audits.jsonl`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SkillRecord {
    pub v: u32,
    pub kind: String,
    pub skill: String,
    pub audited_at: String,
    pub sessions: Vec<String>,
    pub judge: JudgeRecord,
    pub prompt: String,
    pub rates: Rates,
    pub result: SkillReport,
}

/// The latest skill record of each skill in `<data>/audits.jsonl`; older ones
/// stay in the file.
pub fn latest(data: &Path) -> Result<BTreeMap<String, SkillRecord>> {
    Ok(parsed::<SkillRecord>(data)?.into_iter().filter(|r| r.kind == "skill").map(|r| (r.skill.clone(), r)).collect())
}

/// A session record as stage 2 reads it.
#[derive(Serialize, Deserialize)]
struct Judged {
    #[serde(skip_serializing)]
    kind: String,
    #[serde(skip_serializing)]
    skill: String,
    session: String,
    agent: Agent,
    session_ts: Option<String>,
    skill_commit: Option<String>,
    result: Verdict,
}

/// The stage-2 answer checked against its schema and the session records it
/// may cite. The error says what failed.
fn validate_report(answer: Value, judged: &[Judged]) -> std::result::Result<SkillReport, String> {
    let r: SkillReport = serde_json::from_value(answer).map_err(|e| format!("not the answer schema: {e}"))?;
    let over = |s: &String, max: usize| s.chars().count() > max;
    if over(&r.verdict, VERDICT_CHARS) || over(&r.versus_grades, VERDICT_CHARS) {
        return Err(format!("verdict or versus_grades over {VERDICT_CHARS} characters"));
    }
    let texts = r
        .patterns
        .iter()
        .chain(r.works.iter().chain(&r.failures).map(|c| &c.text))
        .chain(r.improvements.iter().flat_map(|i| [&i.change, &i.reason]))
        .chain(r.examples.iter().flat_map(|e| [&e.situation, &e.asked, &e.happened, &e.tip]));
    let lists = [r.patterns.len(), r.works.len(), r.failures.len(), r.improvements.len()];
    if texts.into_iter().any(|t| over(t, REPORT_CHARS))
        || lists.iter().any(|n| *n > REPORT_ITEMS)
        || r.examples.len() > REPORT_EXAMPLES
    {
        return Err(format!(
            "over {REPORT_ITEMS} items, {REPORT_EXAMPLES} examples, or a text over {REPORT_CHARS} characters"
        ));
    }
    let cited = r
        .works
        .iter()
        .chain(&r.failures)
        .flat_map(|c| &c.sessions)
        .chain(r.improvements.iter().flat_map(|i| &i.sessions))
        .chain(r.examples.iter().map(|e| &e.session));
    if let Some(s) = cited.into_iter().find(|s| !judged.iter().any(|j| j.session == **s)) {
        return Err(format!("cites session {s}, which has no session record"));
    }
    Ok(r)
}

/// `text` between `<open>` and its closing tag; a closing tag inside is
/// written `<\/tag>`, so the text cannot end its own frame.
fn frame(open: &str, text: &str) -> String {
    let tag = open.split(' ').next().unwrap_or(open);
    let close = format!("</{tag}>");
    format!("<{open}>\n{}\n{close}\n", text.trim_end().replace(&close, &format!("<\\/{tag}>")))
}

/// The stage-2 judge's input: the current `SKILL.md`, its last
/// 30 commits, every grade's and retro's frontmatter with the latest grade
/// body and the latest three retro bodies cut, the card's usage section, and
/// every session record of the skill.
fn synthesis_input(a: &Audit, judged: &[Judged]) -> Result<String> {
    let dir = a.source.join(a.skill);
    let path = dir.join("SKILL.md");
    let skill = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let commits = git(&dir, &["log", COMMITS, "--format=%as %s", "--", "."]).unwrap_or_default();
    // Each file's frontmatter and body; one that no longer reads is left out.
    let split = |file: &Path| fs::read_to_string(file).ok().and_then(|t| crate::yaml::split_frontmatter(&t).ok());
    let cut = |body: &str, max: usize| body.chars().take(max).collect::<String>();
    let fronts = |files: &mut dyn Iterator<Item = &Path>| -> String {
        files
            .filter_map(|f| {
                let name = f.file_name()?.to_string_lossy();
                Some(format!("--- {name}\n{}\n", split(f)?.0))
            })
            .collect()
    };
    let grades: Vec<&Path> = a.evals.grades.iter().map(|g| g.eval.file.as_path()).collect();
    let retros: Vec<&Path> = a.evals.retros.iter().map(|r| r.file.as_path()).collect();
    let latest_grade = grades.last().and_then(|f| split(f)).map(|(_, b)| cut(&b, GRADE_BODY_CHARS));
    let retro_bodies: String = retros
        .iter()
        .rev()
        .take(RETRO_BODIES)
        .filter_map(|f| {
            Some(format!("--- {}\n{}\n", f.file_name()?.to_string_lossy(), cut(&split(f)?.1, RETRO_BODY_CHARS)))
        })
        .collect();
    let records: String = judged.iter().map(|j| serde_json::to_string(j).expect("record serialises") + "\n").collect();
    Ok([
        frame(&format!("skill name=\"{}\"", a.skill), &skill),
        frame("commits", &commits),
        frame("grades", &fronts(&mut grades.iter().copied())),
        frame("latest-grade", latest_grade.as_deref().unwrap_or_default()),
        frame("retros", &fronts(&mut retros.iter().copied())),
        frame("latest-retros", &retro_bodies),
        frame("usage", &serde_json::to_string_pretty(a.usage).expect("usage serialises")),
        frame("session-records", &records),
    ]
    .join("\n"))
}

/// Every session record of the skill.
fn session_records(a: &Audit) -> Result<Vec<Judged>> {
    Ok(parsed::<Judged>(a.data)?.into_iter().filter(|j| j.kind == "session" && j.skill == a.skill).collect())
}

/// Stage 2: one call over every session record of the skill; with
/// a budget, capped at it minus what is spent, and skipped when that is under
/// a cent. A valid answer is appended as the skill record with the rates skima
/// computes; an invalid one is retried once, then dropped. A judge that
/// failed without an answer stops the run.
fn synthesize(a: &Audit, spent: &mut u64) -> Result<Synthesis> {
    let judged = session_records(a)?;
    if judged.is_empty() {
        return Ok(Synthesis::NotDue);
    }
    let input = synthesis_input(a, &judged)?;
    let progress = format!("reporting on {} sessions", judged.len());
    Ok(match judge(a, &SKILL, a.budget, spent, &input, &progress, |v| validate_report(v, &judged))? {
        Ruling::Valid(result, cost) => {
            let record = SkillRecord {
                v: 1,
                kind: "skill".into(),
                skill: a.skill.into(),
                audited_at: stamp(),
                sessions: judged.iter().map(|j| j.session.clone()).collect(),
                judge: JudgeRecord::new(a.judge, cost),
                prompt: SKILL_PROMPT_VERSION.into(),
                rates: Rates::new(&judged),
                result,
            };
            append(a.data, &record)?;
            Synthesis::Written(Box::new(record))
        }
        Ruling::Dropped(why) | Ruling::Unfunded(Some(why)) => Synthesis::Dropped(why),
        Ruling::Unfunded(None) => Synthesis::Skipped,
        Ruling::Stopped(why) => {
            eprintln!("ERROR: skill report: {why}");
            hint(a);
            Synthesis::Stopped(why)
        }
    })
}

/// Input tokens estimated from condensed characters.
pub fn estimate_tokens(chars: usize) -> usize {
    chars / 4
}

/// The dollar cost of `chars` condensed characters at the rate earlier session
/// records paid per condensed character, with how many records set it.
fn estimate_cost(data: &Path, chars: usize) -> Result<Option<(u64, usize)>> {
    let (mut cost, mut seen, mut n) = (0u128, 0u128, 0usize);
    for r in parsed::<Recorded>(data)? {
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

/// The model the Pi judge uses: `[judge] model`, else the default provider
/// and model of the user's Pi `settings.json`.
fn pi_model(a: &Audit) -> String {
    if let Some(m) = &a.judge.model {
        return m.clone();
    }
    let settings: Value =
        fs::read(a.pi_settings).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    match (settings["defaultProvider"].as_str(), settings["defaultModel"].as_str()) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        _ => format!("unknown; {} names no defaultProvider and defaultModel", a.pi_settings.display()),
    }
}

/// `skima audit --dry-run`: each session the audit would judge with its agent,
/// project and condensed size, the judge command, the billing (and the model
/// for Pi), the token estimate and, once session records exist, the dollar
/// estimate.
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
            let ts = render::local(&c.event.ts, &tz, render::MINUTE);
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
    let quoted: Vec<String> = command(a.judge, &SESSION)
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
    let model = match a.judge.agent {
        Agent::Pi => format!("model: {}\n", pi_model(a)),
        _ => String::new(),
    };
    let mut out = format!(
        "{skill}: {n} session{} to judge with the {} judge\n\n{}\njudge: {judge}\nbilling: {}\n{model}input: {total} condensed characters, about {} tokens\n",
        if n == 1 { "" } else { "s" },
        name(a.judge.agent),
        render::table(&header, &rows, tty),
        a.judge.billing.name(),
        estimate_tokens(total),
    );
    if let (Some(_), Some((cost, records))) = (a.budget, estimate_cost(a.data, total)?) {
        out.push_str(&format!(
            "cost: about {}, at the rate of {records} earlier session record{}\n",
            a.usd(cost),
            if records == 1 { "" } else { "s" }
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Child {
        Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// `claude_login` on a stub whose `auth status` runs `script`.
    fn login_with(script: &str, limit: Duration) -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("claude");
        fs::write(&stub, format!("#!/bin/sh\n{script}")).unwrap();
        Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
        claude_login(stub.as_os_str(), dir.path(), limit)
    }

    #[test]
    fn a_claude_login_that_cannot_be_read_fails_the_check() {
        let limit = Duration::from_secs(30);
        assert_eq!(login_with("echo '{\"authMethod\":\"claude.ai\"}'\n", limit), Ok(()));
        let started = Instant::now();
        let err = login_with("sleep 10\n", Duration::from_millis(100)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5), "the check waited out its stub");
        assert!(err.starts_with("claude auth status gave no answer within 0 seconds; check it with"), "{err}");
        for script in ["echo 'Not logged in'\nexit 1\n", "echo '{\"authMethod\":\"claude.ai\"}'\nexit 1\n", ""] {
            let err = login_with(script, limit).unwrap_err();
            assert!(err.starts_with("could not read the Claude judge's login from claude auth status ("), "{err}");
            assert!(err.ends_with("check it with claude auth status, or set billing = \"api\" under [judge]"), "{err}");
        }
    }

    #[test]
    fn wait_returns_the_output_of_a_judge_that_exits() {
        let out = wait(sh("cat; echo e >&2"), Some("in".into()), Duration::from_secs(30)).unwrap().unwrap();
        assert!(out.status.success());
        assert_eq!((out.stdout.as_slice(), out.stderr.as_slice()), (b"in".as_slice(), b"e\n".as_slice()));
    }

    #[test]
    fn wait_kills_a_judge_past_its_limit_even_when_a_grandchild_holds_its_pipes() {
        let started = Instant::now();
        assert!(wait(sh("sleep 10 & sleep 10"), None, Duration::from_millis(100)).unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
        // A launcher that forwards SIGTERM takes the process doing the work with it.
        let pid = tempfile::NamedTempFile::new().unwrap();
        let launcher = format!("sleep 10 & echo $! > {}; trap 'kill $!; exit' TERM; wait", pid.path().display());
        assert!(wait(sh(&launcher), None, Duration::from_millis(200)).unwrap().is_none());
        let worker = fs::read_to_string(pid.path()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let alive = Command::new("kill").args(["-0", worker.trim()]).stderr(Stdio::null()).status().unwrap();
        assert!(!alive.success(), "the worker outlived its launcher");
        // An exited judge whose grandchild keeps stdout open costs its output, not a hang.
        let started = Instant::now();
        let out = wait(sh("sleep 10 & echo x"), None, Duration::from_secs(30)).unwrap().unwrap();
        assert!(out.status.success());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
