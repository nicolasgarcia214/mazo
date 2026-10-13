//! Claude Code transcripts (SPEC §4.2). Stub: implemented by the Claude adapter task.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{Adapter, ParseCtx, Parsed, Raw};
use crate::config::Home;
use crate::tail::Chunk;

pub struct Claude;

impl Adapter for Claude {
    fn files(&self, home: &Home) -> Vec<PathBuf> {
        super::jsonl_files(&home.transcripts())
    }

    fn parse(&self, _ctx: &ParseCtx, _path: &Path, _header: Option<&[u8]>, _chunk: &Chunk) -> Parsed {
        todo!("claude adapter")
    }
}

/// One-time `typed` events from `<home>/history.jsonl` (SPEC §4.2 History backfill).
pub fn backfill(_home: &Home, _known: &BTreeSet<String>) -> Vec<Raw> {
    todo!("claude history backfill")
}
