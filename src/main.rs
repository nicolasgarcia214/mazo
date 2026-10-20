use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use skima::config::{self, Config, Home};
use skima::inventory::Inventory;
use skima::ledger::{self, Agent, Channel, Ledger};
use skima::report::{self, Since, UsageArgs};
use skima::{audit, html, links, policy, render, scan};

#[derive(Parser)]
#[command(name = "skima", version, about = "Skill usage from agent transcripts, and sync of skill links")]
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
        #[arg(long, default_value_t = 20)]
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
    /// Write a config for a skills directory, with every rule off.
    Init {
        /// The skills directory: one subdirectory per skill.
        #[arg(long)]
        source: std::path::PathBuf,
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
    /// Archive a dead skill under `.archive/` and prune its links. Never commits.
    Retire {
        /// The skill to archive.
        name: String,
        /// Retire a held or named skill, or one used in the last 30 days.
        #[arg(long)]
        force: bool,
    },
    /// Judge how a skill performs in real sessions; reports, never edits the source.
    Audit(AuditArgs),
}

#[derive(clap::Args)]
struct AuditArgs {
    /// The skill to audit.
    skill: String,
    /// The most sessions to judge.
    #[arg(long, default_value_t = 20, conflicts_with = "all")]
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
    let cli = Cli::parse();
    match run(cli.cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ERROR: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(cmd: Cmd) -> skima::Result<ExitCode> {
    let path = config::path()?;
    if let Cmd::Init { source } = &cmd {
        config::init(&path, source)?;
        println!("wrote {}", path.display());
        return Ok(ExitCode::SUCCESS);
    }
    let Some(cfg) = config::load(&path)? else {
        eprintln!("ERROR: no config at {}; run skima init --source <skills dir>", path.display());
        return Ok(ExitCode::from(2));
    };
    let inv = Inventory::load(&cfg.source)?;
    let no_color = std::env::var_os("NO_COLOR");
    let tty = render::colour(std::io::stdout().is_terminal(), no_color.as_deref());
    match cmd {
        Cmd::Scan { reparse, report, json } => cmd_scan(&cfg, &inv, reparse, report, json),
        Cmd::Usage { since, agent, channel, all, authoring, json } => {
            let args = UsageArgs { since, agent, channel, all, authoring };
            with_scanned(&cfg, &inv, |input| Ok(report::usage(input, &args, json, tty)))
        }
        Cmd::Unused { since, json } => with_scanned(&cfg, &inv, |input| Ok(report::unused(input, since, json, tty))),
        Cmd::Show { skill, since, limit, json } => {
            // A usage error (exit 2), caught before paying for a scan.
            if let Err(e) = report::check_skill(&inv, &skill) {
                eprintln!("ERROR: {e}");
                return Ok(ExitCode::from(2));
            }
            with_scanned(&cfg, &inv, |input| report::show(input, &skill, since, limit, json, tty))
        }
        Cmd::Report { since, out, open } => cmd_report(&cfg, &inv, since, out, open),
        Cmd::Status { agent, json } => {
            cmd_status(&cfg, &inv, agent, json, render::colour(std::io::stderr().is_terminal(), no_color.as_deref()))
        }
        Cmd::Sync { agent, dry_run } => cmd_sync(&cfg, &inv, agent, dry_run),
        Cmd::New { name, description } => skima::lifecycle::new(&cfg, &name, &description),
        Cmd::Prune { name, yes, force } => {
            skima::lifecycle::prune(&cfg.source, name.as_deref(), yes, force, std::io::stdin().is_terminal())
        }
        Cmd::Retire { name, force } => skima::lifecycle::retire(&cfg, &inv, &name, force),
        Cmd::Audit(args) => cmd_audit(&cfg, &inv, &args, tty),
        Cmd::Init { .. } => unreachable!("handled before the config loads"),
    }
}

/// Scans, then runs `f` on the data directory and the report input loaded from it.
fn scanned<T>(
    cfg: &Config,
    inv: &Inventory,
    f: impl FnOnce(&std::path::Path, &report::Input) -> skima::Result<T>,
) -> skima::Result<T> {
    let data = ledger::data_dir()?;
    scan::scan(cfg, inv, &data, false)?.warn_io_errors();
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
    render: impl FnOnce(&report::Input) -> skima::Result<String>,
) -> skima::Result<ExitCode> {
    scanned(cfg, inv, |_, input| {
        print!("{}", render(input)?);
        for w in policy::freshness(cfg, input.coverage, input.now) {
            eprintln!("{w}");
        }
        Ok(ExitCode::SUCCESS)
    })
}

/// `skima audit`: usage errors exit 2 before the scan.
fn cmd_audit(cfg: &Config, inv: &Inventory, args: &AuditArgs, tty: bool) -> skima::Result<ExitCode> {
    let (skill, budget) = (args.skill.as_str(), args.max_budget_usd);
    let usage = |msg: String| {
        eprintln!("ERROR: {msg}");
        Ok(ExitCode::from(2))
    };
    if !inv.skills.contains(skill) {
        return usage(format!("not a skill in the source: {skill}"));
    }
    // Codex and Pi report no dollar figure, so `--limit` is their only cap.
    let budget = match (cfg.judge.agent, budget) {
        (Agent::Claude, budget) => Some(budget.unwrap_or(1_000_000)),
        (agent, Some(_)) => {
            return usage(format!(
                "--max-budget-usd needs the claude judge; the {} judge is capped by --limit",
                ledger::name(agent)
            ));
        }
        (_, None) => None,
    };
    if budget.is_some_and(|b| b < 4 * audit::CENT) {
        return usage("--max-budget-usd must be at least 0.04".into());
    }
    let codex_auth = audit::codex_auth(&cfg.homes)?;
    let pi_settings = audit::pi_settings(&cfg.homes)?;
    scanned(cfg, inv, |data, input| {
        let limit = (!args.all).then_some(args.limit);
        let candidates = audit::select(input.events, skill, &audit::audited(data, skill)?, limit);
        let a = audit::Audit {
            skill,
            source: &cfg.source,
            data,
            judge: &cfg.judge,
            budget,
            codex_auth: &codex_auth,
            pi_settings: &pi_settings,
            evals: &skima::evals::load(inv, skill),
            usage: &report::usage_section(input, skill),
        };
        if args.dry_run {
            print!("{}", audit::dry_run(&a, &candidates, tty)?);
            return Ok(ExitCode::SUCCESS);
        }
        // A failed login check is a usage error: no judge was started.
        if audit::calls(&a, &candidates, args.resynth)?
            && let Err(e) = audit::login(&a)
        {
            return usage(e);
        }
        let run = audit::run(&a, &candidates, args.resynth)?;
        print!("{}", if args.json { run.json(&a) } else { run.text(&a) });
        Ok(if run.ok() { ExitCode::SUCCESS } else { ExitCode::from(1) })
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
) -> skima::Result<ExitCode> {
    let path = match &out {
        // Absolute, so the opener and the printed path do not depend on its cwd.
        Some(p) => std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => ledger::data_dir()?.join("report.html"),
    };
    let code = with_scanned(cfg, inv, |input| {
        let page = html::render(input, since)?;
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

fn cmd_scan(cfg: &Config, inv: &Inventory, reparse: bool, detail: bool, json: bool) -> skima::Result<ExitCode> {
    let data = ledger::data_dir()?;
    let was = if reparse { Some(home_counts(&Ledger::load(&data)?)) } else { None };
    let r = scan::scan(cfg, inv, &data, reparse)?;
    if json {
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
    r.warn_io_errors();
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

/// `colour` paints the `ERROR:` and `warning:` prefixes on stderr.
fn cmd_status(
    cfg: &Config,
    inv: &Inventory,
    agent: Option<Agent>,
    json: bool,
    colour: bool,
) -> skima::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let skill_errors = policy::per_skill(cfg, inv);
    let plan = links::plan(cfg, inv, &homes, None);
    let policy_errors = policy::home_errors(cfg, inv, &homes);
    let home_errors = || policy_errors.iter().chain(&plan.conflicts);
    let data = ledger::data_dir()?;
    let warnings = policy::freshness(cfg, &ledger::load_coverage(&data)?, jiff::Timestamp::now());
    let drift: Vec<&links::Entry> = plan.entries.iter().filter(|e| e.state != links::State::Ok).collect();
    let failed = !skill_errors.is_empty() || home_errors().next().is_some() || drift.iter().any(|e| e.state.is_error());
    if json {
        let entries: Vec<serde_json::Value> = drift
            .iter()
            .map(|e| serde_json::json!({"home": e.home, "path": e.path, "state": e.state.label(), "error": e.state.is_error()}))
            .collect();
        let policy: Vec<&String> = skill_errors.iter().chain(policy_errors.iter().map(|(_, e)| e)).collect();
        let conflicts: Vec<&String> = plan.conflicts.iter().map(|(_, e)| e).collect();
        let v = serde_json::json!({"drift": entries, "policy": policy, "conflicts": conflicts, "warnings": warnings});
        println!("{v}");
    } else {
        for h in &homes {
            let (errors, info): (Vec<&links::Entry>, Vec<&links::Entry>) =
                drift.iter().filter(|e| e.home == h.name).partition(|e| e.state.is_error());
            if errors.is_empty() && !home_errors().any(|(home, _)| *home == h.name) {
                println!("{}: in sync ({} skills)", h.name, inv.skills.len());
            }
            for e in errors.into_iter().chain(info) {
                let name = e.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                println!("{}: {name} {}", e.home, e.state.finding());
            }
        }
        for e in skill_errors.iter().chain(home_errors().map(|(_, e)| e)).chain(&warnings) {
            eprintln!("{}", render::paint_prefix(e, colour));
        }
    }
    Ok(if failed { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

fn cmd_sync(cfg: &Config, inv: &Inventory, agent: Option<Agent>, dry_run: bool) -> skima::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let plan = links::plan(cfg, inv, &homes, None);
    let errors = links::sync_errors(cfg, inv, &homes, &plan);
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("{e}");
        }
        return Ok(ExitCode::from(1));
    }
    if plan.actions.is_empty() {
        println!("in sync: {} homes, {} skills", homes.len(), inv.skills.len());
        return Ok(ExitCode::SUCCESS);
    }
    if dry_run {
        for a in &plan.actions {
            match a {
                links::Action::Mkdir(d) => println!("would create {}", d.display()),
                links::Action::Link { path, .. } => println!("would link  {}", path.display()),
                links::Action::Remove(p) => println!("would prune {}", p.display()),
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    for line in links::apply(&plan)? {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
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
        undocumented(&Cli::command(), "skima", &mut out);
        assert_eq!(out, Vec::<String>::new());
    }
}
