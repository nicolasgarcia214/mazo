//! The TUI of bare `mazo`: the usage table, the unused list and one skill's
//! card, rendered from the typed report model over a finished scan. Its
//! actions, `retire`, `sync`, `add`, `update` and `audit`, each ask first and
//! run the CLI command's own code path; the data is re-read after each. An
//! add, an update or an audit runs on a background thread, one at a time,
//! and reports in the job pane; an add's picker and y/N questions open
//! dialogs over the screen.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use ratatui::Frame;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::audit::{self, SkillRecord};
use crate::config::{Config, Home};
use crate::inventory::Inventory;
use crate::ledger::{self, Agent, Channel, Coverage, Ledger, name};
use crate::lifecycle::{self, Retired};
use crate::links::{self, Plan};
use crate::pick::{Picker, Step};
use crate::render::{self, Align, Colour};
use crate::report::{self, Input, SHOW_LIMIT, Since, Unused, Usage, UsageArgs};
use crate::store::{self, Question};

/// The windows `w` cycles through.
const WINDOWS: [Since; 3] = [Since::Days(30), Since::Days(60), Since::All];

/// The most lines the job pane shows: its last ones.
const PANE_LINES: usize = 6;

/// How long quitting waits for a cancelled job: longer than a judge has to
/// exit on SIGTERM before it is killed.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// The keys, as the footer lists them.
const KEYS: [&str; 15] = [
    "↑↓←→/hjkl move",
    "Enter card/retire",
    "Esc back",
    "Tab usage/unused",
    "/ filter",
    "w window",
    "a agent",
    "c channel",
    "A authoring",
    "r retire",
    "s sync",
    "i add",
    "u/U update one/all",
    "x audit",
    "q quit",
];

/// [`KEYS`] joined with ` · ` into lines of at most `width` columns.
fn keys(width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<String> = vec![];
    for key in KEYS {
        match lines.last_mut() {
            Some(l) if l.chars().count() + 3 + key.chars().count() <= width => *l = format!("{l} · {key}"),
            _ => lines.push(key.to_string()),
        }
    }
    lines.into_iter().map(|l| Line::raw(l).dim()).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Usage,
    Unused,
    Card,
}

/// What an [`Input`] borrows, owned, so an action can re-read it and a job
/// can take a copy.
#[derive(Clone)]
struct Data {
    cfg: Config,
    inv: Inventory,
    events: Vec<ledger::Event>,
    coverage: BTreeMap<String, Coverage>,
    audits: BTreeMap<String, SkillRecord>,
    now: jiff::Timestamp,
    tz: jiff::tz::TimeZone,
    /// The data directory the actions write and [`Data::reload`] reads.
    dir: PathBuf,
}

impl Data {
    fn input(&self) -> Input<'_> {
        Input {
            cfg: &self.cfg,
            inv: &self.inv,
            events: &self.events,
            coverage: &self.coverage,
            audits: &self.audits,
            now: self.now,
            tz: self.tz.clone(),
        }
    }

    /// Re-reads the inventory as `mazo` loads it, the ledger, the coverage
    /// and the audits, as of now.
    fn reload(&mut self) -> crate::Result<()> {
        self.inv = store::inventory(self.cfg.source.as_deref(), &self.dir)?;
        self.events = Ledger::load(&self.dir)?.events().to_vec();
        self.coverage = ledger::load_coverage(&self.dir)?;
        self.audits = audit::latest(&self.dir)?;
        self.now = jiff::Timestamp::now();
        Ok(())
    }
}

/// A change `y` applies at once, through its CLI command's code path.
enum Act {
    Retire(String),
    /// The plan the dialog shows, applied as shown.
    Sync(Plan),
}

/// What `y` does in a dialog.
enum Ask {
    Act(Act),
    /// Starts the job.
    Audit(String),
    /// Starts the job: one store skill, or every one.
    Update(Option<String>),
    /// Stops the running job and quits.
    Quit,
    /// The add source typed so far; Enter starts the job, Esc cancels.
    Source(String),
    /// The add job's picker; its selection, none when cancelled, is the reply.
    Pick(Picker, mpsc::Sender<Vec<String>>),
    /// The add job's y/N question and its default: Enter takes it, `y` and
    /// `n` answer, and Esc or quitting is No.
    Answer(mpsc::Sender<bool>, bool),
}

/// A background add, audit or update and what its pane shows.
struct Job {
    /// `add`, `audit` or `update`.
    verb: &'static str,
    /// The skill, every store skill, or the add source.
    target: String,
    /// What the run reported, then what its CLI command prints.
    lines: Vec<String>,
    rx: mpsc::Receiver<String>,
    /// What an add or update asks, each opened as a dialog once none is open.
    asks: Option<mpsc::Receiver<Question>>,
    cancel: Arc<AtomicBool>,
    /// The thread until it ends; it returns whether the run succeeded.
    thread: Option<JoinHandle<bool>>,
    ok: bool,
}

/// Lines over the screen that take the next key: `y` does `ask`, any other
/// key closes it; a source, a picker or an answer takes keys of its own.
struct Dialog {
    lines: Vec<String>,
    ask: Option<Ask>,
}

/// The TUI's state over one report input.
pub struct App {
    data: Data,
    screen: Screen,
    /// The list a card goes back to.
    back: Screen,
    usage_since: Since,
    unused_since: Since,
    card_since: Since,
    agent: Option<Agent>,
    channel: Option<Channel>,
    authoring: bool,
    /// The substring a listed skill's name must contain.
    filter: String,
    /// Whether `/` is still taking the filter.
    typing: bool,
    selected: usize,
    /// Rows a page move skips: the list rows or card lines the last draw showed.
    page: usize,
    usage: Usage,
    unused: Unused,
    /// The open card's skill and its `show` lines.
    card: Option<(String, Vec<String>)>,
    scroll: usize,
    dialog: Option<Dialog>,
    /// The running job, or the last one until `Esc` closes its pane.
    job: Option<Job>,
    /// Whether an action ran since the last draw: it may have written under the screen.
    clear: bool,
}

/// The next of `all` after `cur`, `None` (no filter) after the last.
fn cycle<T: Copy + PartialEq>(all: &[T], cur: Option<T>) -> Option<T> {
    match cur {
        None => all.first().copied(),
        Some(c) => all.iter().skip_while(|x| **x != c).nth(1).copied(),
    }
}

/// Applies the confirmed sync `plan` under the data lock, as `mazo sync`
/// does, or refuses when a re-plan on the inventory read under it differs.
fn synced(d: &Data, plan: &Plan) -> crate::Result<Vec<String>> {
    let _lock = ledger::lock(&d.dir)?;
    let inv = store::inventory(d.cfg.source.as_deref(), &d.dir)?;
    let homes: Vec<&Home> = d.cfg.homes.iter().collect();
    match links::checked(&d.cfg, &inv, &homes) {
        Ok(now) if now.actions == plan.actions => links::apply(plan),
        _ => Err("sync refused: the links changed since the plan; press s to plan again".into()),
    }
}

fn next_window(since: Since) -> Since {
    cycle(&WINDOWS, Some(since)).unwrap_or(WINDOWS[0])
}

fn window(since: Since) -> String {
    match since {
        Since::Days(n) => format!("{n}d"),
        Since::All => "all".into(),
    }
}

/// `t` styled in its [`render::status_colour`].
fn token(t: &str) -> Span<'static> {
    let span = Span::raw(t.to_string());
    match render::status_colour(t) {
        Some(Colour::Red) => span.red(),
        Some(Colour::Yellow) => span.yellow(),
        None => span,
    }
}

/// A cell's spans: each `; `-separated note, and a grade's `+N` or `+wip`
/// mark apart from its score, styled as a status token.
fn styled(cell: &str) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (i, piece) in cell.split("; ").enumerate() {
        if i > 0 {
            out.push(Span::raw("; "));
        }
        match piece.find('+') {
            Some(at) if at > 0 && render::status_colour(&piece[at..]).is_some() => {
                out.extend([token(&piece[..at]), token(&piece[at..])]);
            }
            _ => out.push(token(piece)),
        }
    }
    out
}

/// The header and rows of a table as lines, one space between columns. The
/// columns past the first two that do not fit in `width` are dropped from
/// the right. The skill column is never styled, so a name never takes a
/// status colour.
fn table(header: &[(&str, Align)], rows: &[Vec<String>], width: usize) -> Vec<Line<'static>> {
    let chars = |s: &str| s.chars().count();
    let mut widths: Vec<usize> = header.iter().map(|(h, _)| chars(h)).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(chars(cell));
        }
    }
    let mut used = 0;
    let mut shown = 0;
    for (i, w) in widths.iter().enumerate() {
        let need = used + w + usize::from(i > 0);
        if i >= 2 && need > width {
            break;
        }
        (used, shown) = (need, i + 1);
    }
    let line = |cells: Vec<&str>, style: bool| {
        let mut spans = Vec::new();
        for (i, (cell, (&w, (_, align)))) in cells.into_iter().zip(widths.iter().zip(header)).take(shown).enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            let pad = Span::raw(" ".repeat(w - chars(cell)));
            let text = if style && i > 0 { styled(cell) } else { vec![Span::raw(cell.to_string())] };
            match align {
                Align::Left => spans.extend(text.into_iter().chain([pad])),
                Align::Right => spans.extend(std::iter::once(pad).chain(text)),
            }
        }
        Line::from(spans)
    };
    let mut out = vec![line(header.iter().map(|(h, _)| *h).collect(), false).bold()];
    out.extend(rows.iter().map(|r| line(r.iter().map(String::as_str).collect(), true)));
    out
}

impl App {
    /// The usage view of `input`, over each command's default window and
    /// filter. The actions write under, and re-read from, the data directory `data`.
    pub fn new(input: &Input, data: &Path) -> App {
        let (usage_since, unused_since) = (Since::Days(30), Since::Days(60));
        App {
            data: Data {
                cfg: input.cfg.clone(),
                inv: input.inv.clone(),
                events: input.events.to_vec(),
                coverage: input.coverage.clone(),
                audits: input.audits.clone(),
                now: input.now,
                tz: input.tz.clone(),
                dir: data.to_path_buf(),
            },
            screen: Screen::Usage,
            back: Screen::Usage,
            usage_since,
            unused_since,
            card_since: usage_since,
            agent: None,
            channel: None,
            authoring: false,
            filter: String::new(),
            typing: false,
            selected: 0,
            page: 1,
            usage: report::usage_table(input, &UsageArgs::new(usage_since)),
            unused: report::unused_list(input, unused_since),
            card: None,
            scroll: 0,
            dialog: None,
            job: None,
            clear: false,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// Whether a job is running.
    pub fn busy(&self) -> bool {
        self.running().is_some()
    }

    fn running(&self) -> Option<&Job> {
        self.job.as_ref().filter(|j| j.thread.is_some())
    }

    /// What the usage view and its cards count over `since`.
    fn args(&self, since: Since) -> UsageArgs {
        UsageArgs { agent: self.agent, channel: self.channel, authoring: self.authoring, ..UsageArgs::new(since) }
    }

    fn matches(&self, skill: &str) -> bool {
        skill.contains(&self.filter)
    }

    /// The current list: on a card, the one it was opened from.
    fn list(&self) -> Screen {
        if self.screen == Screen::Card { self.back } else { self.screen }
    }

    /// The skills the current list shows.
    fn listed(&self) -> Vec<&str> {
        let skills: Vec<&str> = match self.list() {
            Screen::Unused => self.unused.skills.iter().map(|r| r.skill.as_str()).collect(),
            _ => self.usage.rows.iter().map(|r| r.skill.as_str()).collect(),
        };
        skills.into_iter().filter(|s| self.matches(s)).collect()
    }

    fn open(&mut self, skill: &str) {
        let input = self.data.input();
        let card = report::skill_card(&input, skill, &self.args(self.card_since), SHOW_LIMIT);
        let lines = report::card_text(&input, &card, false).lines().map(String::from).collect();
        self.card = Some((skill.to_string(), lines));
    }

    /// Moves the selection, or scrolls the card, by `by` rows.
    fn shift(&mut self, by: isize) {
        let len = match &self.card {
            Some((_, lines)) if self.screen == Screen::Card => lines.len(),
            _ => self.listed().len(),
        };
        let pos = if self.screen == Screen::Card { &mut self.scroll } else { &mut self.selected };
        *pos = pos.saturating_add_signed(by).min(len.saturating_sub(1));
    }

    /// Applies `key`; false when it quits.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return self.quit();
        }
        if let Some(Dialog { lines, ask }) = self.dialog.take() {
            let y = key.code == KeyCode::Char('y') && key.modifiers.is_empty();
            match ask {
                Some(Ask::Source(mut source)) => match key.code {
                    KeyCode::Enter => self.add(source),
                    KeyCode::Esc => {}
                    code => {
                        match code {
                            KeyCode::Char(c) if (key.modifiers - KeyModifiers::SHIFT).is_empty() => source.push(c),
                            KeyCode::Backspace => _ = source.pop(),
                            _ => {}
                        }
                        self.dialog = Some(source_dialog(source));
                    }
                },
                Some(Ask::Pick(mut picker, reply)) => match picker.key(key) {
                    Step::Open => self.dialog = Some(Dialog { lines, ask: Some(Ask::Pick(picker, reply)) }),
                    Step::Done(names) => _ = reply.send(names),
                },
                Some(Ask::Answer(reply, default)) => match key.code {
                    // A Ctrl or Alt chord answers nothing.
                    _ if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                        self.dialog = Some(Dialog { lines, ask: Some(Ask::Answer(reply, default)) })
                    }
                    KeyCode::Enter => _ = reply.send(default),
                    KeyCode::Char('y' | 'Y') => _ = reply.send(true),
                    KeyCode::Char('n' | 'N') | KeyCode::Esc => _ = reply.send(false),
                    // The unanswered question is No.
                    KeyCode::Char('q') => return self.quit(),
                    _ => self.dialog = Some(Dialog { lines, ask: Some(Ask::Answer(reply, default)) }),
                },
                Some(Ask::Act(act)) if y => {
                    let lines = self.act(act);
                    self.dialog = Some(Dialog { lines, ask: None });
                }
                Some(Ask::Audit(skill)) if y => self.run_audit(skill),
                Some(Ask::Update(skill)) if y => self.run_update(skill),
                Some(Ask::Quit) if y => {
                    self.stop();
                    return false;
                }
                _ => {}
            }
            return true;
        }
        if self.typing {
            match key.code {
                KeyCode::Char(c) if (key.modifiers - KeyModifiers::SHIFT).is_empty() => self.filter.push(c),
                KeyCode::Backspace => _ = self.filter.pop(),
                KeyCode::Enter => self.typing = false,
                KeyCode::Esc => (self.typing, self.filter) = (false, String::new()),
                _ => {}
            }
            self.selected = 0;
            return true;
        }
        let list = self.screen != Screen::Card;
        match key.code {
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Up | KeyCode::Char('k') => self.shift(-1),
            KeyCode::Down | KeyCode::Char('j') => self.shift(1),
            KeyCode::Left | KeyCode::Char('h') => self.shift(-(self.page as isize)),
            KeyCode::Right | KeyCode::Char('l') => self.shift(self.page as isize),
            // An unused store skill is past its install-date window: retire first.
            KeyCode::Enter if self.screen == Screen::Unused && self.selected_unused_store().is_some() => {
                self.retire(self.selected_unused_store().unwrap());
            }
            KeyCode::Enter if list => {
                if let Some(skill) = self.listed().get(self.selected).map(|s| s.to_string()) {
                    (self.back, self.screen, self.card_since, self.scroll) =
                        (self.screen, Screen::Card, self.usage_since, 0);
                    self.open(&skill);
                }
            }
            KeyCode::Esc if !list => self.screen = self.back,
            KeyCode::Esc if !self.busy() => self.job = None,
            KeyCode::Tab if list => {
                self.screen = if self.screen == Screen::Usage { Screen::Unused } else { Screen::Usage };
                self.selected = 0;
            }
            KeyCode::Char('/') if list => (self.typing, self.filter, self.selected) = (true, String::new(), 0),
            KeyCode::Char('w') => match self.screen {
                Screen::Usage => {
                    self.usage_since = next_window(self.usage_since);
                    self.refresh();
                }
                Screen::Unused => {
                    self.unused_since = next_window(self.unused_since);
                    self.unused = report::unused_list(&self.data.input(), self.unused_since);
                    self.clamp();
                }
                Screen::Card => {
                    self.card_since = next_window(self.card_since);
                    self.refresh();
                }
            },
            KeyCode::Char(c @ ('a' | 'c' | 'A')) if self.screen != Screen::Unused => {
                match c {
                    'a' => self.agent = cycle(Agent::value_variants(), self.agent),
                    'c' => self.channel = cycle(Channel::value_variants(), self.channel),
                    _ => self.authoring = !self.authoring,
                }
                self.refresh();
            }
            // Its plan is of the inventory a running job may change.
            KeyCode::Char('s') if self.running().is_some() => self.dialog = self.one_job(),
            KeyCode::Char('r') => {
                if let Some(skill) = self.chosen() {
                    self.retire(skill);
                }
            }
            KeyCode::Char('s') => {
                let homes: Vec<&Home> = self.data.cfg.homes.iter().collect();
                self.dialog = Some(match links::checked(&self.data.cfg, &self.data.inv, &homes) {
                    Err(errors) => Dialog { lines: [vec!["sync refused:".into()], errors].concat(), ask: None },
                    Ok(plan) if plan.actions.is_empty() => {
                        Dialog { lines: vec!["in sync: nothing to apply".into()], ask: None }
                    }
                    Ok(plan) => Dialog { lines: links::planned(&plan), ask: Some(Ask::Act(Act::Sync(plan))) },
                });
            }
            KeyCode::Char('u') => {
                if let Some(skill) = self.chosen() {
                    self.dialog = Some(self.update(Some(skill)));
                }
            }
            KeyCode::Char('U') => self.dialog = Some(self.update(None)),
            KeyCode::Char('i') => self.dialog = Some(self.one_job().unwrap_or_else(|| source_dialog(String::new()))),
            KeyCode::Char('x') => {
                if let Some(skill) = self.chosen() {
                    self.dialog = Some(self.audit(skill));
                }
            }
            _ => {}
        }
        true
    }

    /// The card's skill, or the selected one.
    fn chosen(&self) -> Option<String> {
        match &self.card {
            Some((skill, _)) if self.screen == Screen::Card => Some(skill.clone()),
            _ => self.listed().get(self.selected).map(|s| s.to_string()),
        }
    }

    /// False, quitting, unless a job is running: then it asks first.
    fn quit(&mut self) -> bool {
        let Some(job) = self.running() else { return false };
        let lines = vec![format!("the {} of {} is running; stop its judge and quit?", job.verb, job.target)];
        self.dialog = Some(Dialog { lines, ask: Some(Ask::Quit) });
        true
    }

    /// The dialog that asks to audit `skill` under the cap `mazo audit` would
    /// use, or shows why it would refuse.
    fn audit(&self, skill: String) -> Dialog {
        if let Some(busy) = self.one_job() {
            return busy;
        }
        let d = &self.data;
        match audit::plan(&d.cfg, &d.inv, &skill, None) {
            Err(e) => Dialog { lines: vec![format!("ERROR: {e}")], ask: None },
            Ok(p) => {
                let judge = name(p.judge.agent);
                let cap = match p.budget {
                    Some(b) => format!("at most {} sessions and {}", audit::LIMIT, audit::usd(p.judge, b)),
                    None => format!("at most {} sessions; the {judge} judge reports no cost", audit::LIMIT),
                };
                let lines =
                    vec![format!("audit {skill} with the {judge} judge in the background?"), format!("cap: {cap}")];
                Dialog { lines, ask: Some(Ask::Audit(skill)) }
            }
        }
    }

    /// The dialog that refuses a second job while one runs.
    fn one_job(&self) -> Option<Dialog> {
        let job = self.running()?;
        let lines = vec![format!("the {} of {} is running; one job at a time", job.verb, job.target)];
        Some(Dialog { lines, ask: None })
    }

    /// The dialog that asks to update `skill`, or every store skill, as
    /// `mazo update` does, or shows why `skill` is not updatable. Each
    /// re-vet's verdict is asked in a dialog; with no `--force`, a security
    /// finding still refuses.
    fn update(&self, skill: Option<String>) -> Dialog {
        if let Some(busy) = self.one_job() {
            return busy;
        }
        let d = &self.data;
        let why = skill.as_ref().and_then(|s| match d.inv.installed.get(s) {
            None => Some("not a store skill"),
            Some(l) => (!l.updatable()).then_some("installed from a local path"),
        });
        if let (Some(s), Some(why)) = (&skill, why) {
            return Dialog { lines: vec![format!("{s}: not updatable; {why}")], ask: None };
        }
        let what = skill.as_deref().unwrap_or("every store skill");
        let lines = match &d.cfg.judge {
            None => vec![
                format!("update {what} in the background?"),
                "an untrusted skill is skipped: re-vetting it needs a [judge] in the config".into(),
            ],
            Some(judge) => {
                let judge_name = name(judge.agent);
                let cap = match audit::backend(judge, &d.cfg.homes) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(b) if b.budget_flag().is_some() => {
                        format!("cap: {} per call", audit::usd(judge, audit::DEFAULT_BUDGET))
                    }
                    Ok(_) => format!("the {judge_name} judge reports no cost"),
                };
                vec![
                    format!("update {what} and ask about each judge verdict?"),
                    format!("an untrusted skill that changed is re-vetted with one {judge_name} judge call; {cap}"),
                    "a security finding still refuses its update".into(),
                ]
            }
        };
        Dialog { lines, ask: Some(Ask::Update(skill)) }
    }

    /// Adds from `source` in the background as `mazo add <source>` does, its
    /// picker and y/N questions answered in dialogs, or shows the refusal
    /// `add` gives before reading the source. There is no `--force`.
    fn add(&mut self, source: String) {
        if let Err(e) = store::precheck(&self.data.cfg, &source) {
            self.dialog = Some(Dialog { lines: vec![format!("ERROR: {e}")], ask: None });
            return;
        }
        let (tx, rx) = mpsc::channel();
        let target = source.clone();
        self.start("add", target, Some(rx), move |data, pane| {
            let args = store::Add {
                source: &source,
                rename: None,
                pick: None,
                all: false,
                trust: false,
                yes: false,
                force: false,
                project: false,
                pane: Some(pane),
                asks: Some(&tx),
            };
            match store::add(&data.cfg, &data.inv, &data.dir, &args) {
                Ok(code) => (String::new(), code == ExitCode::SUCCESS),
                Err(e) => (format!("ERROR: {e}"), false),
            }
        });
    }

    /// Runs `job` on a thread over a copy of the data, as the `verb` of
    /// `target`: the lines it reports, then the text it returns, go to the
    /// job pane, and what it `asks` to the dialogs.
    fn start(
        &mut self,
        verb: &'static str,
        target: String,
        asks: Option<mpsc::Receiver<Question>>,
        job: impl FnOnce(&Data, &audit::Pane) -> (String, bool) + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let pane = audit::Pane { lines: tx, cancel: cancel.clone() };
        let data = self.data.clone();
        let thread = std::thread::spawn(move || {
            let (out, ok) = caught(verb, || job(&data, &pane));
            for line in out.lines() {
                _ = pane.lines.send(line.into());
            }
            ok
        });
        self.job = Some(Job { verb, target, lines: Vec::new(), rx, asks, cancel, thread: Some(thread), ok: false });
    }

    /// Audits `skill` in the background as `mazo audit <skill>` does.
    fn run_audit(&mut self, skill: String) {
        let name = skill.clone();
        self.start("audit", skill, None, move |data, pane| {
            let args = audit::Args { limit: Some(audit::LIMIT), resynth: false, dry_run: false, json: false };
            let ran = audit::plan(&data.cfg, &data.inv, &name, None)
                .and_then(|p| audit::command(&p, &args, &data.input(), &data.dir, false, Some(pane)));
            ran.unwrap_or_else(|e| (format!("ERROR: {e}"), false))
        });
    }

    /// Updates `skill`, or every store skill, in the background as `mazo
    /// update [skill]` does, each re-vet's y/N answered in a dialog.
    fn run_update(&mut self, skill: Option<String>) {
        let target = skill.clone().unwrap_or_else(|| "every store skill".into());
        let (tx, rx) = mpsc::channel();
        self.start("update", target, Some(rx), move |data, pane| {
            let names = Vec::from_iter(skill);
            let asks = Some(&tx);
            let args =
                store::Update { names: &names, yes: false, force: false, pane: Some(pane), project: false, asks };
            match store::update(&data.cfg, &data.inv, &data.dir, &args) {
                Ok(code) => (String::new(), code == ExitCode::SUCCESS),
                Err(e) => (format!("ERROR: {e}"), false),
            }
        });
    }

    /// Inserts pasted `text`, its line breaks dropped, into an open source
    /// dialog; anywhere else a paste does nothing.
    pub fn paste(&mut self, text: &str) {
        if let Some(Dialog { ask: Some(Ask::Source(source)), .. }) = &mut self.dialog {
            source.extend(text.chars().filter(|c| !matches!(c, '\r' | '\n')));
        }
    }

    /// Takes the lines the job reported, and opens its next question when no
    /// dialog is open; once its thread ended, re-reads the data. Called
    /// before each draw.
    pub fn tick(&mut self) {
        let Some(job) = &mut self.job else { return };
        if self.dialog.is_none()
            && let Some(question) = job.asks.as_ref().and_then(|a| a.try_recv().ok())
        {
            self.dialog = Some(match question {
                Question::Pick(items, reply) => {
                    Dialog { lines: Vec::new(), ask: Some(Ask::Pick(Picker::new(items), reply)) }
                }
                Question::YesNo { mut lines, prompt, default, reply } => {
                    lines.push(prompt);
                    Dialog { lines, ask: Some(Ask::Answer(reply, default)) }
                }
            });
        }
        loop {
            match job.rx.try_recv() {
                Ok(line) => job.lines.push(line),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        let Some(thread) = job.thread.take() else { return };
        job.ok = thread.join().unwrap_or(false);
        self.clear = true;
        if let Err(e) = self.reload() {
            self.job.as_mut().expect("the job just ended").lines.push(format!("ERROR: {e}"));
        }
    }

    /// Cancels a running job, which kills its judge call in flight, and waits
    /// up to [`STOP_WAIT`] for its thread.
    fn stop(&mut self) {
        let Some(job) = &mut self.job else { return };
        let Some(thread) = job.thread.take() else { return };
        job.cancel.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + STOP_WAIT;
        while !thread.is_finished() && Instant::now() < deadline {
            // A question the job waits on is dropped unanswered: No.
            while job.asks.as_ref().is_some_and(|a| a.try_recv().is_ok()) {}
            std::thread::sleep(Duration::from_millis(10));
        }
        if thread.is_finished() {
            _ = thread.join();
        }
    }

    /// Opens the y/N dialog that retires `skill`, unless a job is running:
    /// the plan is of the inventory the job may change.
    fn retire(&mut self, skill: String) {
        if let Some(busy) = self.one_job() {
            self.dialog = Some(busy);
            return;
        }
        let what = match self.data.cfg.source.as_deref() {
            Some(source) if self.data.inv.root(&skill) == Some(source) => {
                format!("move it to {}", source.join(".archive").join(&skill).display())
            }
            _ => "remove it from the store".into(),
        };
        let lines = vec![format!("retire {skill}: {what} and prune its links?")];
        self.dialog = Some(Dialog { lines, ask: Some(Ask::Act(Act::Retire(skill))) });
    }

    /// The selected unused-list skill when it is `unused` and read from the store.
    fn selected_unused_store(&self) -> Option<String> {
        let row = self.unused.skills.iter().filter(|r| self.matches(&r.skill)).nth(self.selected)?;
        let store = self.data.inv.root(&row.skill) != self.data.inv.source.as_deref();
        (row.status == "unused" && store).then(|| row.skill.clone())
    }

    /// Runs `act` through the CLI command's code path, never forced, then
    /// re-reads the data. The lines it reports.
    fn act(&mut self, act: Act) -> Vec<String> {
        self.clear = true;
        let d = &self.data;
        let mut out = Vec::new();
        let ran = match act {
            Act::Retire(skill) => {
                lifecycle::retire(&d.cfg, &d.inv, &d.dir, &skill, false, false, &mut out).map(|retired| match retired {
                    Retired::Blocked(lines) | Retired::Refused(lines) => {
                        [vec![format!("retire {skill} refused:")], lines].concat()
                    }
                    Retired::Done(lines) => lines,
                })
            }
            Act::Sync(plan) => synced(d, &plan),
        };
        out.extend(ran.unwrap_or_else(|e| vec![format!("ERROR: {e}")]));
        if let Err(e) = self.reload() {
            out.push(format!("ERROR: {e}"));
        }
        out
    }

    /// Re-reads the data and recounts every view; a card whose skill is
    /// gone closes.
    fn reload(&mut self) -> crate::Result<()> {
        self.data.reload()?;
        if self.card.as_ref().is_some_and(|(skill, _)| !self.data.inv.skills.contains(skill)) {
            self.card = None;
            if self.screen == Screen::Card {
                self.screen = self.back;
            }
        }
        self.unused = report::unused_list(&self.data.input(), self.unused_since);
        self.refresh();
        self.clamp();
        Ok(())
    }

    /// Keeps the selection on a row of the current list.
    fn clamp(&mut self) {
        self.selected = self.selected.min(self.listed().len().saturating_sub(1));
    }

    /// Recounts the usage view and the open card under the current filter.
    fn refresh(&mut self) {
        self.usage = report::usage_table(&self.data.input(), &self.args(self.usage_since));
        if self.list() == Screen::Usage {
            self.clamp();
        }
        if let Some((skill, _)) = self.card.take() {
            self.open(&skill);
            self.shift(0);
        }
    }

    /// The footer's first line: the view, its window and the active filters.
    fn status(&self) -> String {
        match &self.dialog {
            Some(Dialog { ask: Some(Ask::Source(_)), .. }) => return "Enter add · Esc cancels".into(),
            Some(Dialog { ask: Some(Ask::Pick(..)), .. }) => return "add: pick the skills to install".into(),
            Some(Dialog { ask: Some(Ask::Answer(_, default)), .. }) => {
                let default = if *default { "yes" } else { "no" };
                return format!("Enter {default} · y yes · n/Esc no · q quit");
            }
            Some(Dialog { ask: Some(_), .. }) => return "y apply · any other key cancels".into(),
            Some(_) => return "any key closes".into(),
            None => {}
        }
        let counts = |since: Since| {
            let agent = self.agent.map_or("all".into(), name);
            let channel = self.channel.map_or("all".into(), name);
            let authoring = if self.authoring { "on" } else { "off" };
            format!("window {} · agent {agent} · channel {channel} · authoring {authoring}", window(since))
        };
        let mut line = match (self.screen, &self.card) {
            (Screen::Card, Some((skill, _))) => format!("card {skill} · {}", counts(self.card_since)),
            (Screen::Unused, _) => format!("unused · window {}", window(self.unused_since)),
            _ => format!("usage · {}", counts(self.usage_since)),
        };
        if self.screen != Screen::Card {
            line.push_str(&format!(" · {} skills", self.listed().len()));
            if self.typing || !self.filter.is_empty() {
                line.push_str(&format!(" · filter /{}{}", self.filter, if self.typing { "_" } else { "" }));
            }
        }
        line
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let mut footer_lines = vec![Line::raw(self.status())];
        footer_lines.extend(keys(frame.area().width.into()));
        let height = Constraint::Length(footer_lines.len() as u16);
        let pane_height = self.job.as_ref().map_or(0, |j| j.lines.len().clamp(1, PANE_LINES) as u16 + 2);
        let [body, pane, info, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(pane_height), Constraint::Length(1), height])
                .areas(frame.area());
        frame.render_widget(Paragraph::new(footer_lines), footer);
        if let Some(job) = &self.job {
            let state = match (&job.thread, job.ok) {
                (Some(_), _) => "running",
                (None, true) => "done · Esc closes",
                (None, false) => "failed · Esc closes",
            };
            let lines: Vec<Line> = job
                .lines
                .iter()
                .skip(job.lines.len().saturating_sub(PANE_LINES))
                .map(|l| Line::raw(l.as_str()))
                .collect();
            let block = Block::bordered().title(format!(" {} {} · {state} ", job.verb, job.target));
            frame.render_widget(Paragraph::new(lines).block(block), pane);
        }
        if let Some(dialog) = &self.dialog {
            let area = body.union(info);
            if let Some(Ask::Pick(picker, _)) = &dialog.ask {
                let block = Block::bordered();
                frame.render_widget(Clear, area);
                picker.draw(frame, block.inner(area));
                frame.render_widget(block, area);
                return;
            }
            let room = usize::from(area.height.saturating_sub(2)).max(1);
            let mut lines: Vec<Line> = dialog.lines.iter().flat_map(|l| l.lines()).map(Line::raw).collect();
            if let Some(Ask::Source(source)) = &dialog.ask {
                lines.push(Line::raw(typed_line(source, area.width.saturating_sub(2).into())));
            }
            if lines.len() > room {
                let more = lines.len() - room + 1;
                lines.truncate(room - 1);
                lines.push(Line::raw(format!("… and {more} more")));
            }
            frame.render_widget(Clear, area);
            frame.render_widget(Paragraph::new(lines).block(Block::bordered()), area);
            return;
        }
        if let (Screen::Card, Some((_, lines))) = (self.screen, &self.card) {
            let area = body.union(info);
            let text: Vec<Line> = lines.iter().skip(self.scroll).map(|l| Line::raw(l.as_str())).collect();
            frame.render_widget(Paragraph::new(text), area);
            self.page = usize::from(area.height).max(1);
            return;
        }
        let input = self.data.input();
        let (mut lines, note) = match self.screen {
            Screen::Unused => {
                let rows: Vec<Vec<String>> = self
                    .unused
                    .skills
                    .iter()
                    .filter(|r| self.matches(&r.skill))
                    .map(|r| report::unused_cells(r, false))
                    .collect();
                (
                    table(&report::UNUSED_HEADER, &rows, body.width.into()),
                    report::coverage_note("window", &self.unused.window),
                )
            }
            _ => {
                let rows: Vec<Vec<String>> = self
                    .usage
                    .rows
                    .iter()
                    .filter(|r| self.matches(&r.skill))
                    .map(|r| report::usage_cells(&input, r, false))
                    .collect();
                (
                    table(&report::USAGE_HEADER, &rows, body.width.into()),
                    report::coverage_note("coverage", &self.usage.coverage),
                )
            }
        };
        frame.render_widget(Paragraph::new(note), info);
        // The header, then the rows from the one that keeps the selection in view.
        self.page = usize::from(body.height).saturating_sub(1).max(1);
        let rows = lines.split_off(1.min(lines.len()));
        let first = self.selected.saturating_sub(self.page - 1);
        let reversed = Style::new().add_modifier(Modifier::REVERSED);
        lines.extend(
            rows.into_iter()
                .enumerate()
                .skip(first)
                .map(|(i, l)| if i == self.selected { l.patch_style(reversed) } else { l }),
        );
        frame.render_widget(Paragraph::new(lines), body);
    }
}

/// The dialog that takes an add's source.
fn source_dialog(source: String) -> Dialog {
    let lines = vec!["add a skill from a local path, a git URL or owner/repo[/path]:".into()];
    Dialog { lines, ask: Some(Ask::Source(source)) }
}

/// The source as typed, `> source_`, cut to its last `width` columns with
/// `…` marking the cut, so the cursor stays in view.
fn typed_line(source: &str, width: usize) -> String {
    let line = format!("> {source}_");
    let n = line.chars().count();
    if n <= width {
        return line;
    }
    format!("…{}", line.chars().skip(n + 1 - width.max(1)).collect::<String>())
}

/// What `job`, the `verb` run, returns, or a failure naming its panic: the
/// pane shows it and the screen stays up.
fn caught(verb: &str, job: impl FnOnce() -> (String, bool)) -> (String, bool) {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).unwrap_or_else(|panic| {
        let why = panic.downcast_ref::<&str>().copied().or_else(|| panic.downcast_ref::<String>().map(String::as_str));
        (format!("ERROR: the {verb} panicked: {}", why.unwrap_or("no message")), false)
    })
}

/// Opens the TUI on `input` and the data directory `data`, with bracketed
/// paste on, and runs it until `q` or Ctrl-C, asking first while a job
/// runs. `ratatui::init` installs a panic hook that restores the terminal,
/// run here only on this thread, after paste is turned off again: a job's
/// panic is [`caught`] and shown in its pane while the screen stays up. The
/// terminal shows the cursor again when it drops, after the restore leaves
/// the alternate screen.
pub fn run(input: &Input, data: &Path) -> std::io::Result<()> {
    let mut app = App::new(input, data);
    let mut terminal = ratatui::init();
    // A paste arrives as one event, so a newline in it cannot submit a dialog.
    _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let (restore, ui) = (std::panic::take_hook(), std::thread::current().id());
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == ui {
            _ = execute!(std::io::stdout(), DisableBracketedPaste);
            restore(info);
        }
    }));
    let result = (|| {
        loop {
            app.tick();
            // A scan's progress line may have been written under the screen.
            if std::mem::take(&mut app.clear) {
                terminal.clear()?;
            }
            terminal.draw(|frame| app.draw(frame))?;
            // While a job runs, its lines are drawn as they come.
            if app.busy() && !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if !app.key(key) => return Ok(()),
                Event::Paste(text) => app.paste(&text),
                _ => {}
            }
        }
    })();
    _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::caught;

    #[test]
    fn a_job_that_panics_fails_with_its_message() {
        assert_eq!(caught("audit", || ("done".into(), true)), ("done".into(), true));
        assert_eq!(caught("audit", || panic!("boom")), ("ERROR: the audit panicked: boom".into(), false));
        let n = 2;
        assert_eq!(caught("update", || panic!("boom {n}")), ("ERROR: the update panicked: boom 2".into(), false));
    }
}
