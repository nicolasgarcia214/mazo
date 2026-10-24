//! The screen: the top bar, the list tables, the side card, the footer and
//! the styling they share.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, HighlightSpacing, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
    TableState, Tabs,
};

use super::card::draw_card;
use super::dialog::{Ask, Dialog, draw_dialog, draw_help};
use super::job::{PANE_LINES, draw_job};
use super::{App, Screen};
use crate::ctx;
use crate::ledger::name;
use crate::render::{self, Align, Colour};
use crate::report::{self, Since};
use crate::store;
use crate::theme;

/// The terminal width from which the usage table shows the selected
/// skill's card beside it.
const WIDE: u16 = 140;

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

/// The top bar's tabs.
const TABS: [&str; 2] = ["usage", "unused"];

/// The top bar's name.
fn logo() -> Line<'static> {
    Line::from(vec![Span::raw(" ♠ mazo ").fg(Color::Black).bg(theme::ACCENT).bold(), Span::raw(" ")])
}

/// `s` in the muted colour.
fn muted(s: &str) -> Span<'static> {
    Span::raw(s.to_string()).fg(theme::MUTED)
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

/// The spans of `cell` in the column `head`: a grade of 9 or more green and its mark apart,
/// `today` in the accent, and zero counts and `-` muted. In any other column,
/// the audit mark among them, each `; `-separated note is styled as a status token.
fn styled(head: &str, cell: &str) -> Vec<Span<'static>> {
    let muted = |s: &str| Span::raw(s.to_string()).fg(theme::MUTED);
    match (head, cell) {
        (_, "-" | "0") => vec![muted(cell)],
        (_, "today") => vec![Span::raw("today").fg(theme::ACCENT)],
        ("grade", _) => {
            let (score, mark) = cell.split_once(' ').unwrap_or((cell, ""));
            let high = score.parse::<f64>().is_ok_and(|s| s >= 9.0);
            let score = if high { Span::raw(score.to_string()).fg(theme::GOOD) } else { Span::raw(score.to_string()) };
            let mark = (!mark.is_empty()).then(|| [Span::raw(" "), token(mark)]);
            [score].into_iter().chain(mark.into_iter().flatten()).collect()
        }
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
    /// The column's cells are cut with `…`, down to its header's width.
    Cut(&'static str),
}

/// How the columns give way as the terminal narrows, the first first, each
/// step taken only while the table overflows. A column not named here
/// never narrows; one only cut never drops.
const NARROWING: &[Narrow] = &[
    Narrow::Drop("typed"),
    Narrow::Drop("ref"),
    Narrow::Drop("read"),
    Narrow::Fold("slash", "tool", "via"),
    Narrow::Drop("agents"),
    Narrow::Drop("7d"),
    Narrow::Drop("ctx"),
    Narrow::Cut("notes"),
    Narrow::Cut("skill"),
];

/// `cols` narrowed by [`NARROWING`] until they fit in `room` columns, one
/// space between them, and the width of each: the first and the last take
/// what is left over.
fn fit(mut cols: Vec<Column>, room: usize) -> (Vec<Column>, Vec<u16>) {
    let used = |cols: &[Column]| cols.iter().map(Column::width).sum::<usize>() + cols.len().saturating_sub(1);
    for step in NARROWING {
        let over = used(&cols).saturating_sub(room);
        if over == 0 {
            break;
        }
        let at = |cols: &[Column], head| cols.iter().position(|c| c.head == head);
        match *step {
            Narrow::Drop(head) => _ = at(&cols, head).map(|i| cols.remove(i)),
            Narrow::Fold(a, b, head) => {
                if let (Some(i), Some(j)) = (at(&cols, a), at(&cols, b)) {
                    let b = cols.remove(j);
                    let a = &mut cols[if i < j { i } else { i - 1 }];
                    a.cells = a.cells.iter().zip(&b.cells).map(|(x, y)| format!("{x}/{y}")).collect();
                    a.head = head;
                }
            }
            Narrow::Cut(head) => {
                if let Some(col) = at(&cols, head).map(|i| &mut cols[i]) {
                    let keep = col.width().saturating_sub(over).max(head.chars().count());
                    for cell in &mut col.cells {
                        *cell = report::cut(cell, keep);
                    }
                }
            }
        }
    }
    let mut widths: Vec<usize> = cols.iter().map(Column::width).collect();
    let spare = room.saturating_sub(used(&cols));
    let last = widths.len() - 1;
    widths[0] += spare / 2;
    widths[last] += spare - spare / 2;
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

    /// The card's trail after the name: the list it goes back to, then its skill.
    fn trail(&self) -> Option<Line<'static>> {
        let (Screen::Card, Some((skill, ..))) = (self.screen, &self.card) else { return None };
        let back = if self.back == Screen::Unused { "unused" } else { "usage" };
        Some(Line::from(vec![muted(back), muted(" › "), Span::raw(skill.clone()).bold(), Span::raw("  ")]))
    }

    /// The columns the name and the tabs or the trail take.
    fn lead(&self) -> usize {
        // Each tab title is padded with a space either side, a divider between.
        logo().width() + self.trail().map_or(TABS.iter().map(|t| t.len() + 3).sum::<usize>() - 1, |t| t.width())
    }

    /// What follows the lead in `width` columns: the count of listed skills
    /// and the filter, the window, and the agent, channel and authoring
    /// filters when set; the tokens in context at the right edge, each
    /// agent's in the warning colour near its listing budget, their label
    /// shortened where the chips would crowd them; and the bundled skills
    /// not installed, after the chips when they fit beside the tokens, else
    /// returned as a row of its own.
    fn top(&self, width: usize) -> (Line<'static>, Line<'static>, Option<Line<'static>>) {
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
        let mut chips = Line::from(spans);
        let tokens = |label: &str| {
            let total = format!("~{}", ctx::approx(self.tokens.total()));
            let mut spans = vec![Span::raw(total).fg(theme::ACCENT), muted(label)];
            for (i, (agent, tokens, near)) in self.tokens.agents(self.data.cfg.context_window).into_iter().enumerate() {
                let colour = if near { theme::WARN } else { theme::ACCENT };
                let sep = if i > 0 { " · " } else { "" };
                spans.extend([muted(&format!("{sep}{} ", agent.letter())), Span::raw(ctx::approx(tokens)).fg(colour)]);
            }
            spans.push(Span::raw(" "));
            Line::from(spans)
        };
        let (long, short) = (tokens(" tok in context: "), tokens(" tok: "));
        let fits = |chips: usize, tokens: &Line| chips + 2 + tokens.width() <= width;
        let missing = store::missing_bundled(&self.data.inv);
        let mut notice = (!missing.is_empty())
            .then(|| Span::raw(format!("{} not installed, press i", missing.join(", "))).fg(theme::WARN));
        // Joined by ` · `, three columns.
        if let Some(n) = notice.take_if(|n| fits(chips.width() + 3 + n.width(), &short)) {
            chips.spans.extend([muted(" · "), n]);
        }
        let line = if fits(chips.width(), &long) { long } else { short };
        (chips, line, notice.map(|n| Line::from(vec![Span::raw(" "), n])))
    }

    /// The top bar in `area`: the name, the tabs or the card's trail, then
    /// [`App::top`]'s chips and tokens, and on a second row, when `area` has
    /// one, its notice.
    fn draw_top(&self, frame: &mut Frame, area: Rect) {
        let [row, below] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        // The first `width` columns of `area`, and the rest.
        let split = |area, width: usize| {
            Layout::horizontal([Constraint::Length(width as u16), Constraint::Fill(1)]).areas(area)
        };
        let [at, area] = split(row, logo().width());
        frame.render_widget(logo(), at);
        let [at, area] = split(area, self.lead() - logo().width());
        match self.trail() {
            Some(trail) => frame.render_widget(trail, at),
            None => {
                let tabs = Tabs::new(TABS)
                    .select(usize::from(self.screen == Screen::Unused))
                    .style(theme::MUTED)
                    .highlight_style(Style::new().fg(theme::ACCENT).bold().underlined())
                    .divider(Span::raw("│").fg(theme::MUTED));
                frame.render_widget(tabs, at);
            }
        }
        let (chips, line, notice) = self.top(area.width.into());
        let [at, right] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(line.width() as u16)]).areas(area);
        frame.render_widget(chips, at);
        frame.render_widget(line, right);
        if let Some(notice) = notice {
            frame.render_widget(notice, below);
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let footer_lines = match self.hints() {
            Some(hints) => vec![theme::hints(&hints)],
            None => keys(self.screen, frame.area().width.into()),
        };
        let height = Constraint::Length(footer_lines.len() as u16);
        let pane_height = self.job.as_ref().map_or(0, |j| j.lines.len().clamp(1, PANE_LINES) as u16 + 2);
        // The top bar's notice takes a second row when it does not fit the first.
        let rest = usize::from(frame.area().width).saturating_sub(self.lead());
        let top_height = 1 + u16::from(self.top(rest).2.is_some());
        let [top, body, info, pane, footer] = Layout::vertical([
            Constraint::Length(top_height),
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
            (Screen::Card, Some((skill, lines, chart))) => {
                self.page = draw_card(frame, skill, lines, chart, self.card_since, self.scroll, body.union(info));
            }
            (Screen::Usage, _) if body.width >= WIDE => {
                let side = Constraint::Length((body.width * 2 / 5).max(50));
                let [table, side] = Layout::horizontal([Constraint::Fill(1), side]).areas(body);
                self.draw_list(frame, table, info);
                self.draw_side(frame, side);
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

    /// The selected skill's card in `area`, behind a rule, from its top.
    fn draw_side(&mut self, frame: &mut Frame, area: Rect) {
        let Some(skill) = self.listed().get(self.selected).map(|s| s.to_string()) else { return };
        if self.side.as_ref().is_none_or(|(s, ..)| *s != skill) {
            self.side = Some(self.shown(&skill, self.usage_since));
        }
        let rule = Block::new().borders(Borders::LEFT).border_style(theme::MUTED);
        frame.render_widget(&rule, area);
        let (skill, lines, chart) = self.side.as_ref().unwrap();
        draw_card(frame, skill, lines, chart, self.usage_since, 0, rule.inner(area));
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

/// `line` with a leading status token, such as `ERROR:`, in its colour.
pub(super) fn status_line(line: &str) -> Line<'static> {
    match line.split_once(' ') {
        Some((head, rest)) if render::status_colour(head).is_some() => {
            Line::from(vec![token(head).bold(), Span::raw(format!(" {rest}"))])
        }
        _ => Line::raw(line.to_string()),
    }
}

/// A scrollbar down the right edge of `area` when `total` rows overflow the
/// `page` shown from row `first`.
pub(super) fn scrollbar(frame: &mut Frame, area: Rect, total: usize, first: usize, page: usize) {
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
