//! The read-only TUI of bare `skima`: the usage table, the unused list and
//! one skill's card, rendered from the typed report model over a finished
//! scan. It writes nothing.

use clap::ValueEnum;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ledger::{Agent, Channel, name};
use crate::render::{self, Align, Colour};
use crate::report::{self, Input, SHOW_LIMIT, Since, Unused, Usage, UsageArgs};

/// The windows `w` cycles through.
const WINDOWS: [Since; 3] = [Since::Days(30), Since::Days(60), Since::All];

/// The keys, as the footer lists them.
const KEYS: [&str; 10] = [
    "↑↓←→/hjkl move",
    "Enter card",
    "Esc back",
    "Tab usage/unused",
    "/ filter",
    "w window",
    "a agent",
    "c channel",
    "A authoring",
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

/// The TUI's state over one report input.
pub struct App<'a> {
    input: &'a Input<'a>,
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
    /// List rows a page move skips: the rows the last draw showed.
    page: usize,
    usage: Usage,
    unused: Unused,
    /// The open card's skill and its `show` lines.
    card: Option<(&'a str, Vec<String>)>,
    scroll: usize,
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

/// `label: home window · …`.
fn coverage(label: &str, homes: &std::collections::BTreeMap<String, String>) -> String {
    let parts: Vec<String> = homes.iter().map(|(h, t)| format!("{h} {t}")).collect();
    format!("{label}: {}", parts.join(" · "))
}

impl<'a> App<'a> {
    /// The usage view, over each command's default window and filter.
    pub fn new(input: &'a Input<'a>) -> App<'a> {
        let (usage_since, unused_since) = (Since::Days(30), Since::Days(60));
        App {
            input,
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

    /// The skills the current list shows.
    fn listed(&self) -> Vec<&str> {
        let skills: Vec<&str> = match self.screen {
            Screen::Unused => self.unused.skills.iter().map(|r| r.skill.as_str()).collect(),
            _ => self.usage.rows.iter().map(|r| r.skill.as_str()).collect(),
        };
        skills.into_iter().filter(|s| self.matches(s)).collect()
    }

    fn open(&mut self, skill: &'a str) {
        let card = report::skill_card(self.input, skill, &self.args(self.card_since), SHOW_LIMIT);
        let lines = report::card_text(self.input, &card, false).lines().map(String::from).collect();
        self.card = Some((skill, lines));
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
        if self.typing {
            match key.code {
                KeyCode::Char(c) => self.filter.push(c),
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
                let input = self.input;
                let skill = self.listed().get(self.selected).and_then(|s| input.inv.skills.get(*s));
                if let Some(skill) = skill {
                    (self.back, self.screen, self.card_since, self.scroll) =
                        (self.screen, Screen::Card, self.usage_since, 0);
                    self.open(skill);
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
                    self.unused = report::unused_list(self.input, self.unused_since);
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
            _ => {}
        }
        true
    }

    /// Recounts the usage view and the open card under the current filter.
    fn refresh(&mut self) {
        self.usage = report::usage_table(self.input, &self.args(self.usage_since));
        self.selected = self.selected.min(self.listed().len().saturating_sub(1));
        if let Some((skill, _)) = self.card {
            self.open(skill);
            self.shift(0);
        }
    }

    /// The footer's first line: the view, its window and the active filters.
    fn status(&self) -> String {
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
        if let (Screen::Card, Some((_, lines))) = (self.screen, &self.card) {
            let text: Vec<Line> = lines.iter().skip(self.scroll).map(|l| Line::raw(l.as_str())).collect();
            frame.render_widget(Paragraph::new(text), body.union(info));
            return;
        }
        let (mut lines, note) = match self.screen {
            Screen::Unused => {
                let rows: Vec<Vec<String>> = self
                    .unused
                    .skills
                    .iter()
                    .filter(|r| self.matches(&r.skill))
                    .map(|r| report::unused_cells(r, false))
                    .collect();
                (table(&report::UNUSED_HEADER, &rows, body.width.into()), coverage("window", &self.unused.window))
            }
            _ => {
                let rows: Vec<Vec<String>> = self
                    .usage
                    .rows
                    .iter()
                    .filter(|r| self.matches(&r.skill))
                    .map(|r| report::usage_cells(self.input, r, false))
                    .collect();
                (table(&report::USAGE_HEADER, &rows, body.width.into()), coverage("coverage", &self.usage.coverage))
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

/// Opens the TUI on `input` and runs it until `q` or Ctrl-C. `ratatui::init`
/// installs a panic hook that restores the terminal; the terminal shows the
/// cursor again when it drops, after the restore leaves the alternate screen.
pub fn run(input: &Input) -> std::io::Result<()> {
    let mut app = App::new(input);
    let mut terminal = ratatui::init();
    let result = (|| {
        loop {
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
