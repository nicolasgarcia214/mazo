use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};

pub const ACCENT: Color = Color::Cyan;
pub const MUTED: Color = Color::DarkGray;
pub const GOOD: Color = Color::Green;
pub const WARN: Color = Color::Yellow;
pub const BAD: Color = Color::Red;

pub fn selected() -> Style {
    Style::new().bg(Color::Indexed(236)).bold()
}

pub fn gutter(selected: bool) -> Span<'static> {
    if selected { Span::raw("▌ ").fg(ACCENT) } else { Span::raw("  ") }
}

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
