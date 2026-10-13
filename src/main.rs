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
    let cfg = config::load()?;
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
    }
}

/// Usage reports scan first, then render, then print freshness warnings (SPEC D3, §5.4).
fn with_scanned(
    cfg: &Config,
    inv: &Inventory,
    render: impl FnOnce(&report::Input) -> skima::Result<String>,
) -> skima::Result<ExitCode> {
    let data = ledger::data_dir()?;
    let scanned = scan::scan(cfg, inv, &data, false)?;
    for (home, r) in &scanned.homes {
        for e in &r.io_errors {
            eprintln!("warning: {home}: {e}");
        }
    }
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
    let summary: Vec<String> = r.homes.iter().map(|(n, h)| format!("{n} +{}", h.new_events)).collect();
    println!("scan: {}", summary.join(" · "));
    for (name, h) in &r.homes {
        for e in &h.io_errors {
            eprintln!("warning: {name}: {e}");
        }
    }
    if detail {
        let ledger = Ledger::load(&data)?;
        for (name, h) in &r.homes {
            let mut counts = std::collections::BTreeMap::<Channel, usize>::new();
            for e in ledger.events().iter().filter(|e| &e.home == name) {
                *counts.entry(e.channel).or_default() += 1;
            }
            let counts: Vec<String> = counts.iter().map(|(c, n)| format!("{c:?}={n}").to_lowercase()).collect();
            println!("{name}: {} files; ledger {}", h.files, counts.join(" "));
            println!("  unparsed skill commands: {}", h.unparsed);
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

fn selected(cfg: &Config, agent: Option<Agent>) -> Vec<&Home> {
    cfg.homes.iter().filter(|h| agent.is_none_or(|a| h.agent == a)).collect()
}

/// Per-skill rules always; per-home rules for the selected homes.
fn policy_errors(cfg: &Config, inv: &Inventory, homes: &[&Home]) -> Vec<String> {
    let mut errors = policy::per_skill(cfg, inv);
    for h in homes {
        errors.extend(policy::per_home(inv, h));
    }
    errors
}

fn cmd_status(cfg: &Config, inv: &Inventory, agent: Option<Agent>, json: bool) -> skima::Result<ExitCode> {
    let homes = selected(cfg, agent);
    let errors = policy_errors(cfg, inv, &homes);
    let plan = links::plan(cfg, inv, &homes);
    let data = ledger::data_dir()?;
    let warnings = policy::freshness(cfg, &ledger::load_coverage(&data)?, jiff::Timestamp::now());
    let drift: Vec<&links::Entry> = plan.entries.iter().filter(|e| e.state != links::State::Ok).collect();
    let failed = !errors.is_empty() || drift.iter().any(|e| e.state.is_error()) || !plan.conflicts.is_empty();
    if json {
        let entries: Vec<serde_json::Value> = drift
            .iter()
            .map(|e| serde_json::json!({"home": e.home, "path": e.path, "state": e.state.label(), "error": e.state.is_error()}))
            .collect();
        let v =
            serde_json::json!({"drift": entries, "policy": errors, "conflicts": plan.conflicts, "warnings": warnings});
        println!("{v}");
    } else {
        for e in &drift {
            let name = e.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            println!("{}: {} {name}", e.home, e.state.label());
        }
        for e in errors.iter().chain(&plan.conflicts) {
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
    let mut errors = policy_errors(cfg, inv, &homes);
    let plan = links::plan(cfg, inv, &homes);
    errors.extend(plan.conflicts.iter().cloned());
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("{e}");
        }
        return Ok(ExitCode::from(1));
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
