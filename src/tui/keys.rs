//! The keys the TUI takes: on a list, on a card, in the filter and in each
//! dialog, and a paste.

use clap::ValueEnum;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::dialog::{Act, Ask, Dialog, source_dialog};
use super::{App, Screen};
use crate::config::Home;
use crate::help;
use crate::ledger::{Agent, Channel};
use crate::links;
use crate::pick::Step;
use crate::report::{self, Since};

/// The windows `w` cycles through.
const WINDOWS: [Since; 3] = [report::SINCE, report::UNUSED_SINCE, Since::All];

/// The next of `all` after `cur`, `None` (no filter) after the last.
fn cycle<T: Copy + PartialEq>(all: &[T], cur: Option<T>) -> Option<T> {
    match cur {
        None => all.first().copied(),
        Some(c) => all.iter().skip_while(|x| **x != c).nth(1).copied(),
    }
}

fn next_window(since: Since) -> Since {
    cycle(&WINDOWS, Some(since)).unwrap_or(WINDOWS[0])
}

impl App {
    /// Applies `key`; false when it quits.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return self.quit();
        }
        if let Some(Dialog { lines, ask }) = self.dialog.take() {
            let y = key.code == KeyCode::Char('y') && key.modifiers.is_empty();
            match ask {
                Some(Ask::Source(mut source)) => match key.code {
                    KeyCode::Enter => self.add(source),
                    KeyCode::Esc => {}
                    code => {
                        match code {
                            KeyCode::Char(c) if (key.modifiers - KeyModifiers::SHIFT).is_empty() => source.push(c),
                            KeyCode::Backspace => _ = source.pop(),
                            _ => {}
                        }
                        self.dialog = Some(source_dialog(source));
                    }
                },
                Some(Ask::Pick(mut picker, reply)) => match picker.key(key) {
                    Step::Open => self.dialog = Some(Dialog { lines, ask: Some(Ask::Pick(picker, reply)) }),
                    Step::Done(names) => _ = reply.send(names),
                },
                Some(Ask::Answer(reply, default)) => match key.code {
                    // A Ctrl or Alt chord answers nothing.
                    _ if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                        self.dialog = Some(Dialog { lines, ask: Some(Ask::Answer(reply, default)) })
                    }
                    KeyCode::Enter => _ = reply.send(default),
                    KeyCode::Char('y' | 'Y') => _ = reply.send(true),
                    KeyCode::Char('n' | 'N') | KeyCode::Esc => _ = reply.send(false),
                    // The unanswered question is No.
                    KeyCode::Char('q') => return self.quit(),
                    _ => self.dialog = Some(Dialog { lines, ask: Some(Ask::Answer(reply, default)) }),
                },
                Some(Ask::Help(scroll)) => {
                    let scroll = match key.code {
                        KeyCode::Esc | KeyCode::Char('H') => return true,
                        KeyCode::Down | KeyCode::Char('j') => scroll + 1,
                        KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
                        KeyCode::PageDown => scroll + self.page,
                        KeyCode::PageUp => scroll.saturating_sub(self.page),
                        _ => scroll,
                    };
                    self.dialog = Some(Dialog { lines, ask: Some(Ask::Help(scroll)) });
                }
                Some(Ask::Act(act)) if y => {
                    let lines = self.act(act);
                    self.dialog = Some(Dialog { lines, ask: None });
                }
                Some(Ask::Audit(skill)) if y => self.run_audit(skill),
                Some(Ask::Update(skill)) if y => self.run_update(skill),
                Some(Ask::Quit) if y => {
                    self.stop();
                    return false;
                }
                _ => {}
            }
            return true;
        }
        if self.typing {
            match key.code {
                KeyCode::Char(c) if (key.modifiers - KeyModifiers::SHIFT).is_empty() => self.filter.push(c),
                KeyCode::Backspace => _ = self.filter.pop(),
                KeyCode::Enter => self.typing = false,
                KeyCode::Esc => (self.typing, self.filter) = (false, String::new()),
                _ => {}
            }
            self.selected = 0;
            return true;
        }
        let list = self.screen != Screen::Card;
        match key.code {
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Up | KeyCode::Char('k') => self.shift(-1),
            KeyCode::Down | KeyCode::Char('j') => self.shift(1),
            KeyCode::Left | KeyCode::Char('h') => self.shift(-(self.page as isize)),
            KeyCode::Right | KeyCode::Char('l') => self.shift(self.page as isize),
            // An unused store skill is past its install-date window: retire first.
            KeyCode::Enter
                if self.screen == Screen::Unused
                    && let Some(skill) = self.selected_unused_store() =>
            {
                self.retire(skill);
            }
            KeyCode::Enter if list => {
                if let Some(skill) = self.listed().get(self.selected).map(|s| s.to_string()) {
                    (self.back, self.screen, self.card_since, self.scroll) =
                        (self.screen, Screen::Card, self.usage_since, 0);
                    self.open(&skill);
                }
            }
            KeyCode::Esc if !list => self.screen = self.back,
            KeyCode::Esc if !self.busy() => self.job = None,
            KeyCode::Tab if list => {
                self.screen = if self.screen == Screen::Usage { Screen::Unused } else { Screen::Usage };
                self.selected = 0;
            }
            KeyCode::Char('/') if list => (self.typing, self.filter, self.selected) = (true, String::new(), 0),
            KeyCode::Char('w') => match self.screen {
                Screen::Usage => {
                    self.usage_since = next_window(self.usage_since);
                    self.refresh();
                }
                Screen::Unused => {
                    self.unused_since = next_window(self.unused_since);
                    self.unused = report::unused_list(&self.data.input(), self.unused_since);
                    self.clamp();
                }
                Screen::Card => {
                    self.card_since = next_window(self.card_since);
                    self.refresh();
                }
            },
            KeyCode::Char(c @ ('a' | 'c' | 'A')) if self.screen != Screen::Unused => {
                match c {
                    'a' => self.agent = cycle(Agent::value_variants(), self.agent),
                    'c' => self.channel = cycle(Channel::value_variants(), self.channel),
                    _ => self.authoring = !self.authoring,
                }
                self.refresh();
            }
            // Its plan is of the inventory a running job may change.
            KeyCode::Char('s') if self.running().is_some() => self.dialog = self.one_job(),
            KeyCode::Char('r') => {
                if let Some(skill) = self.chosen() {
                    self.retire(skill);
                }
            }
            KeyCode::Char('s') => {
                let homes: Vec<&Home> = self.data.cfg.homes.iter().collect();
                self.dialog = Some(match links::checked(&self.data.cfg, &self.data.inv, &homes) {
                    Err(errors) => Dialog { lines: [vec!["sync refused:".into()], errors].concat(), ask: None },
                    Ok(plan) if plan.actions.is_empty() => {
                        Dialog { lines: vec!["in sync: nothing to apply".into()], ask: None }
                    }
                    Ok(plan) => Dialog { lines: links::planned(&plan), ask: Some(Ask::Act(Act::Sync(plan))) },
                });
            }
            KeyCode::Char('u') => {
                if let Some(skill) = self.chosen() {
                    self.dialog = Some(self.update(Some(skill)));
                }
            }
            KeyCode::Char('U') => self.dialog = Some(self.update(None)),
            KeyCode::Char('i') => self.dialog = Some(self.one_job().unwrap_or_else(|| source_dialog(String::new()))),
            KeyCode::Char('x') => {
                if let Some(skill) = self.chosen() {
                    self.dialog = Some(self.audit(skill));
                }
            }
            KeyCode::Char('H') => {
                let lines = help::lines(self.list() == Screen::Unused);
                self.dialog = Some(Dialog { lines, ask: Some(Ask::Help(0)) });
            }
            _ => {}
        }
        true
    }

    /// The card's skill, or the selected one.
    fn chosen(&self) -> Option<String> {
        match &self.card {
            Some((skill, ..)) if self.screen == Screen::Card => Some(skill.clone()),
            _ => self.listed().get(self.selected).map(|s| s.to_string()),
        }
    }

    /// False, quitting, unless a job is running: then it asks first.
    fn quit(&mut self) -> bool {
        let Some(job) = self.running() else { return false };
        let lines = vec![format!("the {} of {} is running; stop its judge and quit?", job.verb, job.target)];
        self.dialog = Some(Dialog { lines, ask: Some(Ask::Quit) });
        true
    }

    /// Inserts pasted `text`, its line breaks dropped, into an open source
    /// dialog; anywhere else a paste does nothing.
    pub fn paste(&mut self, text: &str) {
        if let Some(Dialog { ask: Some(Ask::Source(source)), .. }) = &mut self.dialog {
            source.extend(text.chars().filter(|c| !matches!(c, '\r' | '\n')));
        }
    }

    /// The selected unused-list skill when it is `unused` and read from the store.
    fn selected_unused_store(&self) -> Option<String> {
        let row = self.unused.skills.iter().filter(|r| self.matches(&r.skill)).nth(self.selected)?;
        let store = self.data.inv.root(&row.skill) != self.data.inv.source.as_deref();
        (row.status == "unused" && store).then(|| row.skill.clone())
    }
}
