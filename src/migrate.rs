//! `mazo evals migrate`: a one-time move of `<source>/.grades/<skill>/*.md`
//! and `<source>/.retros/<skill>/*.md` into `evals_dir`, each with the
//! [`content_hash`] of its skill as of the commit that last touched it.
//! Removed in the release after the one it ships in.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;
use crate::evals::dirs;
use crate::store::content_hash;

/// What a run did.
#[derive(Debug, Default)]
pub struct Migrated {
    pub moved: usize,
    /// Files left in the source because their destination exists.
    pub left: usize,
}

/// Moves every grade and retro of `source` into `evals_dir`, passing each
/// move to `say`. A file moves without a `content_hash` when it is
/// uncommitted or its commit or skill directory cannot be found, and its line
/// says why. A destination is never overwritten: its source file stays and
/// is listed. Only moved files leave the source, and then any directory they
/// left empty.
pub fn migrate(source: &Path, evals_dir: &Path, say: &mut dyn FnMut(String)) -> Result<Migrated> {
    let scratch = std::env::temp_dir().join(format!(
        "mazo-migrate-{}-{}",
        std::process::id(),
        jiff::Timestamp::now().as_nanosecond()
    ));
    let run = moves(source, evals_dir, &scratch, say);
    drop(fs::remove_dir_all(&scratch));
    let done = run?;
    say(if done.moved == 0 { "moved nothing".into() } else { format!("moved {} files", done.moved) });
    Ok(done)
}

fn moves(source: &Path, evals_dir: &Path, scratch: &Path, say: &mut dyn FnMut(String)) -> Result<Migrated> {
    let mut done = Migrated::default();
    // The historical hash per (commit, skill), and the scratch copies made for it.
    let mut hashes = BTreeMap::<(String, String), std::result::Result<String, String>>::new();
    for (i, kind) in ["grades", "retros"].into_iter().enumerate() {
        let old = source.join(format!(".{kind}"));
        for skill in sorted(&old).into_iter().filter(|p| p.is_dir()) {
            let name = skill.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let to = &dirs(evals_dir, &name)[i];
            for file in sorted(&skill).into_iter().filter(|p| p.is_file() && p.extension() == Some(OsStr::new("md"))) {
                let dest = to.join(file.file_name().unwrap_or_default());
                let rel = file.strip_prefix(source).unwrap_or(&file);
                let text = fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                let copies = hashes.len();
                let hashed = last_commit(source, rel)
                    .and_then(|commit| {
                        let copy = scratch.join(copies.to_string());
                        let hash = hashes.entry((commit, name.clone()));
                        hash.or_insert_with_key(|(commit, skill)| tree_hash(source, commit, skill, &copy)).clone()
                    })
                    .and_then(|hash| with_hash(&text, &hash).ok_or_else(|| "no frontmatter".to_string()));
                fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
                if !create(&dest, hashed.as_deref().unwrap_or(&text[..]))? {
                    say(format!("left {}: {} exists", file.display(), dest.display()));
                    done.left += 1;
                    continue;
                }
                fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                done.moved += 1;
                let why = hashed.err().map_or_else(String::new, |why| format!(" without a content_hash: {why}"));
                say(format!("moved {} -> {}{why}", file.display(), dest.display()));
            }
            drop(fs::remove_dir(&skill));
        }
        drop(fs::remove_dir(&old));
    }
    Ok(done)
}

/// The entries of `dir`, sorted; none when it does not read.
fn sorted(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    paths.sort();
    paths
}

/// Writes `bytes` to a new file at `path`; false when one is already there.
fn create(path: &Path, bytes: &[u8]) -> Result<bool> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(fail(e).into()),
    };
    // Synced, so the source is never removed before its copy is on disk.
    f.write_all(bytes).and_then(|()| f.sync_all()).map_err(|e| {
        drop(fs::remove_file(path));
        fail(e)
    })?;
    Ok(true)
}

/// The commit that last touched `rel`, which must have no changes since; or
/// why there is none.
fn last_commit(source: &Path, rel: &Path) -> std::result::Result<String, String> {
    let rel = rel.as_os_str();
    let log = git(source, &["log".as_ref(), "-1".as_ref(), "--format=%H".as_ref(), "--".as_ref(), rel])
        .ok_or("no git history")?;
    let commit = String::from_utf8_lossy(&log).trim().to_string();
    if commit.is_empty() {
        return Err("uncommitted".into());
    }
    let status =
        git(source, &["status".as_ref(), "--porcelain".as_ref(), "--".as_ref(), rel]).ok_or("no git status")?;
    if !status.is_empty() {
        return Err("uncommitted changes".into());
    }
    Ok(commit)
}

/// The [`content_hash`] of `<source>/<skill>` at `commit`, computed over a
/// checkout of that tree, filters applied, written to `scratch`; or why there
/// is none.
fn tree_hash(source: &Path, commit: &str, skill: &str, scratch: &Path) -> std::result::Result<String, String> {
    let short = &commit[..commit.len().min(12)];
    let prefix = format!("{skill}/");
    let tree = git(
        source,
        &["ls-tree".as_ref(), "-r".as_ref(), "-z".as_ref(), commit.as_ref(), "--".as_ref(), prefix.as_ref()],
    )
    .ok_or_else(|| format!("commit {short} not found"))?;
    if tree.is_empty() {
        return Err(format!("no {skill} directory at commit {short}"));
    }
    let dir = scratch.join(skill);
    for entry in tree.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        // `<mode> <type> <object>\t<path>`
        let tab = entry.iter().position(|b| *b == b'\t').ok_or("unreadable git ls-tree")?;
        let (meta, path) = (String::from_utf8_lossy(&entry[..tab]), Path::new(OsStr::from_bytes(&entry[tab + 1..])));
        let [mode, kind, _] = meta.split(' ').collect::<Vec<_>>()[..] else {
            return Err("unreadable git ls-tree".into());
        };
        if kind != "blob" || mode == "120000" {
            return Err(format!("{} at commit {short} is not a regular file", path.display()));
        }
        // As a checkout writes it: eol conversion and smudge filters applied.
        let mut spec = OsString::from(format!("{commit}:./"));
        spec.push(path);
        let blob = git(source, &["cat-file".as_ref(), "--filters".as_ref(), &spec])
            .ok_or_else(|| format!("{} at commit {short} does not read", path.display()))?;
        let to = dir.join(path.strip_prefix(&prefix).unwrap_or(path));
        let write = to.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&to, blob));
        write.map_err(|e| format!("{}: {e}", to.display()))?;
    }
    content_hash(&dir).map_err(|e| e.to_string())
}

/// Runs `git -C dir args…`, pathspecs taken literally, and returns stdout on success.
fn git(dir: &Path, args: &[&OsStr]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .args(["--no-optional-locks", "--literal-pathspecs", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// `text` with `content_hash: <hash>` as the first frontmatter key, replacing
/// any `content_hash` line there; None when `text` has no frontmatter or never
/// closes it.
fn with_hash(text: &[u8], hash: &str) -> Option<Vec<u8>> {
    let mut lines = text.split_inclusive(|b| *b == b'\n');
    let fence = lines.next()?;
    let bare = fence.strip_prefix("\u{feff}".as_bytes()).unwrap_or(fence);
    if bare.trim_ascii_end() != b"---" {
        return None;
    }
    let eol = &fence[fence.trim_ascii_end().len()..];
    let mut out = [fence, b"content_hash: ", hash.as_bytes(), if eol.is_empty() { b"\n" } else { eol }].concat();
    let mut inside = true;
    for line in lines {
        if inside && line.trim_ascii_end() == b"---" {
            inside = false;
        } else if inside && line.starts_with(b"content_hash:") {
            continue;
        }
        out.extend_from_slice(line);
    }
    (!inside).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::with_hash;

    #[test]
    fn the_hash_leads_the_frontmatter_and_replaces_an_old_one() {
        let text = b"---\r\ndate: 2026-01-01\r\ncontent_hash: old\r\n---\r\ncontent_hash: body\r\n";
        let want = b"---\r\ncontent_hash: h\r\ndate: 2026-01-01\r\n---\r\ncontent_hash: body\r\n";
        assert_eq!(with_hash(text, "h").unwrap(), want);
        assert_eq!(with_hash(b"# no frontmatter\n", "h"), None);
        assert_eq!(with_hash(b"---\ndate: 2026-01-01\ncontent_hash: body\n", "h"), None, "an unclosed frontmatter");
    }
}
