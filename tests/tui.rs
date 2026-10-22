//! The TUI of bare `mazo`: its views rendered into ratatui's `TestBackend`
//! from the golden fixture, its retire, sync and audit actions driven by key
//! events against a temp config and a stub judge, and the binary opening and
//! quitting it on a pseudo-terminal.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;

use common::golden::Golden;
use mazo::inventory::Inventory;
use mazo::ledger::{self, Agent, Channel, Event, Ledger};
use mazo::render::{self, Colour};
use mazo::report::{self, Input, Since, UsageArgs};
use mazo::tui::{App, Screen};
use mazo::{config, links, store};

/// The app over the golden fixture, whose views read no data directory.
fn app(input: &Input) -> App {
    App::new(input, Path::new("/nonexistent"))
}

/// The screen of `app` at `width`×`height`: each row's text and the
/// foreground colour of each of its cells.
fn draw(app: &mut App, width: u16, height: u16) -> Vec<(String, Vec<Color>)> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buf = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let cells: Vec<_> = (0..width).map(|x| &buf[(x, y)]).collect();
            (
                cells.iter().map(|c| c.symbol()).collect::<String>().trim_end().to_string(),
                cells.iter().map(|c| c.fg).collect(),
            )
        })
        .collect()
}

fn text(app: &mut App, width: u16, height: u16) -> Vec<String> {
    draw(app, width, height).into_iter().map(|(t, _)| t).collect()
}

fn press(app: &mut App, code: KeyCode) -> bool {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        press(app, KeyCode::Char(c));
    }
}

/// The table rows on `screen`: the lines after the header, up to the blank
/// line or the coverage note.
fn rows(screen: &[String]) -> Vec<String> {
    screen[1..]
        .iter()
        .take_while(|l| !l.is_empty() && !l.starts_with("coverage:") && !l.starts_with("window:"))
        .cloned()
        .collect()
}

/// The words of `skill`'s row on `screen`.
fn row(screen: &[String], skill: &str) -> Vec<String> {
    let line = rows(screen).into_iter().find(|l| l.split_whitespace().next() == Some(skill));
    line.unwrap_or_else(|| panic!("no {skill} row in {screen:#?}")).split_whitespace().map(String::from).collect()
}

/// The usage table's rows under `args`, as the text output words them.
fn expected(input: &Input, args: &UsageArgs) -> Vec<Vec<String>> {
    report::usage_table(input, args)
        .rows
        .iter()
        .map(|r| report::usage_cells(input, r, false).join(" ").split_whitespace().map(String::from).collect())
        .collect()
}

fn ratatui_colour(token: &str) -> Color {
    match render::status_colour(token) {
        Some(Colour::Red) => Color::Red,
        Some(Colour::Yellow) => Color::Yellow,
        None => Color::Reset,
    }
}

/// The foreground of `token`'s first cell in the line of `screen` that contains `line`.
fn colour_of(screen: &[(String, Vec<Color>)], line: &str, token: &str) -> Color {
    let (text, colours) = screen.iter().find(|(t, _)| t.contains(line)).unwrap();
    colours[text[..text.find(token).unwrap()].chars().count()]
}

#[test]
fn usage_row_shows_its_sessions_and_its_grade_mark_in_the_status_colour() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let mut app = app(&input);
    let screen = draw(&mut app, 200, 40);
    let lines: Vec<String> = screen.iter().map(|(t, _)| t.clone()).collect();
    let unslop =
        report::usage_table(&input, &UsageArgs::new(Since::Days(30))).rows.into_iter().find(|r| r.skill == "unslop");
    let sess = unslop.unwrap().sess;
    assert!(sess > 0);
    assert_eq!(row(&lines, "unslop")[1], sess.to_string());
    let words = row(&lines, "unslop");
    let quality = words.iter().position(|w| w == "audit:").unwrap();
    assert!(words[quality - 1].ends_with("+1"), "{lines:#?}");
    assert_eq!(colour_of(&screen, "unslop", "+1"), ratatui_colour("+1"));
    assert_eq!(colour_of(&screen, "unslop", "unslop"), Color::Reset);
}

/// The quality column shows `unslop`'s audit, a store skill's vet and
/// `not judged` for the rest, cut as `usage` cuts it.
#[test]
fn usage_rows_show_the_audit_else_the_vet_else_not_judged() {
    let mut golden = Golden::new();
    let data = tempfile::tempdir().unwrap();
    common::vetted(data.path());
    golden.world.inv = store::inventory(Some(golden.repo.path()), data.path()).unwrap();
    let input = golden.world.input(&golden.events);
    let lines = text(&mut app(&input), 200, 40);
    let tail = |skill: &str| row(&lines, skill).join(" ");
    assert!(tail("unslop").ends_with(" audit: Good on <short> texts"), "{lines:#?}");
    assert!(tail("vetted").ends_with(" vet: quality weak, value low value"), "{lines:#?}");
    assert!(tail("goal-plan").ends_with(" not judged"), "{lines:#?}");
}

#[test]
fn enter_opens_the_card_whose_audit_line_matches_show_json_and_esc_goes_back() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let mut app = app(&input);
    typed(&mut app, "/unslop");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.screen(), Screen::Card);
    let screen = text(&mut app, 200, 100);
    // The card opens with the manual, as `show` prints it.
    assert_eq!(screen[..3], ["for:      Removes slop from prose.", "your use: - Tighten the release notes.", ""]);
    assert_eq!(screen[3], format!("unslop: since {}", common::ago(30).get(..10).unwrap()));

    let json = report::show(&input, "unslop", report::SHOW_SINCE, report::SHOW_LIMIT, true, false).unwrap();
    let audit = &serde_json::from_str::<serde_json::Value>(&json).unwrap()["audit"];
    let line = format!(
        "audit: {}, {} sessions, {} billing: {}",
        &audit["audited_at"].as_str().unwrap()[..10],
        audit["sessions"].as_array().unwrap().len(),
        audit["judge"]["billing"].as_str().unwrap_or("unknown"),
        audit["result"]["verdict"].as_str().unwrap(),
    );
    assert!(screen.contains(&line), "{line:?} not in {screen:#?}");

    // A page is the card's visible height, not the list's.
    let card = report::skill_card(&input, "unslop", &UsageArgs::new(report::SHOW_SINCE), report::SHOW_LIMIT);
    let card = report::card_text(&input, &card, false);
    let height = text(&mut app, 200, 30).iter().position(|l| l.starts_with("card unslop · ")).unwrap();
    press(&mut app, KeyCode::Char('l'));
    assert_eq!(text(&mut app, 200, 30)[0], card.lines().nth(height).unwrap());

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.screen(), Screen::Usage);
}

#[test]
fn tab_shows_unused_with_the_held_and_named_by_notes_in_their_colours() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let mut app = app(&input);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.screen(), Screen::Unused);
    let screen = draw(&mut app, 200, 40);
    let lines: Vec<String> = screen.iter().map(|(t, _)| t.clone()).collect();
    assert_eq!(row(&lines, "goal-plan")[1], "held");
    let line = lines.iter().find(|l| l.starts_with("goal-plan ")).unwrap();
    assert!(line.ends_with("held by goal-smart; named by unslop"), "{line}");
    assert_eq!(colour_of(&screen, "goal-plan", "held"), ratatui_colour("held"));
    assert_eq!(colour_of(&screen, "goal-plan", "named by"), ratatui_colour("named by unslop"));
    assert_eq!(colour_of(&screen, "goal-plan", "held by"), Color::Reset);
    assert!(lines.iter().any(|l| l.starts_with("unused · window 60d")), "{lines:#?}");

    // From the last row of 30d, the shorter 60d list keeps a row selected.
    press(&mut app, KeyCode::Char('w'));
    press(&mut app, KeyCode::Char('w'));
    let before = rows(&text(&mut app, 200, 40)).len();
    for _ in 0..before {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Char('w'));
    let after = rows(&text(&mut app, 200, 40));
    assert!(after.len() < before, "{after:#?}");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.screen(), Screen::Card);

    // `a` on a card opened from unused leaves the unused selection where it was.
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.screen(), Screen::Unused);
    press(&mut app, KeyCode::Enter);
    let skill = after.last().unwrap().split_whitespace().next().unwrap();
    assert!(text(&mut app, 200, 40).iter().any(|l| l.starts_with(&format!("{skill}: "))));
}

#[test]
fn slash_leaves_only_the_rows_whose_name_matches() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let mut app = app(&input);
    let all = rows(&text(&mut app, 200, 40)).len();
    typed(&mut app, "/goal");
    app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    let screen = text(&mut app, 200, 40);
    let shown = rows(&screen);
    assert!(!shown.is_empty() && shown.len() < all, "{screen:#?}");
    assert!(shown.iter().all(|r| r.starts_with("goal-")), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains(&format!("{} skills · filter /goal", shown.len()))), "{screen:#?}");
}

#[test]
fn w_a_c_and_capital_a_change_the_footer_and_counts_as_the_usage_flags_do() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let default = UsageArgs::new(Since::Days(30));
    let cases: [(char, &str, UsageArgs); 4] = [
        ('w', "window 60d", UsageArgs::new(Since::Days(60))),
        ('a', "agent claude", UsageArgs { agent: Some(Agent::Claude), ..UsageArgs::new(Since::Days(30)) }),
        ('c', "channel slash", UsageArgs { channel: Some(Channel::Slash), ..UsageArgs::new(Since::Days(30)) }),
        ('A', "authoring on", UsageArgs { authoring: true, ..UsageArgs::new(Since::Days(30)) }),
    ];
    for (key, footer, args) in cases {
        let mut app = app(&input);
        let words = |screen: &[String]| {
            rows(screen).iter().map(|r| r.split_whitespace().map(String::from).collect()).collect::<Vec<Vec<String>>>()
        };
        assert_eq!(words(&text(&mut app, 200, 40)), expected(&input, &default));
        press(&mut app, KeyCode::Char(key));
        let screen = text(&mut app, 200, 40);
        assert!(screen.iter().any(|l| l.starts_with("usage · ") && l.contains(footer)), "{key}: {screen:#?}");
        assert_ne!(expected(&input, &args), expected(&input, &default), "{key} changes nothing in the fixture");
        assert_eq!(words(&screen), expected(&input, &args), "{key}");
    }
}

#[test]
fn a_narrow_terminal_drops_columns_from_the_right_and_keeps_skill_and_sess() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let mut app = app(&input);
    let screen = text(&mut app, 60, 30);
    let keys = screen.iter().rev().take_while(|l| !l.starts_with("usage · ")).flat_map(|l| l.split(" · ")).count();
    assert_eq!(keys, 15, "every key shows: {screen:#?}");
    let header: Vec<&str> = screen[0].split_whitespace().collect();
    assert_eq!(header[..2], ["skill", "sess"]);
    assert!(!header.contains(&"grade"), "{screen:#?}");
    let sess = report::usage_table(&input, &UsageArgs::new(Since::Days(30))).rows[0].sess;
    assert_eq!(row(&screen, "unslop")[1], sess.to_string());
}

#[test]
fn q_and_ctrl_c_quit() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let mut app = app(&input);
    assert!(press(&mut app, KeyCode::Char('j')));
    assert!(!press(&mut app, KeyCode::Char('q')));
    assert!(!app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
}

/// A source of `idle`, `busy` used 3 days ago and `held` cited by `busy`,
/// unless `source` is false; the store skill `stored`; one Claude home with
/// every skill linked into it.
struct Actions {
    _temp: tempfile::TempDir,
    source: PathBuf,
    skills: PathBuf,
    data: PathBuf,
    cfg: config::Config,
}

impl Actions {
    fn new(source: bool) -> Actions {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let (dir, home, data) = (root.join("skills"), root.join("home/.claude"), root.join("data"));
        for (name, body) in [("idle", ""), ("held", ""), ("busy", "Run the held skill.\n")] {
            common::write(
                &dir.join(name).join("SKILL.md"),
                &format!("---\nname: {name}\ndescription: X.\n---\n\n{body}"),
            );
        }
        common::write(&root.join("in/stored/SKILL.md"), "---\nname: stored\ndescription: X.\n---\n");
        let source_line = if source { format!("source = \"{}\"\n\n", dir.display()) } else { String::new() };
        let toml =
            format!("{source_line}[[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"{}\"\n", home.display());
        common::write(&root.join("mazo.toml"), &toml);
        common::write(&home.join("settings.json"), "{}\n");
        let ts = jiff::Timestamp::now().checked_sub(jiff::SignedDuration::from_hours(72)).unwrap().to_string();
        let e = Event { cwd: None, ..common::event("e1", "busy", Channel::Slash, "s1", &ts) };
        common::write(&data.join("events.jsonl"), &(serde_json::to_string(&e).unwrap() + "\n"));
        let cfg = config::load(&root.join("mazo.toml")).unwrap().unwrap();
        let fx = Actions { _temp: temp, skills: home.join("skills"), source: dir, data, cfg };
        // `add` links every skill.
        let stored = root.join("in/stored");
        let stored = stored.to_string_lossy();
        let args = store::Add {
            source: &stored,
            rename: None,
            pick: None,
            all: false,
            trust: true,
            yes: false,
            force: false,
            project: false,
            pane: None,
            asks: None,
        };
        store::add(&fx.cfg, &fx.inv(), &fx.data, &args).unwrap();
        fx
    }

    fn inv(&self) -> Inventory {
        store::inventory(self.cfg.source.as_deref(), &self.data).unwrap()
    }

    /// The app over what is on disk now.
    fn app(&self) -> App {
        self.app_at(jiff::Timestamp::now())
    }

    /// The app over what is on disk, as of `now`.
    fn app_at(&self, now: jiff::Timestamp) -> App {
        let (inv, ledger) = (self.inv(), Ledger::load(&self.data).unwrap());
        let coverage = ledger::load_coverage(&self.data).unwrap();
        let audits = Default::default();
        let input = Input {
            cfg: &self.cfg,
            inv: &inv,
            events: ledger.events(),
            coverage: &coverage,
            audits: &audits,
            now,
            tz: jiff::tz::TimeZone::UTC,
        };
        App::new(&input, &self.data)
    }

    /// The source, the store, the lockfile and the home's links.
    fn disk(&self) -> common::sync::Snapshot {
        common::sync::snapshot(&[&self.source, &store::dir(&self.data), &self.data.join("mazo.lock"), &self.skills])
    }

    fn linked(&self, name: &str) -> bool {
        self.skills.join(name).symlink_metadata().is_ok()
    }

    /// Adds a `[judge]` of `agent`, for `command` the [`stub`] judge, and a
    /// strong load of `busy` in a real transcript for it to judge.
    fn judge(&mut self, agent: &str) {
        let root = self.data.parent().unwrap().to_path_buf();
        let argv = match agent {
            "command" => format!("argv = [\"{}\"]\n", stub(&root).display()),
            _ => String::new(),
        };
        let toml = std::fs::read_to_string(root.join("mazo.toml")).unwrap();
        common::write(&root.join("mazo.toml"), &format!("{toml}\n[judge]\nagent = \"{agent}\"\n{argv}"));
        self.cfg = config::load(&root.join("mazo.toml")).unwrap().unwrap();
        let ts = jiff::Timestamp::now().checked_sub(jiff::SignedDuration::from_hours(48)).unwrap().to_string();
        let src = format!("{}:5", common::fixture("claude/condense.jsonl").display());
        let e = Event { src, ..common::event("e2", "busy", Channel::Tool, "s2", &ts) };
        let events = std::fs::read_to_string(self.data.join("events.jsonl")).unwrap();
        common::write(&self.data.join("events.jsonl"), &(events + &serde_json::to_string(&e).unwrap() + "\n"));
    }

    /// The pids the stub judge wrote, one per call.
    fn pids(&self) -> Vec<String> {
        let pids = std::fs::read_to_string(self.data.parent().unwrap().join("pids")).unwrap_or_default();
        pids.lines().map(String::from).collect()
    }
}

/// A `#!/bin/sh` command judge under `root` that appends its pid to
/// `<root>/pids`, then answers either stage, or, while `<root>/hang` exists,
/// becomes a `sleep` that outlasts any test and ignores the hangup of a
/// closing terminal, so only mazo's kill ends it.
fn stub(root: &Path) -> PathBuf {
    let r = root.display();
    let script = format!(
        r#"#!/bin/sh
echo $$ >> {r}/pids
in=$(cat)
[ -f {r}/hang ] && trap '' HUP && exec sleep 600
if printf '%s\n' "$in" | grep -q '^<session-records>$'; then
  sid=$(printf '%s\n' "$in" | grep -o '"session":"[^"]*"' | head -n 1 | cut -d '"' -f 4)
  sed "s/SESSION/$sid/g" {r}/skill.json
else
  line=$(printf '%s\n' "$in" | grep -o '^L[0-9]*:' | head -n 1 | tr -d 'L:')
  sed "s/\"LINE\"/$line/g" {r}/session.json
fi
"#
    );
    let path = root.join("bin/judge");
    common::write(&path, &script);
    Command::new("chmod").arg("+x").arg(&path).status().unwrap();
    common::write(&root.join("session.json"), &common::judge::answer("LINE").to_string());
    common::write(&root.join("skill.json"), &common::judge::skill_answer("SESSION").to_string());
    path
}

/// Whether process `pid` exists.
fn alive(pid: &str) -> bool {
    Command::new("kill").args(["-0", pid]).stderr(std::process::Stdio::null()).status().unwrap().success()
}

/// Ticks `app` until `done` holds, for at most 20 seconds.
fn until(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(Instant::now() < deadline, "timed out");
        app.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Selects `skill` on the current list through the filter.
fn select(app: &mut App, skill: &str) {
    typed(app, &format!("/{skill}"));
    press(app, KeyCode::Enter);
}

#[test]
fn r_retires_a_source_skill_from_usage_after_y_and_the_list_drops_it() {
    let fx = Actions::new(true);
    assert!(fx.linked("idle"));
    let mut app = fx.app();
    select(&mut app, "idle");
    press(&mut app, KeyCode::Char('r'));
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.contains("retire idle: move it to ")), "{screen:#?}");
    assert!(screen.iter().any(|l| l == "y apply · any other key cancels"), "{screen:#?}");
    press(&mut app, KeyCode::Char('y'));
    assert!(fx.source.join(".archive/idle/SKILL.md").is_file());
    assert!(!fx.source.join("idle").exists());
    assert!(!fx.linked("idle"));
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.contains("archived ")), "{screen:#?}");

    // Any key closes the result; the list was re-read.
    press(&mut app, KeyCode::Char('j'));
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.starts_with("usage · ") && l.contains("0 skills · filter /idle")), "{screen:#?}");
}

#[test]
fn r_retires_a_store_skill_from_its_card() {
    let fx = Actions::new(true);
    assert!(fx.linked("stored"));
    let mut app = fx.app();
    select(&mut app, "stored");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.screen(), Screen::Card);
    press(&mut app, KeyCode::Char('r'));
    assert!(text(&mut app, 200, 20).iter().any(|l| l.contains("retire stored: remove it from the store")));
    press(&mut app, KeyCode::Char('y'));
    assert!(!store::dir(&fx.data).join("stored").exists());
    assert!(store::Lock::load(&fx.data).unwrap().skills.is_empty());
    assert!(!fx.linked("stored"));
    // The card of a retired skill closes to its list.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.screen(), Screen::Usage);
}

#[test]
fn with_no_source_r_removes_a_store_skill_and_s_is_in_sync() {
    let fx = Actions::new(false);
    assert!(fx.linked("stored") && !fx.linked("idle"));
    let mut app = fx.app();
    select(&mut app, "stored");
    press(&mut app, KeyCode::Char('r'));
    assert!(text(&mut app, 200, 20).iter().any(|l| l.contains("retire stored: remove it from the store")));
    press(&mut app, KeyCode::Char('y'));
    assert!(!store::dir(&fx.data).join("stored").exists());
    assert!(!fx.linked("stored"));
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('s'));
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.contains("in sync: nothing to apply")), "{screen:#?}");
}

#[test]
fn any_key_but_y_cancels_a_retire() {
    let fx = Actions::new(true);
    let before = fx.disk();
    let mut app = fx.app();
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.screen(), Screen::Unused);
    select(&mut app, "idle");
    for key in [KeyCode::Char('n'), KeyCode::Char('Y'), KeyCode::Enter, KeyCode::Esc] {
        press(&mut app, KeyCode::Char('r'));
        assert!(text(&mut app, 200, 20).iter().any(|l| l.contains("retire idle")));
        press(&mut app, key);
        assert!(!text(&mut app, 200, 20).iter().any(|l| l.contains("retire idle")), "{key:?}");
    }
    assert_eq!(fx.disk(), before);
}

#[test]
fn a_blocked_retire_shows_its_blockers_and_changes_nothing() {
    let fx = Actions::new(true);
    let before = fx.disk();
    let mut app = fx.app();
    for (skill, why) in [("busy", "busy has a strong event in the last 30 days"), ("held", "held is held by busy")] {
        select(&mut app, skill);
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('y'));
        let screen = text(&mut app, 200, 20);
        assert!(screen.iter().any(|l| l.contains(&format!("retire {skill} refused:"))), "{screen:#?}");
        assert!(screen.iter().any(|l| l.contains(why)), "{screen:#?}");
        assert!(!screen.iter().any(|l| l.contains("--force")), "{screen:#?}");
        press(&mut app, KeyCode::Esc);
    }
    assert_eq!(fx.disk(), before);
}

#[test]
fn a_store_skill_installed_long_ago_is_not_new_after_an_action() {
    let fx = Actions::new(true);
    let lock = fx.data.join("mazo.lock");
    let toml = std::fs::read_to_string(&lock).unwrap();
    let at = toml.lines().find(|l| l.starts_with("installed_at")).unwrap();
    let long_ago = jiff::Timestamp::now().checked_sub(jiff::SignedDuration::from_hours(24 * 100)).unwrap();
    std::fs::write(&lock, toml.replace(at, &format!("installed_at = \"{long_ago}\""))).unwrap();
    let mut app = fx.app();
    press(&mut app, KeyCode::Tab);
    assert_eq!(row(&text(&mut app, 200, 20), "stored")[1], "unused");
    select(&mut app, "idle");
    press(&mut app, KeyCode::Char('r'));
    press(&mut app, KeyCode::Char('y'));
    assert!(fx.source.join(".archive/idle").is_dir());
    press(&mut app, KeyCode::Esc);
    typed(&mut app, "/");
    press(&mut app, KeyCode::Enter);
    assert_eq!(row(&text(&mut app, 200, 20), "stored")[1], "unused");
}

#[test]
fn enter_on_an_unused_store_skill_asks_to_retire_it_and_on_any_other_row_opens_the_card() {
    let fx = Actions::new(true);
    let fresh = fx.data.parent().unwrap().join("in/fresh");
    common::write(&fresh.join("SKILL.md"), "---\nname: fresh\ndescription: X.\n---\n");
    let fresh = fresh.to_string_lossy();
    let args = store::Add {
        source: &fresh,
        rename: None,
        pick: None,
        all: false,
        trust: true,
        yes: false,
        force: false,
        project: false,
        pane: None,
        asks: None,
    };
    store::add(&fx.cfg, &fx.inv(), &fx.data, &args).unwrap();
    let now: jiff::Timestamp = "2026-06-01T12:00:00Z".parse().unwrap();
    let lock = fx.data.join("mazo.lock");
    let mut lines: Vec<String> = std::fs::read_to_string(&lock).unwrap().lines().map(String::from).collect();
    let mut name = "";
    for line in &mut lines {
        if let Some(n) = line.strip_prefix("name = ") {
            name = if n.contains("fresh") { "fresh" } else { "stored" };
        } else if line.starts_with("installed_at") {
            let days = if name == "fresh" { 1 } else { 100 };
            let at = now.checked_sub(jiff::SignedDuration::from_hours(24 * days)).unwrap();
            *line = format!("installed_at = \"{at}\"");
        }
    }
    std::fs::write(&lock, lines.join("\n") + "\n").unwrap();
    let before = fx.disk();

    let mut app = fx.app_at(now);
    press(&mut app, KeyCode::Tab);
    let screen = text(&mut app, 200, 20);
    assert_eq!(row(&screen, "stored")[1], "unused", "{screen:#?}");
    assert_eq!(row(&screen, "fresh")[1], "new", "{screen:#?}");
    assert_eq!(row(&screen, "idle")[1], "unused", "{screen:#?}");

    select(&mut app, "stored");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.screen(), Screen::Unused);
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.contains("retire stored: remove it from the store")), "{screen:#?}");
    assert!(screen.iter().any(|l| l == "y apply · any other key cancels"), "{screen:#?}");
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(fx.disk(), before);

    for skill in ["fresh", "idle"] {
        select(&mut app, skill);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.screen(), Screen::Card, "{skill}");
        let screen = text(&mut app, 200, 20);
        assert!(screen.iter().any(|l| l.starts_with(&format!("{skill}: "))), "{skill}: {screen:#?}");
        assert!(!screen.iter().any(|l| l.contains(&format!("retire {skill}"))), "{skill}: {screen:#?}");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.screen(), Screen::Unused);
    }
    assert_eq!(fx.disk(), before);
}

#[test]
fn s_shows_the_dry_run_plan_and_y_applies_it() {
    let fx = Actions::new(true);
    std::fs::remove_file(fx.skills.join("idle")).unwrap();
    let homes: Vec<_> = fx.cfg.homes.iter().collect();
    let plan = links::planned(&links::checked(&fx.cfg, &fx.inv(), &homes).unwrap());
    assert_eq!(plan, [format!("would link  {}", fx.skills.join("idle").display())]);
    let mut app = fx.app();
    press(&mut app, KeyCode::Char('s'));
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.contains(&plan[0])), "{screen:#?}");
    let before = fx.disk();
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(fx.disk(), before);

    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(std::fs::read_link(fx.skills.join("idle")).unwrap(), fx.source.join("idle"));
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('s'));
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.contains("in sync: nothing to apply")), "{screen:#?}");
    assert!(screen.iter().any(|l| l == "any key closes"), "{screen:#?}");
}

#[test]
fn x_shows_the_audit_cap_and_any_key_but_y_starts_nothing() {
    let mut fx = Actions::new(true);
    fx.judge("command");
    let before = fx.disk();
    let mut app = fx.app();
    select(&mut app, "busy");
    press(&mut app, KeyCode::Char('x'));
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.contains("audit busy with the command judge in the background?")), "{screen:#?}");
    assert!(
        screen.iter().any(|l| l.contains("cap: at most 20 sessions; the command judge reports no cost")),
        "{screen:#?}"
    );
    assert!(screen.iter().any(|l| l == "y apply · any other key cancels"), "{screen:#?}");
    press(&mut app, KeyCode::Char('n'));
    let screen = text(&mut app, 200, 20);
    assert!(!app.busy());
    assert!(!screen.iter().any(|l| l.contains("audit busy")), "{screen:#?}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(fx.pids().is_empty());
    assert!(!fx.data.join("audits.jsonl").exists());
    assert_eq!(fx.disk(), before);

    // A judge that reports its cost shows `mazo audit`'s default budget too.
    let mut fx = Actions::new(true);
    fx.judge("claude");
    let mut app = fx.app();
    select(&mut app, "busy");
    press(&mut app, KeyCode::Char('x'));
    let screen = text(&mut app, 200, 20);
    assert!(screen.iter().any(|l| l.contains("cap: at most 20 sessions and $1.00 API-equivalent")), "{screen:#?}");
}

#[test]
fn x_without_a_judge_shows_the_refusal_mazo_audit_gives() {
    let fx = Actions::new(true);
    let mut app = fx.app();
    select(&mut app, "busy");
    press(&mut app, KeyCode::Char('x'));
    let screen = text(&mut app, 200, 20);
    let refusal = ["ERROR: no judge configured; add one to the config, for example:", "[judge]", "agent = \"claude\""];
    for line in refusal {
        assert!(screen.iter().any(|l| l.contains(line)), "{line:?} not in {screen:#?}");
    }
    assert!(screen.iter().any(|l| l == "any key closes"), "{screen:#?}");
    press(&mut app, KeyCode::Char('y'));
    assert!(!app.busy());
}

#[test]
fn y_audits_in_the_background_and_the_pane_shows_the_result() {
    let mut fx = Actions::new(true);
    fx.judge("command");
    let mut app = fx.app();
    select(&mut app, "busy");
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.busy());
    until(&mut app, |app| !app.busy());
    assert_eq!(fx.pids().len(), 2, "one session call, then the skill report");
    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains(" audit busy · done · Esc closes ")), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains("busy: 1 judged, 0 dropped")), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains("busy: skill report over 1 sessions: ")), "{screen:#?}");

    // The data was re-read: the card shows the new audit.
    press(&mut app, KeyCode::Enter);
    let screen = text(&mut app, 200, 60);
    assert!(screen.iter().any(|l| l.starts_with("audit: ") && l.contains(", 1 sessions, ")), "{screen:#?}");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Esc);
    assert!(!text(&mut app, 200, 30).iter().any(|l| l.contains(" audit busy ")));
}

#[test]
fn quitting_with_a_job_running_asks_and_kills_its_judge() {
    let mut fx = Actions::new(true);
    fx.judge("command");
    common::write(&fx.data.parent().unwrap().join("hang"), "");
    let mut app = fx.app();
    select(&mut app, "busy");
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('y'));
    until(&mut app, |_| !fx.pids().is_empty());
    let pid = fx.pids()[0].clone();
    assert!(alive(&pid));

    // Navigation keeps working, the pane shows the progress, and a second job is refused.
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.screen(), Screen::Unused);
    app.tick();
    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains(" audit busy · running ")), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains("judging 1/1 s2")), "{screen:#?}");
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Char('x'));
    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains("the audit of busy is running; one job at a time")), "{screen:#?}");
    press(&mut app, KeyCode::Esc);

    for quit in [
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        assert!(app.key(quit));
        let screen = text(&mut app, 200, 30);
        assert!(
            screen.iter().any(|l| l.contains("the audit of busy is running; stop its judge and quit?")),
            "{screen:#?}"
        );
        assert!(press(&mut app, KeyCode::Char('n')));
        assert!(app.busy() && alive(&pid));
    }
    let started = Instant::now();
    assert!(press(&mut app, KeyCode::Char('q')));
    assert!(!press(&mut app, KeyCode::Char('y')));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert!(!alive(&pid), "the judge outlived the TUI");
    assert_eq!(fx.pids().len(), 1, "no call started after the cancel");
}

impl Actions {
    /// A bare repository `origin/<name>.git` holding the skill `name` at its
    /// root, and its `file://` URL.
    fn repo(&self, name: &str) -> String {
        let root = self.data.parent().unwrap();
        let work = root.join("work").join(name);
        common::write(&work.join("SKILL.md"), &format!("---\nname: {name}\ndescription: From a repo.\n---\n"));
        common::write(&work.join("agents/openai.yaml"), "policy:\n  allow_implicit_invocation: false\n");
        common::git(&work, &["init", "-q"], common::NOW);
        common::git(&work, &["add", "."], common::NOW);
        common::git(&work, &["commit", "-q", "-m", "skill"], common::NOW);
        let bare = root.join("origin").join(format!("{name}.git"));
        common::git(&work, &["clone", "-q", "--bare", ".", &bare.display().to_string()], common::NOW);
        format!("file://{}", bare.display())
    }

    /// Installs the skill at `url` into the store, recorded as `trusted` or not.
    fn install(&self, url: &str, trusted: bool) {
        let args = store::Add {
            source: url,
            rename: None,
            pick: None,
            all: false,
            trust: true,
            yes: false,
            force: false,
            project: false,
            pane: None,
            asks: None,
        };
        store::add(&self.cfg, &self.inv(), &self.data, &args).unwrap();
        if !trusted {
            let name = url.rsplit('/').next().unwrap().trim_end_matches(".git");
            let lock = self.data.join("mazo.lock");
            let toml = std::fs::read_to_string(&lock).unwrap();
            let (head, tail) = toml.split_at(toml.find(&format!("name = \"{name}\"")).unwrap());
            std::fs::write(&lock, head.to_string() + &tail.replacen("trusted = true", "trusted = false", 1)).unwrap();
        }
    }

    /// Commits a new `SKILL.md` body to the skill `name`'s repository and pushes it.
    fn push(&self, name: &str, body: &str) {
        let root = self.data.parent().unwrap();
        let work = root.join("work").join(name);
        common::write(&work.join("SKILL.md"), &format!("---\nname: {name}\ndescription: From a repo.\n---\n\n{body}"));
        common::git(&work, &["commit", "-q", "-am", "change"], common::NOW);
        let bare = root.join("origin").join(format!("{name}.git"));
        common::git(&work, &["push", "-q", &bare.display().to_string(), "HEAD"], common::NOW);
    }

    /// Adds a command `[judge]` that appends its pid to `<root>/pids` and
    /// gives a vet verdict with `security` as its finding, or, while
    /// `<root>/hang` exists, sleeps until it is killed.
    fn vetter(&mut self, security: Option<&str>) {
        let root = self.data.parent().unwrap().to_path_buf();
        let r = root.display();
        let path = root.join("bin/vet");
        common::write(
            &path,
            &format!(
                "#!/bin/sh\necho $$ >> {r}/pids\ncat > /dev/null\n[ -f {r}/hang ] && exec sleep 600\ncat {r}/verdict.json\n"
            ),
        );
        Command::new("chmod").arg("+x").arg(&path).status().unwrap();
        let verdict = serde_json::json!({
            "security": security,
            "quality": "good",
            "value": "worth trying",
            "redundant_with": null,
            "reasons": [],
            "for": "Tidies notes.",
            "try": [],
        });
        common::write(&root.join("verdict.json"), &verdict.to_string());
        let toml = std::fs::read_to_string(root.join("mazo.toml")).unwrap();
        let judge = format!("\n[judge]\nagent = \"command\"\nargv = [\"{}\"]\n", path.display());
        common::write(&root.join("mazo.toml"), &(toml + &judge));
        self.cfg = config::load(&root.join("mazo.toml")).unwrap().unwrap();
    }

    fn commit(&self, name: &str) -> Option<String> {
        let lock = store::Lock::load(&self.data).unwrap();
        lock.skills.into_iter().find(|l| l.name == name).unwrap().commit
    }
}

/// Runs the update `key` asks for on the selected skill, after `y`, to its end.
fn update(app: &mut App, key: char) -> Vec<String> {
    press(app, KeyCode::Char(key));
    press(app, KeyCode::Char('y'));
    assert!(app.busy());
    until(app, |app| !app.busy());
    text(app, 200, 30)
}

#[test]
fn u_updates_a_trusted_skill_without_the_judge_and_re_vets_an_untrusted_one_in_the_pane() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    let (ours, theirs) = (fx.repo("ours"), fx.repo("theirs"));
    fx.install(&ours, true);
    fx.install(&theirs, false);
    fx.push("ours", "v2\n");
    fx.push("theirs", "v2\n");
    let mut app = fx.app();

    select(&mut app, "ours");
    press(&mut app, KeyCode::Char('u'));
    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains("update ours and accept judge verdicts?")), "{screen:#?}");
    let revet = "an untrusted skill that changed is re-vetted with one command judge call; \
                 the command judge reports no cost";
    assert!(screen.iter().any(|l| l.contains(revet)), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains("a security finding still refuses its update")), "{screen:#?}");
    press(&mut app, KeyCode::Char('n'));
    assert!(!app.busy());

    let screen = update(&mut app, 'u');
    let to = store::dir(&fx.data).join("ours");
    assert!(screen.iter().any(|l| l.contains(" update ours · done · Esc closes ")), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains(&format!("updated {} to ", to.display()))), "{screen:#?}");
    assert!(std::fs::read_to_string(to.join("SKILL.md")).unwrap().ends_with("v2\n"));
    assert!(fx.pids().is_empty(), "a trusted update calls no judge");

    let was = fx.commit("theirs");
    select(&mut app, "theirs");
    let screen = update(&mut app, 'u');
    assert_eq!(fx.pids().len(), 1, "one re-vet");
    assert!(screen.iter().any(|l| l.contains(" update theirs · done · Esc closes ")), "{screen:#?}");
    for line in ["vetting theirs", "vet theirs: quality good, value worth trying", "for: Tidies notes."] {
        assert!(screen.iter().any(|l| l.contains(line)), "{line}: {screen:#?}");
    }
    assert!(screen.iter().any(|l| l.contains("updated ") && l.contains("theirs to ")), "{screen:#?}");
    assert_ne!(fx.commit("theirs"), was);
    let lock = store::Lock::load(&fx.data).unwrap();
    let entry = lock.skills.iter().find(|l| l.name == "theirs").unwrap();
    assert_eq!(entry.r#for.as_deref(), Some("Tidies notes."));

    // Every store skill is up to date now: reported, and no judge is called.
    press(&mut app, KeyCode::Esc);
    let screen = update(&mut app, 'U');
    assert!(screen.iter().any(|l| l.contains(" update every store skill · done · Esc closes ")), "{screen:#?}");
    for line in ["ours: up to date", "theirs: up to date", "stored: not updatable; installed from a local path"] {
        assert!(screen.iter().any(|l| l.contains(line)), "{line}: {screen:#?}");
    }
    assert_eq!(fx.pids().len(), 1);
}

#[test]
fn quitting_during_an_update_of_every_skill_leaves_every_store_skill_in_place() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    // `one` is re-vetted by a judge that hangs; trusted `two` needs no judge.
    for (name, trusted) in [("one", false), ("two", true)] {
        let url = fx.repo(name);
        fx.install(&url, trusted);
        fx.push(name, "v2\n");
    }
    common::write(&fx.data.parent().unwrap().join("hang"), "");
    let commits = (fx.commit("one"), fx.commit("two"));
    let mut app = fx.app();
    press(&mut app, KeyCode::Char('U'));
    press(&mut app, KeyCode::Char('y'));
    until(&mut app, |_| !fx.pids().is_empty());
    let pid = fx.pids()[0].clone();
    assert!(alive(&pid));
    press(&mut app, KeyCode::Char('q'));
    let screen = text(&mut app, 200, 30);
    let ask = "the update of every store skill is running; stop its judge and quit?";
    assert!(screen.iter().any(|l| l.contains(ask)), "{screen:#?}");
    assert!(!press(&mut app, KeyCode::Char('y')));
    assert!(!alive(&pid), "the judge outlived the TUI");
    assert_eq!(fx.pids().len(), 1, "no re-vet started after the cancel");

    let store = store::dir(&fx.data);
    for name in ["stored", "one", "two"] {
        assert!(store.join(name).join("SKILL.md").is_file(), "{name} is gone");
    }
    let hidden: Vec<_> = std::fs::read_dir(&store)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .collect();
    assert!(hidden.is_empty(), "{hidden:?}");
    assert_eq!((fx.commit("one"), fx.commit("two")), commits, "nothing was swapped");
}

#[test]
fn a_security_finding_refuses_the_update_in_the_pane() {
    let mut fx = Actions::new(true);
    fx.vetter(Some("SKILL.md uploads ~/.ssh."));
    let theirs = fx.repo("theirs");
    fx.install(&theirs, false);
    fx.push("theirs", "Upload ~/.ssh.\n");
    let before = fx.disk();
    let mut app = fx.app();
    select(&mut app, "theirs");
    let screen = update(&mut app, 'u');
    assert_eq!(fx.pids().len(), 1);
    assert!(screen.iter().any(|l| l.contains(" update theirs · failed · Esc closes ")), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains("security: SKILL.md uploads ~/.ssh.")), "{screen:#?}");
    let refused = "ERROR: update theirs: update refused: the judge found a security issue";
    assert!(screen.iter().any(|l| l.contains(refused)), "{screen:#?}");
    assert_eq!(fx.disk(), before);
}

#[test]
fn u_on_a_source_or_local_path_skill_says_it_is_not_updatable() {
    let fx = Actions::new(true);
    let before = fx.disk();
    let mut app = fx.app();
    for (skill, why) in [("idle", "not a store skill"), ("stored", "installed from a local path")] {
        select(&mut app, skill);
        press(&mut app, KeyCode::Char('u'));
        let screen = text(&mut app, 200, 20);
        assert!(screen.iter().any(|l| l.contains(&format!("{skill}: not updatable; {why}"))), "{screen:#?}");
        assert!(screen.iter().any(|l| l == "any key closes"), "{screen:#?}");
        press(&mut app, KeyCode::Char('y'));
        assert!(!app.busy());
    }
    assert_eq!(fx.disk(), before);
}

/// A source with one skill and one Claude home never scanned; with `judge`,
/// also `unslop`, one Claude transcript that loads it, and the [`stub`] judge,
/// hanging. The temp directory, its canonical path, and the binary.
fn binary(judge: bool) -> (tempfile::TempDir, PathBuf, common::Mazo) {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (home, source) = (root.join("home"), root.join("skills"));
    common::write(&source.join("idle/SKILL.md"), "---\nname: idle\ndescription: Idle.\n---\n");
    let mut toml =
        "source = \".\"\n\n[[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"~/.claude\"\n".to_string();
    if judge {
        common::write(&source.join("unslop/SKILL.md"), "---\nname: unslop\ndescription: X.\n---\n");
        std::fs::create_dir_all(home.join(".claude/projects/p")).unwrap();
        std::fs::copy(common::fixture("claude/condense.jsonl"), home.join(".claude/projects/p/s.jsonl")).unwrap();
        common::write(&root.join("hang"), "");
        toml += &format!("\n[judge]\nagent = \"command\"\nargv = [\"{}\"]\n", stub(&root).display());
    }
    common::write(&source.join("mazo.toml"), &toml);
    common::write(&home.join(".claude/settings.json"), "{}\n");
    let mazo = common::Mazo::new(&home, &home, &source.join("mazo.toml"), &home.join("data"));
    (temp, root, mazo)
}

#[test]
fn bare_mazo_off_a_terminal_prints_help_and_exits_2() {
    let (_temp, _, mazo) = binary(false);
    let out = mazo.output(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(common::stderr(&out).contains("Usage: mazo <COMMAND>"), "{}", common::stderr(&out));
}

/// Bare `mazo` under `script` on an 80×24 pseudo-terminal: once the screen
/// opens, each step's keys are written to it as soon as its condition holds.
/// The exit code and everything it printed.
#[cfg(target_os = "linux")]
fn on_a_pty(mut mazo: common::Mazo, steps: &[(&[u8], &dyn Fn() -> bool)]) -> Option<(Option<i32>, Vec<u8>)> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::mpsc;

    if std::process::Command::new("script").arg("--version").output().is_err() {
        eprintln!("skipped: no `script`");
        return None;
    }
    mazo.set("SHELL", Some("/bin/sh".as_ref()));
    let command = format!("stty rows 24 cols 80 && exec '{}'", env!("CARGO_BIN_EXE_mazo"));
    let mut child = mazo
        .wrap("script")
        .args(["-qec", &command, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0; 4096];
        while let Ok(n @ 1..) = stdout.read(&mut buf) {
            tx.send(buf[..n].to_vec()).unwrap();
        }
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut out = Vec::new();
    let mut stdin = child.stdin.take().unwrap();
    let mut sent = 0;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => out.extend(chunk),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                child.kill().unwrap();
                panic!("timed out: {}", String::from_utf8_lossy(&out));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // The alternate screen is entered after raw mode, so the bytes are keys.
        if let Some((keys, ready)) = steps.get(sent)
            && ready()
            && out.windows(8).any(|w| w == b"\x1b[?1049h")
        {
            stdin.write_all(keys).unwrap();
            sent += 1;
        }
    }
    assert_eq!(sent, steps.len(), "the screen never opened: {}", String::from_utf8_lossy(&out));
    Some((child.wait().unwrap().code(), out))
}

#[cfg(target_os = "linux")]
#[test]
fn bare_mazo_on_a_terminal_quits_on_q_and_ctrl_c_and_restores_the_terminal() {
    for byte in [b'q', 0x03] {
        let (_temp, _, mazo) = binary(false);
        let Some((code, out)) = on_a_pty(mazo, &[(&[byte], &|| true)]) else { return };
        assert_eq!(code, Some(0), "{byte:#x}: {}", String::from_utf8_lossy(&out));
        assert!(out.ends_with(b"\x1b[?1049l\x1b[?25h"), "{byte:#x}: {:?}", String::from_utf8_lossy(&out));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn quitting_bare_mazo_mid_audit_kills_the_judge_and_restores_the_terminal() {
    let (_temp, root, mazo) = binary(true);
    let pids = || std::fs::read_to_string(root.join("pids")).unwrap_or_default();
    let steps: [(&[u8], &dyn Fn() -> bool); 2] = [(b"/unslop\rxy", &|| true), (b"qy", &|| !pids().is_empty())];
    let Some((code, out)) = on_a_pty(mazo, &steps) else { return };
    assert_eq!(code, Some(0), "{}", String::from_utf8_lossy(&out));
    assert!(out.ends_with(b"\x1b[?1049l\x1b[?25h"), "{:?}", String::from_utf8_lossy(&out));
    let pid = pids();
    assert!(!alive(pid.trim()), "the judge outlived mazo");
}

impl Actions {
    /// A bare repository `origin/<name>.git` holding each of `skills` in a
    /// child directory, and its `file://` URL.
    fn repo_of(&self, name: &str, skills: &[&str]) -> String {
        let root = self.data.parent().unwrap();
        let work = root.join("work").join(name);
        for skill in skills {
            common::write(
                &work.join(skill).join("SKILL.md"),
                &format!("---\nname: {skill}\ndescription: The {skill} skill.\n---\n"),
            );
        }
        common::git(&work, &["init", "-q"], common::NOW);
        common::git(&work, &["add", "."], common::NOW);
        common::git(&work, &["commit", "-q", "-m", "skills"], common::NOW);
        let bare = root.join("origin").join(format!("{name}.git"));
        common::git(&work, &["clone", "-q", "--bare", ".", &bare.display().to_string()], common::NOW);
        format!("file://{}", bare.display())
    }

    fn locked(&self, name: &str) -> Option<store::Locked> {
        store::Lock::load(&self.data).unwrap().skills.into_iter().find(|l| l.name == name)
    }
}

/// Opens the add dialog and submits `source`.
fn add(app: &mut App, source: &str) {
    press(app, KeyCode::Char('i'));
    typed(app, source);
    press(app, KeyCode::Enter);
}

/// Ticks `app` until a screen line contains `line`, for at most 20 seconds: that screen.
fn shown(app: &mut App, line: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.tick();
        let screen = text(app, 200, 30);
        if screen.iter().any(|l| l.contains(line)) {
            return screen;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {line:?}: {screen:#?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn i_adds_an_untrusted_skill_after_the_call_count_and_the_verdict_dialog() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    let url = fx.repo("theirs");
    let mut app = fx.app();
    press(&mut app, KeyCode::Char('i'));
    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains("add a skill from a local path, a git URL or owner/repo[/path]:")));
    assert!(screen.iter().any(|l| l == "Enter add · Esc cancels"), "{screen:#?}");
    typed(&mut app, &url);
    assert!(text(&mut app, 200, 30).iter().any(|l| l.contains(&format!("> {url}_"))));
    press(&mut app, KeyCode::Enter);
    assert!(app.busy());

    let screen = shown(&mut app, "make 1 judge call? [y/N]");
    assert!(screen.iter().any(|l| l.contains("vetting 1 skill makes 1 judge call")), "{screen:#?}");
    assert!(fx.pids().is_empty(), "no judge call before the count is agreed to");
    press(&mut app, KeyCode::Char('y'));
    let screen = shown(&mut app, "install theirs? [Y/n]");
    assert!(screen.iter().any(|l| l.contains("vet theirs: quality good, value worth trying")), "{screen:#?}");
    assert!(screen.iter().any(|l| l == "Enter yes · y yes · n/Esc no · q quit"), "{screen:#?}");
    // Any key but Enter, y, n, Esc and q leaves the question open.
    press(&mut app, KeyCode::Char('x'));
    assert!(text(&mut app, 200, 30).iter().any(|l| l.contains("install theirs? [Y/n]")));
    press(&mut app, KeyCode::Enter);
    until(&mut app, |app| !app.busy());

    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains(&format!(" add {url} · done · Esc closes "))), "{screen:#?}");
    let to = store::dir(&fx.data).join("theirs");
    assert!(screen.iter().any(|l| l.contains(&format!("installed {}", to.display()))), "{screen:#?}");
    assert!(to.join("SKILL.md").is_file());
    let entry = fx.locked("theirs").unwrap();
    assert!(!entry.trusted);
    assert_eq!(entry.r#for.as_deref(), Some("Tidies notes."));
    assert!(fx.linked("theirs"));
    assert_eq!(fx.pids().len(), 1);
    // The data was re-read: the usage table lists the new skill.
    assert!(rows(&screen).iter().any(|l| l.split_whitespace().next() == Some("theirs")), "{screen:#?}");
}

#[test]
fn n_on_the_verdict_installs_nothing() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    let url = fx.repo("theirs");
    let before = fx.disk();
    let mut app = fx.app();
    add(&mut app, &url);
    shown(&mut app, "make 1 judge call? [y/N]");
    press(&mut app, KeyCode::Char('y'));
    shown(&mut app, "install theirs? [Y/n]");
    press(&mut app, KeyCode::Char('n'));
    until(&mut app, |app| !app.busy());
    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains(&format!(" add {url} · failed · Esc closes "))), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains("add declined; nothing was installed")), "{screen:#?}");
    assert_eq!(fx.disk(), before);
    assert_eq!(fx.locked("theirs"), None);
    assert_eq!(fx.pids().len(), 1);
}

#[test]
fn the_picker_in_a_dialog_installs_only_the_skill_selected() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    let url = fx.repo_of("pack", &["one", "two"]);
    let mut app = fx.app();
    add(&mut app, &url);
    let screen = shown(&mut app, "install from this source: 0 of 2 selected");
    assert!(screen.iter().any(|l| l.contains("[ ] two") && l.contains("The two skill.")), "{screen:#?}");
    assert!(screen.iter().any(|l| l == "add: pick the skills to install"), "{screen:#?}");
    for key in [KeyCode::Down, KeyCode::Char(' '), KeyCode::Enter] {
        press(&mut app, key);
    }
    shown(&mut app, "make 1 judge call? [y/N]");
    press(&mut app, KeyCode::Char('y'));
    shown(&mut app, "install two? [Y/n]");
    press(&mut app, KeyCode::Char('y'));
    until(&mut app, |app| !app.busy());
    assert_eq!(fx.locked("two").map(|l| l.subpath), Some("two".into()));
    assert_eq!(fx.locked("one"), None);
    assert!(!store::dir(&fx.data).join("one").exists());
    assert_eq!(fx.pids().len(), 1);
}

#[test]
fn declining_the_call_count_makes_no_judge_call_and_installs_nothing() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    let url = fx.repo("theirs");
    let before = fx.disk();
    let mut app = fx.app();
    add(&mut app, &url);
    shown(&mut app, "make 1 judge call? [y/N]");
    // Enter takes the default, No.
    press(&mut app, KeyCode::Enter);
    until(&mut app, |app| !app.busy());
    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains("add declined; nothing was installed")), "{screen:#?}");
    assert!(fx.pids().is_empty());
    assert_eq!(fx.disk(), before);
}

#[test]
fn a_trusted_source_installs_without_a_confirmation_or_a_judge_call() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    let url = fx.repo("ours");
    // A `file://` URL's owner is the first segment of its path.
    fx.cfg.trusted = vec![url.trim_start_matches("file:///").split('/').next().unwrap().to_string()];
    let mut app = fx.app();
    add(&mut app, &url);
    until(&mut app, |app| !app.busy());
    let screen = text(&mut app, 200, 30);
    assert!(screen.iter().any(|l| l.contains(&format!(" add {url} · done · Esc closes "))), "{screen:#?}");
    assert!(fx.locked("ours").unwrap().trusted);
    assert!(fx.pids().is_empty());
}

#[test]
fn i_with_no_judge_shows_the_refusal_and_starts_nothing() {
    let fx = Actions::new(true);
    let url = fx.repo("theirs");
    let before = fx.disk();
    let mut app = fx.app();
    add(&mut app, &url);
    assert!(!app.busy());
    let screen = text(&mut app, 200, 60);
    assert!(screen.iter().any(|l| l.contains("vetting it needs a [judge], which the config lacks")), "{screen:#?}");
    assert!(screen.iter().any(|l| l.contains("agent = \"command\"")), "{screen:#?}");
    assert!(screen.iter().any(|l| l == "any key closes"), "{screen:#?}");
    // Esc cancels the source dialog.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('i'));
    press(&mut app, KeyCode::Esc);
    assert!(!app.busy());
    assert_eq!(fx.disk(), before);
}

#[test]
fn quitting_during_an_add_kills_the_judge_and_installs_nothing() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    let url = fx.repo("theirs");
    common::write(&fx.data.parent().unwrap().join("hang"), "");
    let before = fx.disk();
    let mut app = fx.app();
    add(&mut app, &url);
    shown(&mut app, "make 1 judge call? [y/N]");
    press(&mut app, KeyCode::Char('y'));
    until(&mut app, |_| !fx.pids().is_empty());
    let pid = fx.pids()[0].clone();
    press(&mut app, KeyCode::Char('i'));
    let busy = format!("the add of {url} is running; one job at a time");
    assert!(text(&mut app, 200, 30).iter().any(|l| l.contains(&busy)));
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('q'));
    let ask = format!("the add of {url} is running; stop its judge and quit?");
    assert!(text(&mut app, 200, 30).iter().any(|l| l.contains(&ask)));
    assert!(!press(&mut app, KeyCode::Char('y')));
    assert!(!alive(&pid), "the judge outlived the TUI");
    assert_eq!(fx.disk(), before);
}

#[test]
fn q_on_the_verdict_answers_no_and_asks_to_quit() {
    let mut fx = Actions::new(true);
    fx.vetter(None);
    let url = fx.repo("theirs");
    let before = fx.disk();
    let mut app = fx.app();
    add(&mut app, &url);
    shown(&mut app, "make 1 judge call? [y/N]");
    press(&mut app, KeyCode::Char('y'));
    shown(&mut app, "install theirs? [Y/n]");
    press(&mut app, KeyCode::Char('q'));
    shown(&mut app, "stop its judge and quit?");
    // Not quitting leaves the add to end on its No.
    press(&mut app, KeyCode::Char('n'));
    until(&mut app, |app| !app.busy());
    assert!(text(&mut app, 200, 30).iter().any(|l| l.contains("add declined; nothing was installed")));
    assert_eq!(fx.disk(), before);
}
