//! Reports over the ledger: usage, unused, the card (show);
//! collapse, held and cluster.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use crate::audit::SkillRecord;
use crate::config::Config;
use crate::evals::{self, Eval, Evals, Grade};
use crate::examples::{self, Examples};
use crate::history::Commit;
use crate::inventory::{self, Inventory, Scope};
use crate::ledger::{Actor, Agent, Channel, Coverage, Event, Origin, name};
use crate::render::{self, Align};
use crate::store::Locked;

/// A report window: the last N days, or everything.
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
            .map(Since::Days)
            .ok_or_else(|| format!("expected <N>d or all, got {s:?}"))
    }
}

/// Everything a report reads. `now` and `tz` are injected so tests are
/// deterministic.
pub struct Input<'a> {
    pub cfg: &'a Config,
    pub inv: &'a Inventory,
    pub events: &'a [Event],
    pub coverage: &'a BTreeMap<String, Coverage>,
    /// The latest skill record of each skill ([`crate::audit::latest`]).
    pub audits: &'a BTreeMap<String, SkillRecord>,
    pub now: jiff::Timestamp,
    /// The zone that days are bucketed in and times shown in.
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
    /// The default filter over `since`: what `show` and the report's cards count.
    pub fn new(since: Since) -> UsageArgs {
        UsageArgs { since, agent: None, channel: None, all: false, authoring: false }
    }
}

/// The card shows the audit's top improvements, this many.
pub const AUDIT_IMPROVEMENTS: usize = 3;

/// `show`'s default window.
pub const SHOW_SINCE: Since = Since::Days(30);

/// `show`'s default `--limit`, which the report's and the TUI's cards list too.
pub const SHOW_LIMIT: usize = 20;

/// The last line of `usage` and `unused`.
const LEGEND: &str = "legend: agents C Claude, X Codex, P Pi; · no events that day; +N commits since the grade; +wip uncommitted changes since the grade\n";

/// Days in the `30d` sparkline.
const SPARK_DAYS: i64 = 30;

/// The collapse key, within one skill.
type Key<'e> = (Agent, &'e str, Channel);

/// The ledger as reports see it: parsed timestamps, local days, the
/// report-time authoring sessions, and the events of project skills.
struct View<'a> {
    input: &'a Input<'a>,
    today: Date,
    events: Vec<(&'a Event, Timestamp)>,
    authoring_sessions: HashSet<(Agent, &'a str)>,
    /// The ids of the events whose path is in a project skill's directory.
    project: HashSet<&'a str>,
}

/// Which events a report counts. The default is canonical origin, no probe
/// actor and no authoring read.
#[derive(Default)]
struct Filter {
    all: bool,
    authoring: bool,
    agent: Option<Agent>,
    channel: Option<Channel>,
}

impl From<&UsageArgs> for Filter {
    fn from(args: &UsageArgs) -> Filter {
        Filter { all: args.all, authoring: args.authoring, agent: args.agent, channel: args.channel }
    }
}

/// `t`'s day in the report zone.
fn day(input: &Input, t: Timestamp) -> Date {
    t.to_zoned(input.tz.clone()).date()
}

/// `today`, `2d ago`.
fn relative(input: &Input, t: Timestamp) -> String {
    match (day(input, input.now) - day(input, t)).get_days() {
        ..=0 => "today".into(),
        n => format!("{n}d ago"),
    }
}

impl<'a> View<'a> {
    fn new(input: &'a Input<'a>) -> View<'a> {
        let today = day(input, input.now);
        let events: Vec<(&Event, Timestamp)> =
            input.events.iter().filter_map(|e| Some((e, e.ts.parse::<Timestamp>().ok()?))).collect();
        // `policy.authoring_skills` matched on the name after the last `:`, so a
        // plugin's `skill-creator:skill-creator` counts.
        let authoring = &input.cfg.policy.authoring_skills;
        let authoring_sessions = input
            .events
            .iter()
            .filter(|e| e.channel.strong() && authoring.iter().any(|s| e.skill.rsplit(':').next() == Some(s.as_str())))
            .map(|e| (e.agent, e.session.as_str()))
            .collect();
        let project = input.inv.project.as_ref().map_or_else(HashSet::new, |p| {
            let of = |e: &Event| e.path.as_deref().and_then(|path| p.skill_of(path)).is_some();
            input.events.iter().filter(|e| of(e)).map(|e| e.id.as_str()).collect()
        });
        View { input, today, events, authoring_sessions, project }
    }

    /// A project skill's event is one in its directory; every other is global.
    fn scope(&self, e: &Event) -> Scope {
        if self.project.contains(e.id.as_str()) { Scope::Project } else { Scope::Global }
    }

    fn date(&self, t: Timestamp) -> Date {
        day(self.input, t)
    }

    fn start(&self, since: Since) -> Option<Timestamp> {
        match since {
            Since::Days(n) => self.input.now.checked_sub(SignedDuration::from_hours(24 * i64::from(n))).ok(),
            Since::All => None,
        }
    }

    /// A weak event is authoring when stored so (cwd) or when its
    /// root session holds a strong authoring-skill event anywhere in the ledger.
    fn is_authoring(&self, e: &Event) -> bool {
        !e.channel.strong() && (e.authoring || self.authoring_sessions.contains(&(e.agent, e.session.as_str())))
    }

    fn kept(&self, e: &Event, f: &Filter) -> bool {
        // A project skill's event is canonical whatever the scan, which knows no project, made it.
        let canonical = e.origin == Origin::Canonical || self.scope(e) == Scope::Project;
        (f.all || (canonical && e.actor != Actor::Probe))
            && (f.authoring || !self.is_authoring(e))
            && f.agent.is_none_or(|a| e.agent == a)
            && f.channel.is_none_or(|c| e.channel == c)
    }

    /// Filtered events of one skill (all skills when `skill` is None) at or after `start`.
    fn select<'s>(
        &'s self,
        f: &'s Filter,
        skill: Option<&'s str>,
        start: Option<Timestamp>,
    ) -> impl Iterator<Item = &'s (&'a Event, Timestamp)> + 's {
        self.events.iter().filter(move |(e, t)| {
            skill.is_none_or(|s| e.skill == s) && start.is_none_or(|s| *t >= s) && self.kept(e, f)
        })
    }

    /// Skills with a strong event at or after `start`, under the default filter.
    fn used(&self, start: Option<Timestamp>) -> BTreeSet<&'a str> {
        let f = Filter::default();
        self.select(&f, None, start).filter(|(e, _)| e.channel.strong()).map(|(e, _)| e.skill.as_str()).collect()
    }

    /// Distinct collapse keys of strong events per local day, for the last `days` days.
    fn daily_strong<'s>(&self, events: impl Iterator<Item = &'s (&'s Event, Timestamp)>, days: i64) -> Vec<usize> {
        let mut per_day: Vec<HashSet<Key>> = vec![HashSet::new(); days as usize];
        for (e, t) in events.filter(|(e, _)| e.channel.strong()) {
            let ago = (self.today - self.date(*t)).get_days() as i64;
            if (0..days).contains(&ago) {
                per_day[(days - 1 - ago) as usize].insert((e.agent, &e.session, e.channel));
            }
        }
        per_day.iter().map(HashSet::len).collect()
    }

    /// Per home: the effective window start, the later of the report window
    /// and the home's coverage, with the earliest typed event.
    fn coverage(&self, start: Option<Timestamp>) -> Vec<(String, String)> {
        self.input
            .cfg
            .homes
            .iter()
            .map(|h| {
                let text = match self.input.coverage.get(&h.name) {
                    None => "never scanned".to_string(),
                    Some(c) => match c.first.as_deref().and_then(|f| f.parse::<Timestamp>().ok()) {
                        None => "no transcripts".to_string(),
                        Some(first) => {
                            let from = start.map_or(first, |s| s.max(first));
                            // Typed events reach back before the first transcript.
                            let typed = self
                                .events
                                .iter()
                                .filter(|(e, _)| e.home == h.name && e.channel == Channel::Typed)
                                .map(|(_, t)| start.map_or(*t, |s| s.max(*t)))
                                .min()
                                .filter(|t| *t < from);
                            match typed {
                                Some(t) => format!("since {} (typed since {})", self.date(from), self.date(t)),
                                None => format!("since {}", self.date(from)),
                            }
                        }
                    },
                };
                (h.name.clone(), text)
            })
            .collect()
    }

    fn coverage_line(&self, label: &str, start: Option<Timestamp>) -> String {
        coverage_note(label, self.coverage(start).iter().map(|(h, t)| (h, t))) + "\n"
    }
}

/// `label: home window · …`: the coverage note under `usage` and `unused`.
pub fn coverage_note<'s>(label: &str, homes: impl IntoIterator<Item = (&'s String, &'s String)>) -> String {
    let parts: Vec<String> = homes.into_iter().map(|(h, t)| format!("{h} {t}")).collect();
    format!("{label}: {}", parts.join(" · "))
}

/// A grade as reports emit it, with its staleness.
#[derive(Serialize)]
pub struct GradeOut {
    #[serde(flatten)]
    pub grade: Grade,
    pub stale: bool,
    /// `+N`, `+wip` or empty.
    pub mark: String,
}

impl From<Grade> for GradeOut {
    fn from(grade: Grade) -> GradeOut {
        GradeOut { stale: grade.stale(), mark: grade.mark(), grade }
    }
}

fn grade_out(evals: &Evals) -> Option<GradeOut> {
    evals.grades.last().cloned().map(GradeOut::from)
}

/// The score as written, then `+N` or `+wip`, painted when `colour`.
fn grade_cell(g: &Option<GradeOut>, colour: bool) -> String {
    g.as_ref().map_or("-".into(), |g| {
        format!("{}{}", g.grade.eval.score.as_deref().unwrap_or("?"), render::paint(&g.mark, colour))
    })
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
    pub last: Option<String>,
    #[serde(rename = "30d")]
    pub last_30d: Vec<usize>,
    pub grade: Option<GradeOut>,
    /// The quality verdict ([`verdict_of`]); null when not judged.
    pub verdict: Option<String>,
    /// Where `verdict` comes from; null when not judged.
    pub verdict_source: Option<VerdictSource>,
}

/// Where a usage row's verdict comes from.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VerdictSource {
    Audit,
    Vet,
}

/// The lockfile entry of `skill`'s store copy; a source skill of the same
/// name shadows it.
fn vet_of<'i>(inv: &'i Inventory, skill: &str) -> Option<&'i Locked> {
    inv.installed.get(skill).filter(|_| inv.root(skill) != inv.source.as_deref())
}

/// `skill`'s quality verdict and its source (D18). The audit verdict is the
/// `result.verdict` of the latest audit, the last `skill` record of `skill`
/// in `audits.jsonl` ([`crate::audit::latest`]), as `show` prints it; else
/// the vet verdict of its lockfile entry ([`vet_of`]) as
/// [`crate::audit::VetVerdict::summary`] words it, led by `security:
/// <finding>; ` when the judge found one, so a cut cell still shows it; else
/// none. A project skill has no audit, and its entry is in the project's
/// lockfile.
fn verdict_of(input: &Input, skill: &str, scope: Scope) -> Option<(VerdictSource, String)> {
    let vet = || {
        let locked = match scope {
            Scope::Global => vet_of(input.inv, skill),
            Scope::Project => input.inv.project.as_ref()?.installed.get(skill),
        };
        let v = locked?.verdict.as_ref()?;
        let text = v.security.as_ref().map_or(v.summary(), |f| format!("security: {f}; {}", v.summary()));
        Some((VerdictSource::Vet, text))
    };
    let audit = || input.audits.get(skill).filter(|_| scope == Scope::Global);
    audit().map(|a| (VerdictSource::Audit, a.result.verdict.clone())).or_else(vet)
}

/// The quality cell in full: `audit: <verdict>`, `vet: <verdict>` or
/// `not judged`. The text table cuts it to [`VERDICT_CHARS`].
pub fn verdict_text(r: &UsageRow) -> String {
    match (r.verdict_source, &r.verdict) {
        (Some(source), Some(v)) => format!("{}: {v}", name(source)),
        _ => "not judged".into(),
    }
}

/// The usage table: what `usage --json` emits.
#[derive(Serialize)]
pub struct Usage {
    pub rows: Vec<UsageRow>,
    /// Per home, the window its transcripts cover.
    pub coverage: BTreeMap<String, String>,
}

/// `value` as one line of `--json`. The `Value` in between sorts the keys.
fn json_line<T: Serialize>(value: &T) -> String {
    format!("{}\n", serde_json::to_value(value).expect("report serialises"))
}

/// The `usage` rows, most used first: one per global skill, then one per
/// project skill, so a name in both has two.
fn usage_rows(view: &View, args: &UsageArgs) -> Vec<UsageRow> {
    let input = view.input;
    let f = Filter::from(args);
    let start = view.start(args.since);
    let mut skills: BTreeSet<&str> = input.inv.skills.iter().map(String::as_str).collect();
    if args.all {
        // Every skill in the ledger, so a retired or pruned one keeps a row.
        skills.extend(input.events.iter().filter(|e| view.scope(e) == Scope::Global).map(|e| e.skill.as_str()));
    }
    let project = input.inv.project.iter().flat_map(|p| &p.skills).map(|s| (s.as_str(), Scope::Project));
    let mut rows: Vec<UsageRow> = skills
        .into_iter()
        .map(|s| (s, Scope::Global))
        .chain(project)
        .map(|(skill, scope)| {
            let select = |start| view.select(&f, Some(skill), start).filter(move |(e, _)| view.scope(e) == scope);
            let window: Vec<_> = select(start).collect();
            let keys: HashSet<Key> = window.iter().map(|(e, _)| (e.agent, e.session.as_str(), e.channel)).collect();
            let count = |c: Channel| keys.iter().filter(|k| k.2 == c).count();
            let sess = keys.iter().filter(|k| k.2.strong()).map(|k| (k.0, k.1)).collect::<HashSet<_>>().len();
            let agents: BTreeSet<Agent> = window.iter().map(|(e, _)| e.agent).collect();
            let last = window.iter().filter(|(e, _)| e.channel.strong()).map(|(_, t)| *t).max();
            let spark_from = input.now.checked_sub(SignedDuration::from_hours(24 * (SPARK_DAYS + 1))).ok();
            let last_30d = view.daily_strong(select(spark_from), SPARK_DAYS);
            // Grades are the source's.
            let global = scope == Scope::Global && input.inv.skills.contains(skill);
            let grade = global.then(|| grade_out(&evals::load(input.inv, skill))).flatten();
            let (verdict_source, verdict) = verdict_of(input, skill, scope).unzip();
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
                last: last.map(|t| t.to_string()),
                last_30d,
                grade,
                verdict,
                verdict_source,
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

fn usage_of(view: &View, args: &UsageArgs) -> Usage {
    Usage { rows: usage_rows(view, args), coverage: view.coverage(None).into_iter().collect() }
}

/// The usage table over `input`.
pub fn usage_table(input: &Input, args: &UsageArgs) -> Usage {
    usage_of(&View::new(input), args)
}

/// The usage table's columns.
pub const USAGE_HEADER: [(&str, Align); 12] = [
    ("skill", Align::Left),
    ("sess", Align::Right),
    ("slash", Align::Right),
    ("tool", Align::Right),
    ("typed", Align::Right),
    ("read", Align::Right),
    ("ref", Align::Right),
    ("agents", Align::Left),
    ("last", Align::Left),
    ("30d", Align::Left),
    ("grade", Align::Right),
    ("quality", Align::Left),
];

/// The usage table's quality cell is cut to this many characters with `…`:
/// a vet cell always fits, a long audit verdict is cut.
const VERDICT_CHARS: usize = 40;

/// `r`'s cells under [`USAGE_HEADER`], the grade mark painted when `tty`.
pub fn usage_cells(input: &Input, r: &UsageRow, tty: bool) -> Vec<String> {
    vec![
        format!("{}{}", r.skill, r.scope.suffix()),
        r.sess.to_string(),
        r.slash.to_string(),
        r.tool.to_string(),
        r.typed.to_string(),
        r.read.to_string(),
        r.ref_.to_string(),
        r.agents.clone(),
        r.last.as_deref().and_then(|t| t.parse().ok()).map_or("-".into(), |t| relative(input, t)),
        render::sparkline(&r.last_30d),
        grade_cell(&r.grade, tty),
        cut(&verdict_text(r), VERDICT_CHARS),
    ]
}

/// Rendered `mazo usage` output (text, or JSON when `json`).
pub fn usage(input: &Input, args: &UsageArgs, json: bool, tty: bool) -> String {
    let view = View::new(input);
    let usage = usage_of(&view, args);
    if json {
        return json_line(&usage);
    }
    let cells: Vec<Vec<String>> = usage.rows.iter().map(|r| usage_cells(input, r, tty)).collect();
    render::table(&USAGE_HEADER, &cells, tty) + &view.coverage_line("coverage", None) + LEGEND
}

/// Skills that cite `skill`.
fn citers(graph: &inventory::Graph, skill: &str) -> Vec<String> {
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
    render::table(&UNUSED_HEADER, &cells, tty) + &view.coverage_line("window", view.start(since)) + LEGEND
}

#[derive(Serialize)]
pub struct ShownEvent<'e> {
    pub ts: &'e str,
    pub agent: Agent,
    pub channel: Channel,
    pub actor: Actor,
    pub cwd: Option<&'e str>,
    pub src: &'e str,
    pub expired: bool,
}

#[derive(Serialize)]
pub struct Day {
    pub date: String,
    pub strong: usize,
    pub weak: usize,
}

#[derive(Serialize)]
pub struct CardHistory<'a> {
    pub first: Option<&'a Commit>,
    pub last: Option<&'a Commit>,
    pub uncommitted: bool,
}

/// A skill the card's skill cites, or one citing it, and whether it is held
/// in the window.
#[derive(Serialize)]
pub struct Link {
    pub skill: String,
    pub held: bool,
}

#[derive(Serialize)]
pub struct ByChannel {
    pub agent: Agent,
    pub channel: Channel,
    pub events: usize,
}

#[derive(Serialize)]
pub struct Project {
    pub cwd: String,
    pub sessions: usize,
}

#[derive(Serialize)]
pub struct CardUsage {
    /// Collapsed strong events in the window per agent and channel.
    pub strong: Vec<ByChannel>,
    /// The latest strong event in the whole ledger.
    pub last_use: Option<String>,
    /// The top three cwds in the window by distinct root sessions with a strong event.
    pub projects: Vec<Project>,
}

/// What a skill is for and how to use it, atop the card.
#[derive(Serialize)]
pub struct Manual {
    /// The `for` of [`Manual::guide`]'s pick, else the frontmatter description.
    pub r#for: Option<String>,
    /// The first line of each real example, cut to [`USE_CHARS`].
    pub your_use: Vec<String>,
    /// The uses to try of [`Manual::guide`]'s pick.
    pub r#try: Vec<String>,
}

/// The manual's Your use and Try each list this many.
const MANUAL_ITEMS: usize = 3;
/// A Your use line is cut to this many characters.
const USE_CHARS: usize = 100;
/// The manual's text is this wide, its label column included; For takes at
/// most `FOR_LINES` and each item `ITEM_LINES`, so it fits 15 lines.
const MANUAL_WIDTH: usize = 80;
const LABEL_WIDTH: usize = 10;
const FOR_LINES: usize = 3;
const ITEM_LINES: usize = 2;

/// `text` cut to `max` characters, the last a `…` when cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.into() } else { text.chars().take(max - 1).chain(['…']).collect() }
}

/// The first line of `text`, cut to [`USE_CHARS`].
fn first_line(text: &str) -> String {
    cut(text.trim().lines().next().unwrap_or_default().trim(), USE_CHARS)
}

/// `text` word-wrapped to `width` characters in at most `max` lines; a cut
/// ends in `…`. A word longer than `width` is split.
fn wrap(text: &str, width: usize, max: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let (mut line, mut len) = (String::new(), 0);
    for word in text.split_whitespace() {
        for piece in word.chars().collect::<Vec<_>>().chunks(width) {
            if len > 0 && len + 1 + piece.len() > width {
                lines.push(std::mem::take(&mut line));
                len = 0;
            }
            if len > 0 {
                line.push(' ');
                len += 1;
            }
            line.extend(piece);
            len += piece.len();
        }
    }
    if len > 0 {
        lines.push(line);
    }
    if lines.len() > max {
        lines.truncate(max);
        let last: String = lines[max - 1].chars().take(width - 1).chain(['…']).collect();
        lines[max - 1] = last;
    }
    lines
}

impl Manual {
    /// For and Try: the latest skill audit's when it has them and is newer
    /// than the vet or there is no vet, else the vet's from the lockfile;
    /// with neither, `description` and no Try.
    fn guide(
        vet: Option<&Locked>,
        audit: Option<&SkillRecord>,
        description: Option<String>,
    ) -> (Option<String>, Vec<String>) {
        let vetted = vet.and_then(|l| l.vetted_at.as_deref()).and_then(|t| t.parse::<Timestamp>().ok());
        let newer = |a: &&SkillRecord| vetted.is_none_or(|v| a.audited_at.parse::<Timestamp>().is_ok_and(|at| at > v));
        let (r#for, r#try) = match audit.filter(|a| a.result.r#for.is_some()).filter(newer) {
            Some(a) => (a.result.r#for.clone(), &a.result.r#try[..]),
            None => (vet.and_then(|l| l.r#for.clone()).or(description), vet.map_or(&[][..], |l| &l.r#try[..])),
        };
        (r#for, r#try.iter().take(MANUAL_ITEMS).cloned().collect())
    }

    /// For, Your use and Try under a label column, each omitted when empty;
    /// empty when all three are.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let mut part = |label: &str, rows: Vec<String>| {
            for (i, row) in rows.iter().enumerate() {
                out.push_str(&format!("{:<LABEL_WIDTH$}{row}\n", if i == 0 { label } else { "" }));
            }
        };
        part("for:", self.r#for.as_deref().map_or(Vec::new(), |f| wrap(f, MANUAL_WIDTH - LABEL_WIDTH, FOR_LINES)));
        for (label, items) in [("your use:", &self.your_use), ("try:", &self.r#try)] {
            let rows = items.iter().flat_map(|item| {
                let lines = wrap(item, MANUAL_WIDTH - LABEL_WIDTH - 2, ITEM_LINES);
                lines.into_iter().enumerate().map(|(n, l)| format!("{}{l}", if n == 0 { "- " } else { "  " }))
            });
            part(label, rows.collect());
        }
        out
    }
}

/// The skill card: what `show` prints and `show --json` emits.
#[derive(Serialize)]
pub struct Card<'a> {
    pub skill: &'a str,
    pub manual: Manual,
    pub since: Option<String>,
    pub histogram: Vec<Day>,
    /// The newest `limit` events in the window.
    pub events: Vec<ShownEvent<'a>>,
    pub total_events: usize,
    pub description: Option<String>,
    /// None for a name in no root without a source.
    pub path: Option<PathBuf>,
    /// None outside a git worktree.
    pub history: Option<CardHistory<'a>>,
    /// Newest first.
    pub grades: Vec<GradeOut>,
    /// Newest first.
    pub retros: Vec<Eval>,
    /// Strong events on days after the latest retro, over the whole ledger.
    pub strong_since_retro: Option<usize>,
    pub cites: Vec<Link>,
    pub cited_by: Vec<Link>,
    pub usage: CardUsage,
    pub examples: Examples,
    /// The latest skill record.
    pub audit: Option<&'a SkillRecord>,
}

/// `xs` joined with `, `, or `none`.
fn or_none(xs: Vec<String>) -> String {
    if xs.is_empty() { "none".into() } else { xs.join(", ") }
}

/// The card lines `show` prints and the HTML report escapes, so the two
/// read the same fields.
impl Card<'_> {
    /// `first <day> <subject>, last …`; None outside a git worktree.
    pub fn history_line(&self) -> Option<String> {
        let h = self.history.as_ref()?;
        // The committer's own date.
        let commit = |c: &Commit| format!("{} {}", c.date.get(..10).unwrap_or(&c.date), c.subject);
        let mut line = match (h.first, h.last) {
            (Some(first), Some(last)) => format!("first {}, last {}", commit(first), commit(last)),
            _ => "never committed".into(),
        };
        if h.uncommitted {
            line.push_str(", uncommitted changes");
        }
        Some(line)
    }

    /// `claude slash 3, …`: collapsed strong events per agent and channel.
    pub fn strong_line(&self) -> String {
        or_none(
            self.usage.strong.iter().map(|b| format!("{} {} {}", name(b.agent), name(b.channel), b.events)).collect(),
        )
    }

    /// `~/p 2, …`: the top cwds and their sessions.
    pub fn projects_line(&self) -> String {
        or_none(self.usage.projects.iter().map(|p| format!("{} {}", tilde(&p.cwd), p.sessions)).collect())
    }

    /// `none`, or the strong events since the latest retro.
    pub fn retro_note(&self) -> String {
        self.strong_since_retro.map_or("none".into(), |n| format!("{n} strong since the latest"))
    }
}

/// `cites` or `cited by`: each skill as `skill` writes it, `(held)` after a held one.
pub fn links(ls: &[Link], skill: impl Fn(&str) -> String) -> String {
    or_none(ls.iter().map(|l| if l.held { format!("{} (held)", skill(&l.skill)) } else { skill(&l.skill) }).collect())
}

/// A name `show` accepts: any known name (canonical names are known).
pub fn check_skill(inv: &Inventory, skill: &str) -> crate::Result<()> {
    if inv.known.contains(skill) { Ok(()) } else { Err(crate::Error::Usage(format!("unknown skill: {skill}"))) }
}

/// `skill`'s card over the events `args` keeps; `graph` is the inventory's
/// citation graph.
fn card<'a>(view: &View<'a>, graph: &inventory::Graph, skill: &'a str, args: &UsageArgs, limit: usize) -> Card<'a> {
    let inv = view.input.inv;
    let f = Filter::from(args);
    let start = view.start(args.since);
    let mut window: Vec<_> = view.select(&f, Some(skill), start).collect();
    window.sort_by_key(|(_, t)| std::cmp::Reverse(*t));

    let mut days: BTreeMap<Date, (HashSet<Key>, HashSet<Key>)> = BTreeMap::new();
    for (e, t) in &window {
        let day = days.entry(view.date(*t)).or_default();
        let set = if e.channel.strong() { &mut day.0 } else { &mut day.1 };
        set.insert((e.agent, &e.session, e.channel));
    }
    let histogram =
        days.iter().map(|(d, (s, w))| Day { date: d.to_string(), strong: s.len(), weak: w.len() }).collect();

    let events = window
        .iter()
        .take(limit)
        .map(|(e, _)| ShownEvent {
            ts: &e.ts,
            agent: e.agent,
            channel: e.channel,
            actor: e.actor,
            cwd: e.cwd.as_deref(),
            src: &e.src,
            expired: !Path::new(e.src_file()).exists(),
        })
        .collect();

    // A name only in history keeps its place in the source.
    let path = inv.root(skill).or(inv.source.as_deref()).map(|r| r.join(skill));
    let description = inv.description(skill);
    let history = inv.history.as_ref().map(|h| CardHistory {
        first: h.first(skill),
        last: h.last(skill),
        uncommitted: h.wip(skill),
    });

    let Evals { grades, mut retros } = evals::load(inv, skill);
    let grades: Vec<GradeOut> = grades.into_iter().rev().map(GradeOut::from).collect();
    retros.reverse();
    let strong_since_retro = retros.first().and_then(|r| r.date.parse::<Date>().ok()).map(|d| {
        view.select(&f, Some(skill), None)
            .filter(|(e, t)| e.channel.strong() && view.date(*t) > d)
            .map(|(e, _)| (e.agent, e.session.as_str(), e.channel))
            .collect::<HashSet<Key>>()
            .len()
    });

    // A skill is held when a skill citing it was used in the window.
    let used = view.used(start);
    let link =
        |b: &str| Link { skill: b.to_string(), held: citers(graph, b).iter().any(|a| used.contains(a.as_str())) };
    let cites = graph.get(skill).into_iter().flatten().map(|b| link(b)).collect();
    let cited_by = citers(graph, skill).iter().map(|a| link(a)).collect();

    let mut ledger: Vec<_> = view.select(&f, Some(skill), None).collect();
    ledger.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    let agents = view.input.cfg.homes.iter().map(|h| h.agent);
    let mut examples = examples::examples(agents, path.as_deref(), skill, ledger.iter().map(|(e, _)| *e));
    let audit = view.input.audits.get(skill);
    examples.curated = audit.map(|r| r.result.examples.clone()).unwrap_or_default();

    let vet = vet_of(inv, skill);
    let (r#for, r#try) = Manual::guide(vet, audit, description.clone());
    let manual = Manual {
        r#for,
        your_use: examples.real.iter().take(MANUAL_ITEMS).map(|r| first_line(&r.text)).collect(),
        r#try,
    };

    Card {
        skill,
        manual,
        since: start.map(|s| view.date(s).to_string()),
        histogram,
        events,
        total_events: window.len(),
        description,
        path,
        history,
        grades,
        retros,
        strong_since_retro,
        cites,
        cited_by,
        usage: card_usage(view, &f, skill, start),
        examples,
        audit,
    }
}

/// The card's usage section for the events of `skill` that `f` keeps at or after `start`.
fn card_usage(view: &View, f: &Filter, skill: &str, start: Option<Timestamp>) -> CardUsage {
    let mut by: BTreeMap<(Agent, Channel), HashSet<&str>> = BTreeMap::new();
    let mut projects: BTreeMap<&str, HashSet<(Agent, &str)>> = BTreeMap::new();
    for (e, _) in view.select(f, Some(skill), start).filter(|(e, _)| e.channel.strong()) {
        by.entry((e.agent, e.channel)).or_default().insert(&e.session);
        if let Some(cwd) = &e.cwd {
            projects.entry(cwd).or_default().insert((e.agent, &e.session));
        }
    }
    let mut projects: Vec<Project> =
        projects.into_iter().map(|(cwd, s)| Project { cwd: cwd.to_string(), sessions: s.len() }).collect();
    projects.sort_by_key(|p| std::cmp::Reverse(p.sessions));
    projects.truncate(3);
    let last_use = view.select(f, Some(skill), None).filter(|(e, _)| e.channel.strong()).map(|(_, t)| *t).max();
    CardUsage {
        strong: by.into_iter().map(|((agent, channel), s)| ByChannel { agent, channel, events: s.len() }).collect(),
        last_use: last_use.map(|t| t.to_string()),
        projects,
    }
}

/// The usage section of `skill`'s card over `show`'s default window, which
/// the audit's stage 2 reads.
pub fn usage_section(input: &Input, skill: &str) -> serde_json::Value {
    let view = View::new(input);
    serde_json::to_value(card_usage(&view, &Filter::default(), skill, view.start(SHOW_SINCE)))
        .expect("usage serialises")
}

/// `$HOME` as `~`.
pub fn tilde(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// A transcript locator as `<agent>:<file name>:<line>`.
fn locator(e: &ShownEvent) -> String {
    let (file, line) = e.src.rsplit_once(':').unwrap_or((e.src, ""));
    let file = Path::new(file).file_name().map_or(file.into(), |n| n.to_string_lossy());
    format!("{}:{file}:{line}", name(e.agent))
}

/// Rendered `mazo show <skill>` output; Err for an unknown skill.
pub fn show(input: &Input, skill: &str, since: Since, limit: usize, json: bool, tty: bool) -> crate::Result<String> {
    check_skill(input.inv, skill)?;
    let card = skill_card(input, skill, &UsageArgs::new(since), limit);
    Ok(if json { json_line(&card) } else { card_text(input, &card, tty) })
}

/// `skill`'s card over the events `args` keeps, listing the newest `limit`.
pub fn skill_card<'a>(input: &'a Input<'a>, skill: &'a str, args: &UsageArgs, limit: usize) -> Card<'a> {
    let graph = inventory::fan_in(input.inv);
    card(&View::new(input), &graph, skill, args, limit)
}

/// `show`'s text for `card`.
pub fn card_text(input: &Input, card: &Card, tty: bool) -> String {
    let mut out = card.manual.text();
    if !out.is_empty() {
        out.push('\n');
    }
    let from = card.since.as_ref().map_or("all time".to_string(), |d| format!("since {d}"));
    out.push_str(&format!("{}: {from}\n\n", card.skill));
    let max = card.histogram.iter().map(|h| h.strong).max().unwrap_or(0).max(1);
    let hist_rows: Vec<Vec<String>> = card
        .histogram
        .iter()
        .map(|h| {
            vec![h.date.clone(), h.strong.to_string(), h.weak.to_string(), "█".repeat((h.strong * 40).div_ceil(max))]
        })
        .collect();
    let hist_header = [("day", Align::Left), ("strong", Align::Right), ("weak", Align::Right), ("", Align::Left)];
    out.push_str(&render::table(&hist_header, &hist_rows, tty));

    out.push_str(&format!("\nlast {} of {} events:\n", card.events.len(), card.total_events));
    let ev_rows: Vec<Vec<String>> = card
        .events
        .iter()
        .map(|e| {
            let ts = render::local(e.ts, &input.tz, render::MINUTE);
            let src = if e.expired { format!("{} expired", locator(e)) } else { locator(e) };
            vec![ts, name(e.agent), name(e.channel), name(e.actor), e.cwd.map_or("-".into(), tilde), src]
        })
        .collect();
    let ev_header = ["ts", "agent", "channel", "actor", "cwd", "src"].map(|h| (h, Align::Left));
    out.push_str(&render::table(&ev_header, &ev_rows, tty));

    out.push('\n');
    out.push_str(&format!("description: {}\n", card.description.as_deref().unwrap_or("-")));
    out.push_str(&format!("path: {}\n", card.path.as_ref().map_or("-".into(), |p| tilde(&p.to_string_lossy()))));
    if let Some(line) = card.history_line() {
        out.push_str(&format!("history: {line}\n"));
    }

    let score = |e: &Eval| e.score.clone().unwrap_or_else(|| "?".into());
    if card.grades.is_empty() {
        out.push_str("grades: none\n");
    } else {
        out.push_str("grades:\n");
        let rows: Vec<Vec<String>> =
            card.grades.iter().map(|g| vec![g.grade.eval.date.clone(), score(&g.grade.eval), g.grade.mark()]).collect();
        let header = [("date", Align::Left), ("score", Align::Right), ("since", Align::Left)];
        out.push_str(&render::table(&header, &rows, tty));
    }
    out.push_str(&format!("retros: {}\n", card.retro_note()));
    if card.strong_since_retro.is_some() {
        let rows: Vec<Vec<String>> = card
            .retros
            .iter()
            .map(|r| vec![r.date.clone(), score(r), r.verdict.clone().unwrap_or_else(|| "-".into())])
            .collect();
        let header = [("date", Align::Left), ("score", Align::Right), ("verdict", Align::Left)];
        out.push_str(&render::table(&header, &rows, tty));
    }

    out.push_str(&format!("cites: {}\n", links(&card.cites, str::to_string)));
    out.push_str(&format!("cited by: {}\n", links(&card.cited_by, str::to_string)));

    out.push_str(&format!("strong: {}\n", card.strong_line()));
    let last_use = card.usage.last_use.as_deref().and_then(|t| t.parse::<Timestamp>().ok());
    out.push_str(&format!(
        "last use: {}\n",
        last_use.map_or("never".into(), |t| format!("{} ({})", day(input, t), relative(input, t)))
    ));
    out.push_str(&format!("projects: {}\n", card.projects_line()));

    let ex = &card.examples;
    let invoke: Vec<String> = ex.written.iter().map(|i| format!("{} {}", name(i.agent), i.text)).collect();
    out.push_str(&format!("invoke: {}\n", invoke.join(", ")));
    if let Some(p) = &ex.default_prompt {
        out.push_str(&format!("default prompt: {p}\n"));
    }
    if ex.real.is_empty() {
        out.push_str("examples: none\n");
    } else {
        out.push_str("examples:\n");
        for r in &ex.real {
            let date = r.ts.parse::<Timestamp>().map_or(r.ts.clone(), |t| day(input, t).to_string());
            let text = r.text.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push_str(&format!("  {date} {}: {text}\n", r.cwd.as_deref().map_or("-".into(), tilde)));
        }
    }
    if !ex.curated.is_empty() {
        out.push_str("curated:\n");
        for c in &ex.curated {
            out.push_str(&format!(
                "  {} Asked: {} Happened: {} Tip: {} ({})\n",
                c.situation, c.asked, c.happened, c.tip, c.session
            ));
        }
    }
    match card.audit {
        None => out.push_str("audit: none\n"),
        Some(a) => {
            let date = a.audited_at.parse::<Timestamp>().map_or(a.audited_at.clone(), |t| day(input, t).to_string());
            let billing = a.judge.billing.map_or("unknown".into(), name);
            out.push_str(&format!(
                "audit: {date}, {} sessions, {billing} billing: {}\n",
                a.sessions.len(),
                a.result.verdict
            ));
            let rates: Vec<String> = a.rates.labeled().iter().map(|(l, r)| format!("{l} {}", r.of())).collect();
            out.push_str(&format!("  {}\n", rates.join(", ")));
            for (i, imp) in a.result.improvements.iter().take(AUDIT_IMPROVEMENTS).enumerate() {
                out.push_str(&format!("  {}. {} ({})\n", i + 1, imp.change, imp.reason));
            }
        }
    }
    out
}

/// A skill whose grade is missing or stale, with the command that grades it.
#[derive(Serialize)]
pub struct GradeDue {
    pub skill: String,
    /// `missing` or `stale`.
    pub state: &'static str,
    pub command: String,
}

/// The model of `mazo report`: the `usage --all` rows and coverage, the
/// grades to run, and every inventory skill's card. The page renders this
/// struct, so it shows nothing the JSON lacks.
#[derive(Serialize)]
pub struct Report<'a> {
    pub generated: String,
    pub since: Option<String>,
    pub usage: Usage,
    pub grades_due: Vec<GradeDue>,
    pub cards: Vec<Card<'a>>,
    /// The zone the days were bucketed in, which the page shows times in.
    #[serde(skip)]
    pub tz: TimeZone,
}

/// The report model over `input` and `since`.
pub fn report<'a>(input: &'a Input<'a>, since: Since) -> Report<'a> {
    let view = View::new(input);
    let usage = usage_of(&view, &UsageArgs { all: true, ..UsageArgs::new(since) });
    // Rows outside the inventory have no grade and nothing to grade.
    let grades_due = usage
        .rows
        .iter()
        .filter(|r| input.inv.skills.contains(&r.skill))
        .filter_map(|r| {
            let state = match &r.grade {
                None => "missing",
                Some(g) if g.stale => "stale",
                Some(_) => return None,
            };
            Some(GradeDue { skill: r.skill.clone(), state, command: format!("/grade {}", r.skill) })
        })
        .collect();
    let graph = inventory::fan_in(input.inv);
    let args = UsageArgs::new(since);
    let cards = input.inv.skills.iter().map(|s| card(&view, &graph, s, &args, SHOW_LIMIT)).collect();
    Report {
        generated: input.now.to_string(),
        since: view.start(since).map(|s| view.date(s).to_string()),
        usage,
        grades_due,
        cards,
        tz: input.tz.clone(),
    }
}
