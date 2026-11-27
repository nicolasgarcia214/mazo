use std::ffi::OsStr;
use std::path::Path;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Right,
}

pub fn colour(tty: bool, no_color: Option<&OsStr>) -> bool {
    tty && no_color.is_none_or(OsStr::is_empty)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Red,
    Yellow,
}

pub fn status_colour(token: &str) -> Option<Colour> {
    match token {
        "unused" | "!" => Some(Colour::Red),
        "held" | "new" | "stale" => Some(Colour::Yellow),
        t if t.starts_with("named by ") => Some(Colour::Yellow),
        _ => None,
    }
}

pub fn paint(token: &str, on: bool) -> String {
    tint(token, status_colour(token), on)
}

fn tint(token: &str, colour: Option<Colour>, on: bool) -> String {
    let code = match colour {
        Some(Colour::Red) if on => "31",
        Some(Colour::Yellow) if on => "33",
        _ => return token.to_string(),
    };
    format!("\x1b[{code}m{token}\x1b[0m")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

impl Level {
    pub fn colour(self) -> Colour {
        match self {
            Level::Error => Colour::Red,
            Level::Warning => Colour::Yellow,
        }
    }

    fn head(self) -> &'static str {
        match self {
            Level::Error => "ERROR:",
            Level::Warning => "warning:",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub level: Level,
    pub text: String,
}

impl Diag {
    pub fn error(text: impl Into<String>) -> Diag {
        Diag { level: Level::Error, text: text.into() }
    }

    pub fn warning(text: impl Into<String>) -> Diag {
        Diag { level: Level::Warning, text: text.into() }
    }

    pub fn paint(&self, on: bool) -> String {
        format!("{} {}", tint(self.level.head(), Some(self.level.colour()), on), self.text)
    }
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.paint(false))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Plain(String),
    Diag(Diag),
}

impl Note {
    pub fn level(&self) -> Option<Level> {
        match self {
            Note::Plain(_) => None,
            Note::Diag(d) => Some(d.level),
        }
    }

    pub fn split(&self) -> Vec<Note> {
        let plain = |l: &str| Note::Plain(l.to_string());
        match self {
            Note::Plain(s) => s.lines().map(plain).collect(),
            Note::Diag(d) => {
                let mut lines = d.text.lines();
                let first = Note::Diag(Diag { level: d.level, text: lines.next().unwrap_or("").to_string() });
                std::iter::once(first).chain(lines.map(plain)).collect()
            }
        }
    }
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Note::Plain(s) => f.write_str(s),
            Note::Diag(d) => d.fmt(f),
        }
    }
}

impl From<String> for Note {
    fn from(s: String) -> Note {
        Note::Plain(s)
    }
}

impl From<&str> for Note {
    fn from(s: &str) -> Note {
        Note::Plain(s.to_string())
    }
}

impl From<Diag> for Note {
    fn from(d: Diag) -> Note {
        Note::Diag(d)
    }
}

fn width(s: &str) -> usize {
    let mut n = 0;
    let mut rest = s;
    while let Some(i) = rest.find('\x1b') {
        n += rest[..i].width();
        rest = rest[i..].split_once('m').map_or("", |(_, r)| r);
    }
    n + rest.width()
}

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

pub fn inert(text: &str) -> String {
    let spoofs = |c| {
        matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            || matches!(c, '\u{200b}' | '\u{2028}' | '\u{2029}' | '\u{feff}')
    };
    text.chars()
        .map(|c| if (c.is_control() && !matches!(c, '\n' | '\t')) || spoofs(c) { '\u{fffd}' } else { c })
        .collect()
}

pub fn inert_line(text: &str) -> String {
    inert(text).replace(['\n', '\t'], "\u{fffd}")
}

pub struct InertPath<'a>(&'a Path);

impl std::fmt::Display for InertPath<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.pad(&inert_line(&self.0.to_string_lossy()))
    }
}

pub trait PathExt {
    fn inert(&self) -> InertPath<'_>;
}

impl PathExt for Path {
    fn inert(&self) -> InertPath<'_> {
        InertPath(self)
    }
}

pub fn inert_value(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(inert(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(inert_value).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (inert(&k), inert_value(v))).collect()),
        v => v,
    }
}

pub fn local(ts: &str, tz: &TimeZone, format: &str) -> String {
    ts.parse().map_or(ts.to_string(), |t| zoned(t, tz, format))
}

pub fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

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
    fn inert_replaces_controls_but_newline_and_tab_and_spoofing_marks() {
        let osc = "\x1b]52;c;ZXZpbA==\x07\x1b[2J\x7f\u{9b}31m\r";
        assert_eq!(inert(osc), "\u{fffd}]52;c;ZXZpbA==\u{fffd}\u{fffd}[2J\u{fffd}\u{fffd}31m\u{fffd}");
        assert_eq!(inert("a\tb\nc é 日"), "a\tb\nc é 日");
        assert_eq!(
            inert("evil\u{202e}gpj.sh\u{200b}\u{2028}\u{2066}x\u{feff}\u{61c}"),
            "evil\u{fffd}gpj.sh\u{fffd}\u{fffd}\u{fffd}x\u{fffd}\u{fffd}"
        );
        assert_eq!(inert("👨\u{200d}👩 خانه\u{200c}ها"), "👨\u{200d}👩 خانه\u{200c}ها");
    }

    #[test]
    fn inert_line_and_a_shown_path_keep_to_one_line() {
        assert_eq!(inert_line("a\tb\nc\x1b\u{202e}é"), "a\u{fffd}b\u{fffd}c\u{fffd}\u{fffd}é");
        assert_eq!(format!("[{:<6}]", Path::new("x\ny").inert()), "[x\u{fffd}y   ]");
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
        assert_eq!(status_colour("ERROR:"), None);
        assert_eq!(status_colour("named by a"), Some(Colour::Yellow));
        assert_eq!(status_colour("+wip"), None);
        assert_eq!(paint("cluster: a", true), "cluster: a");
        assert_eq!(paint("unused", false), "unused");
    }

    #[test]
    fn a_diag_paints_its_prefix_by_level_only_when_asked() {
        assert_eq!(Diag::error("x: y").paint(true), "\x1b[31mERROR:\x1b[0m x: y");
        assert_eq!(Diag::warning("x").paint(true), "\x1b[33mwarning:\x1b[0m x");
        assert_eq!(Diag::error("x: y").paint(false), "ERROR: x: y");
        assert_eq!(Diag::warning("x").to_string(), "warning: x");
    }

    #[test]
    fn a_split_note_keeps_its_level_on_the_first_line_only() {
        let lines = Note::from(Diag::error("a\n  b")).split();
        assert_eq!(lines, [Note::from(Diag::error("a")), Note::from("  b")]);
        assert_eq!(Note::from(Diag::error("")).split(), [Note::from(Diag::error(""))]);
        assert_eq!(Note::from("a\nb\n").split(), [Note::from("a"), Note::from("b")]);
        assert!(Note::from("").split().is_empty());
    }

    #[test]
    fn colour_needs_a_tty_and_no_no_color() {
        assert!(colour(true, None));
        assert!(colour(true, Some(OsStr::new(""))));
        assert!(!colour(true, Some(OsStr::new("1"))));
        assert!(!colour(false, None));
    }
}
