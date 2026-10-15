//! Reports over the ledger (SPEC F2–F4): usage, unused, show; collapse, held
//! and cluster.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::evals::{self, Evals};
use crate::inventory::{self, Inventory};
use crate::ledger::{Actor, Agent, Channel, Coverage, Event, Origin, name};
use crate::render::{self, Align};

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
    pub now: jiff::Timestamp,
}

pub struct UsageArgs {
    pub since: Since,
    pub agent: Option<Agent>,
    pub channel: Option<Channel>,
    pub all: bool,
    pub authoring: bool,
}

/// Days in the `30d` sparkline.
const SPARK_DAYS: i64 = 30;

/// The collapse key (SPEC §4.1 Collapse), within one skill.
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

    /// SPEC §4.1: a weak event is authoring when stored so (cwd) or when its
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
    /// and the home's coverage (SPEC D8), with the earliest typed event.
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
                            // Typed events (SPEC §4.2) reach back before the first transcript.
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

#[derive(Serialize)]
struct GradeOut {
    score: Option<String>,
    date: String,
    stale: bool,
    file: String,
}

fn grade_out(evals: &Evals) -> Option<GradeOut> {
    evals.grade.as_ref().map(|g| GradeOut {
        score: g.eval.score.clone(),
        date: g.eval.date.clone(),
        stale: g.stale,
        file: g.eval.file.display().to_string(),
    })
}

fn grade_cell(g: &Option<GradeOut>) -> String {
    match g {
        Some(g) => format!("{}{}", g.score.as_deref().unwrap_or("?"), if g.stale { "*" } else { "" }),
        None => "-".into(),
    }
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

/// Rendered `skima usage` output (text, or JSON when `json`).
pub fn usage(input: &Input, args: &UsageArgs, json: bool, tty: bool) -> String {
    let view = View::new(input);
    let f = Filter { all: args.all, authoring: args.authoring, agent: args.agent, channel: args.channel };
    let start = view.start(args.since);
    let mut skills: BTreeSet<&str> = input.inv.skills.iter().map(String::as_str).collect();
    if args.all {
        skills.extend(view.select(&f, None, start).map(|(e, _)| e.skill.as_str()));
    }
    let mut rows: Vec<UsageRow> = skills
        .into_iter()
        .map(|skill| {
            let window: Vec<_> = view.select(&f, Some(skill), start).collect();
            let keys: HashSet<Key> = window.iter().map(|(e, _)| (e.agent, e.session.as_str(), e.channel)).collect();
            let count = |c: Channel| keys.iter().filter(|k| k.2 == c).count();
            let sess = keys.iter().filter(|k| k.2.strong()).map(|k| (k.0, k.1)).collect::<HashSet<_>>().len();
            let agents: BTreeSet<Agent> = window.iter().map(|(e, _)| e.agent).collect();
            let last = view.select(&f, Some(skill), None).filter(|(e, _)| e.channel.strong()).map(|(_, t)| *t).max();
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
                grade_cell(&r.grade),
            ]
        })
        .collect();
    render::table(&header, &cells, tty) + &view.coverage_line("coverage", None)
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
}

/// Rendered `skima unused` output.
pub fn unused(input: &Input, since: Since, json: bool, tty: bool) -> String {
    let view = View::new(input);
    let f = Filter::default();
    let start = view.start(since);
    let window: Vec<_> = view.select(&f, None, start).collect();
    let used: BTreeSet<&str> =
        window.iter().filter(|(e, _)| e.channel.strong()).map(|(e, _)| e.skill.as_str()).collect();
    let weak: BTreeSet<&str> = window.iter().map(|(e, _)| e.skill.as_str()).collect();
    let graph = inventory::fan_in(&input.inv.source, &input.inv.skills);
    let citers = |b: &str| -> Vec<String> {
        graph.iter().filter(|(_, cited)| cited.contains(b)).map(|(a, _)| a.clone()).collect()
    };
    let mut rows: Vec<UnusedRow> = input
        .inv
        .skills
        .iter()
        .filter(|s| !used.contains(s.as_str()))
        .map(|skill| {
            // SPEC §6.3: outside a git worktree no skill is new.
            let is_new = input.inv.history.as_ref().is_some_and(|h| {
                h.first(skill)
                    .and_then(|c| c.date.parse::<Timestamp>().ok())
                    .is_none_or(|t| start.is_some_and(|s| t > s))
            });
            let (holders, others): (Vec<String>, Vec<String>) =
                citers(skill).into_iter().partition(|a| used.contains(a.as_str()));
            let (status, holders, cluster) = if is_new {
                ("new", Vec::new(), Vec::new())
            } else if holders.is_empty() {
                ("unused", Vec::new(), others)
            } else {
                ("held", holders, Vec::new())
            };
            UnusedRow { skill: skill.clone(), status, holders, cluster, weak_only: weak.contains(skill.as_str()) }
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
            if r.weak_only {
                notes.push("weak-only".into());
            }
            vec![r.skill.clone(), r.status.into(), notes.join("; ")]
        })
        .collect();
    render::table(&header, &cells, tty) + &view.coverage_line("window", start)
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

/// A name `show` accepts: any known name (canonical names are known).
pub fn check_skill(inv: &Inventory, skill: &str) -> crate::Result<()> {
    if inv.known.contains(skill) { Ok(()) } else { Err(format!("unknown skill: {skill}")) }
}

/// Rendered `skima show <skill>` output; Err for an unknown skill.
pub fn show(input: &Input, skill: &str, since: Since, limit: usize, json: bool, tty: bool) -> crate::Result<String> {
    check_skill(input.inv, skill)?;
    let view = View::new(input);
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
    let histogram: Vec<(String, usize, usize)> =
        days.iter().map(|(d, (s, w))| (d.to_string(), s.len(), w.len())).collect();

    let shown: Vec<ShownEvent> = window
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

    let evals = evals::load(input.inv, skill);
    let grade = grade_out(&evals);
    let latest_retro = evals.retros.last().map(|r| r.date.clone());
    // Strong loads on days after the latest retro's date, over the whole ledger.
    let since_retro = latest_retro.as_deref().and_then(|d| d.parse::<Date>().ok()).map(|d| {
        view.select(&f, Some(skill), None)
            .filter(|(e, t)| e.channel.strong() && view.date(*t) > d)
            .map(|(e, _)| (e.agent, e.session.as_str(), e.channel))
            .collect::<HashSet<Key>>()
            .len()
    });

    if json {
        let hist: Vec<_> = histogram.iter().map(|(d, s, w)| json!({"date": d, "strong": s, "weak": w})).collect();
        return Ok(format!(
            "{}\n",
            json!({
                "skill": skill,
                "since": start.map(|s| view.date(s).to_string()),
                "histogram": hist,
                "events": shown,
                "total_events": window.len(),
                "grade": grade,
                "retros": {"count": evals.retros.len(), "latest": latest_retro, "strong_since": since_retro},
            })
        ));
    }

    let mut out = String::new();
    let from = start.map_or("all time".to_string(), |s| format!("since {}", view.date(s)));
    out.push_str(&format!("{skill}: {from}\n\n"));
    let max = histogram.iter().map(|h| h.1).max().unwrap_or(0).max(1);
    let hist_rows: Vec<Vec<String>> = histogram
        .iter()
        .map(|(d, s, w)| vec![d.clone(), s.to_string(), w.to_string(), "█".repeat((s * 40).div_ceil(max))])
        .collect();
    let hist_header = [("day", Align::Left), ("strong", Align::Right), ("weak", Align::Right), ("", Align::Left)];
    out.push_str(&render::table(&hist_header, &hist_rows, tty));

    out.push_str(&format!("\nlast {} of {} events:\n", shown.len(), window.len()));
    let ev_rows: Vec<Vec<String>> = shown
        .iter()
        .map(|e| {
            let ts =
                e.ts.parse::<Timestamp>()
                    .map_or(e.ts.to_string(), |t| t.to_zoned(view.tz.clone()).strftime("%Y-%m-%d %H:%M").to_string());
            let src = if e.expired { format!("{} expired", e.src) } else { e.src.to_string() };
            vec![ts, name(e.agent), name(e.channel), name(e.actor), e.cwd.unwrap_or("-").to_string(), src]
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
    match &grade {
        Some(g) => out.push_str(&format!(
            "grade: {} on {}{}\n",
            g.score.as_deref().unwrap_or("?"),
            g.date,
            if g.stale { " (stale: the skill changed after it)" } else { "" }
        )),
        None => out.push_str("grade: none\n"),
    }
    match (&latest_retro, since_retro) {
        (Some(d), Some(n)) => out.push_str(&format!("retros: {}, latest {d}, {n} strong since\n", evals.retros.len())),
        _ => out.push_str("retros: 0\n"),
    }
    Ok(out)
}
