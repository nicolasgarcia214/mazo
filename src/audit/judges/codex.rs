//! The Codex judge: `codex exec` without the tools that read local files,
//! in a read-only sandbox with a scratch `CODEX_HOME`, the prompt and input
//! on stdin, the answer in its `-o` file, and no dollar cost.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

use super::{RESULT_FILE, Reply, SCHEMA_FILE, Subprocess, exited};
use crate::audit::Stage;
use crate::{Error, Result};

pub struct Codex {
    pub model: Option<String>,
    /// The user's `auth.json`, which the scratch home links to.
    pub auth: PathBuf,
}

/// The scratch home under the data directory.
fn home(data: &Path) -> PathBuf {
    data.join("codex-judge-home")
}

impl Subprocess for Codex {
    fn argv(&self, _stage: &Stage) -> Vec<String> {
        let base = [
            "codex",
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            // The shell and the image viewer read any file the user can, and
            // web search carries the input out; the sandbox only stops writes
            // and the network. `multi_agent` drops the v1 subagents; a model
            // on v2 keeps its own, which get these same tools.
            "--disable",
            "shell_tool",
            "--disable",
            "view_image",
            "--disable",
            "multi_agent",
            "-c",
            "web_search=disabled",
            "--sandbox",
            "read-only",
            "--output-schema",
            SCHEMA_FILE,
            "-o",
            RESULT_FILE,
            "--json",
        ];
        super::argv(&base, "-m", &self.model, Some("-"))
    }

    fn stdin(&self, stage: &Stage, input: &str) -> Option<String> {
        Some(format!("{}\n{input}", stage.prompt))
    }

    /// The user's `auth.json` must hold a ChatGPT login.
    fn login(&self, _data: &Path, _cancel: &dyn Fn() -> bool) -> Result<()> {
        let path = self.auth.display();
        let unread = |why: String| {
            Error::Usage(format!(
                "{path}: {why}; the Codex judge needs a ChatGPT login: run codex login, or set billing = \"api\" under [judge]"
            ))
        };
        let bytes = fs::read(&self.auth).map_err(|e| unread(e.to_string()))?;
        let auth: Value = serde_json::from_slice(&bytes).map_err(|e| unread(format!("not JSON: {e}")))?;
        if auth["auth_mode"] == "chatgpt" {
            return Ok(());
        }
        Err(Error::Usage(format!(
            "{path}: no ChatGPT login for the Codex judge; run codex login, or set billing = \"api\" under [judge]"
        )))
    }

    /// `<data>/codex-judge-home` holding only a symlink to the user's
    /// `auth.json`, made afresh so nothing an earlier call left there survives.
    fn scratch(&self, data: &Path) -> Result<Option<(&'static str, PathBuf)>> {
        let home = home(data);
        if !self.auth.exists() {
            return Err(format!("{}: no Codex login for the Codex judge", self.auth.display()).into());
        }
        fs::remove_dir_all(&home)
            .or_else(|e| if e.kind() == ErrorKind::NotFound { Ok(()) } else { Err(e) })
            .and_then(|()| fs::create_dir_all(&home))
            .and_then(|()| std::os::unix::fs::symlink(&self.auth, home.join("auth.json")))
            .map_err(crate::at(&home))?;
        Ok(Some(("CODEX_HOME", home)))
    }

    fn reply(&self, out: &Output, written: Option<Vec<u8>>, _cap: Option<u64>) -> Result<Reply> {
        let written = written.and_then(|b| serde_json::from_slice(&b).ok());
        Ok(Reply { cost: None, answer: exited(out, written.ok_or_else(|| "no answer in the -o file".into())) })
    }

    fn shown(&self, data: &Path, argv: &str) -> String {
        format!("CODEX_HOME={} {argv} < <prompt + input>", home(data).display())
    }
}
