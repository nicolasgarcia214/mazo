//! The self-contained HTML report.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use common::{Mazo, NOW, World, ago, copy_tree, event, git, grade, skill_record, stderr, stdout, write};
use jiff::tz::TimeZone;
use mazo::html;
use mazo::ledger::{Actor, Agent, Channel, Event, Origin};
use mazo::report::{self, Since, UsageArgs};
use regex::Regex;
use serde_json::Value;

/// The report fixture skills, committed, with a current grade for unslop and
/// a grade for goal-plan that a later edit made stale.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path();
    copy_tree(&common::fixture("reports/skills"), src, &|s| s);
    for skill in ["unslop", "goal-plan"] {
        grade(&src.join(".evals"), skill, "2026-09-01", "9.1", Some(&src.join(skill)));
    }
    git(src, &["init", "-q"], NOW);
    git(src, &["add", "."], NOW);
    git(src, &["commit", "-q", "-m", "skills"], "2026-09-01T00:00:00Z");
    write(&src.join("goal-plan/SKILL.md"), "---\nname: goal-plan\ndescription: Changed.\n---\n");
    git(src, &["commit", "-q", "-am", "edit"], "2026-09-02T00:00:00Z");
    dir
}

/// Events for inventory skills, a retired skill whose name needs escaping, an
/// `other` origin and a probe actor, so `usage --all` has rows of every kind.
fn events() -> Vec<Event> {
    let mut other = event("o1", "plugin:helper", Channel::Tool, "s-o1", &ago(3));
    other.origin = Origin::Other;
    let mut probe = event("p1", "probed", Channel::Slash, "s-p1", &ago(4));
    probe.actor = Actor::Probe;
    vec![
        event("u1", "unslop", Channel::Tool, "s-u1", &ago(2)),
        event("u2", "unslop", Channel::Slash, "s-u2", &ago(5)),
        event("g1", "goal-plan", Channel::Read, "s-g1", &ago(1)),
        event("r1", "x\"<retired>", Channel::Slash, "s-r1", &ago(6)),
        other,
        probe,
    ]
}

/// The report fixture's one Claude home.
fn world(source: &Path) -> World {
    World::new(source, &[("claude", Agent::Claude, "2026-09-10T08:00:00Z")])
}

fn page(w: &World, events: &[Event]) -> String {
    html::render(&report::report(&w.input(events), Since::Days(30)))
}

/// The text of the element with `id`, up to the end of its section.
fn section<'h>(html: &'h str, id: &str) -> &'h str {
    let start = html.find(&format!("id=\"{id}\"")).unwrap_or_else(|| panic!("no #{id}"));
    let end = html[start..].find("</section>").unwrap();
    &html[start..start + end]
}

/// No `src` attribute, every `href` an in-page anchor to an existing id,
/// nothing loaded from outside, inline CSS for both colour schemes.
fn assert_self_contained(page: &str) {
    let attr = Regex::new(r#"(?i)\s(src|href)\s*=\s*("([^"]*)"|\S)"#).unwrap();
    let ids: BTreeSet<&str> =
        Regex::new(r#"\sid="([^"]*)""#).unwrap().captures_iter(page).map(|c| c.get(1).unwrap().as_str()).collect();
    let mut hrefs = 0;
    // Attributes live inside tags; escaped text has no `<` to open one.
    let tags = Regex::new(r"<[a-zA-Z][^>]*>").unwrap();
    for c in tags.find_iter(page).flat_map(|t| attr.captures_iter(t.as_str())) {
        assert_eq!(&c[1], "href", "no src attribute: {}", &c[0]);
        let target = c.get(3).map(|m| m.as_str()).unwrap_or_else(|| panic!("unquoted: {}", &c[0]));
        let id = target.strip_prefix('#').unwrap_or_else(|| panic!("href outside the file: {target}"));
        assert!(ids.contains(id), "href to a missing id: {target}");
        hrefs += 1;
    }
    assert!(hrefs > 0, "the usage table links to the cards");
    for outside in ["<link", "<script", "@import", "url(", "<iframe", "<img"] {
        assert!(!page.contains(outside), "{outside} in the page");
    }

    assert!(page.contains("<style>") && page.contains("--bg:"), "inline CSS");
    assert!(page.contains("@media (prefers-color-scheme: dark)"), "dark scheme");
    assert!(page.contains("color-scheme: light dark"), "both schemes");
}

/// AC2: every skill of `usage --all --json`, escaped, is in the page.
#[test]
fn every_usage_all_skill_appears() {
    let repo = repo();
    let (w, events) = (world(repo.path()), events());
    let args = UsageArgs { since: Since::Days(30), agent: None, channel: None, all: true, authoring: false };
    let usage: Value = serde_json::from_str(&report::usage(&w.input(&events), &args, true, false)).unwrap();
    let skills: BTreeSet<&str> =
        usage["rows"].as_array().unwrap().iter().map(|r| r["skill"].as_str().unwrap()).collect();
    for s in ["unslop", "goal-smart", "x\"<retired>", "plugin:helper", "probed"] {
        assert!(skills.contains(s), "usage --all lists {s}");
    }
    let page = page(&w, &events);
    for s in &skills {
        let escaped = s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
        assert!(page.contains(&format!(">{escaped}<")), "{s} missing from the page");
    }
    assert!(!page.contains("x\"<retired>"), "names are escaped");
}

/// The page's model is the `--json` model: its usage is `usage --all --json`
/// and each card is `show --json`, so the page cannot drift from either.
#[test]
fn the_report_model_serializes_to_the_json_outputs() {
    let repo = repo();
    let (w, events) = (world(repo.path()), events());
    let input = w.input(&events);
    let model = serde_json::to_value(report::report(&input, Since::Days(30))).unwrap();
    let args = UsageArgs { since: Since::Days(30), agent: None, channel: None, all: true, authoring: false };
    let usage: Value = serde_json::from_str(&report::usage(&input, &args, true, false)).unwrap();
    assert_eq!(model["usage"], usage);
    let cards = model["cards"].as_array().unwrap();
    assert_eq!(cards.len(), w.inv.skills.len(), "one card per inventory skill");
    for card in cards {
        let skill = card["skill"].as_str().unwrap();
        let show: Value =
            serde_json::from_str(&report::show(&input, skill, Since::Days(30), 20, true, false).unwrap()).unwrap();
        assert_eq!(*card, show, "{skill}'s card");
    }
}

/// A hostile description, default prompt, real prompt, cwd and src path come
/// out as text, and the page stays self-contained.
#[test]
fn hostile_text_is_escaped() {
    let repo = repo();
    let src = repo.path();
    write(&src.join("unslop/SKILL.md"), "---\nname: unslop\ndescription: \"<script>alert('d')</script> & co\"\n---\n");
    write(&src.join("unslop/agents/openai.yaml"), "interface:\n  default_prompt: \"<img src=x onerror=alert(1)>\"\n");
    let prompt = "</blockquote><script>alert(\"p\")</script> tighten this please";
    let transcript = src.join("t<i>\"'.jsonl");
    write(
        &transcript,
        &format!("{}\n", serde_json::json!({"type": "user", "message": {"role": "user", "content": prompt}})),
    );
    let mut e = event("h1", "unslop", Channel::Tool, "s-h1", &ago(1));
    e.cwd = Some("/w/<b>\"cwd\"</b>".into());
    e.src = format!("{}:1", transcript.display());
    let page = page(&world(src), &[e]);

    let card = section(&page, "skill-unslop");
    for escaped in [
        "&lt;script&gt;alert(&#39;d&#39;)&lt;/script&gt; &amp; co",
        "&lt;img src=x onerror=alert(1)&gt;",
        "&lt;/blockquote&gt;&lt;script&gt;alert(&quot;p&quot;)&lt;/script&gt; tighten this please",
        "/w/&lt;b&gt;&quot;cwd&quot;&lt;/b&gt;",
        "t&lt;i&gt;&quot;&#39;.jsonl:1",
    ] {
        assert!(card.contains(escaped), "{escaped} missing: {card}");
    }
    assert!(card.contains("<blockquote>&lt;/blockquote&gt;"), "the real prompt is rendered as an example");
    for raw in ["<script>", "<img", "<b>", "<i>"] {
        assert!(!page.contains(raw), "{raw} unescaped");
    }
    assert_self_contained(&page);
}

/// AC3: nothing outside the file, inline CSS and SVG, both colour schemes.
#[test]
fn the_page_is_self_contained() {
    let repo = repo();
    let page = page(&world(repo.path()), &events());

    assert_self_contained(&page);

    let usage = &page[page.find("<h2>Usage</h2>").unwrap()..page.find("coverage:").unwrap()];
    let rows = usage.matches("<tr><td>").count();
    assert_eq!(rows, 9, "six inventory skills, the retired one, the other origin and the probe");
    assert!(!usage.contains("<svg"), "no 30-day strip");
    let unslop = section(&page, "skill-unslop");
    assert!(unslop.contains("<svg") && unslop.contains("strong 1 weak 0"), "the card histogram: {unslop}");
}

/// The legend is the help's marks and glyphs, the source the TUI's help
/// and `mazo usage` share.
#[test]
fn the_legend_lists_the_helps_marks() {
    let repo = repo();
    let page = page(&world(repo.path()), &events());
    let legend = &page[page.find("legend: ").unwrap()..];
    let legend = &legend[..legend.find("</p>").unwrap()];
    assert_eq!(
        legend,
        "legend: agents C Claude, X Codex, P Pi; \
         ● name and description in context: Claude on, every Codex skill, Pi by default; \
         ◐ name only in context: Claude name-only; \
         ○ explicit invocation only: Claude user-invocable-only or disable-model-invocation, Pi disable-model-invocation; \
         × off: Claude off; \
         stale the skill changed since the grade; (project) a project skill, counted apart"
    );
    assert_eq!(legend, format!("legend: {}", mazo::help::legend(true)));
}

/// AC4: each missing or stale grade names `/grade <skill>`; a current grade
/// and a skill outside the inventory do not appear.
#[test]
fn grades_to_run_name_the_command() {
    let repo = repo();
    let page = page(&world(repo.path()), &events());
    let list = &page[page.find("<h2>Grades to run</h2>").unwrap()..];
    let list = &list[..list.find("</ul>").unwrap()];
    let due: Vec<(&str, &str)> = Regex::new(r"<code>/grade ([^<]+)</code> <span class=.muted.>(\w+)</span>")
        .unwrap()
        .captures_iter(list)
        .map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()))
        .collect();
    assert_eq!(
        due,
        [
            ("goal-plan", "stale"),
            ("goal-execute", "missing"),
            ("goal-smart", "missing"),
            ("wfa-helper", "missing"),
            ("writing-for-agents", "missing"),
        ]
    );
}

/// A config with one Claude home under a private `HOME` and data directory.
struct Bin {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Bin {
    fn new() -> Bin {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        write(
            &root.join("skills/mazo.toml"),
            "source = \".\"\n\n[[home]]\nname = \"claude\"\nagent = \"claude\"\npath = \"~/.claude\"\n",
        );
        write(&root.join("skills/unslop/SKILL.md"), "---\nname: unslop\ndescription: U.\n---\n");
        write(&root.join("home/.claude/settings.json"), "{}\n");
        fs::create_dir_all(root.join("bin")).unwrap();
        Bin { _temp: temp, root }
    }

    fn run(&self) -> Mazo {
        let root = &self.root;
        Mazo::new(root, &root.join("home"), &root.join("skills/mazo.toml"), &root.join("data"))
    }

    fn mazo(&self, args: &[&str]) -> Output {
        self.run().output(args)
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// AC1: the default output is `<data dir>/report.html`, mode 0600, and a
/// previous file of any mode is overwritten.
#[test]
fn report_writes_the_data_dir_file_with_mode_0600() {
    let b = Bin::new();
    let file = b.root.join("data/mazo/report.html");
    write(&file, "old report\n");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

    let out = b.mazo(&["report"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("wrote {}\n", file.display()));
    assert_eq!(mode(&file), 0o600);
    let page = fs::read_to_string(&file).unwrap();
    assert!(page.starts_with("<!doctype html>") && !page.contains("old report"), "overwritten");
    assert!(page.contains("id=\"skill-unslop\""));
    assert!(!stderr(&out).contains("git worktree"));
}

/// A symlink at `--out` is replaced by the report, not followed: its target
/// keeps its text and mode.
#[test]
fn out_replaces_a_symlink_instead_of_writing_through_it() {
    let b = Bin::new();
    let target = b.root.join("target.txt");
    write(&target, "keep me\n");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let link = b.root.join("report.html");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let out = b.mazo(&["report", "--out", link.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!((fs::read_to_string(&target).unwrap().as_str(), mode(&target)), ("keep me\n", 0o644));
    assert!(fs::symlink_metadata(&link).unwrap().is_file());
    assert_eq!(mode(&link), 0o600);
    assert!(fs::read_to_string(&link).unwrap().starts_with("<!doctype html>"));
}

/// AC5: `--out` inside a git worktree warns, outside it does not, and
/// `--open` runs the platform opener on the file.
#[test]
fn out_warns_in_a_worktree_and_open_runs_the_opener() {
    let b = Bin::new();
    git(&b.root.join("skills"), &["init", "-q"], NOW);
    let inside = b.root.join("skills/out/report.html");
    let out = b.mazo(&["report", "--out", inside.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("is inside a git worktree"));
    assert_eq!(mode(&inside), 0o600);

    let log = b.root.join("opened");
    for opener in ["xdg-open", "open"] {
        let stub = b.root.join("bin").join(opener);
        write(&stub, &format!("#!/bin/sh\necho \"{opener} $@\" > '{}'\n", log.display()));
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Relative to mazo's cwd; the opener gets the absolute path.
    let outside = b.root.join("elsewhere/report.html");
    let out = b.mazo(&["report", "--out", "elsewhere/report.html", "--open"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!stderr(&out).contains("git worktree"));
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    assert_eq!(fs::read_to_string(&log).unwrap(), format!("{opener} {}\n", outside.display()));
}

/// Every time on the page is in the local zone, as `show` prints it: an event
/// at 20:30 UTC yesterday is 05:30 today in Tokyo, in the events table, the
/// usage `last`, the card's last use and its histogram alike.
#[test]
fn times_are_local_under_a_fixed_zone() {
    let b = Bin::new();
    let utc_day = jiff::Timestamp::now().to_zoned(TimeZone::UTC).date().yesterday().unwrap();
    let ts = utc_day.at(20, 30, 0, 0).to_zoned(TimeZone::UTC).unwrap().timestamp();
    let jst = TimeZone::fixed(jiff::tz::offset(9));
    let local = ts.to_zoned(jst).date();
    let mut e = event("t1", "unslop", Channel::Slash, "s-t1", &ago(0));
    e.ts = ts.to_string();
    write(&b.root.join("data/mazo/events.jsonl"), &format!("{}\n", serde_json::to_string(&e).unwrap()));

    let out = b.run().command(&[("TZ", "JST-9".as_ref())], &["report"]).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let page = fs::read_to_string(b.root.join("data/mazo/report.html")).unwrap();
    assert!(page.contains(&format!("<td>{local} 05:30</td>")), "event time: {page}");
    assert!(!page.contains(&format!("{utc_day} 20:30")), "no UTC time");
    assert!(page.contains(&format!("<td>{local}</td>")), "usage last");
    assert!(page.contains(&format!("<dt>last use</dt><dd>{local}</dd>")), "card last use");
    assert!(page.contains(&format!("<title>{local} strong 1 weak 0</title>")), "histogram day");
    assert!(page.contains("times and days in JST"), "the zone is named");
    let generated = Regex::new(r"generated \d{4}-\d\d-\d\d \d\d:\d\d ·").unwrap();
    assert!(generated.is_match(&page), "generated to the minute");
}

/// The usage table's audit column carries each skill's latest audit
/// verdict, else its vet verdict, else `not judged`, and the card shows its date, session count, rates, top three improvements
/// and curated examples, escaped; an older record is not shown.
#[test]
fn the_latest_audit_is_in_the_usage_table_and_the_card() {
    let repo = repo();
    let data = tempfile::tempdir().unwrap();
    let latest = "2026-10-12T10:00:00Z";
    let audits = skill_record("OLD VERDICT", "2026-10-01T10:00:00Z")
        + "{\"v\":1,\"kind\":\"session\",\"skill\":\"unslop\",\"session\":\"s1\"}\n"
        + &skill_record("Good on <short> texts & \"notes\"", latest);
    write(&data.path().join("audits.jsonl"), &audits);
    common::vetted(data.path());
    let mut w = world(repo.path());
    w.inv = mazo::store::inventory(Some(repo.path()), data.path()).unwrap();
    w.audits = mazo::audit::latest(data.path()).unwrap();
    let page = page(&w, &events());

    let verdict = "Good on &lt;short&gt; texts &amp; &quot;notes&quot;";
    let usage = &page[page.find("<h2>Usage</h2>").unwrap()..page.find("coverage:").unwrap()];
    assert!(usage.contains("<th>audit</th>"), "{usage}");
    assert!(usage.contains(&format!("<td>audit: {verdict}</td></tr>")), "{usage}");
    assert!(usage.contains("<td>vet: quality weak, value low value</td></tr>"), "{usage}");
    assert_eq!(usage.matches("<td>not judged</td></tr>").count(), 8, "{usage}");
    assert!(!page.contains("OLD VERDICT"), "only the latest record is shown");

    let card = section(&page, "skill-unslop");
    assert!(card.contains("<dt>audit</dt>") && card.contains(&format!("{}, 4 sessions", &latest[..10])), "{card}");
    assert!(card.contains(verdict));
    assert!(card.contains("followed 3 of 4, achieved 2 of 4, abandoned or redone 1 of 4"), "{card}");
    assert!(card.contains("Change 3.") && !card.contains("Change 4."), "the top three improvements");
    assert!(card.contains("<dt>curated</dt>") && card.contains("&lt;b&gt;note&lt;/b&gt;"), "{card}");
    assert!(!page.contains("<b>"), "escaped");
    assert!(section(&page, "skill-goal-plan").contains("<dt>audit</dt><dd>none</dd>"));
    assert_self_contained(&page);
}
