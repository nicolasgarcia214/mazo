use std::path::Path;

use super::{Audit, Backend, DEFAULT_BUDGET, Pane, audited, backend, dry_run, run, select};
use crate::config::{Config, Judge};
use crate::inventory::Inventory;
use crate::report::{self, Input};
use crate::{Error, Result, evals, ledger};

pub const LIMIT: usize = 20;

pub struct Planned<'a> {
    pub skill: &'a str,
    pub root: &'a Path,
    pub judge: &'a Judge,
    pub backend: Box<dyn Backend>,
    pub budget: Option<u64>,
}

pub struct Args {
    pub limit: Option<usize>,
    pub resynth: bool,
    pub dry_run: bool,
    pub json: bool,
}

pub fn plan<'a>(cfg: &'a Config, inv: &'a Inventory, skill: &'a str, budget: Option<u64>) -> Result<Planned<'a>> {
    let Some(root) = inv.root(skill) else {
        return Err(Error::Usage(format!("{skill} is not a canonical skill")));
    };
    let Some(judge) = &cfg.judge else {
        return Err(Error::Usage(
            "no judge configured; add one to the config, for example:\n[judge]\nagent = \"claude\"".into(),
        ));
    };
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
