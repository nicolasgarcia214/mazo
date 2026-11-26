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
use crate::durable;
use crate::evals::Evals;
use crate::ledger::{Agent, name};
use crate::render::{self, Align};

pub const CENT: u64 = 10_000;

pub const DEFAULT_BUDGET: u64 = 1_000_000;

pub(crate) const CANCELLED: &str = "cancelled";

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

fn parsed<T: DeserializeOwned>(data: &Path) -> Result<Vec<T>> {
    Ok(durable::jsonl(&data.join("audits.jsonl"))?.into_iter().filter_map(|(_, r)| r).collect())
}

pub fn audited(data: &Path, skill: &str) -> Result<HashSet<String>> {
    Ok(parsed::<Recorded>(data)?
        .into_iter()
        .filter(|r| r.kind == "session" && r.skill == skill)
        .filter_map(|r| r.session)
        .collect())
}

fn text(max: usize) -> Value {
    json!({"type": "string", "maxLength": max})
}

fn list(items: Value, max: usize) -> Value {
    json!({"type": "array", "maxItems": max, "items": items})
}

fn object(fields: Vec<(&str, Value)>) -> Value {
    let required: Vec<&str> = fields.iter().map(|(k, _)| *k).collect();
    let properties: serde_json::Map<String, Value> = fields.into_iter().map(|(k, v)| (k.into(), v)).collect();
    json!({"type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

const FOR_CHARS: usize = 300;
const TRY_CHARS: usize = 200;
const TRIES: usize = 3;

fn for_try() -> [(&'static str, Value); 2] {
    [("for", text(FOR_CHARS)), ("try", list(text(TRY_CHARS), TRIES))]
}

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

pub struct Stage {
    pub prompt: &'static str,
    pub schema: &'static LazyLock<String>,
}

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
    let shift = exp + 6 - frac.len() as i64;
    let kept = (digits.len() as i64 + shift.min(0)).max(0) as usize;
    let n: u64 = if kept == 0 { 0 } else { digits[..kept].parse().map_err(|_| bad())? };
    let scale = 10u64.checked_pow(shift.max(0).try_into().map_err(|_| bad())?).ok_or_else(bad)?;
    n.checked_mul(scale).ok_or_else(bad)
}

pub fn cents(micros: u64) -> String {
    format!("{}.{:02}", micros / 1_000_000, micros % 1_000_000 / CENT)
}

fn exact(micros: u64) -> String {
    let s = format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

static NONCE: LazyLock<String> = LazyLock::new(|| {
    use std::hash::BuildHasher;
    format!("{:016x}", std::collections::hash_map::RandomState::new().hash_one(()))
});

fn frame(open: &str, text: &str) -> String {
    enclose(open, &text.replace(&*NONCE, "[nonce]"))
}

fn enclose(open: &str, body: &str) -> String {
    let tag = open.split(' ').next().unwrap_or(open);
    let (attrs, nonce) = (&open[tag.len()..], &*NONCE);
    format!("<{tag}-{nonce}{attrs}>\n{}\n</{tag}-{nonce}>\n", body.trim_end())
}

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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JudgeRecord {
    pub agent: JudgeAgent,
    pub model: Option<String>,
    pub cost_usd: Option<Box<RawValue>>,
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

fn stamp() -> String {
    Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn append(data: &Path, record: &impl Serialize) -> Result<()> {
    let line = serde_json::to_string(record).expect("record serialises") + "\n";
    let _lock = durable::lock(data)?;
    durable::append_lines(&data.join("audits.jsonl"), line)
}

pub struct Audit<'a> {
    pub skill: &'a str,
    pub source: &'a Path,
    pub data: &'a Path,
    pub judge: &'a Judge,
    pub backend: &'a dyn Backend,
    pub budget: Option<u64>,
    pub evals: &'a Evals,
    pub usage: &'a Value,
    pub pane: Option<&'a Pane>,
}

pub struct Pane {
    pub lines: mpsc::Sender<String>,
    pub cancel: Arc<AtomicBool>,
}

pub fn usd(judge: &Judge, micros: u64) -> String {
    let label = if judge.billing == Billing::Subscription { " API-equivalent" } else { "" };
    format!("${}{label}", cents(micros))
}

impl Audit<'_> {
    pub fn caller(&self) -> Caller<'_> {
        Caller { data: self.data, judge: self.judge, backend: self.backend, pane: self.pane }
    }

    fn usd(&self, micros: u64) -> String {
        self.caller().usd(micros)
    }
}

#[derive(Clone, Copy)]
pub struct Caller<'a> {
    pub data: &'a Path,
    pub judge: &'a Judge,
    pub backend: &'a dyn Backend,
    pub pane: Option<&'a Pane>,
}

impl Caller<'_> {
    fn usd(&self, micros: u64) -> String {
        usd(self.judge, micros)
    }

    fn note(&self, line: &str) {
        match self.pane {
            Some(pane) => _ = pane.lines.send(line.into()),
            None => eprintln!("{line}"),
        }
    }

    fn cancelled(&self) -> bool {
        self.pane.is_some_and(|p| p.cancel.load(Ordering::Relaxed))
    }

    pub fn login(&self) -> Result<()> {
        match self.judge.billing {
            Billing::Subscription => self.backend.login(self.data, &|| self.cancelled()),
            Billing::Api => Ok(()),
        }
    }

    fn hint(&self) {
        if self.judge.billing == Billing::Subscription {
            self.note("hint: to judge with an API key instead, set billing = \"api\" under [judge]");
        }
    }
}

#[derive(Default)]
pub struct Run<'c> {
    pub candidates: usize,
    pub judged: usize,
    pub dropped: usize,
    pub spent: u64,
    pub left: &'c [Candidate],
    pub stopped: bool,
    pub report: Synthesis,
}

#[derive(Default)]
pub enum Synthesis {
    #[default]
    NotDue,
    Written(Box<SkillRecord>),
    Dropped(String),
    Skipped,
    Stopped(String),
}

enum Ruling<T> {
    Valid(T, Option<u64>),
    Dropped(String),
    Unfunded(Option<String>),
    Stopped(String),
}

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

pub fn calls(a: &Audit, candidates: &[Candidate], resynth: bool) -> Result<bool> {
    Ok(!candidates.is_empty() || resynth && !session_records(a)?.is_empty())
}

pub fn run<'c>(a: &Audit, candidates: &'c [Candidate], resynth: bool) -> Result<Run<'c>> {
    let mut run = judge_sessions(a, candidates)?;
    if !run.stopped && (run.judged > 0 || resynth) {
        run.report = synthesize(a, &mut run.spent)?;
    }
    Ok(run)
}

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
    pub fn ok(&self) -> bool {
        !self.stopped
            && (self.judged > 0 || self.dropped == 0)
            && matches!(self.report, Synthesis::NotDue | Synthesis::Written(_))
    }

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
    let judge = a.backend.dry_run();
    let total: usize = candidates.iter().map(chars).sum();
    let model = a.backend.model().map(|m| format!("model: {m}\n")).unwrap_or_default();
    let mut out = format!(
        "{skill}: {} to judge with the {} judge\n\n{}\njudge: {judge}\nbilling: {}\n{model}input: {total} condensed characters, about {} tokens\n",
        render::count(candidates.len(), "session"),
        name(a.judge.agent),
        render::table(&header, &rows, tty),
        name(a.judge.billing),
        total / 4,
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
