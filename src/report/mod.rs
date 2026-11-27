mod card;
mod unused;
mod usage;
mod view;

use std::collections::BTreeMap;

use jiff::tz::TimeZone;
use serde::Serialize;

use crate::adapters::Transcripts;
use crate::audit::SkillRecord;
use crate::config::Config;
use crate::help;
use crate::inventory::{self, Inventory};
use crate::ledger::{Agent, Channel, Coverage, Event};
use crate::render::Align;

pub(crate) use card::cut;
pub use card::{
    AuditLines, ByChannel, Card, CardHistory, CardUsage, Day, Link, Manual, Project, ShownEvent, SkillOrigin,
    card_text, check_skill, links, show, skill_card, tilde, usage_section,
};
pub use unused::{UNUSED_HEADER, Unused, UnusedRow, retire_blockers, unused, unused_cells, unused_list};
pub use usage::{GradeOut, Trend, USAGE_HEADER, Usage, UsageRow, VerdictSource, usage, usage_cells, usage_table};
pub use view::{Bucket, buckets_of, coverage_note};

use card::card;
use usage::usage_of;
use view::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    Days(u32),
    All,
}

impl std::str::FromStr for Since {
    type Err = String;
    fn from_str(s: &str) -> Result<Since, String> {
        if s == "all" {
            return Ok(Since::All);
        }
        s.strip_suffix('d')
            .and_then(|n| n.parse().ok())
            .filter(|&n| n > 0)
            .map(Since::Days)
            .ok_or_else(|| format!("expected <N>d with N > 0, or all, got {s:?}"))
    }
}

impl std::fmt::Display for Since {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Since::Days(n) => write!(f, "{n}d"),
            Since::All => f.write_str("all"),
        }
    }
}

pub struct Input<'a> {
    pub cfg: &'a Config,
    pub inv: &'a Inventory,
    pub events: &'a [Event],
    pub coverage: &'a BTreeMap<String, Coverage>,
    pub audits: &'a BTreeMap<String, SkillRecord>,
    pub now: jiff::Timestamp,
    pub tz: TimeZone,
}

pub struct UsageArgs {
    pub since: Since,
    pub agent: Option<Agent>,
    pub channel: Option<Channel>,
    pub all: bool,
    pub authoring: bool,
}

impl UsageArgs {
    pub fn new(since: Since) -> UsageArgs {
        UsageArgs { since, agent: None, channel: None, all: false, authoring: false }
    }
}

pub const SINCE: Since = Since::Days(30);

pub const UNUSED_SINCE: Since = Since::Days(60);

pub const SHOW_LIMIT: usize = 20;

fn legend(header: &[(&str, Align)], cells: &[Vec<String>]) -> String {
    let shown = |column: &str, mark: &str| {
        header.iter().position(|(name, _)| *name == column).is_some_and(|i| {
            cells.iter().any(|row| if mark == column { !row[i].is_empty() } else { row[i].contains(mark) })
        })
    };
    let line = help::legend(shown);
    if line.is_empty() { line } else { format!("legend: {line}\n") }
}

fn json_line<T: Serialize>(value: &T) -> String {
    format!("{}\n", serde_json::to_value(value).expect("report serialises"))
}

#[derive(Serialize)]
pub struct GradeDue {
    pub skill: String,
    pub state: &'static str,
    pub command: String,
}

#[derive(Serialize)]
pub struct Report<'a> {
    pub generated: String,
    pub since: Option<String>,
    pub usage: Usage,
    pub grades_due: Vec<GradeDue>,
    pub cards: Vec<Card<'a>>,
    #[serde(skip)]
    pub tz: TimeZone,
}

pub fn report<'a>(input: &'a Input<'a>, since: Since) -> Report<'a> {
    let view = View::new(input);
    let usage = usage_of(&view, &UsageArgs { all: true, ..UsageArgs::new(since) });
    let grades_due = usage
        .rows
        .iter()
        .filter(|r| input.inv.skills.contains(&r.skill))
        .filter_map(|r| {
            let state = match &r.grade {
                None => "missing",
                Some(g) if g.grade.stale => "stale",
                Some(_) => return None,
            };
            Some(GradeDue { skill: r.skill.clone(), state, command: format!("/grade {}", r.skill) })
        })
        .collect();
    let graph = inventory::citations(input.inv).held;
    let args = UsageArgs::new(since);
    let mut memo = Transcripts::default();
    let cards = input.inv.skills.iter().map(|s| card(&view, &graph, s, &args, SHOW_LIMIT, &mut memo)).collect();
    Report {
        generated: input.now.to_string(),
        since: view.start(since).map(|s| view.date(s).to_string()),
        usage,
        grades_due,
        cards,
        tz: input.tz.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legend_explains_only_the_marks_the_table_shows() {
        let row = |skill: &str, agents: &str, ctx: &str, grade: &str, audit: &str| {
            let mut cells = vec!["0".to_string(); USAGE_HEADER.len()];
            for (column, cell) in
                [("skill", skill), ("agents", agents), ("ctx", ctx), ("grade", grade), ("audit", audit)]
            {
                cells[USAGE_HEADER.iter().position(|(name, _)| *name == column).unwrap()] = cell.into();
            }
            cells
        };
        let cells = [row("a", "C", "C◐X●", "7 \x1b[33mstale\x1b[0m", "✓"), row("b", "", "", "-", "-")];
        assert_eq!(
            legend(&USAGE_HEADER, &cells),
            "legend: agents C Claude, X Codex, P Pi; \
             ● name and description in context: Claude on, every Codex skill, Pi by default; \
             ◐ name only in context: Claude name-only; \
             stale the skill changed since the grade; ✓ audited\n"
        );
        let bare = [row("b (project)", "", "", "-", "vet")];
        assert_eq!(
            legend(&USAGE_HEADER, &bare),
            "legend: vet vetted by mazo add, not audited; (project) a project skill, counted apart\n"
        );
    }

    #[test]
    fn an_empty_or_markless_table_has_no_legend() {
        assert_eq!(legend(&USAGE_HEADER, &[]), "");
        assert_eq!(legend(&UNUSED_HEADER, &[]), "");
        let unused = [vec!["a".to_string(), "unused".into(), "cluster: b; weak-only".into()]];
        assert_eq!(legend(&UNUSED_HEADER, &unused), "", "unused has none of the marked columns");
    }
}
