//! The palette the TUI and the picker draw with, and the key-hint lines they
//! share. Named ANSI colours, so the terminal's own theme picks the shades;
//! only the selection's background is a fixed grey.

use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};

pub const ACCENT: Color = Color::Cyan;
/// Text that carries little: zero counts, empty days, labels.
pub const MUTED: Color = Color::DarkGray;
pub const GOOD: Color = Color::Green;
pub const WARN: Color = Color::Yellow;
pub const BAD: Color = Color::Red;

/// The selected row: bold on a dark grey of the 256-colour palette.
pub fn selected() -> Style {
    Style::new().bg(Color::Indexed(236)).bold()
}

/// The two columns in front of a list row: an accent bar on the selected one.
pub fn gutter(selected: bool) -> Span<'static> {
    if selected { Span::raw("▌ ").fg(ACCENT) } else { Span::raw("  ") }
}

/// `text`, `key label` hints joined with ` · `, each key bold in the accent.
pub fn hints(text: &str) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, hint) in text.split(" · ").enumerate() {
        if i > 0 {
            spans.push(Span::raw(" · ").fg(MUTED));
        }
        let (key, label) = hint.split_once(' ').unwrap_or((hint, ""));
        spans.push(Span::raw(key.to_string()).fg(ACCENT).bold());
        if !label.is_empty() {
            spans.push(Span::raw(format!(" {label}")));
        }
    }
    Line::from(spans)
}
