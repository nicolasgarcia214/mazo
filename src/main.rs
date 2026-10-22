use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use mazo::config::{self, Config, Home};
use mazo::inventory::{Inventory, Scope};
use mazo::ledger::{self, Agent, Channel, Ledger};
use mazo::lifecycle::{self, Retired};
use mazo::report::{self, Since, UsageArgs};
use mazo::{Error, audit, html, links, policy, render, scan, store, tui};

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
        /// The window: <N>d or all; 30 days by default.
        #[arg(long, default_value = "30d")]
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
        /// The window: <N>d or all; 60 days by default.
        #[arg(long, default_value = "60d")]
        since: Since,
        /// Print the window and the skills as JSON.
        #[arg(long)]
        json: bool,
    },
    /// One skill's evidence: histogram, recent events with their sources, grades.
    Show {
        /// The skill to show.
        skill: String,
        /// The window: <N>d or all; 30 days by default.
        #[arg(long, default_value = "30d")]
        since: Since,
        /// The most recent events to list.
        #[arg(long, default_value_t = report::SHOW_LIMIT)]
        limit: usize,
        /// Print the whole card as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Write the HTML report: usage, grades to run, and every skill's card.
    Report {
        /// The window: <N>d or all; 30 days by default.
        #[arg(long, default_value = "30d")]
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
    /// Link every canonical skill into every agent home (replaces install.sh).
    Sync {
        /// Only the homes of this agent.
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        /// Print what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a config, with every rule off.
    Init {
        /// Your own skills directory, one subdirectory per skill; needed only by `new`, `prune`, retiring its skills, `loose` and home `links`.
        #[arg(long)]
        source: Option<std::path::PathBuf>,
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
        /// A local path, a git URL (https, ssh, `git@host:owner/repo.git`), or GitHub `owner/repo[/path/to/skill]`.
        source: String,
        /// Install under this name; rewrites the SKILL.md `name:` field.
        #[arg(long = "as", value_name = "NAME")]
        name: Option<String>,
        /// The skill to install from a source holding several; at a terminal, without it or --all, a picker asks.
        #[arg(long, value_name = "NAME")]
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
    if let Some(Cmd::Init { source }) = &cmd {
        let mut stdin = std::io::stdin().lock();
        let ask = stdin.is_terminal().then_some(&mut stdin as &mut dyn std::io::BufRead);
        let judge = config::init(&path, source.as_deref(), ask)?;
        println!("wrote {}", path.display());
        println!("{}", config::init_note(judge));
        return Ok(ExitCode::SUCCESS);
    }
    let Some(cfg) = config::load(&path)? else {
        return Err(Error::Usage(format!(
            "no config at {}; run mazo init (--source <skills dir> for your own skills)",
            path.display()
        )));
    };
    let data = ledger::data_dir()?;
    let inv = store::inventory(cfg.source.as_deref(), &data)?;
    let Some(cmd) = cmd else {
        return scanned(&cfg, &inv, |data, input| {
            tui::run(input, data).map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        });
    };
    let no_color = std::env::var_os("NO_COLOR");
    let tty = render::colour(std::io::stdout().is_terminal(), no_color.as_deref());
    match cmd {
        Cmd::Scan { reparse, report, json } => cmd_scan(&cfg, &inv, reparse, report, json),
        Cmd::Usage { since, agent, channel, all, authoring, json } => {
            let args = UsageArgs { since, agent, channel, all, authoring };
            let inv = Inventory { project: store::Project::current()?, ..inv };
            with_scanned(&cfg, &inv, |input| Ok(report::usage(input, &args, json, tty)))
        }
        Cmd::Unused { since, json } => with_scanned(&cfg, &inv, |input| Ok(report::unused(input, since, json, tty))),
        Cmd::Show { skill, since, limit, json } => {
            // Before paying for a scan.
            report::check_skill(&inv, &skill)?;
            with_scanned(&cfg, &inv, |input| report::show(input, &skill, since, limit, json, tty))
        }
        Cmd::Report { since, out, open } => cmd_report(&cfg, &inv, since, out, open),
        Cmd::Status { agent, json } => {
            let inv = Inventory { project: store::Project::current()?, ..inv };
            cmd_status(&cfg, &inv, agent, json, render::colour(std::io::stderr().is_terminal(), no_color.as_deref()))
        }
        Cmd::Sync { agent, dry_run } => cmd_sync(&cfg, &inv, agent, dry_run),
        Cmd::New { name, description } => lifecycle::new(&cfg, &inv, &name, &description),
        Cmd::Prune { name, yes, force } => {
            lifecycle::prune(cfg.need_source("prune")?, name.as_deref(), yes, force, std::io::stdin().is_terminal())
        }
        Cmd::Retire { name, force, project } => cmd_retire(&cfg, &inv, &data, &name, force, project),
        Cmd::Audit(args) => cmd_audit(&cfg, &inv, &args, tty),
        Cmd::Add { source, name, skill, all, trust, yes, force, project } => {
            let (rename, pick) = (name.as_deref(), skill.as_deref());
            let args =
                store::Add { source: &source, rename, pick, all, trust, yes, force, project, pane: None, asks: None };
            store::add(&cfg, &inv, &data, &args)
        }
        Cmd::Update { names, yes, force, project } => {
            store::update(&cfg, &inv, &data, &store::Update { names: &names, yes, force, pane: None, project })
        }
        Cmd::Init { .. } => unreachable!("handled before the config loads"),
    }
}

/// Scans, then runs `f` on the data directory and the report input loaded from it.
fn scanned<T>(
    cfg: &Config,
    inv: &Inventory,
    f: impl FnOnce(&std::path::Path, &report::Input) -> mazo::Result<T>,
) -> mazo::Result<T> {
    let data = ledger::data_dir()?;
    scan::scan(cfg, inv, &data, false)?.warn();
    let ledger = Ledger::load(&data)?;
    let coverage = ledger::load_coverage(&data)?;
    let audits = audit::latest(&data)?;
    let now = jiff::Timestamp::now();
    let tz = jiff::tz::TimeZone::system();
    f(&data, &report::Input { cfg, inv, events: ledger.events(), coverage: &coverage, audits: &audits, now, tz })
}

/// Usage reports scan first, then render, then print freshness warnings.
fn with_scanned(
    cfg: &Config,
    inv: &Inventory,
    render: impl FnOnce(&report::Input) -> mazo::Result<String>,
) -> mazo::Result<ExitCode> {
    scanned(cfg, inv, |_, input| {
        print!("{}", render(input)?);
        for w in policy::freshness(cfg, input.coverage, input.now) {
            eprintln!("{w}");
        }
        Ok(ExitCode::SUCCESS)
    })
}

/// `mazo audit`: usage errors are caught before the scan, and a failed
/// login check before any judge call.
fn cmd_audit(cfg: &Config, inv: &Inventory, args: &AuditArgs, tty: bool) -> mazo::Result<ExitCode> {
    let planned = audit::plan(cfg, inv, &args.skill, args.max_budget_usd)?;
    let run = audit::Args {
        limit: (!args.all).then_some(args.limit),
        resynth: args.resynth,
        dry_run: args.dry_run,
        json: args.json,
    };
    scanned(cfg, inv, |data, input| {
        let (out, ok) = audit::command(&planned, &run, input, data, tty, None)?;
        print!("{out}");
        Ok(if ok { ExitCode::SUCCESS } else { ExitCode::from(1) })
    })
}

/// The report goes under the data directory, outside any git
/// repository, unless `--out` says otherwise.
fn cmd_report(
    cfg: &Config,
    inv: &Inventory,
    since: Since,
    out: Option<std::path::PathBuf>,
    open: bool,
) -> mazo::Result<ExitCode> {
    let path = match &out {
        // Absolute, so the opener and the printed path do not depend on its cwd.
        Some(p) => std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => ledger::data_dir()?.join("report.html"),
    };
    let code = with_scanned(cfg, inv, |input| {
        let page = html::render(&report::report(input, since));
        html::write(&path, &page)?;
        Ok(format!("wrote {}\n", path.display()))
    })?;
    if out.is_some() && html::in_worktree(&path) {
        eprintln!("warning: {} is inside a git worktree; the report shows cwds and prompt excerpts", path.display());
    }
    if open {
        html::open(&path)?;
    }
    Ok(code)
}

fn cmd_scan(cfg: &Config, inv: &Inventory, reparse: bool, detail: bool, json: bool) -> mazo::Result<ExitCode> {
    let data = ledger::data_dir()?;
    let was = if reparse { Some(home_counts(&Ledger::load(&data)?)) } else { None };
    let r = scan::scan(cfg, inv, &data, reparse)?;
    if json {
        r.warn();
        // Through a `Value`, so the keys print sorted.
        println!("{}", serde_json::to_value(&r.homes).map_err(|e| e.to_string())?);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(was) = was {
        // `+N` is meaningless after a rewrite.
        let now = home_counts(&Ledger::load(&data)?);
        for name in r.homes.keys() {
            let count = |c: &BTreeMap<String, usize>| c.get(name).copied().unwrap_or(0);
            println!("{name} {} events (was {})", count(&now), count(&was));
        }
    } else {
        let summary: Vec<String> = r.homes.iter().map(|(n, h)| format!("{n} +{}", h.new_events.len())).collect();
        println!("scan: {}", summary.join(" · "));
    }
    r.warn();
    if detail {
        let ledger = Ledger::load(&data)?;
        for (name, h) in &r.homes {
            let mut counts = BTreeMap::<Channel, usize>::new();
            for e in ledger.events().iter().filter(|e| &e.home == name) {
                *counts.entry(e.channel).or_default() += 1;
            }
            let counts: Vec<String> = counts.iter().map(|(c, n)| format!("{}={n}", ledger::name(c))).collect();
            println!("{name}: {} files; ledger {}", h.files, counts.join(" "));
            println!("  unparsed skill commands in this scan: {}", h.unparsed);
            println!("  lines that are not JSON in this scan: {}", h.malformed);
            for s in &h.unparsed_samples {
                println!("    {s}");
            }
            if !h.canary.is_empty() {
                println!("  canary: {} files with a skill marker and no event", h.canary.len());
                for f in h.canary.iter().take(5) {
                    println!("    {f}");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Ledger events per home.
fn home_counts(ledger: &Ledger) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for e in ledger.events() {
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
fn cmd_status(cfg: &Config, inv: &Inventory, agent: Option<Agent>, json: bool, colour: bool) -> mazo::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let skill_errors = policy::per_skill(cfg, inv);
    let plan = links::plan(cfg, inv, &homes, None);
    let project = inv.project.as_ref().map(store::Project::links);
    let policy_errors = policy::home_errors(cfg, inv, &homes);
    let home_errors = || policy_errors.iter().chain(&plan.conflicts);
    let data = ledger::data_dir()?;
    let warnings = policy::freshness(cfg, &ledger::load_coverage(&data)?, jiff::Timestamp::now());
    let drift: Vec<&links::Entry> =
        plan.entries.iter().chain(project.iter().flatten()).filter(|e| e.state != links::State::Ok).collect();
    let failed = !skill_errors.is_empty() || home_errors().next().is_some() || drift.iter().any(|e| e.state.is_error());
    let name = |e: &links::Entry| e.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    if json {
        let row = |e: &links::Entry| {
            let (state, error) = (e.state.label(), e.state.is_error());
            serde_json::json!({"home": e.home, "path": e.path, "state": state, "error": error, "scope": e.scope})
        };
        let entries: Vec<serde_json::Value> = drift.iter().map(|e| row(e)).collect();
        let policy: Vec<&String> = skill_errors.iter().chain(policy_errors.iter().map(|(_, e)| e)).collect();
        let conflicts: Vec<&String> = plan.conflicts.iter().map(|(_, e)| e).collect();
        let mut v =
            serde_json::json!({"drift": entries, "policy": policy, "conflicts": conflicts, "warnings": warnings});
        if let Some(project) = &project {
            v["project"] = project.iter().map(row).collect();
        }
        println!("{v}");
    } else {
        for h in &homes {
            let (errors, info): (Vec<&links::Entry>, Vec<&links::Entry>) =
                drift.iter().filter(|e| e.scope == Scope::Global && e.home == h.name).partition(|e| e.state.is_error());
            if errors.is_empty() && !home_errors().any(|(home, _)| *home == h.name) {
                println!("{}: in sync ({} skills)", h.name, inv.skills.len());
            }
            for e in errors.into_iter().chain(info) {
                println!("{}: {} {}", e.home, name(e), e.state.finding());
            }
        }
        for skill in inv.project.iter().flat_map(|p| &p.skills) {
            let suffix = Scope::Project.suffix();
            let mut bad = drift.iter().filter(|e| e.scope == Scope::Project && name(e) == *skill).peekable();
            if bad.peek().is_none() {
                println!("project: {skill}{suffix} in sync");
            }
            for e in bad {
                println!("{}: {skill}{suffix} {}", e.home, e.state.finding());
            }
        }
        for e in skill_errors.iter().chain(home_errors().map(|(_, e)| e)).chain(&warnings) {
            eprintln!("{}", render::paint_prefix(e, colour));
        }
    }
    Ok(if failed { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

fn cmd_sync(cfg: &Config, inv: &Inventory, agent: Option<Agent>, dry_run: bool) -> mazo::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let plan = match links::checked(cfg, inv, &homes) {
        Ok(plan) => plan,
        Err(errors) => {
            for e in &errors {
                eprintln!("{e}");
            }
            return Ok(ExitCode::from(1));
        }
    };
    if plan.actions.is_empty() {
        println!("in sync: {} homes, {} skills", homes.len(), inv.skills.len());
        return Ok(ExitCode::SUCCESS);
    }
    for line in if dry_run { links::planned(&plan) } else { links::apply(&plan)? } {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `mazo retire`: the warnings and blockers on stderr, what it wrote on stdout.
fn cmd_retire(
    cfg: &Config,
    inv: &Inventory,
    data: &std::path::Path,
    name: &str,
    force: bool,
    project: bool,
) -> mazo::Result<ExitCode> {
    let mut warnings = Vec::new();
    let retired = lifecycle::retire(cfg, inv, data, name, force, project, &mut warnings);
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
                println!("{l}");
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
