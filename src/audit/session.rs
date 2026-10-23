//! Stage 1's answer: its schema, the verdict it parses to, and the checks
//! against the session it judged, the no-quote rule among them.

use std::collections::HashSet;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Stage, list, object, text};

/// The stage-1 judge prompt, shipped in the binary, and the version records name.
pub const SESSION_PROMPT: &str = include_str!("../../prompts/session.md");
pub const SESSION_PROMPT_VERSION: &str = "session-v1";

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
    let finding = object(vec![("line", json!({"type": "integer", "minimum": 1})), ("text", text(FINDING_CHARS))]);
    object(vec![
        ("asked", text(ASKED_CHARS)),
        ("followed", json!({"enum": ["yes", "partly", "no"]})),
        ("outcome", json!({"enum": ["achieved", "partial", "failed", "abandoned", "unclear"]})),
        ("redone", json!({"type": "boolean"})),
        ("strengths", list(finding.clone(), FINDINGS)),
        ("frictions", list(finding, FINDINGS)),
        ("notes", text(NOTES_CHARS)),
    ])
    .to_string()
});

/// Stage 1, one call per session.
pub static SESSION: Stage = Stage { prompt: SESSION_PROMPT, schema: &SESSION_SCHEMA };

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

/// Inert, as the answer is checked, lowercased, with every run of whitespace
/// one space.
fn normalize(s: &str) -> String {
    crate::render::inert(s).split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
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
