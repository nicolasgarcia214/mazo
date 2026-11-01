//! The vet judge's view of the user's recent work, from the ledger alone:
//! the projects worked in and the skills used. It carries none of the
//! user's prompts, so a skill under vet cannot have the judge copy them into
//! its verdict, which the lockfile keeps. Built without a judge call, so the
//! same ledger and `now` give the same summary.

use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};

use crate::ledger::{Actor, Event};

/// "Recent" is this many days before `now`.
const DAYS: i64 = 30;
/// Projects, and skills, listed: the most used first.
const PROJECTS: usize = 10;
const SKILLS: usize = 20;
/// The most bytes of the summary the vet judge is sent: a summary past it
/// loses its last whole lines, and says so, so it fits.
const SUMMARY_BYTES: usize = 8 * 1024;
const CUT: &str = "(cut to fit the size cap)\n";

/// The summary of the strong, main-actor `events` of the last [`DAYS`] days
/// before `now`: each project (`cwd`) and skill with its event count, most
/// first and ties by name. With no such event, it says no history is
/// available.
pub fn summary(events: &[Event], now: Timestamp) -> String {
    let since = now - SignedDuration::from_hours(24 * DAYS);
    let recent: Vec<&Event> = events
        .iter()
        .filter(|e| e.channel.strong() && e.actor == Actor::Main && (since..=now).contains(&e.ts))
        .collect();
    if recent.is_empty() {
        return format!("No history is available: the ledger holds no skill use in the last {DAYS} days.\n");
    }
    let counted = |key: fn(&Event) -> Option<&str>, max: usize| {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        recent.iter().filter_map(|e| key(e)).for_each(|k| *counts.entry(k).or_default() += 1);
        let mut counts: Vec<(&str, usize)> = counts.into_iter().collect();
        counts.sort_by(|(a, x), (b, y)| y.cmp(x).then_with(|| a.cmp(b)));
        counts.into_iter().take(max).map(|(k, n)| format!("- {k}: {n}\n")).collect::<String>()
    };
    let out = format!(
        "Skill use in the last {DAYS} days, as event counts.\nProjects:\n{}Skills:\n{}",
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

    fn event(id: &str, skill: &str, cwd: &str, ts: &str) -> Event {
        Event {
            v: 1,
            id: id.into(),
            ts: ts.parse().unwrap(),
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
            src: "/gone:1".into(),
        }
    }

    #[test]
    fn the_summary_counts_the_recent_main_strong_projects_and_skills() {
        let mut sub = event("sub", "unslop", "/sub", "2026-10-13T00:00:00Z");
        sub.actor = Actor::Subagent;
        let mut weak = event("weak", "unslop", "/weak", "2026-10-13T00:00:00Z");
        weak.channel = Channel::Read;
        let events = [
            event("b", "unslop", "/b", "2026-10-12T00:00:00Z"),
            event("a", "grade", "/a", "2026-10-12T00:00:00Z"),
            event("c", "unslop", "/a", "2026-10-11T00:00:00Z"),
            event("again", "unslop", "/a", "2026-10-11T00:00:00Z"),
            event("gone", "retro", "/b", "2026-10-10T00:00:00Z"),
            event("old", "unslop", "/old", "2026-09-01T00:00:00Z"),
            sub,
            weak,
        ];
        let now: Timestamp = NOW.parse().unwrap();
        let got = summary(&events, now);
        assert_eq!(
            got,
            "Skill use in the last 30 days, as event counts.\n\
             Projects:\n- /a: 3\n- /b: 2\n\
             Skills:\n- unslop: 3\n- grade: 1\n- retro: 1\n"
        );
    }

    #[test]
    fn an_empty_ledger_says_no_history_is_available() {
        let got = summary(&[], NOW.parse().unwrap());
        assert_eq!(got, "No history is available: the ledger holds no skill use in the last 30 days.\n");
    }

    #[test]
    fn a_summary_over_the_cap_loses_its_last_whole_lines() {
        let events: Vec<Event> = (0..20)
            .map(|i| event(&format!("e{i}"), "unslop", &format!("/p{i:02}/{}", "x".repeat(1000)), NOW))
            .collect();
        let got = summary(&events, NOW.parse().unwrap());
        assert!(got.len() <= SUMMARY_BYTES, "{} bytes", got.len());
        assert!(got.ends_with(&format!("x: 1\n{CUT}")), "{got}");
        assert_eq!(capped("short\n".into()), "short\n");
    }
}
