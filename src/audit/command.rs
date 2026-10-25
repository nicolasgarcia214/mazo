//! `mazo audit` as the CLI and the TUI both run it: [`plan`] makes the checks
//! that need no scan, [`command`] runs the audit over a scanned input.

use std::path::Path;

use super::{Audit, Backend, DEFAULT_BUDGET, Pane, audited, backend, dry_run, run, select};
use crate::config::{Config, Judge};
use crate::inventory::Inventory;
use crate::report::{self, Input};
use crate::{Error, Result, evals, ledger};

/// The most sessions a run judges unless told otherwise.
pub const LIMIT: usize = 20;

/// An audit that passed the checks [`plan`] makes.
pub struct Planned<'a> {
    pub skill: &'a str,
    /// The root the skill is read from: the source or the store.
    pub root: &'a Path,
    pub judge: &'a Judge,
    pub backend: Box<dyn Backend>,
    /// The dollar cap; only a judge that reports its cost has one.
    pub budget: Option<u64>,
}

/// What a run is asked for beyond its skill and budget.
pub struct Args {
    /// `None` judges every candidate session.
    pub limit: Option<usize>,
    pub resynth: bool,
    pub dry_run: bool,
    pub json: bool,
}

/// The usage errors of auditing `skill` with `--max-budget-usd` `budget`:
/// not a skill, no judge, or a budget the judge cannot take.
pub fn plan<'a>(cfg: &'a Config, inv: &'a Inventory, skill: &'a str, budget: Option<u64>) -> Result<Planned<'a>> {
    let Some(root) = inv.root(skill) else {
        return Err(Error::Usage(format!("{skill} is not a canonical skill")));
    };
    let Some(judge) = &cfg.judge else {
        return Err(Error::Usage(
            "no judge configured; add one to the config, for example:\n[judge]\nagent = \"claude\"".into(),
        ));
    };
    // A judge that reports no dollar figure has `--limit` as its only cap.
    let backend = backend(judge, &cfg.homes)?;
    let budget = match (backend.budget_flag(), budget) {
        (Some(_), budget) => Some(budget.unwrap_or(DEFAULT_BUDGET)),
        (None, Some(_)) => {
            return Err(Error::Usage(format!(
                "--max-budget-usd needs a judge that reports its cost; the {} judge is capped by --limit",
                ledger::name(judge.agent)
            )));
        }
        (None, None) => None,
    };
    if budget.is_some_and(|b| b < 4 * super::CENT) {
        return Err(Error::Usage("--max-budget-usd must be at least 0.04".into()));
    }
    Ok(Planned { skill, root, judge, backend, budget })
}

/// Runs `p` over `input` and the data directory `data`, reporting as it goes
/// to `pane`, or to stderr without one: what `mazo audit` prints on stdout,
/// and whether the run succeeded. A failed login check is a usage error: no
/// judge was started.
pub fn command(
    p: &Planned,
    args: &Args,
    input: &Input,
    data: &Path,
    tty: bool,
    pane: Option<&Pane>,
) -> Result<(String, bool)> {
    let candidates = select(input.events, p.skill, &audited(data, p.skill)?, args.limit);
    let a = Audit {
        skill: p.skill,
        source: p.root,
        data,
        judge: p.judge,
        backend: p.backend.as_ref(),
        budget: p.budget,
        evals: &evals::load(&input.cfg.evals_dir, input.inv, p.skill),
        usage: &report::usage_section(input, p.skill),
        pane,
    };
    if args.dry_run {
        return Ok((dry_run(&a, &candidates, tty)?, true));
    }
    if super::calls(&a, &candidates, args.resynth)? {
        a.caller().login()?;
    }
    let run = run(&a, &candidates, args.resynth)?;
    Ok((if args.json { run.json(&a) } else { run.text(&a) }, run.ok()))
}
