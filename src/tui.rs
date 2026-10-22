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
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, HighlightSpacing, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, TableState, Tabs,
};

use crate::audit::{self, SkillRecord};
use crate::config::{Config, Home};
use crate::help;
use crate::inventory::Inventory;
use crate::ledger::{self, Agent, Channel, Coverage, Ledger, name};
use crate::lifecycle::{self, Retired};
use crate::links::{self, Plan};
use crate::pick::{Picker, Step};
use crate::render::{self, Align, Colour};
use crate::report::{self, Input, SHOW_LIMIT, Since, Unused, Usage, UsageArgs};
use crate::store::{self, Question};
use crate::theme;

/// The windows `w` cycles through.
const WINDOWS: [Since; 3] = [Since::Days(30), Since::Days(60), Since::All];

/// The most lines the job pane shows: its last ones.
const PANE_LINES: usize = 6;

/// How long quitting waits for a cancelled job: longer than a judge has to
/// exit on SIGTERM before it is killed.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// The frames of a running job's spinner, one per 100 ms.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The screens a key works on.
#[derive(Clone, Copy)]
enum On {
    All,
    Lists,
    NotUnused,
}

/// The keys, as the footer lists them.
const KEYS: [(&str, On); 16] = [
    ("↑↓←→/hjkl move", On::All),
    ("Enter card/retire", On::Lists),
    ("Esc back", On::All),
    ("Tab usage/unused", On::Lists),
    ("/ filter", On::Lists),
    ("w window", On::All),
    ("a agent", On::NotUnused),
    ("c channel", On::NotUnused),
    ("A authoring", On::NotUnused),
    ("r retire", On::All),
    ("s sync", On::All),
    ("i add", On::All),
    ("u/U update one/all", On::All),
    ("x audit", On::All),
    ("H help", On::All),
    ("q quit", On::All),
];

/// The [`KEYS`] that work on `screen`, joined with ` · ` into lines of at
/// most `width` columns.
fn keys(screen: Screen, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<String> = vec![];
    for (key, on) in KEYS {
        let works = match on {
            On::All => true,
            On::Lists => screen != Screen::Card,
            On::NotUnused => screen != Screen::Unused,
        };
        match lines.last_mut() {
            _ if !works => {}
            Some(l) if l.chars().count() + 3 + key.chars().count() <= width => *l = format!("{l} · {key}"),
            _ => lines.push(key.to_string()),
        }
    }
    lines.iter().map(|l| theme::hints(l)).collect()
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
    /// The help overlay, its lines scrolled to this first one: `j`/`k` and
    /// `PgUp`/`PgDn` scroll it, and `Esc` or `H` closes it.
    Help(usize),
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
    /// When it started: its spinner's frame and its running time.
    started: Instant,
}

/// Lines over the screen that take the next key: `y` does `ask`, any other
/// key closes it; a source, a picker, an answer or the help takes keys of
/// its own.
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
    /// Rows a page move skips: the list rows, card lines or help lines the last draw showed.
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
        Some(Colour::Red) => span.fg(theme::BAD),
        Some(Colour::Yellow) => span.fg(theme::WARN),
        None => span,
    }
}

/// The spans of `cell` in the column `head`: a sparkline in the accent with
/// its empty days muted, a grade of 9 or more green and its mark apart,
/// `today` in the accent, and zero counts, `-` and `not judged` muted. In
/// any other column each `; `-separated note is styled as a status token.
fn styled(head: &str, cell: &str) -> Vec<Span<'static>> {
    let muted = |s: &str| Span::raw(s.to_string()).fg(theme::MUTED);
    match (head, cell) {
        (_, "-" | "0" | "not judged") => vec![muted(cell)],
        (_, "today") => vec![Span::raw("today").fg(theme::ACCENT)],
        ("30d", _) => cell
            .chars()
            .map(|c| if c == '·' { muted("·") } else { Span::raw(c.to_string()).fg(theme::ACCENT) })
            .collect(),
        ("grade", _) => {
            let (score, mark) = cell.split_at(cell.find('+').unwrap_or(cell.len()));
            let high = score.parse::<f64>().is_ok_and(|s| s >= 9.0);
            let score = if high { Span::raw(score.to_string()).fg(theme::GOOD) } else { Span::raw(score.to_string()) };
            [score].into_iter().chain((!mark.is_empty()).then(|| token(mark))).collect()
        }
        ("audit", _) => match cell.split_once(": ") {
            Some((judge, verdict)) => vec![muted(&format!("{judge}: ")), Span::raw(verdict.to_string())],
            None => vec![Span::raw(cell.to_string())],
        },
        _ => {
            let mut out = Vec::new();
            for (i, piece) in cell.split("; ").enumerate() {
                if i > 0 {
                    out.push(Span::raw("; "));
                }
                out.push(if piece == "weak-only" { muted(piece) } else { token(piece) });
            }
            out
        }
    }
}

/// A table column: its header, alignment and cells.
struct Column {
    head: &'static str,
    align: Align,
    cells: Vec<String>,
}

impl Column {
    /// The width of its widest cell or its header.
    fn width(&self) -> usize {
        self.cells.iter().map(|c| c.chars().count()).chain([self.head.chars().count()]).max().unwrap_or(0)
    }
}

/// How a column gives way as the terminal narrows.
enum Narrow {
    Drop(&'static str),
    /// The two columns fold into one under the header, each cell `a/b`.
    Fold(&'static str, &'static str, &'static str),
}

/// The columns that give way as the terminal narrows, the first first. A
/// column not named here never drops.
const NARROWING: [Narrow; 6] = [
    Narrow::Drop("30d"),
    Narrow::Drop("typed"),
    Narrow::Drop("ref"),
    Narrow::Drop("read"),
    Narrow::Fold("slash", "tool", "via"),
    Narrow::Drop("agents"),
];

/// `cols` narrowed by [`NARROWING`] until they fit in `room` columns, one
/// space between them, and the width of each: the first and the last take
/// what is left over. When they still overflow, the first column's cells are
/// cut with `…` and no other column is.
fn fit(mut cols: Vec<Column>, room: usize) -> (Vec<Column>, Vec<u16>) {
    let used = |cols: &[Column]| cols.iter().map(Column::width).sum::<usize>() + cols.len().saturating_sub(1);
    for step in NARROWING {
        if used(&cols) <= room {
            break;
        }
        let at = |cols: &[Column], head| cols.iter().position(|c| c.head == head);
        match step {
            Narrow::Drop(head) => _ = at(&cols, head).map(|i| cols.remove(i)),
            Narrow::Fold(a, b, head) => {
                if let (Some(i), Some(j)) = (at(&cols, a), at(&cols, b)) {
                    let b = cols.remove(j);
                    let a = &mut cols[if i < j { i } else { i - 1 }];
                    a.cells = a.cells.iter().zip(&b.cells).map(|(x, y)| format!("{x}/{y}")).collect();
                    a.head = head;
                }
            }
        }
    }
    let used = used(&cols);
    let mut widths: Vec<usize> = cols.iter().map(Column::width).collect();
    if let Some(spare) = room.checked_sub(used) {
        let last = widths.len() - 1;
        widths[0] += spare / 2;
        widths[last] += spare - spare / 2;
    } else {
        widths[0] = (widths[0] + room).saturating_sub(used).max(1);
        for cell in &mut cols[0].cells {
            *cell = report::cut(cell, widths[0]);
        }
    }
    (cols, widths.into_iter().map(|w| w as u16).collect())
}

/// `header` and `rows` as a table that fits `width` columns: the header bold
/// in the accent, a cell aligned as its column. The skill column is never
/// styled, so a name never takes a status colour.
fn table(header: &[(&'static str, Align)], rows: Vec<Vec<String>>, width: usize) -> Table<'static> {
    let mut cols: Vec<Column> = header.iter().map(|&(head, align)| Column { head, align, cells: Vec::new() }).collect();
    for row in rows {
        for (col, cell) in cols.iter_mut().zip(row) {
            col.cells.push(cell);
        }
    }
    let (cols, widths) = fit(cols, width);
    let aligned = |line: Line<'static>, align: Align| match align {
        Align::Left => line,
        Align::Right => line.right_aligned(),
    };
    let head = Row::new(cols.iter().map(|c| aligned(Line::raw(c.head), c.align))).fg(theme::ACCENT).bold();
    let rows = (0..cols.first().map_or(0, |c| c.cells.len())).map(|r| {
        Row::new(cols.iter().enumerate().map(|(i, c)| {
            let cell = &c.cells[r];
            let spans = if i > 0 { styled(c.head, cell) } else { vec![Span::raw(cell.clone())] };
            aligned(Line::from(spans), c.align)
        }))
    });
    Table::new(rows, widths.into_iter().map(Constraint::Length))
        .header(head)
        .row_highlight_style(theme::selected())
        .highlight_symbol(theme::gutter(true))
        .highlight_spacing(HighlightSpacing::Always)
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
        let lines = report::card_text(&input, &card, true).lines().map(String::from).collect();
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
                Some(Ask::Help(scroll)) => {
                    let scroll = match key.code {
                        KeyCode::Esc | KeyCode::Char('H') => return true,
                        KeyCode::Down | KeyCode::Char('j') => scroll + 1,
                        KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
                        KeyCode::PageDown => scroll + self.page,
                        KeyCode::PageUp => scroll.saturating_sub(self.page),
                        _ => scroll,
                    };
                    self.dialog = Some(Dialog { lines, ask: Some(Ask::Help(scroll)) });
                }
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
            KeyCode::Char('H') => {
                let lines = help::lines(self.list() == Screen::Unused);
                self.dialog = Some(Dialog { lines, ask: Some(Ask::Help(0)) });
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
        let started = Instant::now();
        self.job =
            Some(Job { verb, target, lines: Vec::new(), rx, asks, cancel, thread: Some(thread), ok: false, started });
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

    /// The footer's hints while a dialog is open or the filter is being
    /// typed; `None` when the footer lists the [`KEYS`].
    fn hints(&self) -> Option<String> {
        Some(match &self.dialog {
            Some(Dialog { ask: Some(Ask::Source(_)), .. }) => "Enter add · Esc cancels".into(),
            Some(Dialog { ask: Some(Ask::Pick(..)), .. }) => "add: pick the skills to install".into(),
            Some(Dialog { ask: Some(Ask::Answer(_, default)), .. }) => {
                let default = if *default { "yes" } else { "no" };
                format!("Enter {default} · y yes · n/Esc no · q quit")
            }
            Some(Dialog { ask: Some(Ask::Help(_)), .. }) => "j/k scroll · PgUp/PgDn page · Esc/H closes".into(),
            Some(Dialog { ask: Some(_), .. }) => "y apply · any other key cancels".into(),
            Some(_) => "any key closes".into(),
            None if self.typing => "Enter keep · Esc clear".into(),
            None => return None,
        })
    }

    /// The top bar in `area`: the name, the tabs or the card's trail, then
    /// the count of listed skills and the filter, the window, and the agent,
    /// channel and authoring filters when set.
    fn draw_top(&self, frame: &mut Frame, area: Rect) {
        let muted = |s: &str| Span::raw(s.to_string()).fg(theme::MUTED);
        // The first `width` columns of `area`, and the rest.
        let split = |area, width: usize| {
            Layout::horizontal([Constraint::Length(width as u16), Constraint::Fill(1)]).areas(area)
        };
        let logo = Line::from(vec![Span::raw(" ♠ mazo ").fg(Color::Black).bg(theme::ACCENT).bold(), Span::raw(" ")]);
        let [at, area] = split(area, logo.width());
        frame.render_widget(logo, at);
        let area = match (self.screen, &self.card) {
            (Screen::Card, Some((skill, _))) => {
                let back = if self.back == Screen::Unused { "unused" } else { "usage" };
                let trail =
                    Line::from(vec![muted(back), muted(" › "), Span::raw(skill.clone()).bold(), Span::raw("  ")]);
                let [at, area] = split(area, trail.width());
                frame.render_widget(trail, at);
                area
            }
            _ => {
                const TABS: [&str; 2] = ["usage", "unused"];
                let tabs = Tabs::new(TABS)
                    .select(usize::from(self.screen == Screen::Unused))
                    .style(theme::MUTED)
                    .highlight_style(Style::new().fg(theme::ACCENT).bold().underlined())
                    .divider("");
                // Each title is padded with a space either side.
                let [at, area] = split(area, TABS.iter().map(|t| t.len() + 2).sum());
                frame.render_widget(tabs, at);
                area
            }
        };
        let mut chips: Vec<(&str, String)> = Vec::new();
        if self.screen != Screen::Card {
            chips.push(("", format!("{} skills", self.listed().len())));
            if self.typing || !self.filter.is_empty() {
                chips.push(("filter ", format!("/{}{}", self.filter, if self.typing { "_" } else { "" })));
            }
        }
        let since = match self.screen {
            Screen::Usage => self.usage_since,
            Screen::Unused => self.unused_since,
            Screen::Card => self.card_since,
        };
        chips.push(("window ", window(since)));
        if self.screen != Screen::Unused {
            chips.extend(self.agent.map(|a| ("agent ", name(a))));
            chips.extend(self.channel.map(|c| ("channel ", name(c))));
            chips.extend(self.authoring.then(|| ("authoring ", "on".to_string())));
        }
        let mut spans = vec![Span::raw(" ")];
        for (i, (label, value)) in chips.into_iter().enumerate() {
            if i > 0 {
                spans.push(muted(" · "));
            }
            spans.extend([muted(label), Span::raw(value).fg(theme::ACCENT)]);
        }
        frame.render_widget(Line::from(spans), area);
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let footer_lines = match self.hints() {
            Some(hints) => vec![theme::hints(&hints)],
            None => keys(self.screen, frame.area().width.into()),
        };
        let height = Constraint::Length(footer_lines.len() as u16);
        let pane_height = self.job.as_ref().map_or(0, |j| j.lines.len().clamp(1, PANE_LINES) as u16 + 2);
        let [top, body, info, pane, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(pane_height),
            height,
        ])
        .areas(frame.area());
        self.draw_top(frame, top);
        frame.render_widget(Paragraph::new(footer_lines), footer);
        if let Some(job) = &self.job {
            draw_job(frame, job, pane);
        }
        match (self.screen, &self.card) {
            (Screen::Card, Some((skill, lines))) => {
                let area = body.union(info);
                let text: Vec<Line> = lines.iter().skip(self.scroll).map(|l| card_line(l, skill)).collect();
                frame.render_widget(Paragraph::new(text), area.inner(Margin::new(2, 0)));
                scrollbar(frame, area, lines.len(), self.scroll, area.height.into());
                self.page = usize::from(area.height).max(1);
            }
            _ => self.draw_list(frame, body, info),
        }
        if let Some(dialog) = &mut self.dialog {
            let area = body.union(info);
            // The screen behind stays in view, dimmed.
            frame.buffer_mut().set_style(area, Style::new().add_modifier(Modifier::DIM));
            match &mut dialog.ask {
                Some(Ask::Help(scroll)) => self.page = draw_help(frame, &dialog.lines, scroll, area),
                _ => draw_dialog(frame, dialog, area),
            }
        }
    }

    /// The current list as a table in `body` from the row that keeps the
    /// selection in view, and its coverage note in `info`.
    fn draw_list(&mut self, frame: &mut Frame, body: Rect, info: Rect) {
        let input = self.data.input();
        let (header, rows, note) = match self.screen {
            Screen::Unused => {
                let rows: Vec<Vec<String>> = self
                    .unused
                    .skills
                    .iter()
                    .filter(|r| self.matches(&r.skill))
                    .map(|r| report::unused_cells(r, false))
                    .collect();
                (&report::UNUSED_HEADER[..], rows, report::coverage_note("window", &self.unused.window))
            }
            _ => {
                let rows: Vec<Vec<String>> = self
                    .usage
                    .rows
                    .iter()
                    .filter(|r| self.matches(&r.skill))
                    .map(|r| report::usage_cells(&input, r, false))
                    .collect();
                (&report::USAGE_HEADER[..], rows, report::coverage_note("coverage", &self.usage.coverage))
            }
        };
        frame.render_widget(Paragraph::new(Line::raw(format!("  {note}")).fg(theme::MUTED).italic()), info);
        if rows.is_empty() {
            let why = match self.screen {
                _ if !self.filter.is_empty() => format!("no skill matches /{}", self.filter),
                Screen::Unused => "no unused skill in this window".into(),
                _ => "no skill used in this window · w widens it".into(),
            };
            let line = Line::raw(why).fg(theme::MUTED).italic().centered();
            frame.render_widget(Paragraph::new(line), body.centered_vertically(Constraint::Length(1)));
            return;
        }
        self.page = usize::from(body.height).saturating_sub(1).max(1);
        let total = rows.len();
        let first = self.selected.saturating_sub(self.page - 1);
        // The scrollbar, when shown, takes the last column.
        let area = Rect { width: body.width.saturating_sub(u16::from(total > self.page)), ..body };
        // Room for the gutter.
        let table = table(header, rows, usize::from(area.width).saturating_sub(2));
        let mut state = TableState::new().with_offset(first).with_selected(Some(self.selected));
        frame.render_stateful_widget(table, area, &mut state);
        let below = Rect { y: body.y + 1, height: body.height.saturating_sub(1), ..body };
        scrollbar(frame, below, total, first, self.page);
    }
}

impl Dialog {
    /// The title and colour of its border: red for what removes or stops,
    /// yellow for a verdict to answer, the accent for the rest.
    fn frame(&self) -> (&'static str, Color) {
        match &self.ask {
            Some(Ask::Act(Act::Retire(_))) => ("retire", theme::BAD),
            Some(Ask::Act(Act::Sync(_))) => ("sync", theme::ACCENT),
            Some(Ask::Audit(_)) => ("audit", theme::ACCENT),
            Some(Ask::Update(_)) => ("update", theme::ACCENT),
            Some(Ask::Quit) => ("quit", theme::BAD),
            Some(Ask::Source(_) | Ask::Pick(..)) => ("add", theme::ACCENT),
            Some(Ask::Answer(..)) => ("confirm", theme::WARN),
            Some(Ask::Help(_)) => ("help", theme::ACCENT),
            None if self.lines.iter().any(|l| l.contains("ERROR:") || l.contains(" refused")) => ("error", theme::BAD),
            None => ("mazo", theme::ACCENT),
        }
    }
}

/// `line` with a leading status token, such as `ERROR:`, in its colour.
fn status_line(line: &str) -> Line<'static> {
    match line.split_once(' ') {
        Some((head, rest)) if render::status_colour(head).is_some() => {
            Line::from(vec![token(head).bold(), Span::raw(format!(" {rest}"))])
        }
        _ => Line::raw(line.to_string()),
    }
}

/// `dialog` in a rounded box centred over `area`, sized to its lines; a
/// picker takes the whole area less a margin.
fn draw_dialog(frame: &mut Frame, dialog: &Dialog, area: Rect) {
    let (title, colour) = dialog.frame();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(colour))
        .title(Span::raw(format!(" {title} ")).fg(colour).bold())
        .padding(Padding::new(2, 2, 1, 1));
    if let Some(Ask::Pick(picker, _)) = &dialog.ask {
        let modal = area.inner(Margin::new(2, 1));
        frame.render_widget(Clear, modal);
        picker.draw(frame, block.inner(modal));
        frame.render_widget(block, modal);
        return;
    }
    // Borders and padding take 6 columns and 4 rows.
    let raw: Vec<&str> = dialog.lines.iter().flat_map(|l| l.lines()).collect();
    let mut wide = raw.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if let Some(Ask::Source(source)) = &dialog.ask {
        wide = wide.max(source.chars().count() + 3);
    }
    let width = (wide as u16 + 6).clamp(area.width.min(48), area.width);
    let mut lines: Vec<Line> = raw.iter().map(|l| status_line(l)).collect();
    // A question's first line is its headline.
    if dialog.ask.is_some()
        && let Some(first) = lines.first_mut()
    {
        *first = first.clone().bold();
    }
    if let Some(Ask::Source(source)) = &dialog.ask {
        let typed = typed_line(source, width.saturating_sub(6).into());
        let typed = typed.strip_suffix('_').unwrap_or(&typed);
        let (prompt, typed) = typed.strip_prefix("> ").map_or(("", typed), |t| ("> ", t));
        lines.push(Line::from(vec![
            Span::raw(prompt).fg(theme::ACCENT).bold(),
            Span::raw(typed.to_string()),
            Span::raw("_").fg(theme::ACCENT).slow_blink(),
        ]));
    }
    let room = usize::from(area.height.saturating_sub(4)).max(1);
    if lines.len() > room {
        let more = lines.len() - room + 1;
        lines.truncate(room - 1);
        lines.push(Line::raw(format!("… and {more} more")).fg(theme::MUTED));
    }
    let modal = area.centered(Constraint::Length(width), Constraint::Length(lines.len() as u16 + 4));
    frame.render_widget(Clear, modal);
    frame.render_widget(Paragraph::new(lines).block(block), modal);
}

/// The help `lines` in a rounded box over `area` less a margin, wrapped to
/// its width from the line `scroll`, which is clamped to the last page; the
/// page's height is returned. A heading is bold in the accent, an entry's
/// name before its two spaces is in the accent, and a question is bold.
fn draw_help(frame: &mut Frame, lines: &[String], scroll: &mut usize, area: Rect) -> usize {
    let modal = area.inner(Margin::new(2, 1));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT))
        .title(Span::raw(" help ").fg(theme::ACCENT).bold())
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(modal);
    let mut text: Vec<Line> = Vec::new();
    for line in lines {
        let indent = line.len() - line.trim_start().len();
        let term = line[indent..].find("  ").map(|at| indent + at);
        for (i, piece) in wrap(line, inner.width.saturating_sub(1).into()).into_iter().enumerate() {
            text.push(match term {
                Some(at) if i == 0 && at < piece.len() => Line::from(vec![
                    Span::raw(piece[..at].to_string()).fg(theme::ACCENT),
                    Span::raw(piece[at..].to_string()),
                ]),
                Some(_) => Line::raw(piece),
                None if indent == 0 => Line::raw(piece).fg(theme::ACCENT).bold(),
                None if indent == 2 => Line::raw(piece).bold(),
                None => Line::raw(piece),
            });
        }
    }
    let page = usize::from(inner.height).max(1);
    *scroll = (*scroll).min(text.len().saturating_sub(page));
    frame.render_widget(Clear, modal);
    frame.render_widget(block, modal);
    let total = text.len();
    frame.render_widget(Paragraph::new(text.into_iter().skip(*scroll).collect::<Vec<_>>()), inner);
    scrollbar(frame, modal.inner(Margin::new(0, 1)), total, *scroll, page);
    page
}

/// `line` broken at spaces into lines of at most `width` columns, each
/// continuation indented two columns past its leading spaces.
fn wrap(line: &str, width: usize) -> Vec<String> {
    let body = line.trim_start();
    let indent = line.len() - body.len();
    let mut out = vec![" ".repeat(indent)];
    for (i, word) in body.split(' ').enumerate() {
        let cur = out.last_mut().unwrap();
        let fits = cur.chars().count() + 1 + word.chars().count() <= width;
        if i == 0 || fits || word.is_empty() {
            if i > 0 {
                cur.push(' ');
            }
            cur.push_str(word);
        } else {
            out.push(format!("{}{word}", " ".repeat(indent + 2)));
        }
    }
    out
}

/// The job pane: a spinner and the running time while the job runs, then a
/// check or a cross, in a box of the same colour, over its last lines.
fn draw_job(frame: &mut Frame, job: &Job, area: Rect) {
    let elapsed = job.started.elapsed();
    let (icon, state, colour) = match (&job.thread, job.ok) {
        (Some(_), _) => {
            let spin = SPINNER[(elapsed.as_millis() / 100) as usize % SPINNER.len()];
            (spin, format!("running · {}s", elapsed.as_secs()), theme::ACCENT)
        }
        (None, true) => ("✓", "done · Esc closes".into(), theme::GOOD),
        (None, false) => ("✗", "failed · Esc closes".into(), theme::BAD),
    };
    let title = Line::from(vec![
        Span::raw(format!(" {icon}")).fg(colour).bold(),
        Span::raw(format!(" {} {} · {state} ", job.verb, job.target)).bold(),
    ]);
    let lines: Vec<Line> =
        job.lines.iter().skip(job.lines.len().saturating_sub(PANE_LINES)).map(|l| status_line(l)).collect();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(colour))
        .title(title)
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// A scrollbar down the right edge of `area` when `total` rows overflow the
/// `page` shown from row `first`.
fn scrollbar(frame: &mut Frame, area: Rect, total: usize, first: usize, page: usize) {
    if total <= page {
        return;
    }
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .track_symbol(Some("│"))
        .track_style(Style::new().fg(theme::MUTED))
        .thumb_symbol("┃")
        .thumb_style(Style::new().fg(theme::ACCENT));
    let mut state = ScrollbarState::new(total - page + 1).position(first).viewport_content_length(page);
    frame.render_stateful_widget(bar, area, &mut state);
}

/// A line of `skill`'s card as `show` prints it at a terminal: its `ESC [ n
/// m` styles kept, a table header bold in the accent. A line with none
/// takes the heading's accent, a section title's bold, a histogram bar's
/// accent, or the accent on its leading `label:`.
fn card_line(line: &str, skill: &str) -> Line<'static> {
    if line.contains('\x1b') {
        return Line::from(sgr(line));
    }
    if line.starts_with(&format!("{skill}: ")) {
        return Line::raw(line.to_string()).fg(theme::ACCENT).bold();
    }
    if line.ends_with(':') && !line.starts_with(' ') {
        return Line::raw(line.to_string()).bold();
    }
    let at = line.find('█').or_else(|| {
        let (label, _) = line.split_once(": ")?;
        let lower = label.starts_with(|c: char| c.is_ascii_lowercase());
        (lower && label.bytes().all(|b| b.is_ascii_lowercase() || b == b' ')).then_some(label.len() + 1)
    });
    match at {
        Some(at) if line[at..].starts_with('█') => {
            Line::from(vec![Span::raw(line[..at].to_string()), Span::raw(line[at..].to_string()).fg(theme::ACCENT)])
        }
        Some(at) => {
            Line::from(vec![Span::raw(line[..at].to_string()).fg(theme::ACCENT), Span::raw(line[at..].to_string())])
        }
        None => Line::raw(line.to_string()),
    }
}

/// `text` as spans styled by the `ESC [ n m` sequences [`render`] writes:
/// 1, a table header, bold in the accent; 31 and 33 its red and yellow; 0
/// resets.
fn sgr(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut style = Style::new();
    for (i, part) in text.split("\x1b[").enumerate() {
        let (code, rest) = if i == 0 { ("", part) } else { part.split_once('m').unwrap_or(("", part)) };
        style = match code {
            "0" => Style::new(),
            "1" => style.fg(theme::ACCENT).bold(),
            "31" => style.fg(theme::BAD),
            "33" => style.fg(theme::WARN),
            _ => style,
        };
        if !rest.is_empty() {
            spans.push(Span::styled(rest.to_string(), style));
        }
    }
    spans
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
