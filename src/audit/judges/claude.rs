//! The Claude judge: `claude -p` with tools off, the input on stdin, a dollar
//! cost in its JSON result, and `--max-budget-usd` as its cap.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

use super::{NoAnswer, Reply, Subprocess, run_dir, scrub, stop, wait};
use crate::audit::{CANCELLED, Stage, parse_usd};
use crate::config::Billing;
use crate::{Error, Result};

pub struct Claude {
    pub model: Option<String>,
}

/// Claude's `--output-format json` result; the cost stays decimal text.
#[derive(Deserialize)]
struct ClaudeOutput<'a> {
    #[serde(borrow)]
    total_cost_usd: Option<&'a RawValue>,
    #[serde(default)]
    is_error: bool,
    subtype: Option<String>,
    terminal_reason: Option<String>,
    result: Option<String>,
    structured_output: Option<Value>,
}

/// The flag that caps a call's dollar cost.
const BUDGET_FLAG: &str = "--max-budget-usd";

/// How long `claude auth status` may run before the login check fails.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);

impl Subprocess for Claude {
    fn argv(&self, stage: &Stage) -> Vec<String> {
        let base = [
            "claude",
            "-p",
            "--safe-mode",
            "--tools",
            "",
            "--system-prompt",
            stage.prompt,
            "--output-format",
            "json",
            "--json-schema",
            stage.schema.as_str(),
            "--no-session-persistence",
        ];
        super::argv(&base, "--model", &self.model, None)
    }

    fn stdin(&self, _stage: &Stage, input: &str) -> Option<String> {
        Some(input.to_string())
    }

    /// `claude auth status`, in the environment and the kind of fresh empty
    /// directory the calls get, must report a claude.ai or OAuth-token login.
    fn login(&self, data: &Path, cancel: &dyn Fn() -> bool) -> Result<()> {
        let (_, dir) = run_dir(data)?;
        let checked = claude_login(OsStr::new("claude"), &dir, AUTH_TIMEOUT, cancel);
        fs::remove_dir_all(&dir).map_err(crate::at(&dir))?;
        checked.map_err(Error::Usage)
    }

    /// A call that ran but reported no cost is counted at its cap, the most it
    /// could have spent short of one turn's overshoot, so the budget still
    /// binds. An `api_error` or output that is no JSON stops the run at the
    /// reported cost or $0, and a signal mazo did not send stops it at the cap.
    fn reply(&self, out: &Output, _written: Option<Vec<u8>>, cap: Option<u64>) -> Result<Reply> {
        let cap = cap.unwrap_or_default();
        if out.status.signal().is_some() {
            return Ok(Reply { cost: Some(cap), answer: Err(stop(out, None)) });
        }
        let Ok(parsed) = serde_json::from_slice::<ClaudeOutput>(&out.stdout) else {
            return Ok(Reply { cost: Some(0), answer: Err(stop(out, None)) });
        };
        let cost = parsed.total_cost_usd.map(|c| parse_usd(c.get())).transpose()?;
        if parsed.is_error && parsed.terminal_reason.as_deref() == Some("api_error") {
            return Ok(Reply { cost: Some(cost.unwrap_or(0)), answer: Err(stop(out, parsed.result.as_deref())) });
        }
        let answer = match parsed.structured_output {
            Some(v) if !parsed.is_error => Ok(v),
            _ => Err(NoAnswer::Retry(format!("no answer ({})", parsed.subtype.as_deref().unwrap_or("error")))),
        };
        Ok(Reply { cost: Some(cost.unwrap_or(cap)), answer })
    }

    fn budget_flag(&self) -> Option<&'static str> {
        Some(BUDGET_FLAG)
    }

    fn shown(&self, _data: &Path, argv: &str) -> String {
        format!("{argv} {BUDGET_FLAG} <cap> < <input>")
    }
}

/// `program auth status` run in `dir` without the billed variables, killed
/// after `limit` or once `cancel` turns true: Ok for a claude.ai or
/// OAuth-token login.
fn claude_login(
    program: &OsStr,
    dir: &Path,
    limit: Duration,
    cancel: &dyn Fn() -> bool,
) -> std::result::Result<(), String> {
    let mut cmd = Command::new(program);
    cmd.args(["auth", "status"]).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    scrub(&mut cmd, Billing::Subscription);
    let failed = |e: std::io::Error| format!("claude auth status: {e}");
    let check = "check it with claude auth status, or set billing = \"api\" under [judge]";
    let Some(out) = wait(cmd.spawn().map_err(failed)?, None, limit, cancel).map_err(failed)? else {
        if cancel() {
            return Err(CANCELLED.into());
        }
        let secs = limit.as_secs();
        return Err(format!("claude auth status gave no answer within {secs} seconds; {check}"));
    };
    let status: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    match (out.status.success(), status["authMethod"].as_str()) {
        (true, Some("claude.ai" | "oauth_token")) => Ok(()),
        (_, Some(method)) if !matches!(method, "claude.ai" | "oauth_token") => Err(format!(
            "the Claude judge would bill {method}; log in with claude /login, or set billing = \"api\" under [judge]"
        )),
        _ => Err(format!("could not read the Claude judge's login from claude auth status ({}); {check}", out.status)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// `claude_login` on a stub whose `auth status` runs `script`, never cancelled.
    fn login_with(script: &str, limit: Duration) -> std::result::Result<(), String> {
        cancelled_login(script, limit, &|| false)
    }

    fn cancelled_login(script: &str, limit: Duration, cancel: &dyn Fn() -> bool) -> std::result::Result<(), String> {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("claude");
        fs::write(&stub, format!("#!/bin/sh\n{script}")).unwrap();
        Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
        claude_login(stub.as_os_str(), dir.path(), limit, cancel)
    }

    #[test]
    fn a_claude_login_that_cannot_be_read_fails_the_check() {
        let limit = Duration::from_secs(30);
        assert_eq!(login_with("echo '{\"authMethod\":\"claude.ai\"}'\n", limit), Ok(()));
        let started = Instant::now();
        let err = login_with("sleep 10\n", Duration::from_millis(100)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5), "the check waited out its stub");
        assert!(err.starts_with("claude auth status gave no answer within 0 seconds; check it with"), "{err}");
        for script in ["echo 'Not logged in'\nexit 1\n", "echo '{\"authMethod\":\"claude.ai\"}'\nexit 1\n", ""] {
            let err = login_with(script, limit).unwrap_err();
            assert!(err.starts_with("could not read the Claude judge's login from claude auth status ("), "{err}");
            assert!(err.ends_with("check it with claude auth status, or set billing = \"api\" under [judge]"), "{err}");
        }
    }

    #[test]
    fn a_cancelled_login_check_kills_its_check_at_once() {
        let pid = tempfile::NamedTempFile::new().unwrap();
        let started = Instant::now();
        let script = format!("echo $$ > {}\nexec sleep 600\n", pid.path().display());
        let pid = pid.path().to_path_buf();
        let written = move || fs::read_to_string(&pid).is_ok_and(|p| !p.is_empty());
        let err = cancelled_login(&script, AUTH_TIMEOUT, &written).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(started.elapsed() < Duration::from_secs(5), "the check waited out its stub");
    }
}
