mod claude;
mod codex;
mod command;
mod http;
mod pi;

use std::fs::{self, DirBuilder};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::session::{SESSION, SESSION_PROMPT, SESSION_SCHEMA};
use super::{CANCELLED, Stage, cents, frame};
use crate::Result;
use crate::config::{Billing, Home, Judge, JudgeAgent};
use crate::ledger::Agent;

pub trait Backend {
    fn call(
        &self,
        data: &Path,
        billing: Billing,
        stage: &Stage,
        cap: Option<u64>,
        input: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Reply>;
    fn login(&self, _data: &Path, _cancel: &dyn Fn() -> bool) -> Result<()> {
        Ok(())
    }
    fn budget_flag(&self) -> Option<&'static str> {
        None
    }
    fn dry_run(&self) -> String;
    fn model(&self) -> Option<String> {
        None
    }
}

pub trait Subprocess {
    fn argv(&self, stage: &Stage) -> Vec<String>;
    fn stdin(&self, stage: &Stage, input: &str) -> Option<String>;
    fn login(&self, _data: &Path, _cancel: &dyn Fn() -> bool) -> Result<()> {
        Ok(())
    }
    fn scratch(&self, _home: &Path) -> Result<Option<&'static str>> {
        Ok(None)
    }
    fn reply(&self, out: &Output, written: Option<Vec<u8>>, cap: Option<u64>) -> Result<Reply>;
    fn budget_flag(&self) -> Option<&'static str> {
        None
    }
    fn shown(&self, argv: &str) -> String;
    fn model(&self) -> Option<String> {
        None
    }
}

impl<T: Subprocess> Backend for T {
    fn call(
        &self,
        data: &Path,
        billing: Billing,
        stage: &Stage,
        cap: Option<u64>,
        input: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Reply> {
        spawn(self, data, billing, stage, cap, input, cancel)
    }

    fn login(&self, data: &Path, cancel: &dyn Fn() -> bool) -> Result<()> {
        Subprocess::login(self, data, cancel)
    }

    fn budget_flag(&self) -> Option<&'static str> {
        Subprocess::budget_flag(self)
    }

    fn dry_run(&self) -> String {
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
        self.shown(&quoted.join(" "))
    }

    fn model(&self) -> Option<String> {
        Subprocess::model(self)
    }
}

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
        JudgeAgent::Http => Box::new(http::Http::new(judge)?),
    })
}

fn agent_home(homes: &[Home], agent: Agent, default: &str) -> Result<PathBuf> {
    Ok(match homes.iter().find(|h| h.agent == agent) {
        Some(h) => h.path.clone(),
        None => PathBuf::from(crate::config::home_dir()?).join(default),
    })
}

pub const SCHEMA_FILE: &str = "<schema file>";
pub const RESULT_FILE: &str = "<result file>";
pub const INPUT_FILE: &str = "<input file>";

fn prompted(stage: &Stage, input: &str) -> String {
    let schema =
        format!("Answer with one JSON object, and nothing else, matching this schema:\n{}", stage.schema.as_str());
    format!("{}\n{}{input}", stage.prompt, frame("answer-schema", &schema))
}

fn argv(base: &[&str], flag: &str, model: &Option<String>, tail: Option<&str>) -> Vec<String> {
    let mut argv: Vec<String> = base.iter().map(|a| a.to_string()).collect();
    if let Some(m) = model {
        argv.extend([flag.to_string(), m.clone()]);
    }
    argv.extend(tail.map(String::from));
    argv
}

pub struct Reply {
    pub cost: Option<u64>,
    pub answer: std::result::Result<Value, NoAnswer>,
}

pub enum NoAnswer {
    Retry(String),
    Stop(String),
}

fn stop(out: &Output, result: Option<&str>) -> NoAnswer {
    let result = result.map(|r| format!(": {r}")).unwrap_or_default();
    let lines: String = String::from_utf8_lossy(&out.stderr)
        .lines()
        .take(STDERR_LINES)
        .map(|l| format!("\n  {}", l.chars().take(STDERR_CHARS).collect::<String>()))
        .collect();
    NoAnswer::Stop(format!("the judge failed without an answer ({}){result}{lines}", out.status))
}

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

const BILLED: [&str; 7] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
];

fn scrub(cmd: &mut Command, billing: Billing) {
    if billing == Billing::Subscription {
        for var in BILLED {
            cmd.env_remove(var);
        }
    }
}

fn run_dir(data: &Path) -> Result<(String, PathBuf)> {
    let unique = crate::unique();
    let runs = data.join("audit-runs");
    let dir = runs.join(&unique);
    fs::create_dir_all(&runs).and_then(|()| fs::create_dir(&dir)).map_err(crate::at(&dir))?;
    Ok((unique, dir))
}

fn spawn(
    backend: &impl Subprocess,
    data: &Path,
    billing: Billing,
    stage: &Stage,
    cap: Option<u64>,
    input: &str,
    cancel: &dyn Fn() -> bool,
) -> Result<Reply> {
    let argv = backend.argv(stage);
    let (unique, dir) = run_dir(data)?;
    let home = dir.with_file_name(format!("{unique}-home"));
    let scratch = backend.scratch(&home).inspect_err(|_| {
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&home);
    })?;
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
        if let Some(var) = scratch {
            cmd.env(var, &home);
        }
        scrub(&mut cmd, billing);
        wait(cmd.spawn()?, stdin, JUDGE_TIMEOUT, cancel)
    });
    let written = fs::read(&result);
    let removed = fs::remove_dir_all(&dir);
    let cleaned = fs::remove_dir_all(&files);
    let homed = scratch.map(|_| fs::remove_dir_all(&home)).transpose();
    let output = output.map_err(|e| format!("{}: {e}", argv[0]))?;
    removed.map_err(crate::at(&dir))?;
    cleaned.map_err(crate::at(&files))?;
    homed.map_err(crate::at(&home))?;
    let Some(output) = output else {
        if cancel() {
            return Err(CANCELLED.into());
        }
        let minutes = JUDGE_TIMEOUT.as_secs() / 60;
        let answer = Err(NoAnswer::Retry(format!("no answer within {minutes} minutes; the judge was killed")));
        return Ok(Reply { cost: cap.filter(|_| backend.budget_flag().is_some()), answer });
    };
    backend.reply(&output, written.ok(), cap)
}

const JUDGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

const STDERR_LINES: usize = 5;
const STDERR_CHARS: usize = 200;

const KILL_GRACE: Duration = Duration::from_secs(5);

pub(crate) fn wait(
    mut child: Child,
    stdin: Option<String>,
    limit: Duration,
    cancel: &dyn Fn() -> bool,
) -> std::io::Result<Option<Output>> {
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
        if Instant::now() >= deadline || cancel() {
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
    use crate::audit::Audit;

    fn sh(script: &str) -> Child {
        Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    struct Canned;

    impl Backend for Canned {
        fn call(
            &self,
            _: &Path,
            _: Billing,
            _: &Stage,
            cap: Option<u64>,
            input: &str,
            _: &dyn Fn() -> bool,
        ) -> Result<Reply> {
            Ok(Reply { cost: cap, answer: Ok(Value::from(input)) })
        }
        fn dry_run(&self) -> String {
            "canned".into()
        }
    }

    #[test]
    fn a_backend_that_owns_its_call_is_called_without_a_subprocess() {
        let data = tempfile::tempdir().unwrap();
        let judge = Judge { agent: JudgeAgent::Claude, ..Judge::default() };
        let evals = crate::evals::Evals::default();
        let a = Audit {
            skill: "s",
            source: data.path(),
            data: data.path(),
            judge: &judge,
            backend: &Canned,
            budget: None,
            evals: &evals,
            usage: &Value::Null,
            pane: None,
        };
        let c = a.caller();
        let reply = c.backend.call(c.data, c.judge.billing, &SESSION, Some(7), "in", &|| false).unwrap();
        assert_eq!((reply.cost, reply.answer.ok()), (Some(7), Some(Value::from("in"))));
        assert!(!data.path().join("audit-runs").exists());
    }

    #[test]
    fn wait_returns_the_output_of_a_judge_that_exits() {
        let out = wait(sh("cat; echo e >&2"), Some("in".into()), Duration::from_secs(30), &|| false).unwrap().unwrap();
        assert!(out.status.success());
        assert_eq!((out.stdout.as_slice(), out.stderr.as_slice()), (b"in".as_slice(), b"e\n".as_slice()));
    }

    #[test]
    fn wait_kills_a_judge_past_its_limit_even_when_a_grandchild_holds_its_pipes() {
        let started = Instant::now();
        assert!(wait(sh("sleep 10 & sleep 10"), None, Duration::from_millis(100), &|| false).unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid = tempfile::NamedTempFile::new().unwrap();
        let launcher =
            format!("trap 'kill $!; wait $!; exit' TERM; sleep 10 & echo $! > {}; wait", pid.path().display());
        assert!(wait(sh(&launcher), None, Duration::from_millis(200), &|| false).unwrap().is_none());
        let worker = fs::read_to_string(pid.path()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let alive = Command::new("kill").args(["-0", worker.trim()]).stderr(Stdio::null()).status().unwrap();
        assert!(!alive.success(), "the worker outlived its launcher");
        let started = Instant::now();
        let out = wait(sh("sleep 10 & echo x"), None, Duration::from_secs(30), &|| false).unwrap().unwrap();
        assert!(out.status.success());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
