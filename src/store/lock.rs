//! The lockfile, `mazo.lock`, and the copy it records: a skill checked,
//! staged and hashed ([`content_hash`]).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    /// Whether it was installed unvetted: a record, which grants `update` no trust.
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
            Ok(text) => parse(&text).map_err(|e| crate::at(&path)(e).into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
            Err(e) => Err(crate::at(&path)(e).into()),
        }
    }

    /// Drops `name`'s entry from the lockfile under `data`, if it has one.
    pub(super) fn remove(data: &Path, name: &str) -> Result<()> {
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
    let src = fs::canonicalize(src).map_err(crate::at(src))?;
    if let Some(clone) = clone
        && !src.starts_with(fs::canonicalize(clone).map_err(crate::at(clone))?)
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

/// Copies `files` of `src` into `staged`, `SKILL.md` as `skill_md`: the
/// copy and its [`content_hash`]. The copy is what the judge reads and what
/// is renamed into place, so that a source changed after it was read
/// installs nothing unvetted.
pub(super) fn stage(src: &Path, files: &[PathBuf], skill_md: &str, staged: Scratch) -> Result<(Scratch, String)> {
    let dir = staged.0.as_path();
    fs::create_dir_all(dir).map_err(crate::at(dir))?;
    files.iter().try_for_each(|rel| {
        let (from, dest) = (src.join(rel), dir.join(rel));
        // Typed on the handle that is read, not on the path [`tree`] listed:
        // an entry swapped for a symlink since is refused, not followed.
        let mut file = unfollowed(src, rel).map_err(crate::at(&from))?;
        let meta = file.metadata().map_err(crate::at(&from))?;
        let ft = meta.file_type();
        if ft.is_dir() {
            fs::create_dir(&dest).map_err(crate::at(&dest))
        } else if !ft.is_file() {
            Err(format!("{} is not a regular file or directory", crate::render::inert(&from.display().to_string())))
        } else if rel == Path::new("SKILL.md") {
            fs::write(&dest, skill_md).map_err(crate::at(&dest))
        } else {
            let mut to = fs::File::create_new(&dest).map_err(crate::at(&dest))?;
            std::io::copy(&mut file, &mut to).map_err(crate::at(&dest))?;
            // As `fs::copy` does, so that a script stays executable.
            to.set_permissions(meta.permissions()).map_err(crate::at(&dest))
        }
    })?;
    let hash = content_hash(dir)?;
    Ok((staged, hash))
}

/// `rel` under `root` opened read-only, one component at a time, none of
/// them followed if a symlink. Non-blocking, so that a FIFO opens to be
/// refused rather than wait for a writer.
fn unfollowed(root: &Path, rel: &Path) -> std::io::Result<fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let mut file = fs::File::open(root)?;
    let mut parts = rel.components().peekable();
    while let Some(part) = parts.next() {
        let name = std::ffi::CString::new(part.as_os_str().as_bytes())?;
        let dir = if parts.peek().is_some() { libc::O_DIRECTORY } else { 0 };
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC | dir;
        // SAFETY: `file` stays open across the call and `name` is NUL-terminated.
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` was just returned by openat(2) and nothing else owns it.
        file = unsafe { fs::File::from_raw_fd(fd) };
    }
    Ok(file)
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
        for e in fs::read_dir(&dir).map_err(crate::at(&dir))? {
            let e = e.map_err(crate::at(&dir))?;
            if e.file_name() == ".git" {
                continue;
            }
            let path = rel.join(e.file_name());
            let ft = e.file_type().map_err(crate::at(&e.path()))?;
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
/// its path, a NUL, an `x` and a NUL if it is executable, its length in
/// decimal, a NUL and its bytes. The `x` makes setting or clearing an
/// executable bit a change that `update` installs, re-vetting it when untrusted.
pub fn content_hash(dir: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    let mut sha = sha1_smol::Sha1::new();
    for rel in tree(dir)? {
        let path = dir.join(&rel);
        if path.is_dir() {
            continue;
        }
        let bytes = fs::read(&path).map_err(crate::at(&path))?;
        let executable = fs::metadata(&path).map_err(crate::at(&path))?.permissions().mode() & 0o111 != 0;
        sha.update(rel.as_os_str().as_encoded_bytes());
        sha.update(format!("\0{}{}\0", if executable { "x\0" } else { "" }, bytes.len()).as_bytes());
        sha.update(&bytes);
    }
    Ok(sha.digest().to_string())
}

#[cfg(test)]
mod tests {
    use super::{Scratch, checked, content_hash, parse, renamed, stage};

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
    fn stage_refuses_an_entry_swapped_for_a_symlink_after_the_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, outside) = (tmp.path().join("src"), tmp.path().join("outside"));
        std::fs::create_dir_all(src.join("refs")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(src.join("SKILL.md"), "a").unwrap();
        std::fs::write(outside.join("x.md"), "secret").unwrap();
        // Listed as regular, then each replaced: the file itself, and a directory above one.
        for (files, link, target) in [
            (vec!["SKILL.md", "x.md"], "x.md", outside.join("x.md")),
            (vec!["SKILL.md", "refs", "refs/x.md"], "refs", outside.clone()),
        ] {
            let _ = std::fs::remove_file(src.join(link));
            let _ = std::fs::remove_dir(src.join(link));
            std::os::unix::fs::symlink(&target, src.join(link)).unwrap();
            let files: Vec<_> = files.into_iter().map(std::path::PathBuf::from).collect();
            let staged = Scratch::new(tmp.path(), "staged");
            let path = staged.0.clone();
            let Err(e) = stage(&src, &files, "a", staged) else { panic!("{link} was followed") };
            assert!(e.to_string().starts_with(&src.join(link).display().to_string()), "{e}");
            assert!(!path.exists(), "a refused copy is removed");
        }
    }

    #[test]
    fn stage_keeps_a_files_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "a").unwrap();
        std::fs::write(src.join("run.sh"), "b").unwrap();
        std::fs::set_permissions(src.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let files = ["SKILL.md", "run.sh"].map(std::path::PathBuf::from);
        let (staged, _) = stage(&src, &files, "a", Scratch::new(tmp.path(), "staged")).unwrap();
        let mode = std::fs::metadata(staged.0.join("run.sh")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn renamed_sets_only_the_frontmatter_name() {
        let text = "---\r\ndescription: d\nname: old\n---\nname: old\n";
        assert_eq!(renamed(text, "new"), "---\r\ndescription: d\nname: new\n---\nname: old\n");
    }

    #[test]
    fn the_content_hash_covers_paths_contents_and_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
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
        let renamed = content_hash(dir).unwrap();
        assert_ne!(renamed, first, "a renamed file changes the hash");
        std::fs::set_permissions(dir.join("refs/y.md"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut sha = sha1_smol::Sha1::new();
        sha.update(b"SKILL.md\x001\x00a");
        sha.update(b"refs/y.md\x00x\x001\x00b");
        assert_eq!(content_hash(dir).unwrap(), sha.digest().to_string(), "a chmod changes the hash");
    }
}
