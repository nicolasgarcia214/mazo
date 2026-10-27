use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use mazo::config::{self, Config, Home};
use mazo::inventory::{Inventory, Scope};
use mazo::ledger::{self, Agent, Channel, Event, Ledger};
use mazo::lifecycle::{self, Retired};
use mazo::render::PathExt;
use mazo::report::{self, Since, UsageArgs};
use mazo::{Error, audit, evals, html, links, out, outln, policy, render, scan, store, tui};

#[derive(Parser)]
#[command(
    name = "mazo",
    version,
    about = "Skill usage from agent transcripts, and sync of skill links",
    after_help = "Bare `mazo` at a terminal opens a TUI: usage, unused and skill cards, with retire, sync and audit.\nExit codes: 0 success, 1 failure or finding, 2 usage error (a bad argument or config)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ingest new transcript lines into the ledger.
    Scan {
        /// Re-derive every event from the transcripts on disk with the current parser.
        #[arg(long)]
        reparse: bool,
        /// Per-channel counts, unparsed samples and the parser canary.
        #[arg(long)]
        report: bool,
        /// Print each home's scan result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// One row per canonical skill: sessions, channel counts, agents, last use.
    Usage {
        /// The window: <N>d or all.
        #[arg(long, default_value_t = report::SINCE)]
        since: Since,
        /// Only events of this agent.
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        /// Only events of this channel.
        #[arg(long, value_enum)]
        channel: Option<Channel>,
        /// Include `other` origins and `probe` actors.
        #[arg(long)]
        all: bool,
        /// Include authoring reads.
        #[arg(long)]
        authoring: bool,
        /// Print the rows and coverage as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Skills with no strong event in the window.
    Unused {
        /// The window: <N>d or all.
        #[arg(long, default_value_t = report::UNUSED_SINCE)]
        since: Since,
        /// Print the window and the skills as JSON.
        #[arg(long)]
        json: bool,
    },
    /// One skill's evidence: histogram, recent events with their sources, grades.
    Show {
        /// The skill to show.
        skill: String,
        /// The window: <N>d or all.
        #[arg(long, default_value_t = report::SINCE)]
        since: Since,
        /// The most recent events to list.
        #[arg(long, default_value_t = report::SHOW_LIMIT)]
        limit: usize,
        /// Print the whole card as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Where grades and retros live.
    Evals {
        #[command(subcommand)]
        cmd: EvalsCmd,
    },
    /// Write the HTML report: usage, grades to run, and every skill's card.
    Report {
        /// The window: <N>d or all.
        #[arg(long, default_value_t = report::SINCE)]
        since: Since,
        /// Defaults to `<data dir>/report.html`.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Open the report with `xdg-open` or `open`.
        #[arg(long)]
        open: bool,
    },
    /// Drift and policy of every agent home. Writes nothing.
    Status {
        /// Only the homes of this agent.
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        /// Print drift, policy errors, conflicts and warnings as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Link every canonical skill into every agent home.
    Sync {
        /// Only the homes of this agent.
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        /// Print what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a config, with every rule off, then install the bundled grade and retro skills from mazo's repository.
    Init {
        /// Your own skills directory, one subdirectory per skill; needed only by `new`, `prune`, retiring its skills, `loose` and home `links`.
        #[arg(long)]
        source: Option<std::path::PathBuf>,
        /// Skip installing grade and retro, which needs the network.
        #[arg(long)]
        no_skills: bool,
    },
    /// Create a skill that already satisfies the configured policy rules.
    New {
        /// The skill's directory name.
        name: String,
        /// The `description` written to its frontmatter.
        #[arg(long)]
        description: String,
    },
    /// Permanently delete archived skills whose work is saved in git.
    Prune {
        /// One entry of `.archive/`; every entry when omitted.
        name: Option<String>,
        /// Delete without asking; required when stdin is not a terminal.
        #[arg(long)]
        yes: bool,
        /// Delete even files whose content is in no HEAD path.
        #[arg(long)]
        force: bool,
    },
    /// Archive a dead source skill under `.archive/`, or remove a store skill, and prune its links. Never commits.
    Retire {
        /// The skill to retire.
        name: String,
        /// Retire a held or named skill, or one used in the last 30 days.
        #[arg(long)]
        force: bool,
        /// Remove a skill of the project at the current directory's git root: its `.mazo/skills/` directory, `mazo.lock` entry, and links in `.claude/skills` and `.agents/skills`.
        #[arg(long)]
        project: bool,
    },
    /// Judge how a skill performs in real sessions; reports, never edits the source.
    Audit(AuditArgs),
    /// Install a skill from a local directory or a git repository into the store and link it into every home.
    Add {
        /// A local path, a git URL (https, ssh, `git@host:owner/repo.git`), GitHub `owner/repo[/path/to/skill]`,
        /// or a GitHub `…/tree/<ref>/<path>` or `…/blob/<ref>/<path>/SKILL.md` URL.
        source: String,
        /// Install under this name; rewrites the SKILL.md `name:` field.
        #[arg(long = "as", value_name = "NAME")]
        name: Option<String>,
        /// The skill to install from a source holding several, by name or by its path in the source; at a terminal, without it or --all, a picker asks.
        #[arg(long, value_name = "NAME|PATH")]
        skill: Option<String>,
        /// Install every skill of a source holding several.
        #[arg(long, conflicts_with_all = ["skill", "name"])]
        all: bool,
        /// Install without vetting.
        #[arg(long)]
        trust: bool,
        /// Accept the vetted skill, and the judge calls of --all, without asking; required when stdin is not a terminal.
        #[arg(long)]
        yes: bool,
        /// Install even when the judge reports a security issue.
        #[arg(long)]
        force: bool,
        /// Install into the project at the current directory's git root: `.mazo/skills/`, `mazo.lock`, and links in `.claude/skills` and `.agents/skills`.
        #[arg(long)]
        project: bool,
    },
    /// Re-clone git-sourced store skills and install what changed; an untrusted source's change is re-vetted first.
    Update {
        /// The store skills to update; every one when omitted.
        names: Vec<String>,
        /// Accept a re-vetted update without asking; required when stdin is not a terminal.
        #[arg(long)]
        yes: bool,
        /// Update even when the judge reports a security issue.
        #[arg(long)]
        force: bool,
        /// Update the skills of the project at the current directory's git root, and its `mazo.lock`.
        #[arg(long)]
        project: bool,
    },
}

#[derive(Subcommand)]
enum EvalsCmd {
    /// Print the skill's grade and retro directories under `evals_dir`.
    Dir {
        /// The skill whose directories to print.
        skill: String,
    },
}

#[derive(clap::Args)]
struct AuditArgs {
    /// The skill to audit.
    skill: String,
    /// The most sessions to judge.
    #[arg(long, default_value_t = audit::LIMIT, conflicts_with = "all")]
    limit: usize,
    /// Judge every candidate session.
    #[arg(long)]
    all: bool,
    /// The most the run may spend, in dollars (default 1); at least 0.04. Claude judge only.
    #[arg(long, value_parser = audit::parse_usd)]
    max_budget_usd: Option<u64>,
    /// Write the skill report from every session record even when no new session was judged.
    #[arg(long)]
    resynth: bool,
    /// List the sessions, the judge command and the input estimate; call nothing.
    #[arg(long)]
    dry_run: bool,
    /// Print the new skill record, the session records written and dropped, and the spend.
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    // Bare `mazo` on a terminal opens the TUI; off one, clap prints the help and exits 2.
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let cmd = if std::env::args_os().len() == 1 && tty { None } else { Some(Cli::parse().cmd) };
    match run(cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ERROR: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}

/// `None` is bare `mazo` on a terminal: the TUI.
fn run(cmd: Option<Cmd>) -> mazo::Result<ExitCode> {
    let path = config::path()?;
    if let Some(Cmd::Init { source, no_skills }) = &cmd {
        let judge = {
            let mut stdin = std::io::stdin().lock();
            let ask = stdin.is_terminal().then_some(&mut stdin as &mut dyn std::io::BufRead);
            config::init(&path, source.as_deref(), ask)?
        };
        outln!("wrote {}", path.inert());
        outln!("{}", config::init_note(judge));
        if *no_skills {
            return Ok(ExitCode::SUCCESS);
        }
        let cfg = config::load(&path)?.ok_or("init wrote no config")?;
        let data = config::data_dir()?;
        return store::install_bundled(&cfg, &store::inventory(cfg.source.as_deref(), &data)?, &data);
    }
    let Some(cfg) = config::load(&path)? else {
        return Err(Error::Usage(format!(
            "no config at {}; run mazo init (--source <skills dir> for your own skills)",
            path.inert()
        )));
    };
    let data = config::data_dir()?;
    let inv = store::inventory(cfg.source.as_deref(), &data)?;
    let Some(cmd) = cmd else {
        return scanned(&cfg, &inv, &data, |input| {
            tui::run(input, &data).map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        });
    };
    let no_color = std::env::var_os("NO_COLOR");
    let tty = render::colour(std::io::stdout().is_terminal(), no_color.as_deref());
    if matches!(cmd, Cmd::Status { .. } | Cmd::Sync { .. }) && cfg.homes.is_empty() {
        return Err(Error::Failed(format!("{} names no home; {}", path.inert(), config::add_home(&path))));
    }
    match cmd {
        Cmd::Scan { reparse, report, json } => cmd_scan(&cfg, &inv, &data, reparse, report, json),
        Cmd::Usage { since, agent, channel, all, authoring, json } => {
            let args = UsageArgs { since, agent, channel, all, authoring };
            let inv = Inventory { project: store::Project::current()?, ..inv };
            with_scanned(&cfg, &inv, &data, |input| Ok(report::usage(input, &args, json, tty)))
        }
        Cmd::Unused { since, json } => {
            with_scanned(&cfg, &inv, &data, |input| Ok(report::unused(input, since, json, tty)))
        }
        Cmd::Show { skill, since, limit, json } => {
            // Before paying for a scan.
            report::check_skill(&inv, &skill)?;
            with_scanned(&cfg, &inv, &data, |input| report::show(input, &skill, since, limit, json, tty))
        }
        Cmd::Evals { cmd: EvalsCmd::Dir { skill } } => {
            report::check_skill(&inv, &skill)?;
            let [grades, retros] = evals::dirs(&cfg.evals_dir, &skill);
            // Skills read these lines as the directories to write into, so they stay raw.
            #[allow(clippy::disallowed_methods)]
            let (grades, retros) = (grades.display(), retros.display());
            outln!("grades\t{grades}\nretros\t{retros}");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Report { since, out, open } => cmd_report(&cfg, &inv, &data, since, out, open),
        Cmd::Status { agent, json } => {
            let inv = Inventory { project: store::Project::current()?, ..inv };
            cmd_status(
                &cfg,
                &inv,
                &data,
                agent,
                json,
                render::colour(std::io::stderr().is_terminal(), no_color.as_deref()),
            )
        }
        Cmd::Sync { agent, dry_run } => cmd_sync(&cfg, inv, &data, agent, dry_run),
        Cmd::New { name, description } => lifecycle::new(&cfg, &inv, &name, &description),
        Cmd::Prune { name, yes, force } => {
            lifecycle::prune(cfg.need_source("prune")?, name.as_deref(), yes, force, std::io::stdin().is_terminal())
        }
        Cmd::Retire { name, force, project } => cmd_retire(&cfg, &data, &name, force, project),
        Cmd::Audit(args) => cmd_audit(&cfg, &inv, &data, &args, tty),
        Cmd::Add { source, name, skill, all, trust, yes, force, project } => {
            let (rename, pick) = (name.as_deref(), skill.as_deref());
            let args =
                store::Add { source: &source, rename, pick, all, trust, yes, force, project, io: &store::Terminal };
            store::add(&cfg, &inv, &data, &args).map(exit)
        }
        Cmd::Update { names, yes, force, project } => {
            let args = store::Update { names: &names, yes, force, project, io: &store::Terminal };
            store::update(&cfg, &inv, &data, &args).map(exit)
        }
        Cmd::Init { .. } => unreachable!("handled before the config loads"),
    }
}

/// The exit code of an add or update that ran: 1 for a failure, 2 for a usage refusal.
fn exit(outcome: store::Outcome) -> ExitCode {
    ExitCode::from(match outcome {
        store::Outcome::Done => 0,
        store::Outcome::Failed => 1,
        store::Outcome::Usage => 2,
    })
}

/// Scans the data directory `data`, then runs `f` on the report input loaded from it.
fn scanned<T>(
    cfg: &Config,
    inv: &Inventory,
    data: &std::path::Path,
    f: impl FnOnce(&report::Input) -> mazo::Result<T>,
) -> mazo::Result<T> {
    let r = scan::scan(cfg, inv, data, false)?;
    r.warn();
    let coverage = ledger::load_coverage(data)?;
    let audits = audit::latest(data)?;
    let now = jiff::Timestamp::now();
    let tz = jiff::tz::TimeZone::system();
    f(&report::Input { cfg, inv, events: &r.events, coverage: &coverage, audits: &audits, now, tz })
}

/// Usage reports scan first, then render, then print freshness warnings.
fn with_scanned(
    cfg: &Config,
    inv: &Inventory,
    data: &std::path::Path,
    render: impl FnOnce(&report::Input) -> mazo::Result<String>,
) -> mazo::Result<ExitCode> {
    scanned(cfg, inv, data, |input| {
        out!("{}", render(input)?);
        for w in policy::freshness(cfg, input.coverage, input.now) {
            eprintln!("{w}");
        }
        Ok(ExitCode::SUCCESS)
    })
}

/// `mazo audit`: usage errors are caught before the scan, and a failed
/// login check before any judge call.
fn cmd_audit(
    cfg: &Config,
    inv: &Inventory,
    data: &std::path::Path,
    args: &AuditArgs,
    tty: bool,
) -> mazo::Result<ExitCode> {
    let planned = audit::plan(cfg, inv, &args.skill, args.max_budget_usd)?;
    let run = audit::Args {
        limit: (!args.all).then_some(args.limit),
        resynth: args.resynth,
        dry_run: args.dry_run,
        json: args.json,
    };
    scanned(cfg, inv, data, |input| {
        let (out, ok) = audit::command(&planned, &run, input, data, tty, None)?;
        out!("{out}");
        Ok(if ok { ExitCode::SUCCESS } else { ExitCode::from(1) })
    })
}

/// The report goes under the data directory, outside any git
/// repository, unless `--out` says otherwise.
fn cmd_report(
    cfg: &Config,
    inv: &Inventory,
    data: &std::path::Path,
    since: Since,
    out: Option<std::path::PathBuf>,
    open: bool,
) -> mazo::Result<ExitCode> {
    let path = match &out {
        // Absolute, so the opener and the printed path do not depend on its cwd.
        Some(p) => std::path::absolute(p).map_err(mazo::at(p))?,
        None => data.join("report.html"),
    };
    let code = with_scanned(cfg, inv, data, |input| {
        let page = html::render(&report::report(input, since));
        html::write(&path, &page)?;
        Ok(format!("wrote {}\n", path.inert()))
    })?;
    if out.is_some() && html::in_worktree(&path) {
        eprintln!("warning: {} is inside a git worktree; the report shows cwds and prompt excerpts", path.inert());
    }
    if open {
        html::open(&path)?;
    }
    Ok(code)
}

fn cmd_scan(
    cfg: &Config,
    inv: &Inventory,
    data: &std::path::Path,
    reparse: bool,
    detail: bool,
    json: bool,
) -> mazo::Result<ExitCode> {
    let was = if reparse { Some(home_counts(Ledger::load(data)?.events())) } else { None };
    let r = scan::scan(cfg, inv, data, reparse)?;
    if json {
        r.warn();
        // Through a `Value`, so the keys print sorted.
        outln!("{}", serde_json::to_value(&r.homes).map_err(|e| e.to_string())?);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(was) = was {
        // `+N` is meaningless after a rewrite.
        let now = home_counts(&r.events);
        for name in r.homes.keys() {
            let count = |c: &BTreeMap<String, usize>| c.get(name).copied().unwrap_or(0);
            outln!("{name} {} events (was {})", count(&now), count(&was));
        }
    } else {
        let summary: Vec<String> = r.homes.iter().map(|(n, h)| format!("{n} +{}", h.new_events.len())).collect();
        outln!("scan: {}", summary.join(" · "));
    }
    r.warn();
    if detail {
        for (name, h) in &r.homes {
            let mut counts = BTreeMap::<Channel, usize>::new();
            for e in r.events.iter().filter(|e| &e.home == name) {
                *counts.entry(e.channel).or_default() += 1;
            }
            let counts: Vec<String> = counts.iter().map(|(c, n)| format!("{}={n}", ledger::name(c))).collect();
            outln!("{name}: {}; ledger {}", render::count(h.files, "file"), counts.join(" "));
            outln!("  unparsed skill commands in this scan: {}", h.unparsed);
            outln!("  lines that are not JSON in this scan: {}", h.malformed);
            outln!("  skill loads with a bad timestamp in this scan: {}", h.bad_timestamps);
            for s in &h.unparsed_samples {
                outln!("    {}", render::inert_line(s));
            }
            if !h.canary.is_empty() {
                outln!("  canary: {} with a skill marker and no event", render::count(h.canary.len(), "file"));
                for f in h.canary.iter().take(5) {
                    outln!("    {}", render::inert_line(f));
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Ledger events per home.
fn home_counts(events: &[Event]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for e in events {
        *counts.entry(e.home.clone()).or_default() += 1;
    }
    counts
}

fn selected(cfg: &Config, agent: Option<Agent>) -> Vec<&Home> {
    cfg.homes.iter().filter(|h| agent.is_none_or(|a| h.agent == a)).collect()
}

/// `colour` paints the `ERROR:` and `warning:` prefixes on stderr. Inside
/// a project, its skills' links are checked too, and `--json` lists them all
/// under `project`.
fn cmd_status(
    cfg: &Config,
    inv: &Inventory,
    data: &std::path::Path,
    agent: Option<Agent>,
    json: bool,
    colour: bool,
) -> mazo::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let skill_errors = policy::per_skill(cfg, inv);
    let plan = links::plan(cfg, inv, &homes, None);
    let project = inv.project.as_ref().map(store::Project::links);
    let policy_errors = policy::home_errors(cfg, inv, &homes);
    let home_errors = || policy_errors.iter().chain(&plan.conflicts);
    let warnings = policy::freshness(cfg, &ledger::load_coverage(data)?, jiff::Timestamp::now());
    let drift: Vec<&links::Entry> =
        plan.entries.iter().chain(project.iter().flatten()).filter(|e| e.state != links::State::Ok).collect();
    let failed = !skill_errors.is_empty() || home_errors().next().is_some() || drift.iter().any(|e| e.state.is_error());
    let name = |e: &links::Entry| e.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    if json {
        let row = |e: &links::Entry| {
            let (state, error) = (e.state.label(), e.state.is_error());
            serde_json::json!({"home": e.home, "path": e.path.to_string_lossy(), "state": state, "error": error, "scope": e.scope})
        };
        let entries: Vec<serde_json::Value> = drift.iter().map(|e| row(e)).collect();
        let bare = |e: &String| render::unprefixed(e).to_string();
        let policy: Vec<String> = skill_errors.iter().chain(policy_errors.iter().map(|(_, e)| e)).map(bare).collect();
        let conflicts: Vec<String> = plan.conflicts.iter().map(|(_, e)| bare(e)).collect();
        let warnings: Vec<String> = warnings.iter().map(bare).collect();
        let mut v =
            serde_json::json!({"drift": entries, "policy": policy, "conflicts": conflicts, "warnings": warnings});
        if let Some(project) = &project {
            v["project"] = project.iter().map(row).collect();
        }
        outln!("{v}");
    } else {
        for h in &homes {
            let (errors, info): (Vec<&links::Entry>, Vec<&links::Entry>) =
                drift.iter().filter(|e| e.scope == Scope::Global && e.home == h.name).partition(|e| e.state.is_error());
            if errors.is_empty() && !home_errors().any(|(home, _)| *home == h.name) {
                let foreign =
                    if info.is_empty() { String::new() } else { format!("; {} foreign left alone", info.len()) };
                outln!("{}: in sync ({}{foreign})", h.name, render::count(inv.skills.len(), "skill"));
            }
            for e in errors.into_iter().chain(info) {
                outln!("{}: {} {}", e.home, render::inert_line(&name(e)), e.state.finding());
            }
        }
        for skill in inv.project.iter().flat_map(|p| &p.skills) {
            let suffix = Scope::Project.suffix();
            let mut bad = drift.iter().filter(|e| e.scope == Scope::Project && name(e) == *skill).peekable();
            if bad.peek().is_none() {
                outln!("project: {skill}{suffix} in sync");
            }
            for e in bad {
                outln!("{}: {skill}{suffix} {}", e.home, e.state.finding());
            }
        }
        for e in skill_errors.iter().chain(home_errors().map(|(_, e)| e)).chain(&warnings) {
            eprintln!("{}", render::paint_prefix(e, colour));
        }
    }
    Ok(if failed { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

fn cmd_sync(
    cfg: &Config,
    mut inv: Inventory,
    data: &std::path::Path,
    agent: Option<Agent>,
    dry_run: bool,
) -> mazo::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let mut plan = links::checked(cfg, &inv, &homes);
    // Re-planned under the data lock, on an inventory read under it, so a
    // concurrent `add`, `update` or `retire` cannot stale what is applied. A
    // dry run, a refusal or an empty plan writes nothing, not even the lock.
    let _lock = if !dry_run && plan.as_ref().is_ok_and(|p| !p.actions.is_empty()) {
        let lock = ledger::lock(data)?;
        inv = store::inventory(cfg.source.as_deref(), data)?;
        plan = links::checked(cfg, &inv, &homes);
        Some(lock)
    } else {
        None
    };
    let plan = match plan {
        Ok(plan) => plan,
        Err(errors) => {
            for e in &errors {
                eprintln!("{e}");
            }
            return Ok(ExitCode::from(1));
        }
    };
    if plan.actions.is_empty() {
        outln!("in sync: {}, {}", render::count(homes.len(), "home"), render::count(inv.skills.len(), "skill"));
        return Ok(ExitCode::SUCCESS);
    }
    for line in if dry_run { links::planned(&plan) } else { links::apply(&plan)? } {
        outln!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `mazo retire`: the warnings and blockers on stderr, what it wrote on stdout.
fn cmd_retire(cfg: &Config, data: &std::path::Path, name: &str, force: bool, project: bool) -> mazo::Result<ExitCode> {
    let mut warnings = Vec::new();
    let retired = lifecycle::retire(cfg, data, name, force, project, &mut warnings);
    for w in &warnings {
        eprintln!("{w}");
    }
    match retired? {
        Retired::Blocked(blockers) => {
            for b in &blockers {
                eprintln!("{b}");
            }
            Err("retire refused; pass --force to retire it anyway".into())
        }
        Retired::Refused(errors) => {
            for e in &errors {
                eprintln!("{e}");
            }
            Ok(ExitCode::from(1))
        }
        Retired::Done(lines) => {
            for l in &lines {
                outln!("{l}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    /// Every argument of every subcommand, `--help` and `--version` aside.
    fn undocumented(cmd: &clap::Command, path: &str, out: &mut Vec<String>) {
        for arg in cmd.get_arguments() {
            if !matches!(arg.get_id().as_str(), "help" | "version") && arg.get_help().is_none() {
                out.push(format!("{path} {}", arg.get_id()));
            }
        }
        for sub in cmd.get_subcommands() {
            undocumented(sub, &format!("{path} {}", sub.get_name()), out);
        }
    }

    #[test]
    fn every_argument_has_help_text() {
        let mut out = Vec::new();
        undocumented(&Cli::command(), "mazo", &mut out);
        assert_eq!(out, Vec::<String>::new());
    }
}
