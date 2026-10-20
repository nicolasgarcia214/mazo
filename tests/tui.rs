//! The TUI of bare `skima`: its views rendered into ratatui's `TestBackend`
//! from the golden fixture, and the binary opening and quitting it on a
//! pseudo-terminal.

mod common;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;

use common::golden::Golden;
use skima::ledger::{Agent, Channel};
use skima::render::{self, Colour};
use skima::report::{self, Input, Since, UsageArgs};
use skima::tui::{App, Screen};

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
    let mut app = App::new(&input);
    let screen = draw(&mut app, 200, 40);
    let lines: Vec<String> = screen.iter().map(|(t, _)| t.clone()).collect();
    let unslop =
        report::usage_table(&input, &UsageArgs::new(Since::Days(30))).rows.into_iter().find(|r| r.skill == "unslop");
    let sess = unslop.unwrap().sess;
    assert!(sess > 0);
    assert_eq!(row(&lines, "unslop")[1], sess.to_string());
    assert!(row(&lines, "unslop").last().unwrap().ends_with("+1"), "{lines:#?}");
    assert_eq!(colour_of(&screen, "unslop", "+1"), ratatui_colour("+1"));
    assert_eq!(colour_of(&screen, "unslop", "unslop"), Color::Reset);
}

#[test]
fn enter_opens_the_card_whose_audit_line_matches_show_json_and_esc_goes_back() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let mut app = App::new(&input);
    typed(&mut app, "/unslop");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.screen(), Screen::Card);
    let screen = text(&mut app, 200, 100);
    assert_eq!(screen[0], format!("unslop: since {}", common::ago(30).get(..10).unwrap()));

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
    let mut app = App::new(&input);
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
    assert!(text(&mut app, 200, 40)[0].starts_with(&format!("{skill}: ")));
}

#[test]
fn slash_leaves_only_the_rows_whose_name_matches() {
    let golden = Golden::new();
    let input = golden.world.input(&golden.events);
    let mut app = App::new(&input);
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
        let mut app = App::new(&input);
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
    let mut app = App::new(&input);
    let screen = text(&mut app, 60, 30);
    let keys = screen.iter().rev().take_while(|l| !l.starts_with("usage · ")).flat_map(|l| l.split(" · ")).count();
    assert_eq!(keys, 10, "every key shows: {screen:#?}");
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
    let mut app = App::new(&input);
    assert!(press(&mut app, KeyCode::Char('j')));
    assert!(!press(&mut app, KeyCode::Char('q')));
    assert!(!app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
}

/// A source with one skill and one Claude home never scanned.
fn binary() -> (tempfile::TempDir, common::Skima) {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (home, source) = (root.join("home"), root.join("skills"));
    common::write(&source.join("idle/SKILL.md"), "---\nname: idle\ndescription: Idle.\n---\n");
    common::write(
        &source.join("skima.toml"),
        "source = \".\"\n\n[[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"~/.claude\"\n",
    );
    common::write(&home.join(".claude/settings.json"), "{}\n");
    let skima = common::Skima::new(&home, &home, &source.join("skima.toml"), &home.join("data"));
    (temp, skima)
}

#[test]
fn bare_skima_off_a_terminal_prints_help_and_exits_2() {
    let (_temp, skima) = binary();
    let out = skima.output(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(common::stderr(&out).contains("Usage: skima <COMMAND>"), "{}", common::stderr(&out));
}

/// Bare `skima` under `script` on an 80×24 pseudo-terminal: once the screen
/// opens, `byte` is written to it. The exit code and everything it printed.
#[cfg(target_os = "linux")]
fn on_a_pty(byte: u8) -> Option<(Option<i32>, Vec<u8>)> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    if std::process::Command::new("script").arg("--version").output().is_err() {
        eprintln!("skipped: no `script`");
        return None;
    }
    let (_temp, mut skima) = binary();
    skima.set("SHELL", Some("/bin/sh".as_ref()));
    let command = format!("stty rows 24 cols 80 && exec '{}'", env!("CARGO_BIN_EXE_skima"));
    let mut child = skima
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
    let mut sent = false;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(chunk) => out.extend(chunk),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                child.kill().unwrap();
                panic!("timed out: {}", String::from_utf8_lossy(&out));
            }
        }
        // The alternate screen is entered after raw mode, so the byte is a key.
        if !sent && out.windows(8).any(|w| w == b"\x1b[?1049h") {
            stdin.write_all(&[byte]).unwrap();
            sent = true;
        }
    }
    assert!(sent, "the screen never opened: {}", String::from_utf8_lossy(&out));
    Some((child.wait().unwrap().code(), out))
}

#[cfg(target_os = "linux")]
#[test]
fn bare_skima_on_a_terminal_quits_on_q_and_ctrl_c_and_restores_the_terminal() {
    for byte in [b'q', 0x03] {
        let Some((code, out)) = on_a_pty(byte) else { return };
        assert_eq!(code, Some(0), "{byte:#x}: {}", String::from_utf8_lossy(&out));
        assert!(out.ends_with(b"\x1b[?1049l\x1b[?25h"), "{byte:#x}: {:?}", String::from_utf8_lossy(&out));
    }
}
