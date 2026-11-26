use std::collections::{BTreeMap, BTreeSet, HashSet};

use jiff::SignedDuration;
use serde::Serialize;

use super::view::{Filter, Key, View, relative};
use super::{Input, UsageArgs, json_line, legend};
use crate::ctx::Ctx;
use crate::evals::{self, Evals, Grade};
use crate::inventory::{Inventory, Scope};
use crate::ledger::{Agent, Channel};
use crate::render::{self, Align};
use crate::store::Locked;

const SPARK_DAYS: i64 = 30;

#[derive(Serialize)]
pub struct GradeOut {
    #[serde(flatten)]
    pub grade: Grade,
    pub mark: &'static str,
}

impl From<Grade> for GradeOut {
    fn from(grade: Grade) -> GradeOut {
        GradeOut { mark: grade.mark(), grade }
    }
}

impl GradeOut {
    pub fn cell(&self, colour: bool) -> String {
        let score = self.grade.eval.score.as_deref().unwrap_or("?");
        if self.mark.is_empty() { score.into() } else { format!("{score} {}", render::paint(self.mark, colour)) }
    }
}

fn grade_out(evals: &Evals) -> Option<GradeOut> {
    evals.grades.last().cloned().map(GradeOut::from)
}

#[derive(Serialize)]
pub struct UsageRow {
    pub skill: String,
    pub scope: Scope,
    pub sess: usize,
    pub slash: usize,
    pub tool: usize,
    pub typed: usize,
    pub read: usize,
    #[serde(rename = "ref")]
    pub ref_: usize,
    pub agents: String,
    pub ctx: Ctx,
    pub last: Option<String>,
    pub sessions_7d: usize,
    pub sessions_30d: usize,
    pub trend: Option<Trend>,
    #[serde(rename = "30d")]
    pub last_30d: Vec<usize>,
    pub grade: Option<GradeOut>,
    pub verdict: Option<String>,
    pub verdict_source: Option<VerdictSource>,
    #[serde(skip)]
    pub mark: &'static str,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Trend {
    Up,
    Down,
    Flat,
    New,
}

impl Trend {
    fn of(later: usize, earlier: usize) -> Option<Trend> {
        Some(match (later, earlier) {
            (0, 0) => return None,
            (_, 0) => Trend::New,
            _ if 5 * later >= 6 * earlier => Trend::Up,
            _ if 5 * later <= 4 * earlier => Trend::Down,
            _ => Trend::Flat,
        })
    }

    pub fn glyph(trend: Option<Trend>) -> &'static str {
        match trend {
            Some(Trend::Up) => "↑",
            Some(Trend::Down) => "↓",
            Some(Trend::Flat) => "→",
            Some(Trend::New) => "new",
            None => "",
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VerdictSource {
    Audit,
    Vet,
}

pub(super) fn vet_of<'i>(inv: &'i Inventory, skill: &str) -> Option<&'i Locked> {
    inv.installed.get(skill).filter(|_| inv.root(skill) != inv.source.as_deref())
}

fn verdict_of(input: &Input, skill: &str, scope: Scope) -> (&'static str, Option<(VerdictSource, String)>) {
    let locked = match scope {
        Scope::Global => vet_of(input.inv, skill),
        Scope::Project => input.inv.project.as_ref().and_then(|p| p.installed.get(skill)),
    };
    let vet = locked.and_then(|l| l.verdict.as_ref());
    let audit = input.audits.get(skill).filter(|_| scope == Scope::Global);
    let mark = match (vet, audit) {
        (Some(v), _) if v.security.is_some() => "!",
        (_, Some(_)) => "✓",
        (Some(_), None) => "vet",
        (None, None) => "-",
    };
    let verdict = audit.map(|a| (VerdictSource::Audit, a.result.verdict.clone()));
    (mark, verdict.or_else(|| vet.map(|v| (VerdictSource::Vet, v.line()))))
}

#[derive(Serialize)]
pub struct Usage {
    pub rows: Vec<UsageRow>,
    pub coverage: BTreeMap<String, String>,
}

fn usage_rows(view: &View, args: &UsageArgs) -> Vec<UsageRow> {
    let input = view.input;
    let f = Filter::from(args);
    let start = view.start(args.since);
    let mut skills: BTreeSet<&str> = input.inv.skills.iter().map(String::as_str).collect();
    let project = input.inv.project.as_ref();
    if args.all {
        let project_only = |s: &str| project.is_some_and(|p| p.skills.contains(s));
        let ledger = input.events.iter().filter(|e| view.scope(e) == Scope::Global).map(|e| e.skill.as_str());
        skills.extend(ledger.filter(|s| !project_only(s)));
    }
    let project = project.into_iter().flat_map(|p| &p.skills).map(|s| (s.as_str(), Scope::Project));
    let mut rows: Vec<UsageRow> = skills
        .into_iter()
        .map(|s| (s, Scope::Global))
        .chain(project)
        .map(|(skill, scope)| {
            let select = |start| view.select(&f, Some(skill), start).filter(move |e| view.scope(e) == scope);
            let window: Vec<_> = select(start).collect();
            let keys: HashSet<Key> = window.iter().map(|e| (e.agent, e.session.as_str(), e.channel)).collect();
            let count = |c: Channel| keys.iter().filter(|k| k.2 == c).count();
            let sess = keys.iter().filter(|k| k.2.strong()).map(|k| (k.0, k.1)).collect::<HashSet<_>>().len();
            let agents: BTreeSet<Agent> = window.iter().map(|e| e.agent).collect();
            let last = window.iter().filter(|e| e.channel.strong()).map(|e| e.ts).max();
            let spark_from = input.now.checked_sub(SignedDuration::from_hours(24 * (SPARK_DAYS + 1))).ok();
            let last_30d = view.daily_strong(select(spark_from), SPARK_DAYS);
            let sessions = |days| view.strong_sessions(select(None), days);
            let sessions_30d = sessions(0..30);
            let global = scope == Scope::Global && input.inv.skills.contains(skill);
            let grade = global.then(|| grade_out(&evals::load(&input.cfg.evals_dir, input.inv, skill))).flatten();
            let (mark, verdict) = verdict_of(input, skill, scope);
            let (verdict_source, verdict) = verdict.unzip();
            UsageRow {
                skill: skill.to_string(),
                scope,
                sess,
                slash: count(Channel::Slash),
                tool: count(Channel::Tool),
                typed: count(Channel::Typed),
                read: count(Channel::Read),
                ref_: count(Channel::Ref),
                agents: agents.iter().map(|a| a.letter()).collect(),
                ctx: view.ctx().of(input.inv, skill, scope),
                last: last.map(|t| t.to_string()),
                sessions_7d: sessions(0..7),
                sessions_30d,
                trend: Trend::of(sessions_30d, sessions(30..60)),
                last_30d,
                grade,
                verdict,
                verdict_source,
                mark,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        (b.sess, b.read + b.ref_)
            .cmp(&(a.sess, a.read + a.ref_))
            .then_with(|| (&a.skill, a.scope).cmp(&(&b.skill, b.scope)))
    });
    rows
}

pub(super) fn usage_of(view: &View, args: &UsageArgs) -> Usage {
    Usage { rows: usage_rows(view, args), coverage: view.coverage(None).into_iter().collect() }
}

pub fn usage_table(input: &Input, args: &UsageArgs) -> Usage {
    usage_of(&View::new(input), args)
}

pub const USAGE_HEADER: [(&str, Align); 15] = [
    ("skill", Align::Left),
    ("sess", Align::Right),
    ("7d", Align::Right),
    ("30d", Align::Right),
    ("trend", Align::Left),
    ("slash", Align::Right),
    ("tool", Align::Right),
    ("typed", Align::Right),
    ("read", Align::Right),
    ("ref", Align::Right),
    ("agents", Align::Left),
    ("ctx", Align::Left),
    ("last", Align::Left),
    ("grade", Align::Right),
    ("audit", Align::Left),
];

pub fn usage_cells(input: &Input, r: &UsageRow, tty: bool) -> Vec<String> {
    r.cells(r.last.as_deref().and_then(|t| t.parse().ok()).map_or("-".into(), |t| relative(input, t)), tty)
}

impl UsageRow {
    pub fn cells(&self, last: String, tty: bool) -> Vec<String> {
        vec![
            format!("{}{}", self.skill, self.scope.suffix()),
            self.sess.to_string(),
            self.sessions_7d.to_string(),
            self.sessions_30d.to_string(),
            Trend::glyph(self.trend).into(),
            self.slash.to_string(),
            self.tool.to_string(),
            self.typed.to_string(),
            self.read.to_string(),
            self.ref_.to_string(),
            self.agents.clone(),
            self.ctx.cell(),
            last,
            self.grade.as_ref().map_or("-".into(), |g| g.cell(tty)),
            render::paint(self.mark, tty),
        ]
    }
}

pub fn usage(input: &Input, args: &UsageArgs, json: bool, tty: bool) -> String {
    let view = View::new(input);
    let usage = usage_of(&view, args);
    if json {
        return json_line(&usage);
    }
    let cells: Vec<Vec<String>> = usage.rows.iter().map(|r| usage_cells(input, r, tty)).collect();
    render::table(&USAGE_HEADER, &cells, tty) + &view.coverage_line("coverage", None) + &legend(&USAGE_HEADER, &cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trend_switches_at_twenty_percent_either_way() {
        let cases = [(6, 5, Some(Trend::Up)), (11, 10, Some(Trend::Flat)), (9, 10, Some(Trend::Flat))];
        let edges = [(4, 5, Some(Trend::Down)), (0, 3, Some(Trend::Down)), (1, 0, Some(Trend::New)), (0, 0, None)];
        for (later, earlier, trend) in cases.into_iter().chain(edges) {
            assert_eq!(Trend::of(later, earlier), trend, "{later} against {earlier}");
        }
        assert_eq!(
            [Trend::Up, Trend::Down, Trend::Flat, Trend::New].map(|t| Trend::glyph(Some(t))),
            ["↑", "↓", "→", "new"]
        );
        assert_eq!(Trend::glyph(None), "");
    }
}
