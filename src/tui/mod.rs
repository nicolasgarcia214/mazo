mod card;
mod dialog;
mod draw;
mod job;
mod keys;
mod logo;
mod logo_art;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
use ratatui::crossterm::execute;

use crate::audit::{self, SkillRecord};
use crate::config::Config;
use crate::ctx::{Contexts, Tokens};
use crate::inventory::Inventory;
use crate::ledger::{self, Agent, Channel, Coverage, Ledger};
use crate::report::{self, Bucket, Input, SHOW_LIMIT, Since, Unused, Usage, UsageArgs};
use crate::store;

use card::card_height;
use dialog::Dialog;
use job::Job;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Usage,
    Unused,
    Card,
}

#[derive(Clone)]
struct Data {
    cfg: Config,
    inv: Inventory,
    events: Vec<ledger::Event>,
    coverage: BTreeMap<String, Coverage>,
    audits: BTreeMap<String, SkillRecord>,
    now: jiff::Timestamp,
    tz: jiff::tz::TimeZone,
    dir: PathBuf,
}

impl Data {
    fn input(&self) -> Input<'_> {
        Input {
            cfg: &self.cfg,
            inv: &self.inv,
            events: &self.events,
            coverage: &self.coverage,
            audits: &self.audits,
            now: self.now,
            tz: self.tz.clone(),
        }
    }

    fn reload(&mut self) -> crate::Result<()> {
        self.inv = store::inventory(self.cfg.source.as_deref(), &self.dir)?;
        self.events = Ledger::load(&self.dir)?.events().to_vec();
        self.coverage = ledger::load_coverage(&self.dir)?;
        self.audits = audit::latest(&self.dir)?;
        self.now = jiff::Timestamp::now();
        Ok(())
    }
}

pub struct App {
    data: Data,
    screen: Screen,
    back: Screen,
    usage_since: Since,
    unused_since: Since,
    card_since: Since,
    agent: Option<Agent>,
    channel: Option<Channel>,
    authoring: bool,
    filter: String,
    typing: bool,
    selected: usize,
    page: usize,
    usage: Usage,
    unused: Unused,
    tokens: Tokens,
    card: Option<Shown>,
    side: Option<Shown>,
    scroll: usize,
    dialog: Option<Dialog>,
    job: Option<Job>,
    clear: bool,
}

type Shown = (String, Vec<String>, Vec<Bucket>);

impl App {
    pub fn new(input: &Input, data: &Path) -> App {
        let (usage_since, unused_since) = (report::SINCE, report::UNUSED_SINCE);
        App {
            data: Data {
                cfg: input.cfg.clone(),
                inv: input.inv.clone(),
                events: input.events.to_vec(),
                coverage: input.coverage.clone(),
                audits: input.audits.clone(),
                now: input.now,
                tz: input.tz.clone(),
                dir: data.to_path_buf(),
            },
            screen: Screen::Usage,
            back: Screen::Usage,
            usage_since,
            unused_since,
            card_since: usage_since,
            agent: None,
            channel: None,
            authoring: false,
            filter: String::new(),
            typing: false,
            selected: 0,
            page: 1,
            usage: report::usage_table(input, &UsageArgs::new(usage_since)),
            unused: report::unused_list(input, unused_since),
            tokens: Contexts::new(input.cfg, input.inv).tokens(input.inv),
            card: None,
            side: None,
            scroll: 0,
            dialog: None,
            job: None,
            clear: false,
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn busy(&self) -> bool {
        self.running().is_some()
    }

    fn running(&self) -> Option<&Job> {
        self.job.as_ref().filter(|j| j.thread.is_some())
    }

    fn args(&self, since: Since) -> UsageArgs {
        UsageArgs { agent: self.agent, channel: self.channel, authoring: self.authoring, ..UsageArgs::new(since) }
    }

    fn matches(&self, skill: &str) -> bool {
        skill.contains(&self.filter)
    }

    fn list(&self) -> Screen {
        if self.screen == Screen::Card { self.back } else { self.screen }
    }

    fn listed(&self) -> Vec<&str> {
        let skills: Vec<&str> = match self.list() {
            Screen::Unused => self.unused.skills.iter().map(|r| r.skill.as_str()).collect(),
            _ => self.usage.rows.iter().map(|r| r.skill.as_str()).collect(),
        };
        skills.into_iter().filter(|s| self.matches(s)).collect()
    }

    fn shown(&self, skill: &str, since: Since) -> Shown {
        let input = self.data.input();
        let card = report::skill_card(&input, skill, &self.args(since), SHOW_LIMIT);
        let lines = report::card_text(&input, &card, true).lines().map(String::from).collect();
        (skill.to_string(), lines, card.activity)
    }

    fn open(&mut self, skill: &str) {
        self.card = Some(self.shown(skill, self.card_since));
    }

    fn shift(&mut self, by: isize) {
        let len = match &self.card {
            Some((_, lines, _)) if self.screen == Screen::Card => card_height(lines),
            _ => self.listed().len(),
        };
        let pos = if self.screen == Screen::Card { &mut self.scroll } else { &mut self.selected };
        *pos = pos.saturating_add_signed(by).min(len.saturating_sub(1));
    }

    fn reload(&mut self) -> crate::Result<()> {
        self.data.reload()?;
        self.tokens = Contexts::new(&self.data.cfg, &self.data.inv).tokens(&self.data.inv);
        if self.card.as_ref().is_some_and(|(skill, ..)| !self.data.inv.skills.contains(skill)) {
            self.card = None;
            if self.screen == Screen::Card {
                self.screen = self.back;
            }
        }
        self.unused = report::unused_list(&self.data.input(), self.unused_since);
        self.refresh();
        self.clamp();
        Ok(())
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.listed().len().saturating_sub(1));
    }

    fn refresh(&mut self) {
        self.side = None;
        self.usage = report::usage_table(&self.data.input(), &self.args(self.usage_since));
        if self.list() == Screen::Usage {
            self.clamp();
        }
        if let Some((skill, ..)) = self.card.take() {
            self.open(&skill);
            self.shift(0);
        }
    }
}

pub fn run(input: &Input, data: &Path) -> std::io::Result<()> {
    let mut app = App::new(input, data);
    let mut terminal = ratatui::init();
    _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let (restore, ui) = (std::panic::take_hook(), std::thread::current().id());
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == ui {
            _ = execute!(std::io::stdout(), DisableBracketedPaste);
            restore(info);
        }
    }));
    let result = (|| {
        loop {
            app.tick();
            if std::mem::take(&mut app.clear) {
                terminal.clear()?;
            }
            terminal.draw(|frame| app.draw(frame))?;
            if app.busy() && !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if !app.key(key) => return Ok(()),
                Event::Paste(text) => app.paste(&text),
                _ => {}
            }
        }
    })();
    _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}
