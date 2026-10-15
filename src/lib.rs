//! skima: skill usage from agent transcripts, and sync of skill links.
//!
//! Three paths (SPEC §9): write `tail → adapters → classify → ledger`, read
//! `ledger + inventory + evals → report`, sync `config + inventory + policy → links`.

pub mod adapters;
pub mod classify;
pub mod config;
pub mod evals;
pub mod examples;
pub mod history;
pub mod inventory;
pub mod ledger;
pub mod lifecycle;
pub mod links;
pub mod policy;
pub mod readcmd;
pub mod render;
pub mod report;
pub mod scan;
pub mod tail;
pub mod yaml;

/// Errors are reported to the user as one `ERROR: …` line (SPEC §5.5).
pub type Result<T> = std::result::Result<T, String>;
