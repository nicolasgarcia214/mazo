use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::Result;

pub fn lock(dir: &Path) -> Result<File> {
    fs::create_dir_all(dir).map_err(crate::at(dir))?;
    let path = dir.join("data.lock");
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(crate::at(&path))?;
    lock.lock().map_err(crate::at(&path))?;
    Ok(lock)
}

pub(crate) fn append_lines(path: &Path, mut out: String) -> Result<()> {
    let err = crate::at(path);
    let mut f = OpenOptions::new().create(true).read(true).append(true).open(path).map_err(err)?;
    if f.metadata().map_err(err)?.len() > 0 {
        let mut last = [0u8];
        f.seek(SeekFrom::End(-1)).map_err(err)?;
        f.read_exact(&mut last).map_err(err)?;
        if last[0] != b'\n' {
            out.insert(0, '\n');
        }
    }
    f.write_all(out.as_bytes()).map_err(err)?;
    Ok(f.sync_data().map_err(err)?)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(crate::at(&tmp)(e).into()),
        _ => {}
    }
    let mut f = OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp).map_err(crate::at(&tmp))?;
    f.write_all(bytes).map_err(crate::at(&tmp))?;
    f.sync_all().map_err(crate::at(&tmp))?;
    fs::rename(&tmp, path).map_err(crate::at(path))?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Ok(fs::File::open(parent).and_then(|d| d.sync_all()).map_err(crate::at(parent))?)
}

pub fn jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<(usize, Option<T>)>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(crate::at(path)(e).into()),
    };
    let mut lines = Vec::new();
    for (n, line) in BufReader::new(file).split(b'\n').enumerate() {
        let line = line.map_err(crate::at(path))?;
        if !line.trim_ascii().is_empty() {
            lines.push((n + 1, serde_json::from_slice(&line).ok()));
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_a_file_in_a_fresh_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fresh").join("state.json");
        fs::create_dir(path.parent().unwrap()).unwrap();
        write_atomic(&path, b"old", 0o600).unwrap();
        write_atomic(&path, b"new", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(!path.with_extension("json.new").exists());
    }
}
