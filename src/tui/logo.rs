//! The logo: the fanned deck of three cards beside the `mazo` wordmark,
//! baked from the logo image into half blocks so each cell holds two square
//! pixels. The cards keep the image's colours; the wordmark takes the
//! terminal's foreground, so it reads on a dark theme and a light one.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::logo_art::{INK, LOGO};
use crate::theme;

/// What the logo stands for, said beside it.
pub const TAGLINE: &str = "Make every skill earn its place.";

pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// The colour of the pixel `p`: `None` for none, the foreground
/// ([`Color::Reset`]) for the wordmark's ink.
fn colour(p: u32) -> Option<Color> {
    match p {
        0 => None,
        INK => Some(Color::Reset),
        rgb => Some(Color::from_u32(rgb)),
    }
}

/// The full logo, [`Logo::WIDTH`] by [`Logo::HEIGHT`] cells.
pub struct Logo;

impl Logo {
    pub const WIDTH: u16 = LOGO[0].len() as u16;
    pub const HEIGHT: u16 = LOGO.len() as u16 / 2;
}

impl Widget for Logo {
    /// From `area`'s top left, clipped to it; a cell with no pixel is left
    /// as it was. Ink is only ever a foreground: a background of
    /// [`Color::Reset`] would be the terminal's background, not its text.
    fn render(self, area: Rect, buf: &mut Buffer) {
        for (y, pair) in (area.y..).zip(LOGO.chunks(2)) {
            for (x, (&top, &bottom)) in (area.x..).zip(pair[0].iter().zip(&pair[1])) {
                if !area.contains(Position::new(x, y)) {
                    continue;
                }
                let cell = &mut buf[(x, y)];
                match (colour(top), colour(bottom)) {
                    (None, None) => {}
                    (Some(t), None) => _ = cell.set_symbol("▀").set_fg(t),
                    (None, Some(b)) => _ = cell.set_symbol("▄").set_fg(b),
                    (Some(t), Some(b)) if t == b => _ = cell.set_symbol("█").set_fg(t),
                    (Some(t), Some(Color::Reset)) => _ = cell.set_symbol("▄").set_fg(Color::Reset).set_bg(t),
                    (Some(t), Some(b)) => _ = cell.set_symbol("▀").set_fg(t).set_bg(b),
                }
            }
        }
    }
}

/// The version, then the tagline.
pub fn about() -> Line<'static> {
    Line::from(vec![
        Span::raw(VERSION).fg(theme::ACCENT),
        Span::raw(" · ").fg(theme::MUTED),
        Span::raw(TAGLINE).fg(theme::MUTED).italic(),
    ])
}

/// The logo in one line: the deck edge on, back to front, then the name.
pub fn mark() -> Line<'static> {
    Line::from(vec![
        Span::raw("▍").fg(Color::Rgb(3, 113, 146)),
        Span::raw("▍").fg(Color::Rgb(3, 175, 213)),
        Span::raw("█").fg(Color::Rgb(16, 220, 240)),
        Span::raw(" mazo ").bold(),
    ])
}
