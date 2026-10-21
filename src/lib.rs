//! mazo: skill usage from agent transcripts, and sync of skill links.
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
pub mod store;
pub mod tail;
pub mod tui;
pub mod yaml;

/// An error is reported to the user as one `ERROR: …` line; its kind picks
/// the exit code.
#[derive(Debug)]
pub enum Error {
    /// The command cannot run as invoked or configured: exit 2.
    Usage(String),
    /// The command failed: exit 1.
    Failed(String),
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Usage(_) => 2,
            Error::Failed(_) => 1,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Usage(m) | Error::Failed(m) => f.write_str(m),
        }
    }
}

impl From<String> for Error {
    fn from(m: String) -> Error {
        Error::Failed(m)
    }
}

impl From<&str> for Error {
    fn from(m: &str) -> Error {
        Error::Failed(m.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
