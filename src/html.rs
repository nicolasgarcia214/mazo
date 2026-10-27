//! The self-contained HTML report: one file with inline CSS and
//! inline SVG, rendered from the typed model of `report::report`.

use std::fs;
use std::path::Path;
use std::process::Command;

use jiff::tz::TimeZone;

use crate::examples::Examples;
use crate::help;
use crate::inventory::Scope;
use crate::ledger::name;
use crate::render::{self, Align, DAY, MINUTE, PathExt};
use crate::report::{self, Card, Day, GradeOut, Link, Report, tilde};

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
.warn { color: var(--warn); }
.wrap { overflow-x: auto; }
svg .s { fill: var(--strong); }
svg .w { fill: var(--weak); }
dl { display: grid; grid-template-columns: max-content 1fr; gap: 2px 12px; }
dt { color: var(--muted); }
dd { margin: 0; }
blockquote { margin: 2px 0 8px; padding-left: 8px; border-left: 2px solid var(--line); white-space: pre-wrap; }
ul, ol { margin: 0; padding-left: 18px; }
";

/// Escapes text for an element body or a quoted attribute.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
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

/// Markup in the warning colour.
fn warn(html: String) -> String {
    format!("<span class=\"warn\">{html}</span>")
}

/// A grade's cell, in the warning colour when stale.
fn grade(g: &GradeOut) -> String {
    let cell = esc(&g.cell(false));
    if g.grade.stale { warn(cell) } else { cell }
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

/// The usage table: the cells `mazo usage` prints, its last load a day, a
/// global skill's name a link to its card.
fn usage(model: &Report) -> String {
    let class = |h: &str, a: &Align| match (h, a) {
        (_, Align::Right) => " class=\"n\"",
        ("agents" | "ctx", _) => " class=\"mono\"",
        _ => "",
    };
    let rows: String = model
        .usage
        .rows
        .iter()
        .map(|r| {
            let carded = r.scope == Scope::Global && model.cards.iter().any(|c| c.skill == r.skill);
            let cells: String = report::USAGE_HEADER
                .iter()
                .zip(r.cells(r.last.as_deref().map_or("-".into(), |t| render::local(t, &model.tz, DAY)), false))
                .map(|((h, a), cell)| {
                    let cell = esc(&cell);
                    let cell = match *h {
                        "skill" if carded => format!("<a href=\"#{}\">{cell}</a>", anchor(&r.skill)),
                        "grade" if r.grade.as_ref().is_some_and(|g| g.grade.stale) => warn(cell),
                        "audit" if r.mark == "!" => warn(cell),
                        _ => cell,
                    };
                    format!("<td{}>{cell}</td>", class(h, a))
                })
                .collect();
            format!("<tr>{cells}</tr>")
        })
        .collect();
    let head: String = report::USAGE_HEADER.iter().map(|(h, a)| format!("<th{}>{h}</th>", class("", a))).collect();
    format!("<h2>Usage</h2><div class=\"wrap\"><table><tr>{head}</tr>{rows}</table></div>")
}

fn coverage(model: &Report) -> String {
    format!(
        "<p class=\"muted\">{}</p><p class=\"muted\">legend: {}</p>",
        esc(&report::coverage_note("coverage", &model.usage.coverage)),
        esc(&help::legend(|_, _| true))
    )
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
                esc(&render::zoned(r.ts, tz, DAY)),
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

/// The latest audit: when and how it ran, its verdict, the rates and the
/// top improvements.
fn audit(c: &Card, tz: &TimeZone) -> String {
    let Some(a) = c.audit_lines(tz) else { return "<dt>audit</dt><dd>none</dd>".into() };
    let improvements: String = a
        .improvements
        .iter()
        .map(|i| format!("<li>{} <span class=\"muted\">{}</span></li>", esc(&i.change), esc(&i.reason)))
        .collect();
    format!(
        "<dt>audit</dt><dd><span class=\"muted\">{}</span> {}<br>{}<ol>{improvements}</ol></dd>",
        esc(&a.run),
        esc(a.verdict),
        esc(&a.rates),
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
                esc(&render::zoned(e.ts, tz, MINUTE)),
                esc(&name(e.agent)),
                esc(&name(e.channel)),
                esc(&name(e.actor)),
                cwd,
                format!("<span class=\"mono\">{src}</span>"),
            ]
        })
        .collect();
    let history = c.history_line().map_or(String::new(), |line| format!("<dt>history</dt><dd>{}</dd>", esc(&line)));
    let grades: Vec<Vec<String>> = c.grades.iter().map(|g| vec![esc(&g.grade.eval.date), grade(g)]).collect();
    let retros: Vec<Vec<String>> = c
        .retros
        .iter()
        .map(|r| vec![esc(&r.date), or_dash(r.score.as_deref()), or_dash(r.verdict.as_deref())])
        .collect();
    format!(
        "<section id=\"{id}\"><h2>{name}</h2><p class=\"muted\">{from}</p>{hist}\
         <h3>Last {shown} of {total}</h3>{events}\
         <dl><dt>description</dt><dd>{desc}</dd><dt>path</dt><dd class=\"mono\">{path}</dd>{history}\
         <dt>cites</dt><dd>{cites}</dd><dt>cited by</dt><dd>{cited_by}</dd>\
         <dt>strong</dt><dd>{strong}</dd><dt>last use</dt><dd>{last_use}</dd><dt>projects</dt><dd>{projects}</dd>\
         <dt>grades</dt><dd>{grades}</dd><dt>retros</dt><dd>{retro_note}{retros}</dd>{skipped}{examples}{vet}{audit}</dl></section>",
        id = anchor(c.skill),
        name = esc(c.skill),
        hist = histogram(&c.histogram),
        shown = c.events.len(),
        total = render::count(c.total_events, "event"),
        events = table(&["ts", "agent", "channel", "actor", "cwd", "src"], &events),
        desc = or_dash(c.description.as_deref()),
        path = or_dash(c.path.as_ref().map(|p| tilde(&p.to_string_lossy())).as_deref()),
        cites = links(&c.cites),
        cited_by = links(&c.cited_by),
        strong = esc(&c.strong_line()),
        last_use = local(c.usage.last_use.as_deref(), tz, DAY),
        projects = esc(&c.projects_line()),
        grades = if grades.is_empty() { "none".into() } else { table(&["date", "score"], &grades) },
        retro_note = esc(&c.retro_note()),
        retros = if retros.is_empty() { String::new() } else { table(&["date", "score", "verdict"], &retros) },
        skipped = c.skipped.iter().map(|s| format!("<dt>skipped</dt><dd>{}</dd>", esc(s))).collect::<String>(),
        examples = examples(&c.examples, tz),
        vet = c.vet.map_or(String::new(), |v| format!("<dt>vet</dt><dd>{}</dd>", esc(&v.line()))),
        audit = audit(c, tz),
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
         <title>mazo report</title><style>{CSS}</style></head>\n<body><h1>mazo report</h1>\
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
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(crate::at(dir))?;
    }
    crate::ledger::write_atomic(path, html.as_bytes(), 0o600)
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
    if status.success() { Ok(()) } else { Err(format!("{opener} {}: {status}", path.inert()).into()) }
}
