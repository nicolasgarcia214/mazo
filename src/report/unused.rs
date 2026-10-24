//! `mazo unused`: unused, held and new skills, and what blocks `retire`.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use serde::Serialize;

use super::view::{Filter, View};
use super::{Input, Since, json_line, legend};
use crate::inventory;
use crate::render::{self, Align};

/// Skills that cite `skill`.
pub(super) fn citers(graph: &inventory::Graph, skill: &str) -> Vec<String> {
    graph.iter().filter(|(_, cited)| cited.contains(skill)).map(|(a, _)| a.clone()).collect()
}

/// Why `retire` refuses `skill`: a strong event in the last `days`
/// days, or a citing or naming skill with one. Empty when it may go.
pub fn retire_blockers(input: &Input, skill: &str, days: u32) -> Vec<String> {
    let view = View::new(input);
    let used = view.used(view.start(Since::Days(days)));
    let mut out = Vec::new();
    if used.contains(skill) {
        out.push(format!("{skill} has a strong event in the last {days} days"));
    }
    let graph = inventory::fan_in(input.inv);
    let named = inventory::mentions(input.inv, &graph);
    for (citing, verb) in [(&graph, "held"), (&named, "named")] {
        let who: Vec<String> = citers(citing, skill).into_iter().filter(|a| used.contains(a.as_str())).collect();
        if !who.is_empty() {
            out.push(format!("{skill} is {verb} by {}", who.join(", ")));
        }
    }
    out
}

#[derive(Serialize)]
pub struct UnusedRow {
    pub skill: String,
    /// `unused`, `held` or `new`.
    pub status: &'static str,
    /// Live citers of a held skill.
    pub holders: Vec<String>,
    /// Citers of an unused skill, none of them used in the window.
    pub cluster: Vec<String>,
    pub weak_only: bool,
    /// Skills used in the window that name this one; never makes it held.
    pub named_by: Vec<String>,
}

/// The unused list: what `unused --json` emits.
#[derive(Serialize)]
pub struct Unused {
    /// The window's first day; None for all time.
    pub since: Option<String>,
    /// Per home, the window its transcripts cover.
    pub window: BTreeMap<String, String>,
    pub skills: Vec<UnusedRow>,
}

fn unused_of(view: &View, since: Since) -> Unused {
    let input = view.input;
    let f = Filter::default();
    let start = view.start(since);
    let used = view.used(start);
    let weak: BTreeSet<&str> = view.select(&f, None, start).map(|(e, _)| e.skill.as_str()).collect();
    let graph = inventory::fan_in(input.inv);
    let named = inventory::mentions(input.inv, &graph);
    let mut rows: Vec<UnusedRow> = input
        .inv
        .skills
        .iter()
        .filter(|s| !used.contains(s.as_str()))
        .map(|skill| {
            // A store skill's first date is its install; a source skill's is its
            // first commit, and outside a git worktree no source skill is new.
            let first = if input.inv.root(skill) == input.inv.source.as_deref() {
                input.inv.history.as_ref().map(|h| h.first(skill).map(|c| c.date.as_str()))
            } else {
                Some(input.inv.installed.get(skill).map(|l| l.installed_at.as_str()))
            };
            let is_new = first.is_some_and(|d| {
                d.and_then(|d| d.parse::<Timestamp>().ok()).is_none_or(|t| start.is_some_and(|s| t > s))
            });
            let (holders, others): (Vec<String>, Vec<String>) =
                citers(&graph, skill).into_iter().partition(|a| used.contains(a.as_str()));
            let (status, holders, cluster) = if is_new {
                ("new", Vec::new(), Vec::new())
            } else if holders.is_empty() {
                ("unused", Vec::new(), others)
            } else {
                ("held", holders, Vec::new())
            };
            let named_by = citers(&named, skill).into_iter().filter(|a| used.contains(a.as_str())).collect();
            UnusedRow {
                skill: skill.clone(),
                status,
                holders,
                cluster,
                weak_only: weak.contains(skill.as_str()),
                named_by,
            }
        })
        .collect();
    let rank = |s: &str| ["unused", "held", "new"].iter().position(|x| *x == s);
    rows.sort_by(|a, b| (rank(a.status), &a.skill).cmp(&(rank(b.status), &b.skill)));
    let since = start.map(|s| view.date(s).to_string());
    Unused { since, window: view.coverage(start).into_iter().collect(), skills: rows }
}

/// The unused list over `input` and `since`.
pub fn unused_list(input: &Input, since: Since) -> Unused {
    unused_of(&View::new(input), since)
}

/// The unused list's columns.
pub const UNUSED_HEADER: [(&str, Align); 3] = [("skill", Align::Left), ("status", Align::Left), ("notes", Align::Left)];

/// `r`'s cells under [`UNUSED_HEADER`], the notes joined with `; `, the
/// status and the `named by` note painted when `tty`.
pub fn unused_cells(r: &UnusedRow, tty: bool) -> Vec<String> {
    let mut notes = Vec::new();
    if !r.holders.is_empty() {
        notes.push(format!("held by {}", r.holders.join(", ")));
    }
    if !r.cluster.is_empty() {
        notes.push(format!("cluster: {}", r.cluster.join(", ")));
    }
    if !r.named_by.is_empty() {
        notes.push(render::paint(&format!("named by {}", r.named_by.join(", ")), tty));
    }
    if r.weak_only {
        notes.push("weak-only".into());
    }
    vec![r.skill.clone(), render::paint(r.status, tty), notes.join("; ")]
}

/// Rendered `mazo unused` output.
pub fn unused(input: &Input, since: Since, json: bool, tty: bool) -> String {
    let view = View::new(input);
    let unused = unused_of(&view, since);
    if json {
        return json_line(&unused);
    }
    let cells: Vec<Vec<String>> = unused.skills.iter().map(|r| unused_cells(r, tty)).collect();
    render::table(&UNUSED_HEADER, &cells, tty)
        + &view.coverage_line("window", view.start(since))
        + &legend(&UNUSED_HEADER, &cells)
}
