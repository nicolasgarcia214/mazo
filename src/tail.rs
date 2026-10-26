//! Incremental JSONL reading: resume offsets, partial-line
//! safety, and the byte prefilter that skips almost every line.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use regex::bytes::Regex;
use serde::{Deserialize, Serialize};

use crate::adapters;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub dev: u64,
    pub ino: u64,
    pub offset: u64,
    /// Lines before `offset`, so `src` can name a line without recounting.
    pub line: u64,
    /// A hash of the [`WINDOW`] bytes before `offset`, so a file rewritten in
    /// place is read again from the start.
    pub window: u64,
}

pub type Cursors = BTreeMap<String, Cursor>;

/// How far before a cursor's offset its file must be unchanged to resume.
const WINDOW: u64 = 4 << 10;

/// The longest line kept. Real transcript lines reach a few MB when a tool
/// returns a large output; a longer one is skipped as malformed, not buffered.
const MAX_LINE: u64 = 64 << 20;

/// Lines that mention any skill marker, the source root or a home's skills dir.
pub struct Prefilter(Regex);

impl Prefilter {
    /// Generic markers, beside each adapter's own.
    const MARKERS: [&'static str; 4] = ["\"Skill\"", "\"commandName\"", "SKILL.md", "/skills/"];

    pub fn new<'a>(extra: impl IntoIterator<Item = &'a str>) -> Prefilter {
        let alts: Vec<String> = adapters::MARKERS
            .into_iter()
            .chain(Self::MARKERS)
            .chain(extra)
            .filter(|s| !s.is_empty())
            .map(regex::escape)
            .collect();
        Prefilter(Regex::new(&alts.join("|")).expect("escaped literals form a valid regex"))
    }
}

/// What one read of a file produced.
pub struct Chunk {
    /// `(1-based line number, bytes without the newline)` for lines that pass the prefilter.
    pub lines: Vec<(u64, Vec<u8>)>,
    /// Complete lines longer than [`MAX_LINE`], skipped unread.
    pub oversized: usize,
    /// The cursor to store: it only ever advances past a `\n`.
    pub cursor: Cursor,
}

/// Reads complete lines of `path` from the stored cursor when it still applies
/// (same device and inode, file at least as long, same bytes before it), else
/// from the start.
pub fn read(path: &Path, stored: Option<&Cursor>, filter: &Prefilter) -> io::Result<Chunk> {
    let file = open(path)?;
    let meta = file.metadata()?;
    let (dev, ino) = (meta.dev(), meta.ino());
    let start = match stored {
        Some(c) if c.dev == dev && c.ino == ino && meta.len() >= c.offset && c.window == window(&file, c.offset)? => *c,
        _ => Cursor { dev, ino, ..Cursor::default() },
    };
    let mut reader = BufReader::with_capacity(1 << 20, file);
    reader.seek(SeekFrom::Start(start.offset))?;
    let mut cursor = start;
    let mut lines = Vec::new();
    let mut oversized = 0;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let mut n = (&mut reader).take(MAX_LINE + 1).read_until(b'\n', &mut buf)?;
        if buf.len() as u64 > MAX_LINE && buf.last() != Some(&b'\n') {
            match skip_line(&mut reader)? {
                Some(rest) => n += rest,
                None => break, // a long line still being written
            }
            cursor.offset += n as u64;
            cursor.line += 1;
            oversized += 1;
            continue;
        }
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
    cursor.window = window(reader.get_ref(), cursor.offset)?;
    Ok(Chunk { lines, oversized, cursor })
}

/// Consumes the rest of a line without keeping it: the bytes through its
/// `\n`, or `None` at EOF before one.
fn skip_line(reader: &mut impl BufRead) -> io::Result<Option<usize>> {
    let mut n = 0;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(None);
        }
        let (used, done) = buf.iter().position(|&b| b == b'\n').map_or((buf.len(), false), |i| (i + 1, true));
        reader.consume(used);
        n += used;
        if done {
            return Ok(Some(n));
        }
    }
}

/// A hash of the [`WINDOW`] bytes of `file` before `offset`.
fn window(file: &File, offset: u64) -> io::Result<u64> {
    let start = offset.saturating_sub(WINDOW);
    let mut buf = vec![0; (offset - start) as usize];
    file.read_exact_at(&mut buf, start)?;
    let digest = sha1_smol::Sha1::from(buf).digest().bytes();
    Ok(u64::from_be_bytes(digest[..8].try_into().expect("a SHA-1 digest is 20 bytes")))
}

/// Opens a regular file, non-blocking so a FIFO is refused rather than waited on.
fn open(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a regular file", path.display())));
    }
    Ok(file)
}

/// Line 1 of a file, which carries the Codex and Pi session header.
pub fn first_line(path: &Path) -> io::Result<Option<Vec<u8>>> {
    Ok(head(path)?.next())
}

/// The complete lines of a file from the start, one at a time, ending at a
/// line over [`MAX_LINE`], an unterminated last line or a read error. A Pi header names its parent's
/// file, so this reads only a regular file and bounded lines.
pub fn head(path: &Path) -> io::Result<impl Iterator<Item = Vec<u8>>> {
    let mut reader = BufReader::new(open(path)?);
    Ok(std::iter::from_fn(move || {
        let mut buf = Vec::new();
        (&mut reader).take(MAX_LINE + 1).read_until(b'\n', &mut buf).ok()?;
        (buf.pop() == Some(b'\n')).then_some(buf)
    })
    .fuse())
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

    #[test]
    fn file_rewritten_in_place_is_read_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let f = Prefilter::new([]);
        std::fs::write(&p, "SKILL.md a\n").unwrap();
        let a = read(&p, None, &f).unwrap();
        // Truncated and rewritten through the same inode, now longer.
        std::fs::write(&p, "SKILL.md one\nSKILL.md two\n").unwrap();
        let b = read(&p, Some(&a.cursor), &f).unwrap();
        assert_eq!(b.cursor.ino, a.cursor.ino);
        assert_eq!(b.lines, [(1, b"SKILL.md one".to_vec()), (2, b"SKILL.md two".to_vec())]);
    }

    #[test]
    fn oversized_line_is_skipped_and_counted_once_complete() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let f = Prefilter::new([]);
        let long = vec![b'x'; MAX_LINE as usize + 1];
        std::fs::write(&p, &long).unwrap();
        let a = read(&p, None, &f).unwrap();
        assert_eq!((a.oversized, a.cursor.offset, a.cursor.line), (0, 0, 0));
        let mut fh = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(fh, "\nSKILL.md").unwrap();
        let b = read(&p, Some(&a.cursor), &f).unwrap();
        assert_eq!(b.oversized, 1);
        assert_eq!(b.lines, [(2, b"SKILL.md".to_vec())]);
        assert_eq!(b.cursor.offset, long.len() as u64 + 10);
    }

    #[test]
    fn a_line_of_exactly_the_cap_is_read_and_the_next_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let mut text = vec![b'x'; MAX_LINE as usize];
        text.extend(b"\nSKILL.md\n");
        std::fs::write(&p, &text).unwrap();
        let a = read(&p, None, &Prefilter::new([])).unwrap();
        assert_eq!(a.oversized, 0);
        assert_eq!(a.lines, [(2, b"SKILL.md".to_vec())]);
        assert_eq!((a.cursor.offset, a.cursor.line), (text.len() as u64, 2));
    }

    #[test]
    fn first_line_refuses_what_is_not_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(first_line(&fifo).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(first_line(Path::new("/dev/zero")).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
}
