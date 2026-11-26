use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

use super::{RESULT_FILE, Reply, SCHEMA_FILE, Subprocess, exited};
use crate::audit::Stage;
use crate::render::PathExt;
use crate::{Error, Result};

pub struct Codex {
    pub model: Option<String>,
    pub auth: PathBuf,
}

impl Subprocess for Codex {
    fn argv(&self, _stage: &Stage) -> Vec<String> {
        let base = [
            "codex",
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
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

    fn login(&self, _data: &Path, _cancel: &dyn Fn() -> bool) -> Result<()> {
        let path = self.auth.inert();
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

    fn scratch(&self, home: &Path) -> Result<Option<&'static str>> {
        if !self.auth.exists() {
            return Err(format!("{}: no Codex login for the Codex judge", self.auth.inert()).into());
        }
        fs::create_dir(home)
            .and_then(|()| std::os::unix::fs::symlink(&self.auth, home.join("auth.json")))
            .map_err(crate::at(home))?;
        Ok(Some("CODEX_HOME"))
    }

    fn reply(&self, out: &Output, written: Option<Vec<u8>>, _cap: Option<u64>) -> Result<Reply> {
        let written = written.and_then(|b| serde_json::from_slice(&b).ok());
        Ok(Reply { cost: None, answer: exited(out, written.ok_or_else(|| "no answer in the -o file".into())) })
    }

    fn shown(&self, argv: &str) -> String {
        format!("CODEX_HOME=<scratch home> {argv} < <prompt + input>")
    }
}
