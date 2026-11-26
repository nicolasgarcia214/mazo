use std::process::Output;

use super::{Reply, Subprocess, exited, prompted};
use crate::Result;
use crate::audit::Stage;

pub struct Command {
    pub argv: Vec<String>,
}

impl Subprocess for Command {
    fn argv(&self, _stage: &Stage) -> Vec<String> {
        self.argv.clone()
    }

    fn stdin(&self, stage: &Stage, input: &str) -> Option<String> {
        Some(prompted(stage, input))
    }

    fn reply(&self, out: &Output, _written: Option<Vec<u8>>, _cap: Option<u64>) -> Result<Reply> {
        let answer = serde_json::from_slice(&out.stdout).map_err(|e| format!("stdout is not JSON: {e}"));
        Ok(Reply { cost: None, answer: exited(out, answer) })
    }

    fn shown(&self, argv: &str) -> String {
        format!("{argv} < <prompt + schema + input>")
    }
}
