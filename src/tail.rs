use std::collections::BTreeMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use regex::bytes::Regex;
use serde::{Deserialize, Serialize};

use crate::adapters;
use crate::render::PathExt;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub dev: u64,
    pub ino: u64,
    pub offset: u64,
    pub line: u64,
    pub window: u64,
}

pub type Cursors = BTreeMap<String, Cursor>;

const WINDOW: u64 = 4 << 10;

const MAX_LINE: u64 = 64 << 20;

pub struct Prefilter(Regex);

impl Prefilter {
    const MARKERS: [&'static str; 4] = ["\"Skill\"", "\"commandName\"", "SKILL.md", "/skills/"];

    pub fn new<'a>(extra: impl IntoIterator<Item = &'a str>) -> Prefilter {
        let alts: Vec<String> = adapters::markers()
            .chain(Self::MARKERS)
            .map(regex::escape)
            .chain(extra.into_iter().map(regex::escape))
            .filter(|s| !s.is_empty())
            .collect();
        Prefilter(Regex::new(&alts.join("|")).expect("escaped literals form a valid regex"))
    }
}

pub struct Chunk {
    pub lines: Vec<(u64, Vec<u8>)>,
    pub oversized: usize,
    pub cursor: Cursor,
    pub header: Option<Vec<u8>>,
}

pub fn read(path: &Path, stored: Option<&Cursor>, filter: &Prefilter) -> io::Result<Chunk> {
    let (file, meta) = open(path)?;
    let (dev, ino) = (meta.dev(), meta.ino());
    let start = match stored {
        Some(c) if c.dev == dev && c.ino == ino && meta.len() >= c.offset && c.window == window(&file, c.offset)? => {
            if meta.len() == c.offset {
                return Ok(Chunk { lines: Vec::new(), oversized: 0, cursor: *c, header: None });
            }
            *c
        }
        _ => Cursor { dev, ino, ..Cursor::default() },
    };
    let mut reader = BufReader::with_capacity(1 << 20, file);
    reader.seek(SeekFrom::Start(start.offset))?;
    let mut cursor = start;
    let (mut lines, mut oversized, mut header) = (Vec::new(), 0, None);
    let mut buf = Vec::new();
    loop {
        let (mut n, line) = next_line(&mut reader, &mut buf)?;
        match line {
            Line::Full => {}
            Line::Oversized => match skip_line(&mut reader)? {
                Some(rest) => {
                    n += rest;
                    oversized += 1;
                }
                None => break,
            },
            Line::Eof | Line::Partial => break,
        }
        cursor.offset += n as u64;
        cursor.line += 1;
        if line != Line::Full {
            continue;
        }
        if cursor.line == 1 {
            header = Some(buf.clone());
        }
        if filter.0.is_match(&buf) {
            lines.push((cursor.line, std::mem::take(&mut buf)));
        }
    }
    cursor.window = window(reader.get_ref(), cursor.offset)?;
    Ok(Chunk { lines, oversized, cursor, header })
}

#[derive(PartialEq, Eq)]
enum Line {
    Eof,
    Full,
    Partial,
    Oversized,
}

fn next_line(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<(usize, Line)> {
    buf.clear();
    let n = reader.take(MAX_LINE + 1).read_until(b'\n', buf)?;
    if n == 0 {
        return Ok((0, Line::Eof));
    }
    if buf.pop_if(|b| *b == b'\n').is_some() {
        return Ok((n, Line::Full));
    }
    Ok((n, if buf.len() as u64 > MAX_LINE { Line::Oversized } else { Line::Partial }))
}

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

fn window(file: &File, offset: u64) -> io::Result<u64> {
    let start = offset.saturating_sub(WINDOW);
    let mut buf = vec![0; (offset - start) as usize];
    file.read_exact_at(&mut buf, start)?;
    let digest = sha1_smol::Sha1::from(buf).digest().bytes();
    Ok(u64::from_be_bytes(digest[..8].try_into().expect("a SHA-1 digest is 20 bytes")))
}

fn open(path: &Path) -> io::Result<(File, Metadata)> {
    let file = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a regular file", path.inert())));
    }
    Ok((file, meta))
}

pub fn whole(path: &Path) -> io::Result<Chunk> {
    let mut reader = BufReader::with_capacity(1 << 20, open(path)?.0);
    let (mut lines, mut oversized, mut line) = (Vec::new(), 0, 0);
    let mut buf = Vec::new();
    loop {
        match next_line(&mut reader, &mut buf)?.1 {
            Line::Eof => break,
            Line::Full | Line::Partial => {
                line += 1;
                lines.push((line, std::mem::take(&mut buf)));
            }
            Line::Oversized => {
                line += 1;
                oversized += 1;
                if skip_line(&mut reader)?.is_none() {
                    break;
                }
            }
        }
    }
    Ok(Chunk { lines, oversized, cursor: Cursor { line, ..Cursor::default() }, header: None })
}

pub fn first_line(path: &Path) -> io::Result<Option<Vec<u8>>> {
    Ok(head(path)?.next())
}

pub fn head(path: &Path) -> io::Result<impl Iterator<Item = Vec<u8>>> {
    let mut reader = BufReader::new(open(path)?.0);
    let mut buf = Vec::new();
    Ok(std::iter::from_fn(move || match next_line(&mut reader, &mut buf).ok()?.1 {
        Line::Full => Some(std::mem::take(&mut buf)),
        _ => None,
    })
    .fuse())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn resumes_from_cursor_and_never_reads_half_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let f = Prefilter::new([]);
        fs::write(&p, "noise\n{\"x\":\"SKILL.md\"}\n{\"half\":\"SKILL").unwrap();
        let a = read(&p, None, &f).unwrap();
        assert_eq!(a.lines.len(), 1);
        assert_eq!(a.lines[0].0, 2);
        assert_eq!(a.cursor.line, 2);
        assert_eq!(a.header.as_deref(), Some(&b"noise"[..]));
        let mut fh = fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(fh, ".md\"}}").unwrap();
        let b = read(&p, Some(&a.cursor), &f).unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0].0, 3);
        assert!(b.header.is_none());
        let again = read(&p, Some(&b.cursor), &f).unwrap();
        assert!(again.lines.is_empty());
    }

    #[test]
    fn replaced_file_is_read_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let f = Prefilter::new([]);
        fs::write(&p, "SKILL.md\n").unwrap();
        let a = read(&p, None, &f).unwrap();
        fs::remove_file(&p).unwrap();
        fs::write(&p, "SKILL.md one\nSKILL.md two\n").unwrap();
        let mut c = a.cursor;
        c.ino = 0;
        let b = read(&p, Some(&c), &f).unwrap();
        assert_eq!(b.lines.len(), 2);
    }

    #[test]
    fn file_rewritten_in_place_is_read_from_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let f = Prefilter::new([]);
        fs::write(&p, "SKILL.md a\n").unwrap();
        let a = read(&p, None, &f).unwrap();
        fs::write(&p, "SKILL.md one\nSKILL.md two\n").unwrap();
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
        fs::write(&p, &long).unwrap();
        let a = read(&p, None, &f).unwrap();
        assert_eq!((a.oversized, a.cursor.offset, a.cursor.line), (0, 0, 0));
        let mut fh = fs::OpenOptions::new().append(true).open(&p).unwrap();
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
        fs::write(&p, &text).unwrap();
        let a = read(&p, None, &Prefilter::new([])).unwrap();
        assert_eq!(a.oversized, 0);
        assert_eq!(a.lines, [(2, b"SKILL.md".to_vec())]);
        assert_eq!((a.cursor.offset, a.cursor.line), (text.len() as u64, 2));
    }

    #[test]
    fn whole_skips_an_oversized_line_keeps_later_numbers_and_reads_an_unterminated_end() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        let mut text = b"one\n".to_vec();
        text.extend(vec![b'x'; MAX_LINE as usize + 1]);
        text.extend(b"\n\nfour");
        fs::write(&p, &text).unwrap();
        let c = whole(&p).unwrap();
        assert_eq!(c.lines, [(1, b"one".to_vec()), (3, Vec::new()), (4, b"four".to_vec())]);
        assert_eq!((c.oversized, c.cursor.line), (1, 4));
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
