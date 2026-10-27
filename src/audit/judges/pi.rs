//! The Pi judge: `pi -p` with tools, extensions, skills and context files
//! off, the input in an `@file`, the answer in its `--mode json` stream, and
//! no dollar cost. Pi picks its provider from its own settings, so no login
//! is checked.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

use super::{INPUT_FILE, Reply, Subprocess, exited};
use crate::Result;
use crate::audit::Stage;
use crate::render::PathExt;

pub struct Pi {
    pub model: Option<String>,
    /// The user's `settings.json`, which names the default model.
    pub settings: PathBuf,
}

impl Subprocess for Pi {
    fn argv(&self, stage: &Stage) -> Vec<String> {
        let base = [
            "pi",
            "-p",
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-context-files",
            "--system-prompt",
            stage.prompt,
            "--mode",
            "json",
        ];
        super::argv(&base, "--model", &self.model, Some(&format!("@{INPUT_FILE}")))
    }

    fn stdin(&self, _stage: &Stage, _input: &str) -> Option<String> {
        None
    }

    fn reply(&self, out: &Output, _written: Option<Vec<u8>>, _cap: Option<u64>) -> Result<Reply> {
        Ok(Reply { cost: None, answer: exited(out, answer(&out.stdout)) })
    }

    fn shown(&self, _data: &Path, argv: &str) -> String {
        argv.to_string()
    }

    /// `[judge] model`, else the default provider and model of the user's
    /// `settings.json`.
    fn model(&self) -> Option<String> {
        if let Some(m) = &self.model {
            return Some(m.clone());
        }
        let settings: Value =
            fs::read(&self.settings).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Some(match (settings["defaultProvider"].as_str(), settings["defaultModel"].as_str()) {
            (Some(provider), Some(model)) => format!("{provider}/{model}"),
            _ => format!("unknown; {} names no defaultProvider and defaultModel", self.settings.inert()),
        })
    }
}

/// Pi's answer: the text of the last assistant `message_end` in its
/// `--mode json` stream, parsed as JSON once a code fence around it is dropped.
fn answer(stdout: &[u8]) -> std::result::Result<Value, String> {
    let message = String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "message_end" && e["message"]["role"] == "assistant")
        .map(|mut e| e["message"].take())
        .ok_or("no assistant message")?;
    if message["stopReason"] != "stop" {
        let why = message["errorMessage"].as_str().or(message["stopReason"].as_str()).unwrap_or("error");
        return Err(format!("no answer ({why})"));
    }
    let text: String = message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect();
    let text = text.trim();
    let bare = text.strip_prefix("```json").or_else(|| text.strip_prefix("```")).and_then(|t| t.strip_suffix("```"));
    serde_json::from_str(bare.unwrap_or(text).trim()).map_err(|e| format!("the final message is not JSON: {e}"))
}
