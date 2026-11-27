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

use crate::Result;
use crate::audit::{self, SkillRecord};
use crate::config::Config;
use crate::ctx::{Contexts, Tokens};
use crate::inventory::{self, Graph, Inventory};
use crate::ledger::{self, Agent, Channel, Coverage, Ledger};
use crate::report::{self, Bucket, Input, SHOW_LIMIT, Since, Unused, Usage, UsageArgs};
use crate::store;

use card::card_height;
use dialog::Dialog;
use job::Job;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Usage,
    Unused,
    Card,
}

#[derive(Clone)]
pub struct Data {
    pub cfg: Config,
    pub inv: Inventory,
    pub events: Vec<ledger::Event>,
    pub coverage: BTreeMap<String, Coverage>,
    pub audits: BTreeMap<String, SkillRecord>,
    pub now: jiff::Timestamp,
    pub tz: jiff::tz::TimeZone,
    pub dir: PathBuf,
}

impl Data {
    pub fn load(cfg: Config, inv: Inventory, events: Vec<ledger::Event>, dir: &Path) -> Result<Data> {
        let mut data = Data {
            cfg,
            inv,
            events,
            coverage: BTreeMap::new(),
            audits: BTreeMap::new(),
            now: jiff::Timestamp::now(),
            tz: jiff::tz::TimeZone::system(),
            dir: dir.to_path_buf(),
        };
        data.refresh()?;
        Ok(data)
    }

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

    fn reload(&mut self) -> Result<()> {
        self.inv = store::inventory(self.cfg.source.as_deref(), &self.dir)?;
        self.events = Ledger::load(&self.dir)?.into_events();
        self.refresh()
    }

    fn refresh(&mut self) -> Result<()> {
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
    graph: Graph,
    card: Option<Shown>,
    side: Option<Shown>,
    scroll: usize,
    dialog: Option<Dialog>,
    job: Option<Job>,
    clear: bool,
}

type Shown = (String, Vec<String>, Vec<Bucket>);

impl App {
    pub fn new(data: Data) -> App {
        let (usage_since, unused_since) = (report::SINCE, report::UNUSED_SINCE);
        let input = data.input();
        let (usage, unused) =
            (report::usage_table(&input, &UsageArgs::new(usage_since)), report::unused_list(&input, unused_since));
        let tokens = Contexts::new(&data.cfg, &data.inv).tokens(&data.inv);
        let graph = inventory::citations(&data.inv).held;
        App {
            data,
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
            usage,
            unused,
            tokens,
            graph,
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
        let card = report::skill_card(&input, &self.graph, skill, &self.args(since), SHOW_LIMIT);
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

    fn reload(&mut self) -> Result<()> {
        self.data.reload()?;
        self.tokens = Contexts::new(&self.data.cfg, &self.data.inv).tokens(&self.data.inv);
        self.graph = inventory::citations(&self.data.inv).held;
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

pub fn run(data: Data) -> std::io::Result<()> {
    let mut app = App::new(data);
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
