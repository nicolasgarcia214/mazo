//! The self-contained HTML report: one file with inline CSS and
//! inline SVG, rendered from the typed model of `report::report`.

use std::fs::{self, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

use jiff::tz::TimeZone;

use crate::audit::SkillRecord;
use crate::config::Billing;
use crate::examples::Examples;
use crate::ledger::name;
use crate::render::{self, DAY, MINUTE};
use crate::report::{self, AUDIT_IMPROVEMENTS, Card, Day, GradeOut, Link, Report, tilde};

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

/// A timestamp in the report's zone, as `show` prints it; `-` for none.
fn local(t: Option<&str>, tz: &TimeZone, format: &str) -> String {
    t.map_or("-".into(), |t| esc(&render::local(t, tz, format)))
}

/// Escaped text, `-` for none.
fn or_dash(s: Option<&str>) -> String {
    s.map_or("-".into(), esc)
}

fn anchor(skill: &str) -> String {
    format!("skill-{}", esc(skill))
}

/// The score as written, then `+N` or `+wip`.
fn grade(g: Option<&GradeOut>) -> String {
    let Some(g) = g else { return "-".into() };
    let score = format!("{}{}", esc(g.grade.eval.score.as_deref().unwrap_or("?")), esc(&g.mark));
    if g.stale { format!("<span class=\"stale\">{score}</span>") } else { score }
}

/// The 30-day sparkline: one bar per day, a tick for a day with no event.
fn sparkline(counts: &[usize]) -> String {
    let max = counts.iter().copied().max().unwrap_or(0).max(1);
    let bars: String = counts
        .iter()
        .enumerate()
        .map(|(i, &c)| match c {
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
fn histogram(days: &[Day]) -> String {
    let (Some(first), Some(last)) = (days.first(), days.last()) else {
        return "<p class=\"muted\">no events in the window</p>".into();
    };
    const H: usize = 60;
    let max = days.iter().map(|d| d.strong + d.weak).max().unwrap_or(0).max(1);
    let cols: String = days
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let (s, w) = (d.strong, d.weak);
            let (sh, wh) = (s * H / max, w * H / max);
            let x = i * 10;
            format!(
                "<g><title>{} strong {s} weak {w}</title>\
                 <rect class=\"w\" x=\"{x}\" y=\"{}\" width=\"8\" height=\"{wh}\"/>\
                 <rect class=\"s\" x=\"{x}\" y=\"{}\" width=\"8\" height=\"{sh}\"/></g>",
                esc(&d.date),
                H - sh - wh,
                H - sh,
            )
        })
        .collect();
    format!(
        "<div class=\"wrap\"><svg width=\"{}\" height=\"{H}\" role=\"img\">{cols}</svg></div>\
         <p class=\"muted\">{} to {}; dark strong, light weak</p>",
        days.len() * 10,
        esc(&first.date),
        esc(&last.date),
    )
}

fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let head: String = header.iter().map(|h| format!("<th>{h}</th>")).collect();
    let body: String = rows
        .iter()
        .map(|r| format!("<tr>{}</tr>", r.iter().map(|c| format!("<td>{c}</td>")).collect::<String>()))
        .collect();
    format!("<div class=\"wrap\"><table><tr>{head}</tr>{body}</table></div>")
}

fn usage(model: &Report) -> String {
    let card = |skill: &str| model.cards.iter().find(|c| c.skill == skill);
    let n = |v: usize| format!("<td class=\"n\">{v}</td>");
    let rows: String = model
        .usage
        .rows
        .iter()
        .map(|r| {
            let name = if card(&r.skill).is_some() {
                format!("<a href=\"#{}\">{}</a>", anchor(&r.skill), esc(&r.skill))
            } else {
                esc(&r.skill)
            };
            let verdict = card(&r.skill).and_then(|c| c.audit).map(|a| a.result.verdict.as_str());
            format!(
                "<tr><td>{name}</td>{}{}{}{}{}{}<td class=\"mono\">{}</td><td>{}</td><td>{}</td><td class=\"n\">{}</td><td>{}</td></tr>",
                n(r.sess),
                n(r.slash),
                n(r.tool),
                n(r.typed),
                n(r.read),
                n(r.ref_),
                esc(&r.agents),
                local(r.last.as_deref(), &model.tz, DAY),
                sparkline(&r.last_30d),
                grade(r.grade.as_ref()),
                or_dash(verdict),
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

fn coverage(model: &Report) -> String {
    let parts: Vec<String> = model.usage.coverage.iter().map(|(home, c)| format!("{} {}", esc(home), esc(c))).collect();
    format!("<p class=\"muted\">coverage: {}</p><p class=\"muted\">legend: {LEGEND}</p>", parts.join(" · "))
}

fn grades_due(model: &Report) -> String {
    if model.grades_due.is_empty() {
        return "<h2>Grades</h2><p>Every skill has a current grade.</p>".into();
    }
    let items: String = model
        .grades_due
        .iter()
        .map(|g| format!("<li><code>{}</code> <span class=\"muted\">{}</span></li>", esc(&g.command), esc(g.state)))
        .collect();
    format!("<h2>Grades to run</h2><ul>{items}</ul>")
}

/// `cites` or `cited by`, each skill a link to its card.
fn links(ls: &[Link]) -> String {
    report::links(ls, |skill| format!("<a href=\"#{}\">{}</a>", anchor(skill), esc(skill)))
}

/// The card's examples: the invocation per agent, the manifest's
/// default prompt, and real prompts read from transcripts.
fn examples(ex: &Examples, tz: &TimeZone) -> String {
    let written: Vec<String> =
        ex.written.iter().map(|i| format!("{} <code>{}</code>", esc(&name(i.agent)), esc(&i.text))).collect();
    let mut out =
        format!("<dt>invoke</dt><dd>{}</dd>", if written.is_empty() { "-".into() } else { written.join(", ") });
    if let Some(p) = &ex.default_prompt {
        out.push_str(&format!("<dt>default prompt</dt><dd>{}</dd>", esc(p)));
    }
    let real: String = ex
        .real
        .iter()
        .map(|r| {
            let cwd = r.cwd.as_deref().map_or("-".into(), |p| esc(&tilde(p)));
            format!(
                "<li><span class=\"muted\">{} {} {cwd}</span><blockquote>{}</blockquote></li>",
                local(Some(&r.ts), tz, DAY),
                esc(&name(r.agent)),
                esc(&r.text),
            )
        })
        .collect();
    let real = if real.is_empty() { "none".into() } else { format!("<ul>{real}</ul>") };
    out.push_str(&format!("<dt>examples</dt><dd>{real}</dd>"));
    let curated: String = ex
        .curated
        .iter()
        .map(|c| {
            format!(
                "<li>{} <span class=\"muted\">asked</span> {} <span class=\"muted\">happened</span> {} \
                 <span class=\"muted\">tip</span> {} <span class=\"muted mono\">{}</span></li>",
                esc(&c.situation),
                esc(&c.asked),
                esc(&c.happened),
                esc(&c.tip),
                esc(&c.session),
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
fn audit(a: Option<&SkillRecord>, tz: &TimeZone) -> String {
    let Some(a) = a else { return "<dt>audit</dt><dd>none</dd>".into() };
    let rates: Vec<String> = a.rates.labeled().iter().map(|(l, r)| format!("{l} {}", r.of())).collect();
    let improvements: String = a
        .result
        .improvements
        .iter()
        .take(AUDIT_IMPROVEMENTS)
        .map(|i| format!("<li>{} <span class=\"muted\">{}</span></li>", esc(&i.change), esc(&i.reason)))
        .collect();
    format!(
        "<dt>audit</dt><dd><span class=\"muted\">{}, {} sessions, {} billing</span> {}<br>{}<ol>{improvements}</ol></dd>",
        local(Some(&a.audited_at), tz, DAY),
        a.sessions.len(),
        esc(a.judge.billing.map_or("unknown", Billing::name)),
        esc(&a.result.verdict),
        rates.join(", "),
    )
}

fn card(c: &Card, tz: &TimeZone) -> String {
    let from = c.since.as_deref().map_or("all time".into(), |d| format!("since {}", esc(d)));
    let events: Vec<Vec<String>> = c
        .events
        .iter()
        .map(|e| {
            let cwd = e.cwd.map_or("-".into(), |p| esc(&tilde(p)));
            let mut src = esc(&tilde(e.src));
            if e.expired {
                src.push_str(" <span class=\"muted\">expired</span>");
            }
            vec![
                local(Some(e.ts), tz, MINUTE),
                esc(&name(e.agent)),
                esc(&name(e.channel)),
                esc(&name(e.actor)),
                cwd,
                format!("<span class=\"mono\">{src}</span>"),
            ]
        })
        .collect();
    let history = c.history_line().map_or(String::new(), |line| format!("<dt>history</dt><dd>{}</dd>", esc(&line)));
    let grades: Vec<Vec<String>> = c.grades.iter().map(|g| vec![esc(&g.grade.eval.date), grade(Some(g))]).collect();
    let retros: Vec<Vec<String>> = c
        .retros
        .iter()
        .map(|r| vec![esc(&r.date), or_dash(r.score.as_deref()), or_dash(r.verdict.as_deref())])
        .collect();
    format!(
        "<section id=\"{id}\"><h2>{name}</h2><p class=\"muted\">{from}</p>{hist}\
         <h3>Last {shown} of {total} events</h3>{events}\
         <dl><dt>description</dt><dd>{desc}</dd><dt>path</dt><dd class=\"mono\">{path}</dd>{history}\
         <dt>cites</dt><dd>{cites}</dd><dt>cited by</dt><dd>{cited_by}</dd>\
         <dt>strong</dt><dd>{strong}</dd><dt>last use</dt><dd>{last_use}</dd><dt>projects</dt><dd>{projects}</dd>\
         <dt>grades</dt><dd>{grades}</dd><dt>retros</dt><dd>{retro_note}{retros}</dd>{examples}{audit}</dl></section>",
        id = anchor(c.skill),
        name = esc(c.skill),
        hist = histogram(&c.histogram),
        shown = c.events.len(),
        total = c.total_events,
        events = table(&["ts", "agent", "channel", "actor", "cwd", "src"], &events),
        desc = or_dash(c.description.as_deref()),
        path = esc(&tilde(&c.path.to_string_lossy())),
        cites = links(&c.cites),
        cited_by = links(&c.cited_by),
        strong = esc(&c.strong_line()),
        last_use = local(c.usage.last_use.as_deref(), tz, DAY),
        projects = esc(&c.projects_line()),
        grades = if grades.is_empty() { "none".into() } else { table(&["date", "score"], &grades) },
        retro_note = esc(&c.retro_note()),
        retros = if retros.is_empty() { String::new() } else { table(&["date", "score", "verdict"], &retros) },
        examples = examples(&c.examples, tz),
        audit = audit(c.audit, tz),
    )
}

/// The report page for `model`, its times shown in the zone its days were
/// bucketed in.
pub fn render(model: &Report) -> String {
    let tz = &model.tz;
    let from = model.since.as_deref().map_or("all time".into(), |d| format!("since {}", esc(d)));
    let cards: String = model.cards.iter().map(|c| card(c, tz)).collect();
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>skima report</title><style>{CSS}</style></head>\n<body><h1>skima report</h1>\
         <p class=\"muted\">generated {} · usage {from} · times and days in {}</p>{}{}{}<h2>Skills</h2>{cards}</body></html>\n",
        local(Some(&model.generated), tz, MINUTE),
        local(Some(&model.generated), tz, "%Z"),
        usage(model),
        coverage(model),
        grades_due(model),
    )
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
