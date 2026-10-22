//! The TUI of bare `mazo`: the usage table, the unused list and one skill's
//! card, rendered from the typed report model over a finished scan. Its
//! actions, `retire` and `sync`, each ask y/N first and run the CLI command's
//! own code path; the data is re-read after each.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
use crate::render::{self, Align, Colour};
use crate::report::{self, Input, SHOW_LIMIT, Since, Unused, Usage, UsageArgs};

/// The windows `w` cycles through.
const WINDOWS: [Since; 3] = [Since::Days(30), Since::Days(60), Since::All];

/// The keys, as the footer lists them.
const KEYS: [&str; 12] = [
    "↑↓←→/hjkl move",
    "Enter card",
    "Esc back",
    "Tab usage/unused",
    "/ filter",
    "w window",
    "a agent",
    "c channel",
    "A authoring",
    "r retire",
    "s sync",
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

/// What an [`Input`] borrows, owned, so an action can re-read it.
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

    /// Re-reads the inventory over the same roots, the ledger, the coverage
    /// and the audits, as of now.
    fn reload(&mut self) -> crate::Result<()> {
        let roots: Vec<&Path> = self.inv.roots.iter().map(PathBuf::as_path).collect();
        self.inv = Inventory::load_roots(self.inv.source.as_deref(), &roots)?;
        self.events = Ledger::load(&self.dir)?.events().to_vec();
        self.coverage = ledger::load_coverage(&self.dir)?;
        self.audits = audit::latest(&self.dir)?;
        self.now = jiff::Timestamp::now();
        Ok(())
    }
}

/// What `y` runs in a dialog.
enum Act {
    Retire(String),
    /// The plan the dialog shows, applied as shown.
    Sync(Plan),
}

/// Lines over the screen that take the next key: `y` runs `act`, any other
/// key closes it.
struct Dialog {
    lines: Vec<String>,
    act: Option<Act>,
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
            clear: false,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
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
            return false;
        }
        if let Some(dialog) = self.dialog.take() {
            if let Some(act) = dialog.act
                && key.code == KeyCode::Char('y')
                && key.modifiers.is_empty()
            {
                let lines = self.act(act);
                self.dialog = Some(Dialog { lines, act: None });
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
            KeyCode::Char('q') => return false,
            KeyCode::Up | KeyCode::Char('k') => self.shift(-1),
            KeyCode::Down | KeyCode::Char('j') => self.shift(1),
            KeyCode::Left | KeyCode::Char('h') => self.shift(-(self.page as isize)),
            KeyCode::Right | KeyCode::Char('l') => self.shift(self.page as isize),
            KeyCode::Enter if list => {
                if let Some(skill) = self.listed().get(self.selected).map(|s| s.to_string()) {
                    (self.back, self.screen, self.card_since, self.scroll) =
                        (self.screen, Screen::Card, self.usage_since, 0);
                    self.open(&skill);
                }
            }
            KeyCode::Esc if !list => self.screen = self.back,
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
            KeyCode::Char('r') => {
                let skill = match &self.card {
                    Some((skill, _)) if !list => Some(skill.clone()),
                    _ => self.listed().get(self.selected).map(|s| s.to_string()),
                };
                if let Some(skill) = skill {
                    let what = match self.data.cfg.source.as_deref() {
                        Some(source) if self.data.inv.root(&skill) == Some(source) => {
                            format!("move it to {}", source.join(".archive").join(&skill).display())
                        }
                        _ => "remove it from the store".into(),
                    };
                    let lines = vec![format!("retire {skill}: {what} and prune its links?")];
                    self.dialog = Some(Dialog { lines, act: Some(Act::Retire(skill)) });
                }
            }
            KeyCode::Char('s') => {
                let homes: Vec<&Home> = self.data.cfg.homes.iter().collect();
                self.dialog = Some(match links::checked(&self.data.cfg, &self.data.inv, &homes) {
                    Err(errors) => Dialog { lines: [vec!["sync refused:".into()], errors].concat(), act: None },
                    Ok(plan) if plan.actions.is_empty() => {
                        Dialog { lines: vec!["in sync: nothing to apply".into()], act: None }
                    }
                    Ok(plan) => Dialog { lines: links::planned(&plan), act: Some(Act::Sync(plan)) },
                });
            }
            _ => {}
        }
        true
    }

    /// Runs `act` through the CLI command's code path, never forced, then
    /// re-reads the data. The lines it reports.
    fn act(&mut self, act: Act) -> Vec<String> {
        self.clear = true;
        let d = &self.data;
        let ran = match act {
            Act::Retire(skill) => lifecycle::retire(&d.cfg, &d.inv, &d.dir, &skill, false).map(|(mut out, retired)| {
                match retired {
                    Retired::Blocked(lines) | Retired::Refused(lines) => {
                        out.push(format!("retire {skill} refused:"));
                        out.extend(lines);
                    }
                    Retired::Done(lines) => out.extend(lines),
                }
                out
            }),
            Act::Sync(plan) => links::apply(&plan),
        };
        let mut out = ran.unwrap_or_else(|e| vec![format!("ERROR: {e}")]);
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
            Some(Dialog { act: Some(_), .. }) => return "y apply · any other key cancels".into(),
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
        let [body, info, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1), height]).areas(frame.area());
        frame.render_widget(Paragraph::new(footer_lines), footer);
        if let Some(dialog) = &self.dialog {
            let area = body.union(info);
            let room = usize::from(area.height.saturating_sub(2)).max(1);
            let mut lines: Vec<Line> = dialog.lines.iter().flat_map(|l| l.lines()).map(Line::raw).collect();
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

/// Opens the TUI on `input` and the data directory `data` and runs it until
/// `q` or Ctrl-C. `ratatui::init` installs a panic hook that restores the
/// terminal; the terminal shows the cursor again when it drops, after the
/// restore leaves the alternate screen.
pub fn run(input: &Input, data: &Path) -> std::io::Result<()> {
    let mut app = App::new(input, data);
    let mut terminal = ratatui::init();
    let result = (|| {
        loop {
            // A scan's progress line may have been written under the screen.
            if std::mem::take(&mut app.clear) {
                terminal.clear()?;
            }
            terminal.draw(|frame| app.draw(frame))?;
            if let Event::Key(key) = event::read()?
                && !app.key(key)
            {
                return Ok(());
            }
        }
    })();
    ratatui::restore();
    result
}
