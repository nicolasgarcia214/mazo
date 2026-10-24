//! The skill card: what `show` prints and the report lists per skill.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::civil::Date;
use serde::Serialize;

use super::unused::citers;
use super::usage::{GradeOut, vet_of};
use super::view::{Bucket, Filter, Key, View, day, relative};
use super::{AUDIT_IMPROVEMENTS, Input, SHOW_SINCE, Since, UsageArgs, json_line};
use crate::audit::{SkillRecord, VetVerdict};
use crate::ctx::Ctx;
use crate::evals::{self, Eval, Evals};
use crate::examples::{self, Examples};
use crate::history::Commit;
use crate::inventory::{self, Inventory, Scope};
use crate::ledger::{Actor, Agent, Channel, name};
use crate::render::{self, Align};
use crate::store::Locked;

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
pub(crate) fn cut(text: &str, max: usize) -> String {
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
    /// The TUI's chart: strong sessions per day, week or month of the window.
    #[serde(skip)]
    pub activity: Vec<Bucket>,
    /// The newest `limit` events in the window.
    pub events: Vec<ShownEvent<'a>>,
    pub total_events: usize,
    pub description: Option<String>,
    /// None for a name in no root without a source.
    pub path: Option<PathBuf>,
    /// The GitHub repository and commit the skill came from (D24).
    pub origin: Option<SkillOrigin>,
    /// The [`crate::store::content_hash`] of the skill's directory; None for a name in no root.
    pub content_hash: Option<String>,
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
    /// The verdict `mazo add` recorded when it vetted the skill.
    pub vet: Option<&'a VetVerdict>,
    pub ctx: Ctx,
}

/// Where a skill came from on GitHub: a store skill's lockfile entry, or the
/// source repository's `origin` remote at its HEAD.
#[derive(Serialize)]
pub struct SkillOrigin {
    /// `https://github.com/<owner>/<repo>.git`.
    pub repo: String,
    /// The skill's directory inside `repo`.
    pub subpath: String,
    pub r#ref: Option<String>,
    pub commit: String,
}

/// `skill`'s [`SkillOrigin`]; None for a local install or one off github.com,
/// and for a source repository with no `origin` remote on github.com. Read
/// from the lockfile and git config only, never the network.
fn origin_of(view: &View, skill: &str) -> Option<SkillOrigin> {
    let inv = view.input.inv;
    if let Some(locked) = vet_of(inv, skill) {
        return Some(SkillOrigin {
            repo: crate::store::github_repo(&locked.origin)?,
            subpath: locked.subpath.clone(),
            r#ref: locked.r#ref.clone(),
            commit: locked.commit.clone()?,
        });
    }
    let source = inv.source.as_deref().filter(|s| inv.root(skill) == Some(*s))?;
    let worktree = inv.worktree.as_deref()?;
    // Once per report, whatever the number of cards.
    let (repo, head) = view
        .upstream
        .get_or_init(|| {
            let remote = inventory::git(worktree, &["config", "--get", "remote.origin.url"])?;
            let repo = crate::store::github_repo(remote.trim())?;
            Some((repo, inventory::git(worktree, &["rev-parse", "HEAD"])?.trim().to_string()))
        })
        .clone()?;
    let dir = std::fs::canonicalize(source.join(skill)).ok()?;
    Some(SkillOrigin {
        repo,
        subpath: dir.strip_prefix(worktree).ok()?.to_string_lossy().into_owned(),
        r#ref: None,
        commit: head,
    })
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
pub(super) fn card<'a>(
    view: &View<'a>,
    graph: &inventory::Graph,
    skill: &'a str,
    args: &UsageArgs,
    limit: usize,
) -> Card<'a> {
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

    let Evals { content_hash, grades, mut retros } = evals::load(&view.input.cfg.evals_dir, inv, skill);
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
        activity: view.buckets(view.select(&f, Some(skill), start), args.since),
        events,
        total_events: window.len(),
        description,
        origin: origin_of(view, skill),
        path,
        content_hash,
        history,
        grades,
        retros,
        strong_since_retro,
        cites,
        cited_by,
        usage: card_usage(view, &f, skill, start),
        examples,
        audit,
        vet: vet.and_then(|l| l.verdict.as_ref()),
        // `show` takes only a known name, never a project-only one, so a card is a global skill's.
        ctx: view.ctx().of(inv, skill, Scope::Global),
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
            card.grades.iter().map(|g| vec![g.grade.eval.date.clone(), score(&g.grade.eval), g.mark.into()]).collect();
        let header = [("date", Align::Left), ("score", Align::Right), ("mark", Align::Left)];
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
    if let Some(v) = card.vet {
        out.push_str(&format!("vet: {}\n", v.line()));
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
