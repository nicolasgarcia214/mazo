//! The ledger as reports see it, and the windows they count over.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp, ToSpan};

use super::{Input, Since, UsageArgs};
use crate::ctx::Contexts;
use crate::inventory::Scope;
use crate::ledger::{Actor, Agent, Channel, Event, Origin};

/// The collapse key, within one skill.
pub(super) type Key<'e> = (Agent, &'e str, Channel);

/// The ledger as reports see it: parsed timestamps, local days, the
/// report-time authoring sessions, and the events of project skills.
pub(super) struct View<'a> {
    pub(super) input: &'a Input<'a>,
    today: Date,
    events: Vec<(&'a Event, Timestamp)>,
    authoring_sessions: HashSet<(Agent, &'a str)>,
    /// The ids of the events whose path is in a project skill's directory.
    project: HashSet<&'a str>,
    /// The source repository's GitHub URL and HEAD, read once a card needs them.
    pub(super) upstream: OnceCell<Option<(String, String)>>,
    /// What each skill's ctx reads, read once a row or card needs it.
    ctx: OnceCell<Contexts>,
}

/// Which events a report counts. The default is canonical origin, no probe
/// actor and no authoring read.
#[derive(Default)]
pub(super) struct Filter {
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
pub(super) fn day(input: &Input, t: Timestamp) -> Date {
    t.to_zoned(input.tz.clone()).date()
}

/// `today`, `2d ago`.
pub(super) fn relative(input: &Input, t: Timestamp) -> String {
    match (day(input, input.now) - day(input, t)).get_days() {
        ..=0 => "today".into(),
        n => format!("{n}d ago"),
    }
}

impl<'a> View<'a> {
    pub(super) fn new(input: &'a Input<'a>) -> View<'a> {
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
        View { input, today, events, authoring_sessions, project, upstream: OnceCell::new(), ctx: OnceCell::new() }
    }

    pub(super) fn ctx(&self) -> &Contexts {
        self.ctx.get_or_init(|| Contexts::new(self.input.cfg, self.input.inv))
    }

    /// A project skill's event is one in its directory; every other is global.
    pub(super) fn scope(&self, e: &Event) -> Scope {
        if self.project.contains(e.id.as_str()) { Scope::Project } else { Scope::Global }
    }

    pub(super) fn date(&self, t: Timestamp) -> Date {
        day(self.input, t)
    }

    pub(super) fn start(&self, since: Since) -> Option<Timestamp> {
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
    pub(super) fn select<'s>(
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
    pub(super) fn used(&self, start: Option<Timestamp>) -> BTreeSet<&'a str> {
        let f = Filter::default();
        self.select(&f, None, start).filter(|(e, _)| e.channel.strong()).map(|(e, _)| e.skill.as_str()).collect()
    }

    /// Distinct (agent, session) pairs with a strong event on a local day
    /// `days` ago, today being 0.
    pub(super) fn strong_sessions<'s>(
        &self,
        events: impl Iterator<Item = &'s (&'s Event, Timestamp)>,
        days: std::ops::Range<i64>,
    ) -> usize {
        let ago = |t: Timestamp| (self.today - self.date(t)).get_days() as i64;
        let pairs = events.filter(|(e, t)| e.channel.strong() && days.contains(&ago(*t)));
        pairs.map(|(e, _)| (e.agent, e.session.as_str())).collect::<HashSet<_>>().len()
    }

    /// Distinct collapse keys of strong events per local day, for the last `days` days.
    pub(super) fn daily_strong<'s>(
        &self,
        events: impl Iterator<Item = &'s (&'s Event, Timestamp)>,
        days: i64,
    ) -> Vec<usize> {
        let mut per_day: Vec<HashSet<Key>> = vec![HashSet::new(); days as usize];
        for (e, t) in events.filter(|(e, _)| e.channel.strong()) {
            let ago = (self.today - self.date(*t)).get_days() as i64;
            if (0..days).contains(&ago) {
                per_day[(days - 1 - ago) as usize].insert((e.agent, &e.session, e.channel));
            }
        }
        per_day.iter().map(HashSet::len).collect()
    }

    /// Distinct (agent, session) pairs with a strong event per bucket of
    /// `since`'s [`Unit`], from the bucket of the window's start (of the first
    /// event, for all time) to today's.
    pub(super) fn buckets<'s>(
        &self,
        events: impl Iterator<Item = &'s (&'s Event, Timestamp)>,
        since: Since,
    ) -> Vec<Bucket> {
        let unit = Unit::of(since);
        let mut per: BTreeMap<Date, HashSet<(Agent, &str)>> = BTreeMap::new();
        for (e, t) in events.filter(|(e, _)| e.channel.strong()) {
            per.entry(unit.start(self.date(*t))).or_default().insert((e.agent, &e.session));
        }
        let today = unit.start(self.today);
        let first = self.start(since).map(|s| unit.start(self.date(s)));
        let mut at = first.or_else(|| per.keys().next().copied()).unwrap_or(today).min(today);
        let mut out = Vec::new();
        while at <= today {
            let sessions = per.get(&at).map_or(0, HashSet::len);
            out.push(Bucket { label: unit.label(at), sessions, partial: at == today });
            at = unit.next(at);
        }
        out
    }

    /// Per home: the effective window start, the later of the report window
    /// and the home's coverage, with the earliest typed event.
    pub(super) fn coverage(&self, start: Option<Timestamp>) -> Vec<(String, String)> {
        self.input
            .cfg
            .homes
            .iter()
            .map(|h| {
                let text = match self.input.coverage.get(&h.name) {
                    None => "never scanned".to_string(),
                    Some(c) => match c.first_at() {
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

    pub(super) fn coverage_line(&self, label: &str, start: Option<Timestamp>) -> String {
        coverage_note(label, self.coverage(start).iter().map(|(h, t)| (h, t))) + "\n"
    }
}

/// What one bar of a card's activity chart spans: a day over 30 days or
/// fewer, an ISO week from Monday over a longer window, a calendar month
/// over all time.
#[derive(Clone, Copy)]
enum Unit {
    Day,
    Week,
    Month,
}

impl Unit {
    fn of(since: Since) -> Unit {
        match since {
            Since::Days(..=30) => Unit::Day,
            Since::Days(_) => Unit::Week,
            Since::All => Unit::Month,
        }
    }

    fn plural(self) -> &'static str {
        match self {
            Unit::Day => "days",
            Unit::Week => "weeks",
            Unit::Month => "months",
        }
    }

    /// The first day of the bucket holding `d`.
    fn start(self, d: Date) -> Date {
        match self {
            Unit::Day => d,
            Unit::Week => d - i64::from(d.weekday().to_monday_zero_offset()).days(),
            Unit::Month => d.first_of_month(),
        }
    }

    fn next(self, d: Date) -> Date {
        match self {
            Unit::Day => d + 1.day(),
            Unit::Week => d + 1.week(),
            Unit::Month => d + 1.month(),
        }
    }

    /// `13`, `W42`, `Oct`: short enough to sit under a chart's bar.
    fn label(self, d: Date) -> String {
        match self {
            Unit::Day => d.strftime("%d").to_string(),
            Unit::Week => format!("W{:02}", d.iso_week_date().week()),
            Unit::Month => d.strftime("%b").to_string(),
        }
    }
}

/// What a bar of a card's activity chart over `since` spans, in the plural:
/// `days`, `weeks` or `months`.
pub fn buckets_of(since: Since) -> &'static str {
    Unit::of(since).plural()
}

/// A bar of a card's activity chart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    pub label: String,
    /// Distinct (agent, session) pairs with a strong event in the bucket.
    pub sessions: usize,
    /// Whether the bucket holds today, so is still filling.
    pub partial: bool,
}

/// `label: home window · …`: the coverage note under `usage` and `unused`.
pub fn coverage_note<'s>(label: &str, homes: impl IntoIterator<Item = (&'s String, &'s String)>) -> String {
    let parts: Vec<String> = homes.into_iter().map(|(h, t)| format!("{h} {t}")).collect();
    format!("{label}: {}", parts.join(" · "))
}
