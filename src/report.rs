//! Reports over the ledger: usage, unused, the card (show);
//! collapse, held and cluster.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use serde_json::json;

use crate::audit::SkillRecord;
use crate::config::{Billing, Config};
use crate::evals::{self, Eval, Evals, Grade};
use crate::examples::{self, Examples};
use crate::history::Commit;
use crate::inventory::{self, Inventory};
use crate::ledger::{Actor, Agent, Channel, Coverage, Event, Origin, name};
use crate::render::{self, Align};
use crate::yaml;

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

/// Everything a report reads. `now` is injected so tests are deterministic.
pub struct Input<'a> {
    pub cfg: &'a Config,
    pub inv: &'a Inventory,
    pub events: &'a [Event],
    pub coverage: &'a BTreeMap<String, Coverage>,
    /// The latest skill record of each skill ([`crate::audit::latest`]).
    pub audits: &'a BTreeMap<String, SkillRecord>,
    pub now: jiff::Timestamp,
}

pub struct UsageArgs {
    pub since: Since,
    pub agent: Option<Agent>,
    pub channel: Option<Channel>,
    pub all: bool,
    pub authoring: bool,
}

/// The card shows the audit's top improvements, this many.
pub const AUDIT_IMPROVEMENTS: usize = 3;

/// `show`'s default window.
pub const SHOW_SINCE: Since = Since::Days(30);

/// The last line of `usage` and `unused`.
const LEGEND: &str = "legend: agents C Claude, X Codex, P Pi; · no events that day; +N commits since the grade; +wip uncommitted changes since the grade\n";

/// Days in the `30d` sparkline.
const SPARK_DAYS: i64 = 30;

/// The collapse key, within one skill.
type Key<'e> = (Agent, &'e str, Channel);

/// The ledger as reports see it: parsed timestamps, local days, and the
/// report-time authoring sessions.
struct View<'a> {
    input: &'a Input<'a>,
    tz: TimeZone,
    today: Date,
    events: Vec<(&'a Event, Timestamp)>,
    authoring_sessions: HashSet<(Agent, &'a str)>,
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

impl<'a> View<'a> {
    fn new(input: &'a Input<'a>) -> View<'a> {
        let tz = TimeZone::system();
        let today = input.now.to_zoned(tz.clone()).date();
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
        View { input, tz, today, events, authoring_sessions }
    }

    fn date(&self, t: Timestamp) -> Date {
        t.to_zoned(self.tz.clone()).date()
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
        (f.all || (e.origin == Origin::Canonical && e.actor != Actor::Probe))
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

    /// `today`, `2d ago`.
    fn relative(&self, t: Timestamp) -> String {
        match (self.today - self.date(t)).get_days() {
            ..=0 => "today".into(),
            n => format!("{n}d ago"),
        }
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
        let parts: Vec<String> = self.coverage(start).into_iter().map(|(h, t)| format!("{h} {t}")).collect();
        format!("{label}: {}\n", parts.join(" · "))
    }
}

/// A grade as reports emit it, with its staleness.
#[derive(Serialize)]
struct GradeOut {
    #[serde(flatten)]
    grade: Grade,
    stale: bool,
    /// `+N`, `+wip` or empty.
    mark: String,
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
struct UsageRow {
    skill: String,
    sess: usize,
    slash: usize,
    tool: usize,
    typed: usize,
    read: usize,
    #[serde(rename = "ref")]
    ref_: usize,
    agents: String,
    last: Option<String>,
    #[serde(rename = "30d")]
    last_30d: Vec<usize>,
    grade: Option<GradeOut>,
}

/// The `usage` rows, most used first.
fn usage_rows(view: &View, args: &UsageArgs) -> Vec<UsageRow> {
    let input = view.input;
    let f = Filter { all: args.all, authoring: args.authoring, agent: args.agent, channel: args.channel };
    let start = view.start(args.since);
    let mut skills: BTreeSet<&str> = input.inv.skills.iter().map(String::as_str).collect();
    if args.all {
        // Every skill in the ledger, so a retired or pruned one keeps a row.
        skills.extend(input.events.iter().map(|e| e.skill.as_str()));
    }
    let mut rows: Vec<UsageRow> = skills
        .into_iter()
        .map(|skill| {
            let window: Vec<_> = view.select(&f, Some(skill), start).collect();
            let keys: HashSet<Key> = window.iter().map(|(e, _)| (e.agent, e.session.as_str(), e.channel)).collect();
            let count = |c: Channel| keys.iter().filter(|k| k.2 == c).count();
            let sess = keys.iter().filter(|k| k.2.strong()).map(|k| (k.0, k.1)).collect::<HashSet<_>>().len();
            let agents: BTreeSet<Agent> = window.iter().map(|(e, _)| e.agent).collect();
            let last = window.iter().filter(|(e, _)| e.channel.strong()).map(|(_, t)| *t).max();
            let spark_from = input.now.checked_sub(SignedDuration::from_hours(24 * (SPARK_DAYS + 1))).ok();
            let last_30d = view.daily_strong(view.select(&f, Some(skill), spark_from), SPARK_DAYS);
            let grade = input.inv.skills.contains(skill).then(|| grade_out(&evals::load(input.inv, skill))).flatten();
            UsageRow {
                skill: skill.to_string(),
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
            }
        })
        .collect();
    rows.sort_by(|a, b| (b.sess, b.read + b.ref_).cmp(&(a.sess, a.read + a.ref_)).then_with(|| a.skill.cmp(&b.skill)));
    rows
}

/// Rendered `skima usage` output (text, or JSON when `json`).
pub fn usage(input: &Input, args: &UsageArgs, json: bool, tty: bool) -> String {
    let view = View::new(input);
    let rows = usage_rows(&view, args);
    if json {
        let coverage: BTreeMap<String, String> = view.coverage(None).into_iter().collect();
        return format!("{}\n", json!({"rows": rows, "coverage": coverage}));
    }
    let header = [
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
    ];
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                r.skill.clone(),
                r.sess.to_string(),
                r.slash.to_string(),
                r.tool.to_string(),
                r.typed.to_string(),
                r.read.to_string(),
                r.ref_.to_string(),
                r.agents.clone(),
                r.last.as_deref().and_then(|t| t.parse().ok()).map_or("-".into(), |t| view.relative(t)),
                render::sparkline(&r.last_30d),
                grade_cell(&r.grade, tty),
            ]
        })
        .collect();
    render::table(&header, &cells, tty) + &view.coverage_line("coverage", None) + LEGEND
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
    let graph = inventory::fan_in(&input.inv.source, &input.inv.skills);
    let holders: Vec<String> = citers(&graph, skill).into_iter().filter(|a| used.contains(a.as_str())).collect();
    if !holders.is_empty() {
        out.push(format!("{skill} is held by {}", holders.join(", ")));
    }
    let named = inventory::mentions(&input.inv.source, &input.inv.skills, &graph);
    let namers: Vec<String> = citers(&named, skill).into_iter().filter(|a| used.contains(a.as_str())).collect();
    if !namers.is_empty() {
        out.push(format!("{skill} is named by {}", namers.join(", ")));
    }
    out
}

#[derive(Serialize)]
struct UnusedRow {
    skill: String,
    /// `unused`, `held` or `new`.
    status: &'static str,
    /// Live citers of a held skill.
    holders: Vec<String>,
    /// Citers of an unused skill, none of them used in the window.
    cluster: Vec<String>,
    weak_only: bool,
    /// Skills used in the window that name this one; never makes it held.
    named_by: Vec<String>,
}

/// Rendered `skima unused` output.
pub fn unused(input: &Input, since: Since, json: bool, tty: bool) -> String {
    let view = View::new(input);
    let f = Filter::default();
    let start = view.start(since);
    let used = view.used(start);
    let weak: BTreeSet<&str> = view.select(&f, None, start).map(|(e, _)| e.skill.as_str()).collect();
    let graph = inventory::fan_in(&input.inv.source, &input.inv.skills);
    let named = inventory::mentions(&input.inv.source, &input.inv.skills, &graph);
    let mut rows: Vec<UnusedRow> = input
        .inv
        .skills
        .iter()
        .filter(|s| !used.contains(s.as_str()))
        .map(|skill| {
            // Outside a git worktree no skill is new.
            let is_new = input.inv.history.as_ref().is_some_and(|h| {
                h.first(skill)
                    .and_then(|c| c.date.parse::<Timestamp>().ok())
                    .is_none_or(|t| start.is_some_and(|s| t > s))
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
    if json {
        let window: BTreeMap<String, String> = view.coverage(start).into_iter().collect();
        return format!(
            "{}\n",
            json!({"since": start.map(|s| view.date(s).to_string()), "window": window, "skills": rows})
        );
    }
    let header = [("skill", Align::Left), ("status", Align::Left), ("notes", Align::Left)];
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
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
        })
        .collect();
    render::table(&header, &cells, tty) + &view.coverage_line("window", start) + LEGEND
}

#[derive(Serialize)]
struct ShownEvent<'e> {
    ts: &'e str,
    agent: Agent,
    channel: Channel,
    actor: Actor,
    cwd: Option<&'e str>,
    src: &'e str,
    expired: bool,
}

#[derive(Serialize)]
struct Day {
    date: String,
    strong: usize,
    weak: usize,
}

#[derive(Serialize)]
struct CardHistory<'a> {
    first: Option<&'a Commit>,
    last: Option<&'a Commit>,
    uncommitted: bool,
}

/// A skill the card's skill cites, or one citing it, and whether it is held
/// in the window.
#[derive(Serialize)]
struct Link {
    skill: String,
    held: bool,
}

#[derive(Serialize)]
struct ByChannel {
    agent: Agent,
    channel: Channel,
    events: usize,
}

#[derive(Serialize)]
struct Project {
    cwd: String,
    sessions: usize,
}

#[derive(Serialize)]
struct CardUsage {
    /// Collapsed strong events in the window per agent and channel.
    strong: Vec<ByChannel>,
    /// The latest strong event in the whole ledger.
    last_use: Option<String>,
    /// The top three cwds in the window by distinct root sessions with a strong event.
    projects: Vec<Project>,
}

/// The skill card: what `show` prints and `show --json` emits.
#[derive(Serialize)]
struct Card<'a> {
    skill: &'a str,
    since: Option<String>,
    histogram: Vec<Day>,
    /// The newest `limit` events in the window.
    events: Vec<ShownEvent<'a>>,
    total_events: usize,
    description: Option<String>,
    path: PathBuf,
    /// None outside a git worktree.
    history: Option<CardHistory<'a>>,
    /// Newest first.
    grades: Vec<GradeOut>,
    /// Newest first.
    retros: Vec<Eval>,
    /// Strong events on days after the latest retro, over the whole ledger.
    strong_since_retro: Option<usize>,
    cites: Vec<Link>,
    cited_by: Vec<Link>,
    usage: CardUsage,
    examples: Examples,
    /// The latest skill record.
    audit: Option<&'a SkillRecord>,
}

/// A name `show` accepts: any known name (canonical names are known).
pub fn check_skill(inv: &Inventory, skill: &str) -> crate::Result<()> {
    if inv.known.contains(skill) { Ok(()) } else { Err(format!("unknown skill: {skill}")) }
}

/// `skill`'s card; `graph` is the inventory's citation graph.
fn card<'a>(view: &'a View<'a>, graph: &inventory::Graph, skill: &'a str, since: Since, limit: usize) -> Card<'a> {
    let inv = view.input.inv;
    let f = Filter::default();
    let start = view.start(since);
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

    let path = inv.source.join(skill);
    let description = std::fs::read_to_string(path.join("SKILL.md"))
        .ok()
        .and_then(|text| yaml::frontmatter(&text).ok())
        .and_then(|fm| yaml::get(&fm, "description").and_then(yaml::text).map(str::to_string));
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
    let mut examples = examples::examples(agents, &path, skill, ledger.iter().map(|(e, _)| *e));
    let audit = view.input.audits.get(skill);
    examples.curated = audit.map(|r| r.result.examples.clone()).unwrap_or_default();

    Card {
        skill,
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
        usage: card_usage(view, skill, start),
        examples,
        audit,
    }
}

/// The card's usage section for the events of `skill` at or after `start`.
fn card_usage(view: &View, skill: &str, start: Option<Timestamp>) -> CardUsage {
    let f = Filter::default();
    let mut by: BTreeMap<(Agent, Channel), HashSet<&str>> = BTreeMap::new();
    let mut projects: BTreeMap<&str, HashSet<(Agent, &str)>> = BTreeMap::new();
    for (e, _) in view.select(&f, Some(skill), start).filter(|(e, _)| e.channel.strong()) {
        by.entry((e.agent, e.channel)).or_default().insert(&e.session);
        if let Some(cwd) = &e.cwd {
            projects.entry(cwd).or_default().insert((e.agent, &e.session));
        }
    }
    let mut projects: Vec<Project> =
        projects.into_iter().map(|(cwd, s)| Project { cwd: cwd.to_string(), sessions: s.len() }).collect();
    projects.sort_by_key(|p| std::cmp::Reverse(p.sessions));
    projects.truncate(3);
    let last_use = view.select(&f, Some(skill), None).filter(|(e, _)| e.channel.strong()).map(|(_, t)| *t).max();
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
    json!(card_usage(&view, skill, view.start(SHOW_SINCE)))
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

/// Rendered `skima show <skill>` output; Err for an unknown skill.
pub fn show(input: &Input, skill: &str, since: Since, limit: usize, json: bool, tty: bool) -> crate::Result<String> {
    check_skill(input.inv, skill)?;
    let view = View::new(input);
    let graph = inventory::fan_in(&input.inv.source, &input.inv.skills);
    let card = card(&view, &graph, skill, since, limit);
    if json {
        return Ok(format!("{}\n", serde_json::to_value(&card).map_err(|e| e.to_string())?));
    }

    let mut out = String::new();
    let from = card.since.as_ref().map_or("all time".to_string(), |d| format!("since {d}"));
    out.push_str(&format!("{skill}: {from}\n\n"));
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
            let ts = render::local(e.ts, &view.tz, render::MINUTE);
            let src = if e.expired { format!("{} expired", locator(e)) } else { locator(e) };
            vec![ts, name(e.agent), name(e.channel), name(e.actor), e.cwd.map_or("-".into(), tilde), src]
        })
        .collect();
    let ev_header = [
        ("ts", Align::Left),
        ("agent", Align::Left),
        ("channel", Align::Left),
        ("actor", Align::Left),
        ("cwd", Align::Left),
        ("src", Align::Left),
    ];
    out.push_str(&render::table(&ev_header, &ev_rows, tty));

    out.push('\n');
    out.push_str(&format!("description: {}\n", card.description.as_deref().unwrap_or("-")));
    out.push_str(&format!("path: {}\n", tilde(&card.path.to_string_lossy())));
    if let Some(h) = &card.history {
        let commit = |c: &Commit| format!("{} {}", c.date.get(..10).unwrap_or(&c.date), c.subject);
        let mut line = match (h.first, h.last) {
            (Some(first), Some(last)) => format!("first {}, last {}", commit(first), commit(last)),
            _ => "never committed".into(),
        };
        if h.uncommitted {
            line.push_str(", uncommitted changes");
        }
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
    match card.strong_since_retro {
        None => out.push_str("retros: none\n"),
        Some(n) => {
            out.push_str(&format!("retros: {n} strong since the latest\n"));
            let rows: Vec<Vec<String>> = card
                .retros
                .iter()
                .map(|r| vec![r.date.clone(), score(r), r.verdict.clone().unwrap_or_else(|| "-".into())])
                .collect();
            let header = [("date", Align::Left), ("score", Align::Right), ("verdict", Align::Left)];
            out.push_str(&render::table(&header, &rows, tty));
        }
    }

    let links = |ls: &[Link]| match ls {
        [] => "none".to_string(),
        _ => ls
            .iter()
            .map(|l| if l.held { format!("{} (held)", l.skill) } else { l.skill.clone() })
            .collect::<Vec<_>>()
            .join(", "),
    };
    out.push_str(&format!("cites: {}\n", links(&card.cites)));
    out.push_str(&format!("cited by: {}\n", links(&card.cited_by)));

    let strong: Vec<String> =
        card.usage.strong.iter().map(|b| format!("{} {} {}", name(b.agent), name(b.channel), b.events)).collect();
    out.push_str(&format!("strong: {}\n", if strong.is_empty() { "none".into() } else { strong.join(", ") }));
    let last_use = card.usage.last_use.as_deref().and_then(|t| t.parse::<Timestamp>().ok());
    out.push_str(&format!(
        "last use: {}\n",
        last_use.map_or("never".into(), |t| format!("{} ({})", view.date(t), view.relative(t)))
    ));
    let projects: Vec<String> =
        card.usage.projects.iter().map(|p| format!("{} {}", tilde(&p.cwd), p.sessions)).collect();
    out.push_str(&format!("projects: {}\n", if projects.is_empty() { "none".into() } else { projects.join(", ") }));

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
            let date = r.ts.parse::<Timestamp>().map_or(r.ts.clone(), |t| view.date(t).to_string());
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
            let date = a.audited_at.parse::<Timestamp>().map_or(a.audited_at.clone(), |t| view.date(t).to_string());
            let billing = a.judge.billing.map_or("unknown", Billing::name);
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
    Ok(out)
}

/// Events listed on each card of the HTML report: `show`'s default `--limit`.
const REPORT_EVENTS: usize = 20;

/// The model of `skima report`: the `usage --all` rows and
/// coverage, the grades to run, and every inventory skill's card. The page
/// renders this value, so it shows nothing the JSON lacks.
pub fn report(input: &Input, since: Since) -> crate::Result<serde_json::Value> {
    let view = View::new(input);
    let args = UsageArgs { since, agent: None, channel: None, all: true, authoring: false };
    let rows = usage_rows(&view, &args);
    // Rows outside the inventory have no grade and nothing to grade.
    let grades_due: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| input.inv.skills.contains(&r.skill))
        .filter_map(|r| {
            let state = match &r.grade {
                None => "missing",
                Some(g) if g.stale => "stale",
                Some(_) => return None,
            };
            Some(json!({"skill": r.skill, "state": state, "command": format!("/grade {}", r.skill)}))
        })
        .collect();
    let graph = inventory::fan_in(&input.inv.source, &input.inv.skills);
    let cards: Vec<Card> = input.inv.skills.iter().map(|s| card(&view, &graph, s, since, REPORT_EVENTS)).collect();
    let coverage: BTreeMap<String, String> = view.coverage(None).into_iter().collect();
    Ok(json!({
        "generated": input.now.to_string(),
        "since": view.start(since).map(|s| view.date(s).to_string()),
        "usage": {"rows": rows, "coverage": coverage},
        "grades_due": grades_due,
        "cards": serde_json::to_value(&cards).map_err(|e| e.to_string())?,
    }))
}
