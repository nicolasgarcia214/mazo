//! A skill's card as the TUI draws it: its `show` lines, styled, and its
//! activity chart.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, Paragraph, Widget};

use super::draw::scrollbar;
use crate::report::{self, Bucket, Since};
use crate::theme;

/// The rows of a card's activity chart: its bars and their labels.
const CHART_ROWS: usize = 6;

/// The rows of a card: its lines, and its chart with a blank row under it.
pub(super) fn card_height(lines: &[String]) -> usize {
    lines.len() + CHART_ROWS + 1
}

/// `skill`'s card `lines` in `area` from row `first`, inset, with its
/// `chart` over `since` under the heading and a scrollbar when they overflow
/// `area`; the rows a page shows. The whole card is drawn off screen, then
/// the rows in view are copied, so the chart scrolls with the lines.
pub(super) fn draw_card(
    frame: &mut Frame,
    skill: &str,
    lines: &[String],
    chart: &[Bucket],
    since: Since,
    first: usize,
    area: Rect,
) -> usize {
    let inset = area.inner(Margin::new(2, 0));
    let total = card_height(lines);
    // Under the heading and its blank line.
    let at = lines.iter().position(|l| l.starts_with(&format!("{skill}: "))).map_or(0, |i| i + 2).min(lines.len());
    let mut card = Buffer::empty(Rect::new(0, 0, inset.width, total as u16));
    let rect = |y: usize, height: usize| Rect::new(0, y as u16, inset.width, height as u16);
    let text = |lines: &[String]| Paragraph::new(lines.iter().map(|l| card_line(l, skill)).collect::<Vec<_>>());
    text(&lines[..at]).render(rect(0, at), &mut card);
    let shown = draw_activity(chart, rect(at, CHART_ROWS), &mut card);
    // A chart cut to the newest buckets says so after the heading.
    let heading = at.checked_sub(2).map(|i| lines[i].chars().count() as u16);
    if let Some(x) = heading.filter(|x| shown < chart.len() && *x < inset.width) {
        let note = format!(" · last {shown} {}", report::buckets_of(since));
        card.set_stringn(x, at as u16 - 2, note, usize::from(inset.width.saturating_sub(x)), theme::MUTED);
    }
    text(&lines[at..]).render(rect(at + CHART_ROWS + 1, lines.len() - at), &mut card);
    let buf = frame.buffer_mut();
    for y in 0..inset.height.min(total.saturating_sub(first) as u16) {
        for x in 0..inset.width {
            buf[(inset.x + x, inset.y + y)] = card[(x, first as u16 + y)].clone();
        }
    }
    scrollbar(frame, area, total, first, area.height.into());
    usize::from(area.height).max(1)
}

/// A card's activity chart in `area`: a bar per bucket as wide as the
/// longest label or count, a column apart, the newest that fit; each
/// labelled, with its session count, today's, still filling, muted. The
/// buckets it shows.
fn draw_activity(chart: &[Bucket], area: Rect, buf: &mut Buffer) -> usize {
    let counts: Vec<String> = chart.iter().map(|b| b.sessions.to_string()).collect();
    let width = chart.iter().map(|b| b.label.len()).chain(counts.iter().map(String::len)).max().unwrap_or(1) as u16;
    let from = chart.len().saturating_sub(usize::from((area.width + 1) / (width + 1)));
    let colour = |b: &Bucket| if b.partial { theme::MUTED } else { theme::ACCENT };
    let bars: Vec<Bar> = chart[from..]
        .iter()
        .map(|b| {
            // The count is written below: ratatui hides one as wide as a short bar, and any on an empty bar.
            Bar::new(b.sessions as u64).label(Line::raw(b.label.clone()).fg(colour(b))).style(colour(b)).text_value("")
        })
        .collect();
    BarChart::vertical(bars).bar_width(width).bar_gap(1).render(area, buf);
    for (i, (b, count)) in chart[from..].iter().zip(&counts[from..]).enumerate() {
        let x = area.x + i as u16 * (width + 1) + (width - count.len() as u16) / 2;
        let style = if b.sessions == 0 { Style::new().fg(theme::MUTED) } else { Style::new().fg(colour(b)).reversed() };
        buf.set_string(x, area.bottom() - 2, count, style);
    }
    chart.len() - from
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

/// `text` as spans styled by the `ESC [ n m` sequences [`render`](crate::render) writes:
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
