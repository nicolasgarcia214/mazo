//! mazo: skill usage from agent transcripts, and sync of skill links.
//!
//! Five paths:
//! - write: `tail → adapters → classify → ledger`;
//! - read: `ledger + inventory + evals → report`;
//! - sync: `config + inventory + policy → links`;
//! - judge: `ledger + adapters (condense) → audit`;
//! - store: `source → audit (vet) → store + mazo.lock → links` for `add` and
//!   `update`, `lifecycle → store` for `retire`; with `--project` these three
//!   act on a project's store and lockfile.
//!
//! The TUI draws the read path's report and runs the CLI's own code for its actions.

#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod adapters;
pub mod audit;
pub mod classify;
pub mod config;
pub mod ctx;
pub mod evals;
mod examples;
pub mod help;
mod history;
pub mod html;
pub mod inventory;
pub mod ledger;
pub mod lifecycle;
pub mod links;
mod pick;
pub mod policy;
mod readcmd;
pub mod render;
pub mod report;
pub mod scan;
pub mod store;
pub mod tail;
pub mod theme;
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

/// The error of a step on `p`: `<p>: <error>`. The path is
/// [`inert`](render::inert), since a skill's author or a transcript may name it.
pub fn at<E: std::fmt::Display>(p: &std::path::Path) -> impl Fn(E) -> String + Copy {
    move |e| format!("{}: {e}", render::PathExt::inert(p))
}

/// A name no other call makes, in this process or any other: the pid, the
/// time, and a count, since the clock repeats across threads where it ticks
/// in microseconds, as on macOS.
pub fn unique() -> String {
    static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{}-{call}", std::process::id(), jiff::Timestamp::now().as_nanosecond())
}

/// Writes `args` to stdout. A reader that closed it early, as `head` does,
/// ends mazo quietly with 0; any other write error ends it with 1.
pub fn stdout(args: std::fmt::Arguments) {
    use std::io::Write;
    match std::io::stdout().write_fmt(args) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(e) => {
            eprintln!("ERROR: writing to stdout: {e}");
            std::process::exit(1)
        }
    }
}

/// `print!` through [`stdout`].
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => { $crate::stdout(format_args!($($arg)*)) };
}

/// `println!` through [`stdout`].
#[macro_export]
macro_rules! outln {
    ($($arg:tt)*) => { $crate::stdout(format_args!("{}\n", format_args!($($arg)*))) };
}

#[cfg(test)]
mod tests {
    /// Threads that ask at once, inside one clock tick, still get names apart.
    #[test]
    fn unique_names_differ_across_threads() {
        let threads: Vec<_> =
            (0..8).map(|_| std::thread::spawn(|| (0..500).map(|_| super::unique()).collect::<Vec<_>>())).collect();
        let names: Vec<String> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
        let distinct: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(distinct.len(), names.len());
    }
}
