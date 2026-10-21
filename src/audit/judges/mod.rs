//! The judge agents. Each [`Backend`] sends one agent a prompt; a
//! [`Subprocess`] holds one agent's argv, input, login check, scratch home and
//! output format, and [`spawn`] runs any of them the same way.

mod claude;
mod codex;
mod command;
mod pi;

use std::fs::{self, DirBuilder};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde_json::Value;

use super::session::{SESSION, SESSION_PROMPT, SESSION_SCHEMA};
use super::{Audit, Stage, cents};
use crate::Result;
use crate::config::{Billing, Home, Judge, JudgeAgent};
use crate::ledger::Agent;

/// How mazo calls one agent as its judge.
pub trait Backend {
    /// One judge call with `stage`'s prompt on `input`, capped at `cap`.
    fn call(&self, a: &Audit, stage: &Stage, cap: Option<u64>, input: &str) -> Result<Reply>;
    /// Under `subscription`, the check a run makes once before its first call,
    /// with `data` for a scratch directory. A failed check is an
    /// [`crate::Error::Usage`] that says what would be billed.
    fn login(&self, _data: &Path) -> Result<()> {
        Ok(())
    }
    /// The flag that caps a call's dollar cost, for an agent that reports one.
    fn budget_flag(&self) -> Option<&'static str> {
        None
    }
    /// The judge line `--dry-run` prints, with `data` for a scratch home.
    fn dry_run(&self, data: &Path) -> String;
    /// The model `--dry-run` names, where the judge line may not.
    fn model(&self) -> Option<String> {
        None
    }
}

/// A judge that is a process: [`spawn`] runs it for each call.
pub trait Subprocess {
    /// The argv of a call with `stage`'s prompt and schema, `[judge] model`, and
    /// [`SCHEMA_FILE`], [`RESULT_FILE`] or [`INPUT_FILE`] standing in for the files
    /// [`spawn`] writes.
    fn argv(&self, stage: &Stage) -> Vec<String>;
    /// What the call reads on stdin; none closes it.
    fn stdin(&self, stage: &Stage, input: &str) -> Option<String>;
    /// As [`Backend::login`].
    fn login(&self, _data: &Path) -> Result<()> {
        Ok(())
    }
    /// A variable naming a scratch home under `data`, made afresh before and
    /// after each call.
    fn scratch(&self, _data: &Path) -> Result<Option<(&'static str, PathBuf)>> {
        Ok(None)
    }
    /// What a finished call returned, from its output and the result file it
    /// wrote; `cap` is the call's dollar cap.
    fn reply(&self, out: &Output, written: Option<Vec<u8>>, cap: Option<u64>) -> Result<Reply>;
    /// As [`Backend::budget_flag`].
    fn budget_flag(&self) -> Option<&'static str> {
        None
    }
    /// The judge line `--dry-run` prints for the quoted argv.
    fn shown(&self, data: &Path, argv: &str) -> String;
    /// As [`Backend::model`].
    fn model(&self) -> Option<String> {
        None
    }
}

impl<T: Subprocess> Backend for T {
    fn call(&self, a: &Audit, stage: &Stage, cap: Option<u64>, input: &str) -> Result<Reply> {
        spawn(self, a, stage, cap, input)
    }

    fn login(&self, data: &Path) -> Result<()> {
        Subprocess::login(self, data)
    }

    fn budget_flag(&self) -> Option<&'static str> {
        Subprocess::budget_flag(self)
    }

    /// The stage-1 argv, with its prompt, schema and empty arguments quoted
    /// for a shell.
    fn dry_run(&self, data: &Path) -> String {
        let quoted: Vec<String> = self
            .argv(&SESSION)
            .iter()
            .map(|arg| match arg.as_str() {
                SESSION_PROMPT => "\"$(cat <prompt>)\"".into(),
                s if s == *SESSION_SCHEMA => "<schema>".into(),
                "" => "\"\"".into(),
                s => s.into(),
            })
            .collect();
        self.shown(data, &quoted.join(" "))
    }

    fn model(&self) -> Option<String> {
        Subprocess::model(self)
    }
}

/// The backend of `judge`; Codex links the configured Codex home's
/// `auth.json`, Pi reads its `settings.json` for the default model, and
/// the command judge runs `[judge] argv`.
pub fn backend(judge: &Judge, homes: &[Home]) -> Result<Box<dyn Backend>> {
    let model = judge.model.clone();
    Ok(match judge.agent {
        JudgeAgent::Claude => Box::new(claude::Claude { model }),
        JudgeAgent::Codex => {
            Box::new(codex::Codex { model, auth: agent_home(homes, Agent::Codex, ".codex")?.join("auth.json") })
        }
        JudgeAgent::Pi => {
            Box::new(pi::Pi { model, settings: agent_home(homes, Agent::Pi, ".pi/agent")?.join("settings.json") })
        }
        JudgeAgent::Command => Box::new(command::Command { argv: judge.argv.clone() }),
    })
}

/// The configured home of `agent`, else `~/<default>`.
fn agent_home(homes: &[Home], agent: Agent, default: &str) -> Result<PathBuf> {
    Ok(match homes.iter().find(|h| h.agent == agent) {
        Some(h) => h.path.clone(),
        None => PathBuf::from(crate::config::home_dir()?).join(default),
    })
}

/// Stand-ins in a [`Backend::argv`] for the files each call writes in its own
/// temporary directory: the answer schema and the result, each a whole
/// argument, and the input, which an argument may embed (Pi's `@<input file>`).
pub const SCHEMA_FILE: &str = "<schema file>";
pub const RESULT_FILE: &str = "<result file>";
pub const INPUT_FILE: &str = "<input file>";

/// `base`, then `flag model` when a model is set, then `tail`.
fn argv(base: &[&str], flag: &str, model: &Option<String>, tail: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = base.iter().map(|a| a.to_string()).collect();
    if let Some(m) = model {
        argv.extend([flag.to_string(), m.clone()]);
    }
    argv.extend(tail.map(String::from));
    argv
}

/// What one judge call returned: its reported cost, none from a judge that
/// reports none, and the answer or why there is none.
pub struct Reply {
    pub cost: Option<u64>,
    pub answer: std::result::Result<Value, NoAnswer>,
}

/// Why a judge call gave no answer.
pub enum NoAnswer {
    /// Over its cap, past its time, or an answer that does not parse: retried as it was.
    Retry(String),
    /// A judge that failed without an answer: a login, key or network failure,
    /// or a signal mazo did not send. The run stops.
    Stop(String),
}

/// A [`NoAnswer::Stop`] for `out`, with the reason the judge gave and the
/// first lines of its stderr.
fn stop(out: &Output, result: Option<&str>) -> NoAnswer {
    let result = result.map(|r| format!(": {r}")).unwrap_or_default();
    let lines: String = String::from_utf8_lossy(&out.stderr)
        .lines()
        .take(STDERR_LINES)
        .map(|l| format!("\n  {}", l.chars().take(STDERR_CHARS).collect::<String>()))
        .collect();
    NoAnswer::Stop(format!("the judge failed without an answer ({}){result}{lines}", out.status))
}

/// No answer stops the run after a nonzero exit and is retried after a zero one.
fn exited(out: &Output, answer: std::result::Result<Value, String>) -> std::result::Result<Value, NoAnswer> {
    answer.map_err(|e| {
        if out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            NoAnswer::Retry(format!("{e} ({}): {}", out.status, stderr.lines().next().unwrap_or_default()))
        } else {
            stop(out, Some(&e))
        }
    })
}

/// The variables that bill an API key or a cloud account instead of a login:
/// Claude's key and token, its cloud routes, and the keys Codex and Pi may read.
const BILLED: [&str; 7] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
];

/// Removes [`BILLED`] from `cmd`'s environment under `subscription`.
fn scrub(cmd: &mut Command, billing: Billing) {
    if billing == Billing::Subscription {
        for var in BILLED {
            cmd.env_remove(var);
        }
    }
}

/// A fresh empty directory under `<data>/audit-runs/`, and its unique name.
fn run_dir(data: &Path) -> Result<(String, PathBuf)> {
    let unique = format!("{}-{}", std::process::id(), Timestamp::now().as_nanosecond());
    let runs = data.join("audit-runs");
    let dir = runs.join(&unique);
    fs::create_dir_all(&runs).and_then(|()| fs::create_dir(&dir)).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok((unique, dir))
}

/// One subprocess judge call: `backend`'s argv run in a fresh empty directory under
/// `<data>/audit-runs/`, with the files it names in a fresh temporary
/// directory, both removed after, and its scratch home made afresh before and
/// after. A capped call of an agent with a [`Subprocess::budget_flag`] gets that
/// flag, and is counted at its cap when killed past its time, the most it
/// could have spent.
fn spawn(backend: &impl Subprocess, a: &Audit, stage: &Stage, cap: Option<u64>, input: &str) -> Result<Reply> {
    let argv = backend.argv(stage);
    let (unique, dir) = run_dir(a.data)?;
    let scratch = backend.scratch(a.data)?;
    let files = std::env::temp_dir().join(format!("mazo-judge-{unique}"));
    let result = files.join("result.json");
    let output = DirBuilder::new().mode(0o700).create(&files).and_then(|()| {
        let mut cmd = Command::new(&argv[0]);
        for arg in &argv[1..] {
            match arg.as_str() {
                SCHEMA_FILE => {
                    let path = files.join("schema.json");
                    fs::write(&path, stage.schema.as_str())?;
                    cmd.arg(path)
                }
                RESULT_FILE => cmd.arg(&result),
                s if s.contains(INPUT_FILE) => {
                    let path = files.join("input.md");
                    fs::write(&path, input)?;
                    cmd.arg(s.replace(INPUT_FILE, &path.to_string_lossy()))
                }
                _ => cmd.arg(arg),
            };
        }
        let stdin = backend.stdin(stage, input);
        cmd.current_dir(&dir)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let (Some(cap), Some(flag)) = (cap, backend.budget_flag()) {
            cmd.args([flag, &cents(cap)]);
        }
        if let Some((var, home)) = &scratch {
            cmd.env(var, home);
        }
        scrub(&mut cmd, a.judge.billing);
        wait(cmd.spawn()?, stdin, JUDGE_TIMEOUT)
    });
    let written = fs::read(&result);
    let removed = fs::remove_dir_all(&dir);
    let cleaned = fs::remove_dir_all(&files);
    let output = output.map_err(|e| format!("{}: {e}", argv[0]))?;
    removed.map_err(|e| format!("{}: {e}", dir.display()))?;
    cleaned.map_err(|e| format!("{}: {e}", files.display()))?;
    if scratch.is_some() {
        backend.scratch(a.data)?;
    }
    let Some(output) = output else {
        let minutes = JUDGE_TIMEOUT.as_secs() / 60;
        let answer = Err(NoAnswer::Retry(format!("no answer within {minutes} minutes; the judge was killed")));
        return Ok(Reply { cost: cap.filter(|_| backend.budget_flag().is_some()), answer });
    };
    backend.reply(&output, written.ok(), cap)
}

/// How long one judge call may run before it is killed and counted as invalid.
const JUDGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The most stderr lines a stopped judge prints, and the most of each.
const STDERR_LINES: usize = 5;
const STDERR_CHARS: usize = 200;

/// How long a judge past its limit has to exit on SIGTERM before SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(5);

/// `child`'s output once it exits, after writing `stdin` to it; `None` when it
/// was still running after `limit` and was killed: SIGTERM first, which
/// launchers such as Codex's npm shim forward to the process doing the work
/// and SIGKILL would orphan, then SIGKILL after [`KILL_GRACE`]. The pipes are read on
/// detached threads given a second after the exit, so a grandchild left
/// holding one costs its output, not a hang.
fn wait(mut child: Child, stdin: Option<String>, limit: Duration) -> std::io::Result<Option<Output>> {
    // A judge that exits without reading its input closes the pipe; its
    // output says why.
    if let (Some(mut pipe), Some(stdin)) = (child.stdin.take(), stdin) {
        std::thread::spawn(move || pipe.write_all(stdin.as_bytes()));
    }
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        if let Some(mut pipe) = pipe {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                let _ = tx.send(bytes);
            });
        }
        rx
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            // SAFETY: kill(2) on our own child, unreaped, so its pid is not reused.
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
            let grace = Instant::now() + KILL_GRACE;
            while child.try_wait()?.is_none() && Instant::now() < grace {
                std::thread::sleep(Duration::from_millis(10));
            }
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let read = |rx: mpsc::Receiver<Vec<u8>>| rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
    Ok(Some(Output { status, stdout: read(stdout), stderr: read(stderr) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Child {
        Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// A judge that answers without a subprocess.
    struct Canned;

    impl Backend for Canned {
        fn call(&self, _: &Audit, _: &Stage, cap: Option<u64>, input: &str) -> Result<Reply> {
            Ok(Reply { cost: cap, answer: Ok(Value::from(input)) })
        }
        fn dry_run(&self, _: &Path) -> String {
            "canned".into()
        }
    }

    #[test]
    fn a_backend_that_owns_its_call_is_called_without_a_subprocess() {
        let data = tempfile::tempdir().unwrap();
        let judge = Judge { agent: JudgeAgent::Claude, model: None, billing: Billing::Subscription, argv: vec![] };
        let evals = crate::evals::Evals { grades: vec![], retros: vec![] };
        let a = Audit {
            skill: "s",
            source: data.path(),
            data: data.path(),
            judge: &judge,
            backend: &Canned,
            budget: None,
            evals: &evals,
            usage: &Value::Null,
        };
        let reply = a.backend.call(&a, &SESSION, Some(7), "in").unwrap();
        assert_eq!((reply.cost, reply.answer.ok()), (Some(7), Some(Value::from("in"))));
        assert!(!data.path().join("audit-runs").exists());
    }

    #[test]
    fn wait_returns_the_output_of_a_judge_that_exits() {
        let out = wait(sh("cat; echo e >&2"), Some("in".into()), Duration::from_secs(30)).unwrap().unwrap();
        assert!(out.status.success());
        assert_eq!((out.stdout.as_slice(), out.stderr.as_slice()), (b"in".as_slice(), b"e\n".as_slice()));
    }

    #[test]
    fn wait_kills_a_judge_past_its_limit_even_when_a_grandchild_holds_its_pipes() {
        let started = Instant::now();
        assert!(wait(sh("sleep 10 & sleep 10"), None, Duration::from_millis(100)).unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
        // A launcher that forwards SIGTERM takes the process doing the work with it. The
        // launcher reaps the worker so `kill -0` cannot see a zombie no one has reaped.
        let pid = tempfile::NamedTempFile::new().unwrap();
        let launcher =
            format!("trap 'kill $!; wait $!; exit' TERM; sleep 10 & echo $! > {}; wait", pid.path().display());
        assert!(wait(sh(&launcher), None, Duration::from_millis(200)).unwrap().is_none());
        let worker = fs::read_to_string(pid.path()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let alive = Command::new("kill").args(["-0", worker.trim()]).stderr(Stdio::null()).status().unwrap();
        assert!(!alive.success(), "the worker outlived its launcher");
        // An exited judge whose grandchild keeps stdout open costs its output, not a hang.
        let started = Instant::now();
        let out = wait(sh("sleep 10 & echo x"), None, Duration::from_secs(30)).unwrap().unwrap();
        assert!(out.status.success());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
