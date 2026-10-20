//! skima: skill usage from agent transcripts, and sync of skill links.
//!
//! Four paths: write `tail → adapters → classify → ledger`, read
//! `ledger + inventory + evals → report`, sync `config + inventory + policy → links`,
//! judge `ledger + adapters (condense) → audit`.

pub mod adapters;
pub mod audit;
pub mod classify;
pub mod config;
pub mod evals;
pub mod examples;
pub mod history;
pub mod html;
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
pub mod tui;
pub mod yaml;

/// Errors are reported to the user as one `ERROR: …` line.
pub type Result<T> = std::result::Result<T, String>;
