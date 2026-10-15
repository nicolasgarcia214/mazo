use std::io::IsTerminal;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use skima::config::{self, Config, Home};
use skima::inventory::Inventory;
use skima::ledger::{self, Agent, Channel, Ledger};
use skima::report::{self, Since, UsageArgs};
use skima::{links, policy, scan};

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
        #[arg(long)]
        json: bool,
    },
    /// One row per canonical skill: sessions, channel counts, agents, last use.
    Usage {
        #[arg(long, default_value = "30d")]
        since: Since,
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        #[arg(long, value_enum)]
        channel: Option<Channel>,
        /// Include `other` origins and `probe` actors.
        #[arg(long)]
        all: bool,
        /// Include authoring reads.
        #[arg(long)]
        authoring: bool,
        #[arg(long)]
        json: bool,
    },
    /// Skills with no strong event in the window.
    Unused {
        #[arg(long, default_value = "60d")]
        since: Since,
        #[arg(long)]
        json: bool,
    },
    /// One skill's evidence: histogram, recent events with their sources, grades.
    Show {
        skill: String,
        #[arg(long, default_value = "30d")]
        since: Since,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Drift and policy of every agent home. Writes nothing.
    Status {
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        #[arg(long)]
        json: bool,
    },
    /// Link every canonical skill into every agent home (replaces install.sh).
    Sync {
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a config for a skills directory, with every rule off.
    Init {
        #[arg(long)]
        source: std::path::PathBuf,
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
    let tty = std::io::stdout().is_terminal();
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
        Cmd::Status { agent, json } => cmd_status(&cfg, &inv, agent, json),
        Cmd::Sync { agent, dry_run } => cmd_sync(&cfg, &inv, agent, dry_run),
        Cmd::Prune { name, yes, force } => {
            skima::lifecycle::prune(&cfg.source, name.as_deref(), yes, force, std::io::stdin().is_terminal())
        }
        Cmd::Init { .. } => unreachable!("handled before the config loads"),
    }
}

/// Usage reports scan first, then render, then print freshness warnings (SPEC D3, §5.4).
fn with_scanned(
    cfg: &Config,
    inv: &Inventory,
    render: impl FnOnce(&report::Input) -> skima::Result<String>,
) -> skima::Result<ExitCode> {
    let data = ledger::data_dir()?;
    warn_io_errors(&scan::scan(cfg, inv, &data, false)?);
    let ledger = Ledger::load(&data)?;
    let coverage = ledger::load_coverage(&data)?;
    let now = jiff::Timestamp::now();
    let input = report::Input { cfg, inv, events: ledger.events(), coverage: &coverage, now };
    print!("{}", render(&input)?);
    for w in policy::freshness(cfg, &coverage, now) {
        eprintln!("{w}");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_scan(cfg: &Config, inv: &Inventory, reparse: bool, detail: bool, json: bool) -> skima::Result<ExitCode> {
    let data = ledger::data_dir()?;
    let r = scan::scan(cfg, inv, &data, reparse)?;
    if json {
        let homes: serde_json::Map<String, serde_json::Value> = r
            .homes
            .iter()
            .map(|(name, h)| {
                (
                    name.clone(),
                    serde_json::json!({
                        "new_events": h.new_events,
                        "files": h.files,
                        "io_errors": h.io_errors,
                        "unparsed": h.unparsed,
                        "unparsed_samples": h.unparsed_samples,
                        "canary": h.canary,
                    }),
                )
            })
            .collect();
        println!("{}", serde_json::Value::Object(homes));
        return Ok(ExitCode::SUCCESS);
    }
    let summary: Vec<String> = r.homes.iter().map(|(n, h)| format!("{n} +{}", h.new_events.len())).collect();
    println!("scan: {}", summary.join(" · "));
    warn_io_errors(&r);
    if detail {
        let ledger = Ledger::load(&data)?;
        for (name, h) in &r.homes {
            let mut counts = std::collections::BTreeMap::<Channel, usize>::new();
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

fn warn_io_errors(r: &scan::ScanReport) {
    for (home, h) in &r.homes {
        for e in &h.io_errors {
            eprintln!("warning: {home}: {e}");
        }
    }
}

fn selected(cfg: &Config, agent: Option<Agent>) -> Vec<&Home> {
    cfg.homes.iter().filter(|h| agent.is_none_or(|a| h.agent == a)).collect()
}

/// Per-home rules for the selected homes (SPEC §5.4), as `(home, line)`.
fn home_errors(cfg: &Config, inv: &Inventory, homes: &[&Home]) -> Vec<(String, String)> {
    homes.iter().flat_map(|h| policy::per_home(&cfg.policy, inv, h).into_iter().map(|e| (h.name.clone(), e))).collect()
}

fn cmd_status(cfg: &Config, inv: &Inventory, agent: Option<Agent>, json: bool) -> skima::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let skill_errors = policy::per_skill(cfg, inv);
    let plan = links::plan(cfg, inv, &homes);
    let policy_errors = home_errors(cfg, inv, &homes);
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
        for e in skill_errors.iter().chain(home_errors().map(|(_, e)| e)) {
            eprintln!("{e}");
        }
        for w in &warnings {
            eprintln!("{w}");
        }
    }
    Ok(if failed { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

fn cmd_sync(cfg: &Config, inv: &Inventory, agent: Option<Agent>, dry_run: bool) -> skima::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let plan = links::plan(cfg, inv, &homes);
    let errors: Vec<String> = policy::per_skill(cfg, inv)
        .into_iter()
        .chain(home_errors(cfg, inv, &homes).into_iter().chain(plan.conflicts.iter().cloned()).map(|(_, e)| e))
        .collect();
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
