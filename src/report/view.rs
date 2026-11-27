use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use jiff::civil::Date;
use jiff::{SignedDuration, Timestamp, ToSpan};

use super::{Input, Since, UsageArgs};
use crate::ctx::Contexts;
use crate::evals::{self, Evals};
use crate::inventory::Scope;
use crate::ledger::{Actor, Agent, Channel, Event, Origin};

pub(super) type Key<'e> = (Agent, &'e str, Channel);

pub(super) struct View<'a> {
    pub(super) input: &'a Input<'a>,
    today: Date,
    authoring_sessions: HashSet<(Agent, &'a str)>,
    project: HashSet<&'a str>,
    by_skill: HashMap<&'a str, Vec<&'a Event>>,
    used: RefCell<HashMap<Option<Timestamp>, Rc<BTreeSet<&'a str>>>>,
    evals: RefCell<HashMap<String, Evals>>,
    pub(super) upstream: OnceCell<Option<(String, String)>>,
    ctx: OnceCell<Contexts>,
}

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

pub(super) fn day(input: &Input, t: Timestamp) -> Date {
    t.to_zoned(input.tz.clone()).date()
}

pub(super) fn relative(input: &Input, t: Timestamp) -> String {
    match (day(input, input.now) - day(input, t)).get_days() {
        ..=0 => "today".into(),
        n => format!("{n}d ago"),
    }
}

impl<'a> View<'a> {
    pub(super) fn new(input: &'a Input<'a>) -> View<'a> {
        let today = day(input, input.now);
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
        let mut by_skill: HashMap<&str, Vec<&Event>> = HashMap::new();
        for e in input.events {
            by_skill.entry(&e.skill).or_default().push(e);
        }
        View {
            input,
            today,
            authoring_sessions,
            project,
            by_skill,
            used: RefCell::default(),
            evals: RefCell::default(),
            upstream: OnceCell::new(),
            ctx: OnceCell::new(),
        }
    }

    pub(super) fn evals(&self, skill: &str) -> Evals {
        let load = || evals::load(&self.input.cfg.evals_dir, self.input.inv, skill);
        self.evals.borrow_mut().entry(skill.to_string()).or_insert_with(load).clone()
    }

    pub(super) fn ctx(&self) -> &Contexts {
        self.ctx.get_or_init(|| Contexts::new(self.input.cfg, self.input.inv))
    }

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

    fn is_authoring(&self, e: &Event) -> bool {
        !e.channel.strong() && (e.authoring || self.authoring_sessions.contains(&(e.agent, e.session.as_str())))
    }

    fn kept(&self, e: &Event, f: &Filter) -> bool {
        let canonical = e.origin == Origin::Canonical || self.scope(e) == Scope::Project;
        (f.all || (canonical && e.actor != Actor::Probe))
            && (f.authoring || !self.is_authoring(e))
            && f.agent.is_none_or(|a| e.agent == a)
            && f.channel.is_none_or(|c| e.channel == c)
    }

    pub(super) fn select<'s>(
        &'s self,
        f: &'s Filter,
        skill: Option<&'s str>,
        start: Option<Timestamp>,
    ) -> impl Iterator<Item = &'a Event> + 's {
        let all = skill.is_none().then_some(self.input.events).into_iter().flatten();
        let one = skill.and_then(|s| self.by_skill.get(s)).into_iter().flatten().copied();
        all.chain(one).filter(move |e| start.is_none_or(|s| e.ts >= s) && self.kept(e, f))
    }

    pub(super) fn used(&self, start: Option<Timestamp>) -> Rc<BTreeSet<&'a str>> {
        let strong = || {
            let f = Filter::default();
            Rc::new(self.select(&f, None, start).filter(|e| e.channel.strong()).map(|e| e.skill.as_str()).collect())
        };
        self.used.borrow_mut().entry(start).or_insert_with(strong).clone()
    }

    pub(super) fn strong_sessions<'s>(
        &self,
        events: impl Iterator<Item = &'s Event>,
        days: std::ops::Range<i64>,
    ) -> usize {
        let ago = |t: Timestamp| (self.today - self.date(t)).get_days() as i64;
        let pairs = events.filter(|e| e.channel.strong() && days.contains(&ago(e.ts)));
        pairs.map(|e| (e.agent, e.session.as_str())).collect::<HashSet<_>>().len()
    }

    pub(super) fn daily_strong<'s>(&self, events: impl Iterator<Item = &'s Event>, days: i64) -> Vec<usize> {
        let mut per_day: Vec<HashSet<Key>> = vec![HashSet::new(); days as usize];
        for e in events.filter(|e| e.channel.strong()) {
            let ago = (self.today - self.date(e.ts)).get_days() as i64;
            if (0..days).contains(&ago) {
                per_day[(days - 1 - ago) as usize].insert((e.agent, &e.session, e.channel));
            }
        }
        per_day.iter().map(HashSet::len).collect()
    }

    pub(super) fn buckets<'s>(&self, events: impl Iterator<Item = &'s Event>, since: Since) -> Vec<Bucket> {
        let unit = Unit::of(since);
        let mut per: BTreeMap<Date, HashSet<(Agent, &str)>> = BTreeMap::new();
        for e in events.filter(|e| e.channel.strong()) {
            per.entry(unit.start(self.date(e.ts))).or_default().insert((e.agent, &e.session));
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
                            let typed = self
                                .input
                                .events
                                .iter()
                                .filter(|e| e.home == h.name && e.channel == Channel::Typed)
                                .map(|e| start.map_or(e.ts, |s| s.max(e.ts)))
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

    fn label(self, d: Date) -> String {
        match self {
            Unit::Day => d.strftime("%d").to_string(),
            Unit::Week => format!("W{:02}", d.iso_week_date().week()),
            Unit::Month => d.strftime("%b").to_string(),
        }
    }
}

pub fn buckets_of(since: Since) -> &'static str {
    Unit::of(since).plural()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    pub label: String,
    pub sessions: usize,
    pub partial: bool,
}

pub fn coverage_note<'s>(label: &str, homes: impl IntoIterator<Item = (&'s String, &'s String)>) -> String {
    let parts: Vec<String> = homes.into_iter().map(|(h, t)| format!("{h} {t}")).collect();
    if parts.is_empty() {
        return format!("{label}: no home");
    }
    format!("{label}: {}", parts.join(" · "))
}
