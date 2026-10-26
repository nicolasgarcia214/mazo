//! Stage 2: one call over every session record of a skill, the rates mazo
//! computes, and the skill record the card shows.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::session::{Followed, Outcome};
use super::{
    Audit, JudgeRecord, Ruling, Stage, Synthesis, Verdict, append, check_for_try, for_try, frame, judge, list, object,
    parsed, stamp, text,
};
use crate::Result;
use crate::inventory::git;
use crate::ledger::Agent;

/// The stage-2 judge prompt, and the version skill records name.
pub const SKILL_PROMPT: &str = include_str!("../../prompts/skill.md");
pub const SKILL_PROMPT_VERSION: &str = "skill-v2";

/// Stage 2, one call per skill.
pub static SKILL: Stage = Stage { prompt: SKILL_PROMPT, schema: &SKILL_SCHEMA };

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
    let sessions = json!({"type": "array", "minItems": 1, "items": {"type": "string"}});
    let cited = object(vec![("text", text(REPORT_CHARS)), ("sessions", sessions.clone())]);
    let mut fields = vec![
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
    ];
    fields.extend(for_try());
    object(fields).to_string()
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
    /// What the skill is for, one or two sentences; none in a record written
    /// before the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#for: Option<String>,
    /// Up to three uses to try that the audited sessions did not show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r#try: Vec<String>,
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

/// The rates mazo computes from session records, never the judge.
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
pub(super) struct Judged {
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
    check_for_try(r.r#for.as_deref().unwrap_or_default(), &r.r#try)?;
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

/// The stage-2 judge's input: the current `SKILL.md`, its last
/// 30 commits, every grade's and retro's frontmatter with the latest grade
/// body and the latest three retro bodies cut, the card's usage section,
/// every session record of the skill, and the uses those records show.
fn synthesis_input(a: &Audit, judged: &[Judged]) -> Result<String> {
    let dir = a.source.join(a.skill);
    let path = dir.join("SKILL.md");
    let skill = fs::read_to_string(&path).map_err(crate::at(&path))?;
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
    let seen: BTreeSet<&str> = judged.iter().map(|j| j.result.asked.as_str()).collect();
    let seen: String = seen.iter().map(|asked| format!("- {asked}\n")).collect();
    Ok([
        frame(&format!("skill name=\"{}\"", a.skill), &skill),
        frame("commits", &commits),
        frame("grades", &fronts(&mut grades.iter().copied())),
        frame("latest-grade", latest_grade.as_deref().unwrap_or_default()),
        frame("retros", &fronts(&mut retros.iter().copied())),
        frame("latest-retros", &retro_bodies),
        frame("usage", &serde_json::to_string_pretty(a.usage).expect("usage serialises")),
        frame("session-records", &records),
        frame("seen-uses", &seen),
    ]
    .join("\n"))
}

/// Every session record of the skill.
pub(super) fn session_records(a: &Audit) -> Result<Vec<Judged>> {
    Ok(parsed::<Judged>(a.data)?.into_iter().filter(|j| j.kind == "session" && j.skill == a.skill).collect())
}

/// Stage 2: one call over every session record of the skill; with
/// a budget, capped at it minus what is spent, and skipped when that is under
/// a cent. A valid answer is appended as the skill record with the rates mazo
/// computes; an invalid one is retried once, then dropped. A judge that
/// failed without an answer stops the run.
pub(super) fn synthesize(a: &Audit, spent: &mut u64) -> Result<Synthesis> {
    let judged = session_records(a)?;
    if judged.is_empty() {
        return Ok(Synthesis::NotDue);
    }
    let input = synthesis_input(a, &judged)?;
    let progress = format!("reporting on {}", crate::render::count(judged.len(), "session"));
    Ok(match judge(&a.caller(), &SKILL, a.budget, spent, &input, &progress, |v| validate_report(v, &judged))? {
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
            a.caller().note(&format!("ERROR: skill report: {why}"));
            a.caller().hint();
            Synthesis::Stopped(why)
        }
    })
}
