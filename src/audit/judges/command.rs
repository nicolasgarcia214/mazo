//! The command judge: any CLI given as `[judge] argv`, the prompt, the answer
//! schema and the input on stdin, its stdout the answer, and no dollar cost.
//! It is a program mazo knows nothing of, so no login is checked.

use std::path::Path;
use std::process::Output;

use super::{Reply, Subprocess, exited};
use crate::Result;
use crate::audit::{Stage, frame};

pub struct Command {
    pub argv: Vec<String>,
}

impl Subprocess for Command {
    fn argv(&self, _stage: &Stage) -> Vec<String> {
        self.argv.clone()
    }

    /// The prompt, then the schema its answer must match, then the input.
    fn stdin(&self, stage: &Stage, input: &str) -> Option<String> {
        let schema =
            format!("Answer with one JSON object, and nothing else, matching this schema:\n{}", stage.schema.as_str());
        Some(format!("{}\n{}{input}", stage.prompt, frame("answer-schema", &schema)))
    }

    fn reply(&self, out: &Output, _written: Option<Vec<u8>>, _cap: Option<u64>) -> Result<Reply> {
        let answer = serde_json::from_slice(&out.stdout).map_err(|e| format!("stdout is not JSON: {e}"));
        Ok(Reply { cost: None, answer: exited(out, answer) })
    }

    fn shown(&self, _data: &Path, argv: &str) -> String {
        format!("{argv} < <prompt + schema + input>")
    }
}
