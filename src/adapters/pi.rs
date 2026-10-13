//! Pi sessions (SPEC §4.4). Stub: implemented by the Pi adapter task.

use std::path::{Path, PathBuf};

use super::{Adapter, ParseCtx, Parsed};
use crate::config::Home;
use crate::tail::Chunk;

pub struct Pi;

impl Adapter for Pi {
    fn files(&self, home: &Home) -> Vec<PathBuf> {
        super::jsonl_files(&home.transcripts())
    }

    fn parse(&self, _ctx: &ParseCtx, _path: &Path, _header: Option<&[u8]>, _chunk: &Chunk) -> Parsed {
        todo!("pi adapter")
    }
}

/// `(id, parentSession)` from a Pi session header line.
pub fn header_id_parent(_line: &[u8]) -> Option<(String, Option<PathBuf>)> {
    todo!("pi header")
}
