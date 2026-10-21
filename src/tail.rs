//! Incremental JSONL reading: resume offsets, partial-line
//! safety, and the byte prefilter that skips almost every line.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use regex::bytes::Regex;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub dev: u64,
    pub ino: u64,
    pub offset: u64,
    /// Lines before `offset`, so `src` can name a line without recounting.
    pub line: u64,
}

pub type Cursors = BTreeMap<String, Cursor>;

/// Lines that mention any skill marker, the source root or a home's skills dir.
pub struct Prefilter(Regex);

impl Prefilter {
    const MARKERS: [&'static str; 7] = [
        "Base directory for this skill",
        "\"Skill\"",
        "\"commandName\"",
        "<skill>",
        "<skill name=",
        "SKILL.md",
        "/skills/",
    ];

    pub fn new<'a>(extra: impl IntoIterator<Item = &'a str>) -> Prefilter {
        let alts: Vec<String> =
            Self::MARKERS.iter().copied().chain(extra).filter(|s| !s.is_empty()).map(regex::escape).collect();
        Prefilter(Regex::new(&alts.join("|")).expect("escaped literals form a valid regex"))
    }
}

/// What one read of a file produced.
pub struct Chunk {
    /// `(1-based line number, bytes without the newline)` for lines that pass the prefilter.
    pub lines: Vec<(u64, Vec<u8>)>,
    /// The cursor to store: it only ever advances past a `\n`.
    pub cursor: Cursor,
}

/// Reads complete lines of `path` from the stored cursor when it still applies
/// (same device and inode, file at least as long), else from the start.
pub fn read(path: &Path, stored: Option<&Cursor>, filter: &Prefilter) -> io::Result<Chunk> {
    let file = File::open(path)?;
    let meta = file.metadata()?;
    let (dev, ino) = (meta.dev(), meta.ino());
    let start = match stored {
        Some(c) if c.dev == dev && c.ino == ino && meta.len() >= c.offset => *c,
        _ => Cursor { dev, ino, offset: 0, line: 0 },
    };
    let mut reader = BufReader::with_capacity(1 << 20, file);
    reader.seek(SeekFrom::Start(start.offset))?;
    let mut cursor = start;
    let mut lines = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            break; // EOF, or a line still being written
        }
        cursor.offset += n as u64;
        cursor.line += 1;
        buf.pop();
        if filter.0.is_match(&buf) {
            lines.push((cursor.line, buf.clone()));
        }
    }
    Ok(Chunk { lines, cursor })
}

/// Line 1 of a file, which carries the Codex and Pi session header.
pub fn first_line(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    BufReader::new(File::open(path)?).read_until(b'\n', &mut buf)?;
    Ok((buf.pop() == Some(b'\n')).then_some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resumes_from_cursor_and_never_reads_half_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let f = Prefilter::new([]);
        std::fs::write(&p, "noise\n{\"x\":\"SKILL.md\"}\n{\"half\":\"SKILL").unwrap();
        let a = read(&p, None, &f).unwrap();
        assert_eq!(a.lines.len(), 1);
        assert_eq!(a.lines[0].0, 2);
        assert_eq!(a.cursor.line, 2);
        let mut fh = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(fh, ".md\"}}").unwrap();
        let b = read(&p, Some(&a.cursor), &f).unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0].0, 3);
        let again = read(&p, Some(&b.cursor), &f).unwrap();
        assert!(again.lines.is_empty());
    }

    #[test]
    fn replaced_file_is_read_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let f = Prefilter::new([]);
        std::fs::write(&p, "SKILL.md\n").unwrap();
        let a = read(&p, None, &f).unwrap();
        std::fs::remove_file(&p).unwrap();
        std::fs::write(&p, "SKILL.md one\nSKILL.md two\n").unwrap();
        let mut c = a.cursor;
        c.ino = 0; // a different inode, as after replacement
        let b = read(&p, Some(&c), &f).unwrap();
        assert_eq!(b.lines.len(), 2);
    }
}
