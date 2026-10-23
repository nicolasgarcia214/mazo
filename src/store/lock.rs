//! The lockfile, `mazo.lock`, and the copy it records: a skill checked,
//! staged and hashed ([`content_hash`]).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::err;
use super::source::{Remote, Scratch, Source};
use crate::Result;
use crate::audit::VetVerdict;
use crate::{ledger, policy, yaml};

/// `<data dir>/mazo.lock`, or a project's: one `[[skill]]` per installed skill.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    #[serde(default, rename = "skill")]
    pub skills: Vec<Locked>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Locked {
    #[serde(deserialize_with = "skill_name")]
    pub name: String,
    /// Where the skill came from; for a local path, the physical directory.
    pub origin: String,
    /// The skill's directory inside `origin`; `.` when `origin` is the skill.
    pub subpath: String,
    /// The branch, tag or commit `origin` is cloned at; its default branch when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// [`content_hash`] of the installed copy.
    pub content_hash: String,
    pub trusted: bool,
    /// RFC 3339.
    pub installed_at: String,
    /// When the judge vetted the skill, RFC 3339; none for a trusted install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vetted_at: Option<String>,
    /// The judge's one or two sentences on what the skill is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#for: Option<String>,
    /// Up to three uses the judge suggests trying.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r#try: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<VetVerdict>,
}

/// A lockfile's skill name, refused unless [`policy::valid_name`]: it is
/// joined to the store as a path, and a project's lockfile is anyone's commit.
fn skill_name<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let name = String::deserialize(d)?;
    if policy::valid_name(&name) {
        Ok(name)
    } else {
        Err(serde::de::Error::custom(format!("invalid skill name {name:?}")))
    }
}

impl Locked {
    /// The repository `mazo update` fetches it from, at its ref; `None`
    /// when it was installed from a local path.
    pub(super) fn remote(&self) -> std::result::Result<Option<Remote>, String> {
        Ok(match Source::parse(&self.origin)? {
            Source::Git(mut remote) => {
                remote.r#ref.clone_from(&self.r#ref);
                Some(remote)
            }
            Source::Local(_) => None,
        })
    }

    /// Whether `mazo update` can fetch it again.
    pub fn updatable(&self) -> bool {
        !matches!(self.remote(), Ok(None))
    }
}

/// Lockfile text parsed, with every string in it, and in an error that quotes
/// it, [`inert`](crate::render::inert): a project's lockfile is anyone's
/// commit, and its verdicts, origins and refs are printed.
fn parse(text: &str) -> std::result::Result<Lock, String> {
    use crate::render::{inert, inert_value};
    let lock: Lock = toml::from_str(text).map_err(|e| inert(&e.to_string()))?;
    let value = serde_json::to_value(lock).expect("a lockfile serialises");
    Ok(serde_json::from_value(inert_value(value)).expect("an inert lockfile deserialises"))
}

impl Lock {
    /// The lockfile in `data`, or in a project's git root; empty when there is none.
    pub fn load(data: &Path) -> Result<Lock> {
        let path = data.join("mazo.lock");
        match fs::read_to_string(&path) {
            Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display()).into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
            Err(e) => Err(err(&path, e).into()),
        }
    }

    /// Drops `name`'s entry from the lockfile under `data`, if it has one.
    pub fn remove(data: &Path, name: &str) -> Result<()> {
        let mut lock = Lock::load(data)?;
        lock.skills.retain(|s| s.name != name);
        lock.save(data)
    }

    pub(super) fn save(&self, data: &Path) -> Result<()> {
        ledger::write_atomic(
            &data.join("mazo.lock"),
            toml::to_string(self).map_err(|e| e.to_string())?.as_bytes(),
            0o666,
        )
    }
}

/// `src` resolved; inside `clone` when there is one, so that a symlink in the
/// repository cannot lead the copy outside it.
pub(super) fn resolved(src: &Path, clone: Option<&Path>) -> std::result::Result<PathBuf, String> {
    let src = fs::canonicalize(src).map_err(|e| err(src, e))?;
    if let Some(clone) = clone
        && !src.starts_with(fs::canonicalize(clone).map_err(|e| err(clone, e))?)
    {
        return Err(format!("{} leaves the repository", crate::render::inert(&src.display().to_string())));
    }
    Ok(src)
}

/// The `SKILL.md` of the skill at `src` installed as `name`, its `name:`
/// rewritten when `rename`, once it passes the always-on rules; and every
/// entry under `src`.
pub(super) fn checked(src: &Path, name: &str, rename: bool) -> std::result::Result<(String, Vec<PathBuf>), String> {
    // Listed first, so that a `SKILL.md` that is not a regular file is never read.
    let files = tree(src)?;
    let skill_md = fs::read_to_string(src.join("SKILL.md")).map_err(|e| format!("SKILL.md: {e}"))?;
    let skill_md = if rename { renamed(&skill_md, name) } else { skill_md };
    yaml::frontmatter(&skill_md).and_then(|meta| policy::always_on(&meta, name)).map_err(|e| e.to_string())?;
    Ok((skill_md, files))
}

/// Copies `files` of `src` into a fresh `staged`, `SKILL.md` as `skill_md`:
/// the copy, removed when dropped, and its [`content_hash`]. The copy is
/// what the judge reads and what is renamed into place, so that a source
/// changed after it was read installs nothing unvetted.
pub(super) fn stage(src: &Path, files: &[PathBuf], skill_md: &str, staged: PathBuf) -> Result<(Scratch, String)> {
    let staged = Scratch(staged);
    let dir = staged.0.as_path();
    let _ = fs::remove_dir_all(dir);
    reap(dir);
    fs::create_dir_all(dir).map_err(|e| err(dir, e))?;
    files.iter().try_for_each(|rel| {
        let (from, dest) = (src.join(rel), dir.join(rel));
        if from.is_dir() {
            fs::create_dir(&dest).map_err(|e| err(&dest, e))
        } else if rel == Path::new("SKILL.md") {
            fs::write(&dest, skill_md).map_err(|e| err(&dest, e))
        } else {
            fs::copy(&from, &dest).map(drop).map_err(|e| err(&dest, e))
        }
    })?;
    let hash = content_hash(dir)?;
    Ok((staged, hash))
}

/// Removes the copies [`stage`] left beside `staged` for the same skill by a
/// process since gone, such as one interrupted at the judge or the prompt:
/// each is named for its process, so no later run reuses it.
fn reap(staged: &Path) {
    let (Some(parent), Some((prefix, _))) =
        (staged.parent(), staged.file_name().and_then(|n| n.to_str()).and_then(|n| n.rsplit_once('-')))
    else {
        return;
    };
    for entry in fs::read_dir(parent).into_iter().flatten().flatten() {
        let pid =
            entry.file_name().to_str().and_then(|n| n.strip_prefix(prefix)?.strip_prefix('-')?.parse::<u32>().ok());
        let gone = |pid: u32| {
            let signalled = unsafe { libc::kill(pid as libc::pid_t, 0) };
            signalled != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        };
        if pid.is_some_and(gone) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Now, to the second, RFC 3339.
pub(super) fn now() -> Result<String> {
    Ok(jiff::Timestamp::now().round(jiff::Unit::Second).map_err(|e| e.to_string())?.to_string())
}

/// `text` with the frontmatter's top-level `name:` line set to `name`.
fn renamed(text: &str, name: &str) -> String {
    let mut fences = 0;
    let mut done = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare.trim_start_matches('\u{feff}') == "---" {
            fences += 1;
        }
        if fences == 1 && !done && bare.starts_with("name:") {
            out.push_str(&format!("name: {name}{}", &line[bare.len()..]));
            done = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Every directory and file under `root`, relative and sorted, `.git` left
/// out. A symlink, or anything else that is neither, is an error.
fn tree(root: &Path) -> std::result::Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        for e in fs::read_dir(&dir).map_err(|e| err(&dir, e))? {
            let e = e.map_err(|e| err(&dir, e))?;
            if e.file_name() == ".git" {
                continue;
            }
            let path = rel.join(e.file_name());
            let ft = e.file_type().map_err(|io| err(&e.path(), io))?;
            if ft.is_dir() {
                stack.push(path.clone());
            } else if !ft.is_file() {
                let path = crate::render::inert(&e.path().display().to_string());
                return Err(format!("{path} is not a regular file or directory"));
            }
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// SHA-1 over every file under `dir` in relative-path order, sorted
/// component-wise (as `Path` orders, not by string bytes): each contributes
/// its path, a NUL, its length in decimal, a NUL and its bytes.
pub fn content_hash(dir: &Path) -> Result<String> {
    let mut sha = sha1_smol::Sha1::new();
    for rel in tree(dir)? {
        let path = dir.join(&rel);
        if path.is_dir() {
            continue;
        }
        let bytes = fs::read(&path).map_err(|e| err(&path, e))?;
        sha.update(rel.as_os_str().as_encoded_bytes());
        sha.update(format!("\0{}\0", bytes.len()).as_bytes());
        sha.update(&bytes);
    }
    Ok(sha.digest().to_string())
}

#[cfg(test)]
mod tests {
    use super::{checked, content_hash, parse, renamed};

    #[test]
    fn every_string_a_lockfile_holds_is_inert() {
        let text = r#"
            [[skill]]
            name = "theirs"
            origin = "https://h/o/r\u001b[2J"
            subpath = "."
            content_hash = "x"
            trusted = false
            installed_at = "2026-10-01T00:00:00Z"
            for = "Tidies \u001b]52;c;ZXZpbA==\u0007notes."
            [skill.verdict]
            security = "\u009b2J"
            quality = "weak"
            value = "low value"
            reasons = []
        "#;
        let s = &parse(text).unwrap().skills[0];
        assert_eq!(s.origin, "https://h/o/r\u{fffd}[2J");
        assert_eq!(s.r#for.as_deref(), Some("Tidies \u{fffd}]52;c;ZXZpbA==\u{fffd}notes."));
        assert_eq!(s.verdict.as_ref().unwrap().security.as_deref(), Some("\u{fffd}2J"));
        // A syntax error quotes the line it is on.
        let e = parse("x = 1 # \u{1b}]0;PWNED\u{7}\ny = = 2\n").unwrap_err();
        assert!(e.contains("\u{fffd}]0;PWNED\u{fffd}") && !e.contains('\u{1b}'), "{e}");
    }

    #[test]
    fn a_skill_md_that_is_not_a_regular_file_is_refused_unread() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Read, a directory would fail as one; only the listing names it.
        std::os::unix::fs::symlink(dir, dir.join("SKILL.md")).unwrap();
        let e = checked(dir, "x", false).unwrap_err();
        assert_eq!(e, format!("{} is not a regular file or directory", dir.join("SKILL.md").display()));
    }

    #[test]
    fn renamed_sets_only_the_frontmatter_name() {
        let text = "---\r\ndescription: d\nname: old\n---\nname: old\n";
        assert_eq!(renamed(text, "new"), "---\r\ndescription: d\nname: new\n---\nname: old\n");
    }

    #[test]
    fn the_content_hash_covers_paths_and_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir(dir.join("refs")).unwrap();
        std::fs::write(dir.join("SKILL.md"), "a").unwrap();
        std::fs::write(dir.join("refs/x.md"), "b").unwrap();
        let first = content_hash(dir).unwrap();
        let mut sha = sha1_smol::Sha1::new();
        sha.update(b"SKILL.md\x001\x00a");
        sha.update(b"refs/x.md\x001\x00b");
        assert_eq!(first, sha.digest().to_string());
        std::fs::rename(dir.join("refs/x.md"), dir.join("refs/y.md")).unwrap();
        assert_ne!(content_hash(dir).unwrap(), first, "a renamed file changes the hash");
    }
}
