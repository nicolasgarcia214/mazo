//! Text rendering helpers shared by reports: aligned tables, local
//! timestamps and status colours.

use std::ffi::OsStr;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use unicode_width::UnicodeWidthStr;

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

/// A status token's colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Red,
    Yellow,
}

/// The colour of a status token: red for `unused`, `ERROR:` and the audit
/// column's security mark `!`, yellow for
/// `held`, `new`, `warning:`, a `named by` note and a grade's `stale` mark;
/// `None` for any other token.
pub fn status_colour(token: &str) -> Option<Colour> {
    match token {
        "unused" | "ERROR:" | "!" => Some(Colour::Red),
        "held" | "new" | "warning:" | "stale" => Some(Colour::Yellow),
        t if t.starts_with("named by ") => Some(Colour::Yellow),
        _ => None,
    }
}

/// `token` in its [`status_colour`] when `on`; unchanged otherwise.
pub fn paint(token: &str, on: bool) -> String {
    let code = match status_colour(token) {
        Some(Colour::Red) if on => "31",
        Some(Colour::Yellow) if on => "33",
        _ => return token.to_string(),
    };
    format!("\x1b[{code}m{token}\x1b[0m")
}

/// `line` split after a leading `ERROR:` or `warning:`.
fn prefixed(line: &str) -> Option<(&str, &str)> {
    line.split_once(' ').filter(|(head, _)| matches!(*head, "ERROR:" | "warning:"))
}

/// `line` with a leading `ERROR:` or `warning:` painted.
pub fn paint_prefix(line: &str, on: bool) -> String {
    prefixed(line).map_or_else(|| line.to_string(), |(head, rest)| format!("{} {rest}", paint(head, on)))
}

/// `line` without a leading `ERROR:` or `warning:`, for JSON whose key says which.
pub fn unprefixed(line: &str) -> &str {
    prefixed(line).map_or(line, |(_, rest)| rest)
}

/// Visible columns of `s`: its display width, less `ESC [ … m` colour
/// sequences.
fn width(s: &str) -> usize {
    let mut n = 0;
    let mut rest = s;
    while let Some(i) = rest.find('\x1b') {
        n += rest[..i].width();
        rest = rest[i..].split_once('m').map_or("", |(_, r)| r);
    }
    n + rest.width()
}

/// An aligned plain-text table, one space between columns, the header bold
/// when `tty`. Widths count display columns, so painted and wide cells keep
/// their alignment. Trailing spaces are trimmed
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

/// `text` with every control character but newline and tab, C0, DEL and C1,
/// replaced by U+FFFD, so text from a skill or a judge cannot drive the
/// terminal it is printed to.
pub fn inert(text: &str) -> String {
    text.chars().map(|c| if c.is_control() && !matches!(c, '\n' | '\t') { '\u{fffd}' } else { c }).collect()
}

/// `value` with every string in it, keys too, [`inert`]: a judge's answer, or
/// a lockfile anyone can commit.
pub fn inert_value(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(inert(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(inert_value).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (inert(&k), inert_value(v))).collect()),
        v => v,
    }
}

/// The timestamp `ts` in `tz`, formatted with `format`; `ts` unchanged when it
/// does not parse.
pub fn local(ts: &str, tz: &TimeZone, format: &str) -> String {
    ts.parse().map_or(ts.to_string(), |t| zoned(t, tz, format))
}

/// `n noun`, with an `s` unless `n` is 1.
pub fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// `t` in `tz`, formatted with `format`.
pub fn zoned(t: Timestamp, tz: &TimeZone, format: &str) -> String {
    t.to_zoned(tz.clone()).strftime(format).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_formats_in_the_zone_and_keeps_an_unparsed_timestamp() {
        let tz = TimeZone::get("America/Bogota").unwrap();
        assert_eq!(local("2026-03-01T04:30:00Z", &tz, MINUTE), "2026-02-28 23:30");
        assert_eq!(local("2026-03-01T04:30:00Z", &tz, DAY), "2026-02-28");
        assert_eq!(local("yesterday", &tz, MINUTE), "yesterday");
    }

    #[test]
    fn inert_replaces_controls_but_newline_and_tab() {
        let osc = "\x1b]52;c;ZXZpbA==\x07\x1b[2J\x7f\u{9b}31m\r";
        assert_eq!(inert(osc), "\u{fffd}]52;c;ZXZpbA==\u{fffd}\u{fffd}[2J\u{fffd}\u{fffd}31m\u{fffd}");
        assert_eq!(inert("a\tb\nc é 日"), "a\tb\nc é 日");
    }

    #[test]
    fn table_aligns_by_display_width() {
        let rows = [vec!["xyz".into(), "5".into()], vec!["日本".into(), "6".into()]];
        let t = table(&[("a", Align::Left), ("n", Align::Right)], &rows, false);
        assert_eq!(t, "a    n\nxyz  5\n日本 6\n");
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
        assert_eq!(paint("stale", true), "\x1b[33mstale\x1b[0m");
        assert_eq!(paint("+12", true), "+12");
        assert_eq!(status_colour("ERROR:"), Some(Colour::Red));
        assert_eq!(status_colour("named by a"), Some(Colour::Yellow));
        assert_eq!(status_colour("+wip"), None);
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
