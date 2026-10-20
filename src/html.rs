//! The self-contained HTML report: one file with inline CSS and
//! inline SVG, rendered from the JSON model of `report::report`.

use std::fs::{self, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

use jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::Value;

use crate::audit::Rates;
use crate::render::{self, DAY, MINUTE};
use crate::report::{self, AUDIT_IMPROVEMENTS, Input, Since, tilde};

const CSS: &str = "
:root { color-scheme: light dark; --bg: #ffffff; --fg: #1f2328; --muted: #656d76; --line: #d0d7de;
  --strong: #0969da; --weak: #9ec5f4; --warn: #9a6700; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #0d1117; --fg: #e6edf3; --muted: #8d96a0; --line: #30363d;
    --strong: #4493f8; --weak: #1f4a7d; --warn: #d29922; }
}
body { background: var(--bg); color: var(--fg); font: 14px/1.45 system-ui, sans-serif; margin: 0 auto;
  max-width: 1100px; padding: 16px; }
a { color: var(--strong); }
h1, h2 { margin: 1.4em 0 .4em; }
section { border-top: 1px solid var(--line); }
table { border-collapse: collapse; margin: .4em 0; }
th, td { padding: 2px 8px; text-align: left; border-bottom: 1px solid var(--line); vertical-align: middle; }
td.n, th.n { text-align: right; font-variant-numeric: tabular-nums; }
code, .mono { font-family: ui-monospace, monospace; font-size: 13px; }
.muted { color: var(--muted); }
.stale { color: var(--warn); }
.wrap { overflow-x: auto; }
svg .s { fill: var(--strong); }
svg .w { fill: var(--weak); }
svg .z { fill: var(--line); }
dl { display: grid; grid-template-columns: max-content 1fr; gap: 2px 12px; }
dt { color: var(--muted); }
dd { margin: 0; }
blockquote { margin: 2px 0 8px; padding-left: 8px; border-left: 2px solid var(--line); white-space: pre-wrap; }
ul, ol { margin: 0; padding-left: 18px; }
";

const LEGEND: &str = "agents C Claude, X Codex, P Pi · the 30d chart has one bar per day, a grey tick for none · \
                      +N commits since the grade · +wip uncommitted changes since the grade";

/// Escapes text for an element body or a quoted attribute.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A scalar as escaped text, `-` for null or absent.
fn text(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::String(s) => esc(s),
        v => esc(&v.to_string()),
    }
}

fn num(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// A timestamp in the report's zone, as `show` prints it; `-` for none.
fn local(v: &Value, tz: &TimeZone, format: &str) -> String {
    v.as_str().map_or("-".into(), |t| esc(&render::local(t, tz, format)))
}

fn anchor(skill: &str) -> String {
    format!("skill-{}", esc(skill))
}

/// The score as written, then `+N` or `+wip`.
fn grade(g: &Value) -> String {
    if g.is_null() {
        return "-".into();
    }
    let score = format!("{}{}", g["score"].as_str().map_or("?".into(), esc), text(&g["mark"]));
    if g["stale"].as_bool() == Some(true) { format!("<span class=\"stale\">{score}</span>") } else { score }
}

/// The 30-day sparkline: one bar per day, a tick for a day with no event.
fn sparkline(counts: &[Value]) -> String {
    let max = counts.iter().map(num).max().unwrap_or(0).max(1);
    let bars: String = counts
        .iter()
        .enumerate()
        .map(|(i, c)| match num(c) {
            0 => format!("<rect class=\"z\" x=\"{}\" y=\"15\" width=\"2\" height=\"1\"/>", i * 3),
            c => {
                let h = (c * 16).div_ceil(max);
                format!("<rect class=\"s\" x=\"{}\" y=\"{}\" width=\"2\" height=\"{h}\"/>", i * 3, 16 - h)
            }
        })
        .collect();
    format!("<svg width=\"{}\" height=\"16\" role=\"img\">{bars}</svg>", counts.len() * 3)
}

/// The card histogram: one column per day with events, strong below weak.
fn histogram(days: &[Value]) -> String {
    if days.is_empty() {
        return "<p class=\"muted\">no events in the window</p>".into();
    }
    const H: u64 = 60;
    let max = days.iter().map(|d| num(&d["strong"]) + num(&d["weak"])).max().unwrap_or(0).max(1);
    let cols: String = days
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let (s, w) = (num(&d["strong"]), num(&d["weak"]));
            let (sh, wh) = (s * H / max, w * H / max);
            let x = i * 10;
            format!(
                "<g><title>{} strong {s} weak {w}</title>\
                 <rect class=\"w\" x=\"{x}\" y=\"{}\" width=\"8\" height=\"{wh}\"/>\
                 <rect class=\"s\" x=\"{x}\" y=\"{}\" width=\"8\" height=\"{sh}\"/></g>",
                text(&d["date"]),
                H - sh - wh,
                H - sh,
            )
        })
        .collect();
    let first = text(&days[0]["date"]);
    let last = text(&days[days.len() - 1]["date"]);
    format!(
        "<div class=\"wrap\"><svg width=\"{}\" height=\"{H}\" role=\"img\">{cols}</svg></div>\
         <p class=\"muted\">{first} to {last}; dark strong, light weak</p>",
        days.len() * 10,
    )
}

fn list(xs: &Value) -> &[Value] {
    xs.as_array().map_or(&[], Vec::as_slice)
}

fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let head: String = header.iter().map(|h| format!("<th>{h}</th>")).collect();
    let body: String = rows
        .iter()
        .map(|r| format!("<tr>{}</tr>", r.iter().map(|c| format!("<td>{c}</td>")).collect::<String>()))
        .collect();
    format!("<div class=\"wrap\"><table><tr>{head}</tr>{body}</table></div>")
}

fn usage(model: &Value, tz: &TimeZone) -> String {
    let cards = list(&model["cards"]);
    let card = |skill: &str| cards.iter().find(|c| c["skill"].as_str() == Some(skill));
    let n = |v: &Value| format!("<td class=\"n\">{}</td>", text(v));
    let rows: String = list(&model["usage"]["rows"])
        .iter()
        .map(|r| {
            let skill = r["skill"].as_str().unwrap_or_default();
            let name = if card(skill).is_some() {
                format!("<a href=\"#{}\">{}</a>", anchor(skill), esc(skill))
            } else {
                esc(skill)
            };
            format!(
                "<tr><td>{name}</td>{}{}{}{}{}{}<td class=\"mono\">{}</td><td>{}</td><td>{}</td><td class=\"n\">{}</td><td>{}</td></tr>",
                n(&r["sess"]),
                n(&r["slash"]),
                n(&r["tool"]),
                n(&r["typed"]),
                n(&r["read"]),
                n(&r["ref"]),
                text(&r["agents"]),
                local(&r["last"], tz, DAY),
                sparkline(list(&r["30d"])),
                grade(&r["grade"]),
                card(skill).map_or("-".into(), |c| text(&c["audit"]["result"]["verdict"])),
            )
        })
        .collect();
    let head: String =
        ["skill", "sess", "slash", "tool", "typed", "read", "ref", "agents", "last", "30d", "grade", "audit"]
            .iter()
            .enumerate()
            .map(|(i, h)| {
                if (1..=6).contains(&i) || i == 10 {
                    format!("<th class=\"n\">{h}</th>")
                } else {
                    format!("<th>{h}</th>")
                }
            })
            .collect();
    format!("<h2>Usage</h2><div class=\"wrap\"><table><tr>{head}</tr>{rows}</table></div>")
}

fn coverage(model: &Value) -> String {
    let parts: Vec<String> = model["usage"]["coverage"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(home, c)| format!("{} {}", esc(home), text(c)))
        .collect();
    format!("<p class=\"muted\">coverage: {}</p><p class=\"muted\">legend: {LEGEND}</p>", parts.join(" · "))
}

fn grades_due(model: &Value) -> String {
    let due = list(&model["grades_due"]);
    if due.is_empty() {
        return "<h2>Grades</h2><p>Every skill has a current grade.</p>".into();
    }
    let items: String = due
        .iter()
        .map(|g| {
            format!("<li><code>{}</code> <span class=\"muted\">{}</span></li>", text(&g["command"]), text(&g["state"]))
        })
        .collect();
    format!("<h2>Grades to run</h2><ul>{items}</ul>")
}

fn links(ls: &Value) -> String {
    let names: Vec<String> = list(ls)
        .iter()
        .map(|l| {
            let skill = l["skill"].as_str().unwrap_or_default();
            let held = if l["held"].as_bool() == Some(true) { " (held)" } else { "" };
            format!("<a href=\"#{}\">{}</a>{held}", anchor(skill), esc(skill))
        })
        .collect();
    if names.is_empty() { "none".into() } else { names.join(", ") }
}

/// The card's examples: the invocation per agent, the manifest's
/// default prompt, and real prompts read from transcripts.
fn examples(ex: &Value, tz: &TimeZone) -> String {
    let written: Vec<String> = list(&ex["written"])
        .iter()
        .map(|i| format!("{} <code>{}</code>", text(&i["agent"]), text(&i["text"])))
        .collect();
    let mut out =
        format!("<dt>invoke</dt><dd>{}</dd>", if written.is_empty() { "-".into() } else { written.join(", ") });
    if !ex["default_prompt"].is_null() {
        out.push_str(&format!("<dt>default prompt</dt><dd>{}</dd>", text(&ex["default_prompt"])));
    }
    let real: String = list(&ex["real"])
        .iter()
        .map(|r| {
            let cwd = r["cwd"].as_str().map_or("-".into(), |p| esc(&tilde(p)));
            format!(
                "<li><span class=\"muted\">{} {} {cwd}</span><blockquote>{}</blockquote></li>",
                local(&r["ts"], tz, DAY),
                text(&r["agent"]),
                text(&r["text"]),
            )
        })
        .collect();
    let real = if real.is_empty() { "none".into() } else { format!("<ul>{real}</ul>") };
    out.push_str(&format!("<dt>examples</dt><dd>{real}</dd>"));
    let curated: String = list(&ex["curated"])
        .iter()
        .map(|c| {
            format!(
                "<li>{} <span class=\"muted\">asked</span> {} <span class=\"muted\">happened</span> {} \
                 <span class=\"muted\">tip</span> {} <span class=\"muted mono\">{}</span></li>",
                text(&c["situation"]),
                text(&c["asked"]),
                text(&c["happened"]),
                text(&c["tip"]),
                text(&c["session"]),
            )
        })
        .collect();
    if !curated.is_empty() {
        out.push_str(&format!("<dt>curated</dt><dd><ul>{curated}</ul></dd>"));
    }
    out
}

/// The latest audit: its date and session count, verdict, the
/// rates skima computed and the top three improvements.
fn audit(a: &Value, tz: &TimeZone) -> String {
    if a.is_null() {
        return "<dt>audit</dt><dd>none</dd>".into();
    }
    // The model is serialized from a `SkillRecord`, so its rates read back.
    let rates: Vec<String> = Rates::deserialize(&a["rates"])
        .map(|r| r.labeled().iter().map(|(l, r)| format!("{l} {}", r.of())).collect())
        .unwrap_or_default();
    let improvements: String = list(&a["result"]["improvements"])
        .iter()
        .take(AUDIT_IMPROVEMENTS)
        .map(|i| format!("<li>{} <span class=\"muted\">{}</span></li>", text(&i["change"]), text(&i["reason"])))
        .collect();
    format!(
        "<dt>audit</dt><dd><span class=\"muted\">{}, {} sessions, {} billing</span> {}<br>{}<ol>{improvements}</ol></dd>",
        local(&a["audited_at"], tz, DAY),
        list(&a["sessions"]).len(),
        esc(a["judge"]["billing"].as_str().unwrap_or("unknown")),
        text(&a["result"]["verdict"]),
        rates.join(", "),
    )
}

fn card(c: &Value, tz: &TimeZone) -> String {
    let skill = c["skill"].as_str().unwrap_or_default();
    let from = c["since"].as_str().map_or("all time".into(), |d| format!("since {}", esc(d)));
    let events: Vec<Vec<String>> = list(&c["events"])
        .iter()
        .map(|e| {
            let ts = local(&e["ts"], tz, MINUTE);
            let cwd = e["cwd"].as_str().map_or("-".into(), |p| esc(&tilde(p)));
            let mut src = esc(&tilde(e["src"].as_str().unwrap_or_default()));
            if e["expired"].as_bool() == Some(true) {
                src.push_str(" <span class=\"muted\">expired</span>");
            }
            vec![
                ts,
                text(&e["agent"]),
                text(&e["channel"]),
                text(&e["actor"]),
                cwd,
                format!("<span class=\"mono\">{src}</span>"),
            ]
        })
        .collect();
    // The committer's own date, as `show` prints it.
    let commit = |v: &Value| {
        let date = v["date"].as_str().unwrap_or_default();
        format!("{} {}", esc(date.get(..10).unwrap_or(date)), text(&v["subject"]))
    };
    let history = match &c["history"] {
        Value::Null => String::new(),
        h => {
            let mut line = if h["first"].is_null() {
                "never committed".to_string()
            } else {
                format!("first {}, last {}", commit(&h["first"]), commit(&h["last"]))
            };
            if h["uncommitted"].as_bool() == Some(true) {
                line.push_str(", uncommitted changes");
            }
            format!("<dt>history</dt><dd>{line}</dd>")
        }
    };
    let grades: Vec<Vec<String>> = list(&c["grades"]).iter().map(|g| vec![text(&g["date"]), grade(g)]).collect();
    let retros: Vec<Vec<String>> =
        list(&c["retros"]).iter().map(|r| vec![text(&r["date"]), text(&r["score"]), text(&r["verdict"])]).collect();
    let strong: Vec<String> = list(&c["usage"]["strong"])
        .iter()
        .map(|b| format!("{} {} {}", text(&b["agent"]), text(&b["channel"]), text(&b["events"])))
        .collect();
    let projects: Vec<String> = list(&c["usage"]["projects"])
        .iter()
        .map(|p| format!("{} {}", esc(&tilde(p["cwd"].as_str().unwrap_or_default())), text(&p["sessions"])))
        .collect();
    let or_none = |xs: Vec<String>| if xs.is_empty() { "none".into() } else { xs.join(", ") };
    let retro_note = match c["strong_since_retro"].as_u64() {
        None => "none".into(),
        Some(n) => format!("{n} strong since the latest"),
    };
    format!(
        "<section id=\"{id}\"><h2>{name}</h2><p class=\"muted\">{from}</p>{hist}\
         <h3>Last {shown} of {total} events</h3>{events}\
         <dl><dt>description</dt><dd>{desc}</dd><dt>path</dt><dd class=\"mono\">{path}</dd>{history}\
         <dt>cites</dt><dd>{cites}</dd><dt>cited by</dt><dd>{cited_by}</dd>\
         <dt>strong</dt><dd>{strong}</dd><dt>last use</dt><dd>{last_use}</dd><dt>projects</dt><dd>{projects}</dd>\
         <dt>grades</dt><dd>{grades}</dd><dt>retros</dt><dd>{retro_note}{retros}</dd>{examples}{audit}</dl></section>",
        id = anchor(skill),
        name = esc(skill),
        hist = histogram(list(&c["histogram"])),
        shown = list(&c["events"]).len(),
        total = text(&c["total_events"]),
        events = table(&["ts", "agent", "channel", "actor", "cwd", "src"], &events),
        desc = text(&c["description"]),
        path = esc(&tilde(c["path"].as_str().unwrap_or_default())),
        cites = links(&c["cites"]),
        cited_by = links(&c["cited_by"]),
        strong = or_none(strong),
        last_use = local(&c["usage"]["last_use"], tz, DAY),
        projects = or_none(projects),
        grades = if grades.is_empty() { "none".into() } else { table(&["date", "score"], &grades) },
        retros = if retros.is_empty() { String::new() } else { table(&["date", "score", "verdict"], &retros) },
        examples = examples(&c["examples"], tz),
        audit = audit(&c["audit"], tz),
    )
}

/// The report page for the model of `report::report` over `input` and
/// `since`. Times are shown in `input.tz`, the zone the model's days were
/// bucketed in.
pub fn render(input: &Input, since: Since) -> crate::Result<String> {
    let model = &report::report(input, since)?;
    let tz = &input.tz;
    let from = model["since"].as_str().map_or("all time".into(), |d| format!("since {}", esc(d)));
    let cards: String = list(&model["cards"]).iter().map(|c| card(c, tz)).collect();
    Ok(format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>skima report</title><style>{CSS}</style></head>\n<body><h1>skima report</h1>\
         <p class=\"muted\">generated {} · usage {from} · times and days in {}</p>{}{}{}<h2>Skills</h2>{cards}</body></html>\n",
        local(&model["generated"], tz, MINUTE),
        local(&model["generated"], tz, "%Z"),
        usage(model, tz),
        coverage(model),
        grades_due(model),
    ))
}

/// Writes `html` to `path` with mode 0600, replacing any previous file:
/// the page shows cwds and prompt excerpts, the one place prompt text is written.
pub fn write(path: &Path, html: &str) -> crate::Result<()> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(err)?;
    }
    let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(err)?;
    // `mode` applies only on create; an older file keeps its own until this.
    f.set_permissions(Permissions::from_mode(0o600)).map_err(err)?;
    f.write_all(html.as_bytes()).map_err(err)
}

/// Whether the directory holding `path` is inside a git worktree.
pub fn in_worktree(path: &Path) -> bool {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    crate::inventory::git(dir, &["rev-parse", "--is-inside-work-tree"]).is_some_and(|s| s.trim() == "true")
}

/// Opens `path` in the default browser with `open` on macOS, `xdg-open` elsewhere.
pub fn open(path: &Path) -> crate::Result<()> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let status = Command::new(opener).arg(path).status().map_err(|e| format!("{opener}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{opener} {}: {status}", path.display())) }
}
