//! `mazo audit`. Stage 1: selection, the judge's input, one
//! Claude, Codex, Pi, command or http judge call per session, validation of its answer, and
//! the session records in `audits.jsonl`. Stage 2: one call over every
//! session record of the skill, the rates mazo computes, and the skill
//! record the card shows. `--dry-run` shows what a run would judge and calls
//! nothing.
//!
//! [`select`] picks and condenses the sessions, [`session`] checks a stage-1
//! answer, [`skill`] runs stage 2, [`judges`] holds each judge agent, and
//! [`command`] is `mazo audit` as the CLI and the TUI run it; this module is
//! the run both stages share.

mod command;
mod judges;
mod select;
mod session;
mod skill;
mod vet;

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, mpsc};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};

pub use command::{Args, LIMIT, Planned, command, plan};
use judges::NoAnswer;
pub(crate) use judges::wait;
pub use judges::{Backend, backend};
pub use select::{Candidate, MAX_CHARS, SkillText, input, select, skill_at};
pub use session::{Finding, Followed, Outcome, SESSION_PROMPT, SESSION_SCHEMA, Verdict, validate};
use session::{SESSION, SESSION_PROMPT_VERSION};
pub use skill::{
    Cited, Example, Improvement, Rate, Rates, SKILL_PROMPT, SKILL_SCHEMA, SkillRecord, SkillReport, latest,
};
use skill::{session_records, synthesize};
pub use vet::{Quality, VET_PROMPT, VET_SCHEMA, Vet, VetVerdict, Worth, vet, vet_input};

use crate::Result;
use crate::config::{Billing, Judge, JudgeAgent};
use crate::evals::Evals;
use crate::ledger::{self, Agent, name};
use crate::render::{self, Align};

/// One cent in micro-dollars: no call starts with a smaller cap.
pub const CENT: u64 = 10_000;

/// A run's budget when `--max-budget-usd` is not given, for a judge that
/// reports its cost: one dollar.
pub const DEFAULT_BUDGET: u64 = 1_000_000;

/// Why a run the pane cancelled failed.
pub(crate) const CANCELLED: &str = "cancelled";

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

/// The schema of a string of at most `max` characters, the pieces both
/// answer schemas are built from.
fn text(max: usize) -> Value {
    json!({"type": "string", "maxLength": max})
}

/// The schema of an array of at most `max` `items`.
fn list(items: Value, max: usize) -> Value {
    json!({"type": "array", "maxItems": max, "items": items})
}

/// The schema of an object that requires every one of `fields` and allows no other.
fn object(fields: Vec<(&str, Value)>) -> Value {
    let required: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
    let properties: serde_json::Map<String, Value> = fields.into_iter().map(|(k, v)| (k.into(), v)).collect();
    json!({"type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

const FOR_CHARS: usize = 300;
const TRY_CHARS: usize = 200;
const TRIES: usize = 3;

/// The `for` and `try` fields both answer schemas end with.
fn for_try() -> [(&'static str, Value); 2] {
    [("for", text(FOR_CHARS)), ("try", list(text(TRY_CHARS), TRIES))]
}

/// The checks on an answer's `for` and `try`. The error says what failed.
fn check_for_try(r#for: &str, r#try: &[String]) -> std::result::Result<(), String> {
    let over = |s: &str, max: usize| s.chars().count() > max;
    if r#for.trim().is_empty() || over(r#for, FOR_CHARS) {
        return Err(format!("for is empty or over {FOR_CHARS} characters"));
    }
    if r#try.len() > TRIES || r#try.iter().any(|t| t.trim().is_empty() || over(t, TRY_CHARS)) {
        return Err(format!("more than {TRIES} try items, or one empty or over {TRY_CHARS} characters"));
    }
    Ok(())
}

/// A stage's shipped prompt and the JSON schema of its answer.
pub struct Stage {
    pub prompt: &'static str,
    pub schema: &'static LazyLock<String>,
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

/// The token every frame's tag name ends in: 64 bits from the OS-seeded
/// keys of a [`RandomState`](std::collections::hash_map::RandomState), fresh to each process.
static NONCE: LazyLock<String> = LazyLock::new(|| {
    use std::hash::BuildHasher;
    format!("{:016x}", std::collections::hash_map::RandomState::new().hash_one(()))
});

/// `text` in the frame [`enclose`] makes. The nonce is cut from `text`, so
/// no text can close its frame or forge another; any other tag in it is text.
fn frame(open: &str, text: &str) -> String {
    enclose(open, &text.replace(&*NONCE, "[nonce]"))
}

/// `body` between `<open>`, its tag name suffixed with [`NONCE`], and the
/// matching closing tag; `body` is frames, or text cut by [`frame`].
fn enclose(open: &str, body: &str) -> String {
    let tag = open.split(' ').next().unwrap_or(open);
    let (attrs, nonce) = (&open[tag.len()..], &*NONCE);
    format!("<{tag}-{nonce}{attrs}>\n{}\n</{tag}-{nonce}>\n", body.trim_end())
}

/// The input of a retry: the first call's, and why its answer was rejected.
/// A call that gave no answer is retried as it was.
fn retry(input: &str, why: Option<&str>) -> String {
    let Some(why) = why else { return input.into() };
    let note = format!("Your previous answer was rejected: {why}. Answer again with that fixed.");
    format!("{input}\n{}", frame("rejected-answer", &note))
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
    pub agent: JudgeAgent,
    pub model: Option<String>,
    /// None from a judge that reports no cost (Codex, Pi, command, http).
    pub cost_usd: Option<Box<RawValue>>,
    /// None in records written before mazo recorded it: unknown.
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
    /// The root the skill is read from: the source or the store.
    pub source: &'a Path,
    pub data: &'a Path,
    pub judge: &'a Judge,
    /// How `judge` is called.
    pub backend: &'a dyn Backend,
    /// `--max-budget-usd` in micro-dollars; only a judge that reports its cost has one.
    pub budget: Option<u64>,
    /// The skill's grades and retros, for stage 2.
    pub evals: &'a Evals,
    /// The card's usage section, for stage 2.
    pub usage: &'a Value,
    /// The TUI's job pane; without one the run reports on stderr.
    pub pane: Option<&'a Pane>,
}

/// The TUI's job pane: the lines a run reports go to `lines`, and setting
/// `cancel` kills the judge call in flight and fails the run.
pub struct Pane {
    pub lines: mpsc::Sender<String>,
    pub cancel: Arc<AtomicBool>,
}

/// `micros` as dollars; under `subscription` it is Claude's client-side
/// estimate, not a charge, and says so.
pub fn usd(judge: &Judge, micros: u64) -> String {
    let label = if judge.billing == Billing::Subscription { " API-equivalent" } else { "" };
    format!("${}{label}", cents(micros))
}

impl Audit<'_> {
    /// How this run calls its judge.
    pub fn caller(&self) -> Caller<'_> {
        Caller { data: self.data, judge: self.judge, backend: self.backend, pane: self.pane }
    }

    fn usd(&self, micros: u64) -> String {
        self.caller().usd(micros)
    }
}

/// What a judge call needs, and all it needs: the data directory its scratch
/// lives under, the `[judge]` config, the backend, and the pane a TUI job
/// reports to. Audit and vet both call through it.
#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub data: &'a Path,
    pub judge: &'a Judge,
    pub backend: &'a dyn Backend,
    /// Without one, the calls report on stderr and are never cancelled.
    pub pane: Option<&'a Pane>,
}

impl Caller<'_> {
    /// A dollar figure as [`usd`] words it.
    fn usd(&self, micros: u64) -> String {
        usd(self.judge, micros)
    }

    /// Reports `line` on the pane, else on stderr.
    fn note(&self, line: &str) {
        match self.pane {
            Some(pane) => _ = pane.lines.send(line.into()),
            None => eprintln!("{line}"),
        }
    }

    /// Whether the pane asked the run to stop.
    fn cancelled(&self) -> bool {
        self.pane.is_some_and(|p| p.cancel.load(Ordering::Relaxed))
    }

    /// Under `subscription`, the judge's login check, made once before a run's
    /// first call; under `api`, nothing is checked. A failed check is a usage
    /// error: no judge was started.
    pub fn login(&self) -> Result<()> {
        match self.judge.billing {
            Billing::Subscription => self.backend.login(self.data, &|| self.cancelled()),
            Billing::Api => Ok(()),
        }
    }

    /// Under `subscription`, names `billing = "api"` after a judge failure.
    fn hint(&self) {
        if self.judge.billing == Billing::Subscription {
            self.note("hint: to judge with an API key instead, set billing = \"api\" under [judge]");
        }
    }
}

/// What a run did: stage 1's counts and spend, the sessions the budget or a
/// stopped judge left for the next run, and stage 2's result.
#[derive(Default)]
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
#[derive(Default)]
pub enum Synthesis {
    /// No new session record and no `--resynth`, or no session record at all.
    #[default]
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
/// `spent`, and starts only on a cap of a cent or more and an uncancelled run;
/// `progress` names it on the pane, or while stderr is a terminal. A cancel
/// kills the call in flight and fails the run.
fn judge<T>(
    c: &Caller,
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
        if c.cancelled() {
            return Err(CANCELLED.into());
        }
        if c.pane.is_some() || std::io::stderr().is_terminal() {
            let cap = cap.map(|cap| format!(" (cap {})", c.usd(cap))).unwrap_or_default();
            c.note(&format!("{progress}{cap}"));
        }
        let input = retry(input, rejected.as_deref());
        let reply = c.backend.call(c.data, c.judge.billing, stage, cap, &input, &|| c.cancelled())?;
        *spent += reply.cost.unwrap_or_default();
        cost = reply.cost.map(|r| r + cost.unwrap_or_default());
        match reply.answer.map(|a| check(render::inert_value(a))) {
            Ok(Ok(answer)) => return Ok(Ruling::Valid(answer, cost)),
            Ok(Err(e)) => (why, rejected) = (Some(e.clone()), Some(e)),
            Err(NoAnswer::Retry(e)) => (why, rejected) = (Some(render::inert(&e)), None),
            Err(NoAnswer::Stop(e)) => return Ok(Ruling::Stopped(render::inert(&e))),
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
    let mut run = Run { candidates: candidates.len(), ..Run::default() };
    for (i, c) in candidates.iter().enumerate() {
        let (skill, input) = input(a.source, c)?;
        let progress = format!("judging {}/{} {}", i + 1, candidates.len(), c.event.session);
        match judge(&a.caller(), &SESSION, pool, &mut run.spent, &input, &progress, |v| validate(v, &c.condensed))? {
            Ruling::Valid(verdict, cost) => {
                let record = SessionRecord {
                    v: 1,
                    kind: "session",
                    skill: a.skill,
                    session: &c.event.session,
                    agent: c.event.agent,
                    src: &c.event.src,
                    session_ts: Some(c.started().to_string()),
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
                a.caller().note(&format!("warning: {}: judge answer dropped after one retry: {why}", c.event.session));
                run.dropped += 1;
            }
            Ruling::Unfunded(_) => {
                run.left = &candidates[i..];
                break;
            }
            Ruling::Stopped(why) => {
                a.caller().note(&format!("ERROR: {}: {why}", c.event.session));
                a.caller().hint();
                run.left = &candidates[i..];
                run.stopped = true;
                break;
            }
        }
    }
    Ok(run)
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
                out.push_str(&format!("{skill}: skill report over {}: {}\n", render::count(r.sessions.len(), "session"), r.result.verdict));
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
                "{skill}: skill report skipped: {} of the budget is left, under $0.01; run `mazo audit {skill} --resynth`\n",
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

/// `mazo audit --dry-run`: each session the audit would judge with its agent,
/// project and condensed size, the judge command, the billing (and the model
/// for Pi and http), the token estimate and, once session records exist, the dollar
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
            let ts = render::zoned(c.event.ts, &tz, render::MINUTE);
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
    let judge = a.backend.dry_run(a.data);
    let total: usize = candidates.iter().map(chars).sum();
    let model = a.backend.model().map(|m| format!("model: {m}\n")).unwrap_or_default();
    let mut out = format!(
        "{skill}: {} to judge with the {} judge\n\n{}\njudge: {judge}\nbilling: {}\n{model}input: {total} condensed characters, about {} tokens\n",
        render::count(candidates.len(), "session"),
        name(a.judge.agent),
        render::table(&header, &rows, tty),
        name(a.judge.billing),
        total / 4, // about four characters a token
    );
    if let (Some(_), Some((cost, records))) = (a.budget, estimate_cost(a.data, total)?) {
        out.push_str(&format!(
            "cost: about {}, at the rate of {}\n",
            a.usd(cost),
            render::count(records, "earlier session record")
        ));
    }
    Ok(out)
}
