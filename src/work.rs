//! The vet judge's view of the user's recent work: from the ledger, and the
//! transcripts its events point into, the projects worked in, the skills
//! used and the latest prompts. Built without a judge call, so the same
//! ledger, transcripts and `now` give the same summary.

use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};

use crate::adapters;
use crate::ledger::{Actor, Event};

/// "Recent" is this many days before `now`.
const DAYS: i64 = 30;
/// Projects, and skills, listed: the most used first.
const PROJECTS: usize = 10;
const SKILLS: usize = 20;
/// Prompts sampled: the latest, by event timestamp, one per transcript.
const PROMPTS: usize = 10;
/// Transcripts read for them at most, each whole, so that a vet costs a
/// bounded number of reads however long the history.
const READS: usize = 2 * PROMPTS;
/// A prompt is cut to this many characters.
const PROMPT_CHARS: usize = 200;
/// The most bytes of the summary the vet judge is sent: a summary past it
/// loses its last whole lines, and says so, so it fits.
pub const SUMMARY_BYTES: usize = 8 * 1024;
const CUT: &str = "(cut to fit the size cap)\n";

/// The summary of the strong, main-actor `events` of the last [`DAYS`] days
/// before `now`: each project (`cwd`) and skill with its event count, most
/// first and ties by name, then the latest distinct prompts behind them,
/// newest first and ties by event id, each on one line: the prompt of a
/// transcript's latest event, from at most [`READS`] transcripts. With no such event,
/// it says no history is available.
pub fn summary(events: &[Event], now: Timestamp) -> String {
    let since = now - SignedDuration::from_hours(24 * DAYS);
    let mut recent: Vec<(Timestamp, &Event)> = events
        .iter()
        .filter(|e| e.channel.strong() && e.actor == Actor::Main)
        .filter_map(|e| Some((e.ts.parse::<Timestamp>().ok()?, e)))
        .filter(|(ts, _)| (since..=now).contains(ts))
        .collect();
    if recent.is_empty() {
        return format!("No history is available: the ledger holds no skill use in the last {DAYS} days.\n");
    }
    recent.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.id.cmp(&y.id)));
    let counted = |key: fn(&Event) -> Option<&str>, max: usize| {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        recent.iter().filter_map(|(_, e)| key(e)).for_each(|k| *counts.entry(k).or_default() += 1);
        let mut counts: Vec<(&str, usize)> = counts.into_iter().collect();
        counts.sort_by(|(a, x), (b, y)| y.cmp(x).then_with(|| a.cmp(b)));
        counts.into_iter().take(max).map(|(k, n)| format!("- {k}: {n}\n")).collect::<String>()
    };
    let mut prompts: Vec<(&Timestamp, String)> = Vec::new();
    let mut read: Vec<&str> = Vec::new();
    for (ts, e) in &recent {
        if prompts.len() == PROMPTS || read.len() == READS {
            break;
        }
        if read.contains(&e.src_file()) {
            continue;
        }
        read.push(e.src_file());
        let Some(text) = adapters::for_agent(e.agent).prompt_at(&e.src) else { continue };
        let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(PROMPT_CHARS).collect();
        if !text.is_empty() && !prompts.iter().any(|(_, t)| *t == text) {
            prompts.push((ts, text));
        }
    }
    let prompts: String = if prompts.is_empty() {
        "(none readable)\n".into()
    } else {
        prompts.iter().map(|(ts, text)| format!("- {}: {text}\n", ts.strftime("%Y-%m-%d"))).collect()
    };
    let out = format!(
        "Skill use in the last {DAYS} days, as event counts.\nProjects:\n{}Skills:\n{}Latest prompts:\n{prompts}",
        counted(|e| e.cwd.as_deref(), PROJECTS),
        counted(|e| Some(&e.skill), SKILLS),
    );
    capped(out)
}

/// `out` cut after its last whole line that leaves room for [`CUT`] within
/// [`SUMMARY_BYTES`], when it is over.
fn capped(mut out: String) -> String {
    if out.len() <= SUMMARY_BYTES {
        return out;
    }
    let room = &out.as_bytes()[..SUMMARY_BYTES - CUT.len()];
    out.truncate(room.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1));
    out + CUT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{Agent, Channel, Origin};

    const NOW: &str = "2026-10-13T12:00:00Z";

    fn event(id: &str, skill: &str, cwd: &str, ts: &str, src: &str) -> Event {
        Event {
            v: 1,
            id: id.into(),
            ts: ts.into(),
            agent: Agent::Claude,
            home: "claude".into(),
            agent_version: None,
            session: "s".into(),
            actor: Actor::Main,
            skill: skill.into(),
            origin: Origin::Canonical,
            channel: Channel::Tool,
            path: None,
            cwd: Some(cwd.into()),
            authoring: false,
            src: src.into(),
        }
    }

    #[test]
    fn the_summary_counts_projects_and_skills_and_samples_the_latest_prompts() {
        let dir = tempfile::tempdir().unwrap();
        // Line `n` of transcript `t` is the user message `msgs[n - 1]`.
        let at = |t: &str, line: usize| format!("{}:{line}", dir.path().join(t).display());
        for (t, msgs) in [
            ("t1", &["Tidy the README.", "Read again: a transcript is read once."][..]),
            ("t2", &["Write the\n  release notes."]),
            ("t3", &["Tidy the README."]),
            ("t4", &["Old work."]),
        ] {
            let lines: String = msgs
                .iter()
                .map(|m| {
                    format!("{}\n", serde_json::json!({"type": "user", "message": {"role": "user", "content": m}}))
                })
                .collect();
            std::fs::write(dir.path().join(t), lines).unwrap();
        }
        let mut sub = event("sub", "unslop", "/sub", "2026-10-13T00:00:00Z", &at("t1", 1));
        sub.actor = Actor::Subagent;
        let mut weak = event("weak", "unslop", "/weak", "2026-10-13T00:00:00Z", &at("t1", 1));
        weak.channel = Channel::Read;
        let events = [
            event("b", "unslop", "/b", "2026-10-12T00:00:00Z", &at("t1", 1)),
            event("a", "grade", "/a", "2026-10-12T00:00:00Z", &at("t2", 1)),
            event("c", "unslop", "/a", "2026-10-11T00:00:00Z", &at("t3", 1)),
            event("again", "unslop", "/a", "2026-10-11T00:00:00Z", &at("t1", 2)),
            event("gone", "retro", "/b", "2026-10-10T00:00:00Z", "/nonexistent.jsonl:1"),
            event("old", "unslop", "/old", "2026-09-01T00:00:00Z", &at("t4", 1)),
            sub,
            weak,
        ];
        let now: Timestamp = NOW.parse().unwrap();
        let got = summary(&events, now);
        assert_eq!(
            got,
            "Skill use in the last 30 days, as event counts.\n\
             Projects:\n- /a: 3\n- /b: 2\n\
             Skills:\n- unslop: 3\n- grade: 1\n- retro: 1\n\
             Latest prompts:\n- 2026-10-12: Write the release notes.\n- 2026-10-12: Tidy the README.\n"
        );
        assert_eq!(summary(&events, now), got, "two builds are the same");
    }

    #[test]
    fn an_empty_ledger_says_no_history_is_available() {
        let got = summary(&[], NOW.parse().unwrap());
        assert_eq!(got, "No history is available: the ledger holds no skill use in the last 30 days.\n");
    }

    #[test]
    fn a_summary_over_the_cap_loses_its_last_whole_lines() {
        let events: Vec<Event> = (0..20)
            .map(|i| event(&format!("e{i}"), "unslop", &format!("/p{i:02}/{}", "x".repeat(1000)), NOW, "/gone:1"))
            .collect();
        let got = summary(&events, NOW.parse().unwrap());
        assert!(got.len() <= SUMMARY_BYTES, "{} bytes", got.len());
        assert!(got.ends_with(&format!("x: 1\n{CUT}")), "{got}");
        assert_eq!(capped("short\n".into()), "short\n");
    }
}
