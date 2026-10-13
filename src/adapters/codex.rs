//! Codex rollouts (SPEC §4.3). Stub: implemented by the Codex adapter task.

use std::path::{Path, PathBuf};

use super::{Adapter, ParseCtx, Parsed};
use crate::config::Home;
use crate::tail::Chunk;

pub struct Codex;

impl Adapter for Codex {
    fn files(&self, home: &Home) -> Vec<PathBuf> {
        super::jsonl_files(&home.transcripts())
    }

    fn parse(&self, _ctx: &ParseCtx, _path: &Path, _header: Option<&[u8]>, _chunk: &Chunk) -> Parsed {
        todo!("codex adapter")
    }
}

/// The parent rollout id named by a `session_meta` header line, if any.
pub fn parent_of_header(_header: &[u8]) -> Option<String> {
    todo!("codex header parent")
}
