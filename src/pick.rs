//! The multi-select `mazo add` opens at a terminal on a source holding
//! several skills: each skill's name and description, and the ones to install.

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

/// The keys, as the footer lists them.
const KEYS: &str = "↑↓/jk move · Space select · a all · Enter install · Esc/q cancel";

/// The skills on offer, the cursor and which are selected; no rendering state.
#[derive(Debug)]
pub struct Picker {
    /// Each skill's name and description, in list order.
    items: Vec<(String, String)>,
    cursor: usize,
    selected: Vec<bool>,
}

/// Where a key leaves the picker.
#[derive(Debug, PartialEq)]
pub enum Step {
    Open,
    /// The selected names in list order; none when cancelled.
    Done(Vec<String>),
}

impl Picker {
    pub fn new(items: Vec<(String, String)>) -> Picker {
        let selected = vec![false; items.len()];
        Picker { items, cursor: 0, selected }
    }

    /// Applies `key`: ↑↓ or jk move, Space toggles the skill under the
    /// cursor, `a` selects every skill or, when all are, none; Enter confirms;
    /// Esc, `q` and Ctrl-C cancel.
    pub fn key(&mut self, key: KeyEvent) -> Step {
        if key.kind != KeyEventKind::Press {
            return Step::Open;
        }
        let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        match key.code {
            _ if ctrl_c => return Step::Done(Vec::new()),
            KeyCode::Esc | KeyCode::Char('q') => return Step::Done(Vec::new()),
            KeyCode::Enter => return Step::Done(self.names()),
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.items.len().saturating_sub(1))
            }
            KeyCode::Char(' ') => {
                if let Some(s) = self.selected.get_mut(self.cursor) {
                    *s = !*s;
                }
            }
            KeyCode::Char('a') => {
                let all = self.selected.iter().all(|s| *s);
                self.selected.fill(!all);
            }
            _ => {}
        }
        Step::Open
    }

    fn names(&self) -> Vec<String> {
        self.items.iter().zip(&self.selected).filter(|(_, s)| **s).map(|((n, _), _)| n.clone()).collect()
    }

    fn draw(&self, frame: &mut Frame) {
        let [body, footer] = Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(frame.area());
        let count = self.selected.iter().filter(|s| **s).count();
        let status = format!("install from this source: {count} of {} selected", self.items.len());
        frame.render_widget(Paragraph::new(vec![Line::raw(status), Line::raw(KEYS).dim()]), footer);
        let width = self.items.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0);
        // The rows from the one that keeps the cursor in view.
        let first = self.cursor.saturating_sub(usize::from(body.height).saturating_sub(1));
        let reversed = Style::new().add_modifier(Modifier::REVERSED);
        let lines: Vec<Line> = self
            .items
            .iter()
            .zip(&self.selected)
            .enumerate()
            .skip(first)
            .map(|(i, ((name, description), s))| {
                let line = Line::raw(format!("[{}] {name:<width$}  {description}", if *s { 'x' } else { ' ' }));
                if i == self.cursor { line.patch_style(reversed) } else { line }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), body);
    }
}

/// Runs a [`Picker`] over `items` until it is confirmed or cancelled: the
/// selected names. The terminal is restored on every return, and on a panic
/// by the hook `ratatui::try_init` installs.
pub fn pick(items: Vec<(String, String)>) -> std::io::Result<Vec<String>> {
    let mut picker = Picker::new(items);
    let result = ratatui::try_init().and_then(|mut terminal| {
        loop {
            terminal.draw(|frame| picker.draw(frame))?;
            if let Event::Key(key) = event::read()?
                && let Step::Done(names) = picker.key(key)
            {
                return Ok(names);
            }
        }
    });
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use super::{Picker, Step};

    fn picker() -> Picker {
        Picker::new(["one", "two", "three"].iter().map(|n| (n.to_string(), format!("The {n} skill."))).collect())
    }

    /// Applies each key in turn: the last step.
    fn keys(p: &mut Picker, codes: &[KeyCode]) -> Step {
        codes.iter().map(|c| p.key(KeyEvent::new(*c, KeyModifiers::NONE))).last().unwrap()
    }

    fn done(names: &[&str]) -> Step {
        Step::Done(names.iter().map(|n| n.to_string()).collect())
    }

    #[test]
    fn space_toggles_the_skill_under_the_cursor_and_enter_confirms_in_list_order() {
        let mut p = picker();
        assert_eq!(keys(&mut p, &[KeyCode::Down, KeyCode::Down, KeyCode::Char(' ')]), Step::Open);
        let step = keys(&mut p, &[KeyCode::Up, KeyCode::Up, KeyCode::Char(' '), KeyCode::Enter]);
        assert_eq!(step, done(&["one", "three"]));
        let mut p = picker();
        assert_eq!(keys(&mut p, &[KeyCode::Char(' '), KeyCode::Char(' '), KeyCode::Enter]), done(&[]));
    }

    #[test]
    fn moves_stop_at_either_end() {
        let mut p = picker();
        keys(&mut p, &[KeyCode::Up, KeyCode::Char('k')]);
        assert_eq!(p.cursor, 0);
        keys(&mut p, &[KeyCode::Char('j'), KeyCode::Down, KeyCode::Down, KeyCode::Down]);
        assert_eq!(p.cursor, 2);
        assert_eq!(Picker::new(Vec::new()).key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)), Step::Open);
    }

    #[test]
    fn a_selects_every_skill_and_again_none() {
        let mut p = picker();
        keys(&mut p, &[KeyCode::Char(' '), KeyCode::Char('a')]);
        assert_eq!(p.names(), ["one", "two", "three"]);
        keys(&mut p, &[KeyCode::Char('a')]);
        assert_eq!(p.names(), Vec::<String>::new());
    }

    #[test]
    fn esc_q_and_ctrl_c_cancel_with_nothing_selected() {
        for key in [
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let mut p = picker();
            keys(&mut p, &[KeyCode::Char('a')]);
            assert_eq!(p.key(key), done(&[]), "{key:?}");
        }
    }

    #[test]
    fn a_key_release_changes_nothing() {
        let mut p = picker();
        let release = KeyEvent { kind: KeyEventKind::Release, ..KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE) };
        assert_eq!(p.key(release), Step::Open);
    }
}
