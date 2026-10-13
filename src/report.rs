//! Reports over the ledger (SPEC F2–F4): usage, unused, show; collapse, held
//! and cluster. Stub: implemented by the report task.

use std::collections::BTreeMap;

use crate::config::Config;
use crate::inventory::Inventory;
use crate::ledger::{Agent, Channel, Coverage, Event};

/// A report window: the last N days, or everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    Days(u32),
    All,
}

impl std::str::FromStr for Since {
    type Err = String;
    fn from_str(s: &str) -> Result<Since, String> {
        if s == "all" {
            return Ok(Since::All);
        }
        s.strip_suffix('d')
            .and_then(|n| n.parse().ok())
            .map(Since::Days)
            .ok_or_else(|| format!("expected <N>d or all, got {s:?}"))
    }
}

/// Everything a report reads. `now` is injected so tests are deterministic.
pub struct Input<'a> {
    pub cfg: &'a Config,
    pub inv: &'a Inventory,
    pub events: &'a [Event],
    pub coverage: &'a BTreeMap<String, Coverage>,
    pub now: jiff::Timestamp,
}

pub struct UsageArgs {
    pub since: Since,
    pub agent: Option<Agent>,
    pub channel: Option<Channel>,
    pub all: bool,
    pub authoring: bool,
}

/// Rendered `skima usage` output (text, or JSON when `json`).
pub fn usage(_input: &Input, _args: &UsageArgs, _json: bool, _tty: bool) -> String {
    todo!("usage report")
}

/// Rendered `skima unused` output.
pub fn unused(_input: &Input, _since: Since, _json: bool, _tty: bool) -> String {
    todo!("unused report")
}

/// Rendered `skima show <skill>` output; Err for an unknown skill.
pub fn show(_input: &Input, _skill: &str, _since: Since, _limit: usize, _json: bool, _tty: bool) -> crate::Result<String> {
    todo!("show report")
}
