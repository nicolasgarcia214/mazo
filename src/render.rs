//! Text rendering helpers shared by reports: aligned tables, sparklines and
//! local timestamps.

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// Column alignment.
#[derive(Clone, Copy)]
pub enum Align {
    Left,
    Right,
}

/// An aligned plain-text table, one space between columns, the header bold
/// when `tty`. Widths count chars, so sparkline cells are one column each.
/// Trailing spaces are trimmed from every line.
pub fn table(header: &[(&str, Align)], rows: &[Vec<String>], tty: bool) -> String {
    let mut widths: Vec<usize> = header.iter().map(|(h, _)| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let parts: Vec<String> = cells
            .iter()
            .zip(&widths)
            .zip(header)
            .map(|((c, &w), (_, align))| match align {
                Align::Left => format!("{c:<w$}"),
                Align::Right => format!("{c:>w$}"),
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
}
