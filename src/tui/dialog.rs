use std::sync::mpsc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use super::draw::{scrollbar, status_line};
use super::logo::{self, Logo};
use super::{App, Data};
use crate::audit;
use crate::config::Home;
use crate::durable;
use crate::ledger::name;
use crate::lifecycle::{self, Retired};
use crate::links::{self, Plan};
use crate::pick::Picker;
use crate::render::PathExt;
use crate::store;
use crate::theme;

pub(super) enum Act {
    Retire(String),
    Sync(Plan),
}

pub(super) enum Ask {
    Act(Act),
    Audit(String),
    Update(Option<String>),
    Quit(Option<Box<Dialog>>),
    Source(String),
    Pick(Picker, mpsc::Sender<Vec<String>>),
    Answer(mpsc::Sender<bool>, bool),
    Help(usize),
}

pub(super) struct Dialog {
    pub(super) lines: Vec<String>,
    pub(super) ask: Option<Ask>,
}

fn synced(d: &Data, plan: &Plan) -> crate::Result<Vec<String>> {
    let _lock = durable::lock(&d.dir)?;
    let inv = store::inventory(d.cfg.source.as_deref(), &d.dir)?;
    let homes: Vec<&Home> = d.cfg.homes.iter().collect();
    match links::checked(&d.cfg, &inv, &homes) {
        Ok(now) if now.actions == plan.actions => links::apply(plan),
        Err(errors) => Ok([vec!["sync refused:".into()], errors].concat()),
        Ok(_) => Err("sync refused: the links changed since the plan; press s to plan again".into()),
    }
}

impl App {
    pub(super) fn audit(&self, skill: String) -> Dialog {
        if let Some(busy) = self.one_job() {
            return busy;
        }
        let d = &self.data;
        match audit::plan(&d.cfg, &d.inv, &skill, None) {
            Err(e) => Dialog { lines: vec![format!("ERROR: {e}")], ask: None },
            Ok(p) => {
                let judge = name(p.judge.agent);
                let cap = match p.budget {
                    Some(b) => format!("at most {} sessions and {}", audit::LIMIT, audit::usd(p.judge, b)),
                    None => format!("at most {} sessions; the {judge} judge reports no cost", audit::LIMIT),
                };
                let lines =
                    vec![format!("audit {skill} with the {judge} judge in the background?"), format!("cap: {cap}")];
                Dialog { lines, ask: Some(Ask::Audit(skill)) }
            }
        }
    }

    pub(super) fn one_job(&self) -> Option<Dialog> {
        let job = self.running()?;
        let lines = vec![format!("the {} of {} is running; one job at a time", job.verb, job.target)];
        Some(Dialog { lines, ask: None })
    }

    pub(super) fn update(&self, skill: Option<String>) -> Dialog {
        if let Some(busy) = self.one_job() {
            return busy;
        }
        let d = &self.data;
        let why = skill.as_ref().and_then(|s| match d.inv.installed.get(s) {
            None => Some("not a store skill"),
            Some(l) => (!l.updatable()).then_some("installed from a local path"),
        });
        if let (Some(s), Some(why)) = (&skill, why) {
            return Dialog { lines: vec![format!("{s}: not updatable; {why}")], ask: None };
        }
        let what = skill.as_deref().unwrap_or("every store skill");
        let lines = match &d.cfg.judge {
            None => vec![
                format!("update {what} in the background?"),
                "an untrusted skill is skipped: re-vetting it needs a [judge] in the config".into(),
            ],
            Some(judge) => {
                let judge_name = name(judge.agent);
                let cap = match audit::backend(judge, &d.cfg.homes) {
                    Err(e) => format!("ERROR: {e}"),
                    Ok(b) if b.budget_flag().is_some() => {
                        format!("cap: {} per call", audit::usd(judge, audit::DEFAULT_BUDGET))
                    }
                    Ok(_) => format!("the {judge_name} judge reports no cost"),
                };
                vec![
                    format!("update {what} and ask about each judge verdict?"),
                    format!("an untrusted skill that changed is re-vetted with one {judge_name} judge call; {cap}"),
                    "a security finding still refuses its update".into(),
                ]
            }
        };
        Dialog { lines, ask: Some(Ask::Update(skill)) }
    }

    pub(super) fn retire(&mut self, skill: String) {
        if let Some(busy) = self.one_job() {
            self.dialog = Some(busy);
            return;
        }
        let what = match self.data.cfg.source.as_deref() {
            Some(source) if self.data.inv.root(&skill) == Some(source) => {
                format!("move it to {}", source.join(".archive").join(&skill).inert())
            }
            _ => "remove it from the store".into(),
        };
        let lines = vec![format!("retire {skill}: {what} and prune its links?")];
        self.dialog = Some(Dialog { lines, ask: Some(Ask::Act(Act::Retire(skill))) });
    }

    pub(super) fn act(&mut self, act: Act) -> Vec<String> {
        self.clear = true;
        let d = &self.data;
        let mut out = Vec::new();
        let ran = match act {
            Act::Retire(skill) => {
                lifecycle::retire(&d.cfg, &d.dir, &skill, false, false, &mut out).map(|retired| match retired {
                    Retired::Blocked(lines) | Retired::Refused(lines) => {
                        [vec![format!("retire {skill} refused:")], lines].concat()
                    }
                    Retired::Done(lines) => lines,
                })
            }
            Act::Sync(plan) => synced(d, &plan),
        };
        out.extend(ran.unwrap_or_else(|e| vec![format!("ERROR: {e}")]));
        if let Err(e) = self.reload() {
            out.push(format!("ERROR: {e}"));
        }
        out
    }
}

impl Dialog {
    fn frame(&self) -> (&'static str, Color) {
        match &self.ask {
            Some(Ask::Act(Act::Retire(_))) => ("retire", theme::BAD),
            Some(Ask::Act(Act::Sync(_))) => ("sync", theme::ACCENT),
            Some(Ask::Audit(_)) => ("audit", theme::ACCENT),
            Some(Ask::Update(_)) => ("update", theme::ACCENT),
            Some(Ask::Quit(_)) => ("quit", theme::BAD),
            Some(Ask::Source(_) | Ask::Pick(..)) => ("add", theme::ACCENT),
            Some(Ask::Answer(..)) => ("confirm", theme::WARN),
            Some(Ask::Help(_)) => ("help", theme::ACCENT),
            None if self.lines.iter().any(|l| l.contains("ERROR:") || l.contains(" refused")) => ("error", theme::BAD),
            None => ("mazo", theme::ACCENT),
        }
    }
}

pub(super) fn draw_dialog(frame: &mut Frame, dialog: &Dialog, area: Rect) {
    let (title, colour) = dialog.frame();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(colour))
        .title(Span::raw(format!(" {title} ")).fg(colour).bold())
        .padding(Padding::new(2, 2, 1, 1));
    if let Some(Ask::Pick(picker, _)) = &dialog.ask {
        let modal = area.inner(Margin::new(2, 1));
        frame.render_widget(Clear, modal);
        picker.draw(frame, block.inner(modal));
        frame.render_widget(block, modal);
        return;
    }
    let raw: Vec<&str> = dialog.lines.iter().flat_map(|l| l.lines()).collect();
    let mut wide = raw.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    if let Some(Ask::Source(source)) = &dialog.ask {
        wide = wide.max(source.chars().count() + 3);
    }
    let width = (wide as u16 + 6).clamp(area.width.min(48), area.width);
    let mut lines: Vec<Line> = Vec::new();
    for (i, line) in raw.iter().enumerate() {
        for piece in wrap(line, width.saturating_sub(6).into()) {
            let piece = status_line(&piece);
            lines.push(if i == 0 && dialog.ask.is_some() { piece.bold() } else { piece });
        }
    }
    if let Some(Ask::Source(source)) = &dialog.ask {
        let typed = typed_line(source, width.saturating_sub(6).into());
        let typed = typed.strip_suffix('_').unwrap_or(&typed);
        let (prompt, typed) = typed.strip_prefix("> ").map_or(("", typed), |t| ("> ", t));
        lines.push(Line::from(vec![
            Span::raw(prompt).fg(theme::ACCENT).bold(),
            Span::raw(typed.to_string()),
            Span::raw("_").fg(theme::ACCENT).slow_blink(),
        ]));
    }
    let room = usize::from(area.height.saturating_sub(4)).max(1);
    if lines.len() > room {
        let more = lines.len() - room + 1;
        lines.truncate(room - 1);
        lines.push(Line::raw(format!("… and {more} more")).fg(theme::MUTED));
    }
    let modal = area.centered(Constraint::Length(width), Constraint::Length(lines.len() as u16 + 4));
    frame.render_widget(Clear, modal);
    frame.render_widget(Paragraph::new(lines).block(block), modal);
}

pub(super) fn draw_help(frame: &mut Frame, lines: &[String], scroll: &mut usize, area: Rect) -> usize {
    let modal = area.inner(Margin::new(2, 1));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::ACCENT))
        .title(Span::raw(" help ").fg(theme::ACCENT).bold())
        .padding(Padding::new(2, 2, 1, 1));
    frame.render_widget(Clear, modal);
    let mut inner = block.inner(modal);
    frame.render_widget(block, modal);
    if inner.height >= 4 * (Logo::HEIGHT + 2) && inner.width >= Logo::WIDTH {
        let [art, at, _, rest] = Layout::vertical([
            Constraint::Length(Logo::HEIGHT),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(inner);
        frame.render_widget(Logo, art.centered_horizontally(Constraint::Length(Logo::WIDTH)));
        frame.render_widget(logo::about().centered(), at);
        inner = rest;
    }
    let mut text: Vec<Line> = Vec::new();
    for line in lines {
        let indent = line.len() - line.trim_start().len();
        let term = line[indent..].find("  ").map(|at| indent + at);
        for (i, piece) in wrap(line, inner.width.saturating_sub(1).into()).into_iter().enumerate() {
            text.push(match term {
                Some(at) if i == 0 && at < piece.len() => Line::from(vec![
                    Span::raw(piece[..at].to_string()).fg(theme::ACCENT),
                    Span::raw(piece[at..].to_string()),
                ]),
                Some(_) => Line::raw(piece),
                None if indent == 0 => Line::raw(piece).fg(theme::ACCENT).bold(),
                None if indent == 2 => Line::raw(piece).bold(),
                None => Line::raw(piece),
            });
        }
    }
    let page = usize::from(inner.height).max(1);
    *scroll = (*scroll).min(text.len().saturating_sub(page));
    let total = text.len();
    frame.render_widget(Paragraph::new(text.into_iter().skip(*scroll).collect::<Vec<_>>()), inner);
    scrollbar(frame, modal.inner(Margin::new(0, 1)), total, *scroll, page);
    page
}

fn wrap(line: &str, width: usize) -> Vec<String> {
    let body = line.trim_start();
    let indent = line.len() - body.len();
    let mut out = vec![" ".repeat(indent)];
    for (i, word) in body.split(' ').enumerate() {
        let cur = out.last_mut().unwrap();
        let fits = cur.chars().count() + 1 + word.chars().count() <= width;
        if i == 0 || fits || word.is_empty() {
            if i > 0 {
                cur.push(' ');
            }
            cur.push_str(word);
        } else {
            out.push(format!("{}{word}", " ".repeat(indent + 2)));
        }
    }
    out
}

pub(super) fn source_dialog(source: String) -> Dialog {
    let lines = vec!["add a skill from a local path, a git URL or owner/repo[/path]:".into()];
    Dialog { lines, ask: Some(Ask::Source(source)) }
}

fn typed_line(source: &str, width: usize) -> String {
    let line = format!("> {source}_");
    let n = line.chars().count();
    if n <= width {
        return line;
    }
    format!("…{}", line.chars().skip(n + 1 - width.max(1)).collect::<String>())
}
