//! Text rendering helpers shared by reports: aligned tables, sparklines,
//! local timestamps and status colours.

use std::ffi::OsStr;

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// Column alignment.
#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Right,
}

/// Whether a stream gets colour: it is a terminal and `no_color`, the value
/// of `NO_COLOR`, is unset or empty.
pub fn colour(tty: bool, no_color: Option<&OsStr>) -> bool {
    tty && no_color.is_none_or(OsStr::is_empty)
}

/// `token` in its status colour when `on`: red for `unused` and `ERROR:`,
/// yellow for `held`, `new`, `warning:`, a `named by` note and a stale
/// grade's `+N` or `+wip`. Any other token comes back unchanged.
pub fn paint(token: &str, on: bool) -> String {
    let code = match token {
        "unused" | "ERROR:" => "31",
        "held" | "new" | "warning:" => "33",
        "+wip" => "33",
        t if t.starts_with("named by ") => "33",
        t if t.strip_prefix('+').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) => "33",
        _ => return token.to_string(),
    };
    if on { format!("\x1b[{code}m{token}\x1b[0m") } else { token.to_string() }
}

/// `line` with a leading `ERROR:` or `warning:` painted.
pub fn paint_prefix(line: &str, on: bool) -> String {
    match line.split_once(' ') {
        Some((head @ ("ERROR:" | "warning:"), rest)) => format!("{} {rest}", paint(head, on)),
        _ => line.to_string(),
    }
}

/// Visible columns of `s`: chars, less `ESC [ … m` colour sequences.
fn width(s: &str) -> usize {
    let mut n = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            n += 1;
        }
    }
    n
}

/// An aligned plain-text table, one space between columns, the header bold
/// when `tty`. Widths count visible chars, so sparkline cells are one column
/// each and painted cells keep their alignment. Trailing spaces are trimmed
/// from every line.
pub fn table(header: &[(&str, Align)], rows: &[Vec<String>], tty: bool) -> String {
    let mut widths: Vec<usize> = header.iter().map(|(h, _)| width(h)).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(width(cell));
        }
    }
    let line = |cells: Vec<&str>| {
        let parts: Vec<String> = cells
            .iter()
            .zip(&widths)
            .zip(header)
            .map(|((c, &w), (_, align))| {
                let pad = " ".repeat(w - width(c));
                match align {
                    Align::Left => format!("{c}{pad}"),
                    Align::Right => format!("{pad}{c}"),
                }
            })
            .collect();
        parts.join(" ").trim_end().to_string()
    };
    let head = line(header.iter().map(|(h, _)| *h).collect());
    let mut out = if tty { format!("\x1b[1m{head}\x1b[0m\n") } else { format!("{head}\n") };
    for row in rows {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
        out.push('\n');
    }
    out
}

pub const DAY: &str = "%Y-%m-%d";
pub const MINUTE: &str = "%Y-%m-%d %H:%M";

/// The timestamp `ts` in `tz`, formatted with `format`; `ts` unchanged when it
/// does not parse.
pub fn local(ts: &str, tz: &TimeZone, format: &str) -> String {
    ts.parse::<Timestamp>().map_or(ts.to_string(), |t| t.to_zoned(tz.clone()).strftime(format).to_string())
}

const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// One cell per count: `·` for zero, and `▁`…`█` for a non-zero count scaled
/// against the largest, so a single event is still visible.
pub fn sparkline(counts: &[usize]) -> String {
    let max = counts.iter().copied().max().unwrap_or(0);
    counts.iter().map(|&c| if c == 0 { '·' } else { BARS[(c * 8).div_ceil(max) - 1] }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_marks_empty_days_and_keeps_small_counts_visible() {
        assert_eq!(sparkline(&[0, 1, 7, 50, 100]), "·▁▁▄█");
        assert_eq!(sparkline(&[0, 0]), "··");
        assert_eq!(sparkline(&[3]), "█");
    }

    #[test]
    fn local_formats_in_the_zone_and_keeps_an_unparsed_timestamp() {
        let tz = TimeZone::get("America/Bogota").unwrap();
        assert_eq!(local("2026-03-01T04:30:00Z", &tz, MINUTE), "2026-02-28 23:30");
        assert_eq!(local("2026-03-01T04:30:00Z", &tz, DAY), "2026-02-28");
        assert_eq!(local("yesterday", &tz, MINUTE), "yesterday");
    }

    #[test]
    fn table_aligns_by_chars() {
        let t = table(&[("a", Align::Left), ("n", Align::Right)], &[vec!["xyz".into(), "5".into()]], false);
        assert_eq!(t, "a   n\nxyz 5\n");
    }

    #[test]
    fn table_aligns_painted_cells_by_visible_width() {
        let rows = [vec![paint("held", true), "x".into()], vec!["unused".into(), "y".into()]];
        let t = table(&[("s", Align::Left), ("n", Align::Left)], &rows, false);
        assert_eq!(t, "s      n\n\x1b[33mheld\x1b[0m   x\nunused y\n");
    }

    #[test]
    fn paint_colours_only_status_tokens() {
        assert_eq!(paint("unused", true), "\x1b[31munused\x1b[0m");
        assert_eq!(paint("held", true), "\x1b[33mheld\x1b[0m");
        assert_eq!(paint("+wip", true), "\x1b[33m+wip\x1b[0m");
        assert_eq!(paint("+12", true), "\x1b[33m+12\x1b[0m");
        assert_eq!(paint("+x", true), "+x");
        assert_eq!(paint("+", true), "+");
        assert_eq!(paint("cluster: a", true), "cluster: a");
        assert_eq!(paint("unused", false), "unused");
        assert_eq!(paint_prefix("ERROR: x: y", true), "\x1b[31mERROR:\x1b[0m x: y");
        assert_eq!(paint_prefix("warning: x", true), "\x1b[33mwarning:\x1b[0m x");
        assert_eq!(paint_prefix("claude: in sync", true), "claude: in sync");
    }

    #[test]
    fn colour_needs_a_tty_and_no_no_color() {
        assert!(colour(true, None));
        assert!(colour(true, Some(OsStr::new(""))));
        assert!(!colour(true, Some(OsStr::new("1"))));
        assert!(!colour(false, None));
    }
}
