//! The store of installed third-party skills, `<data dir>/store/`, and its
//! lockfile `<data dir>/mazo.lock`: `mazo add`, and the entry `mazo retire`
//! drops.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

use crate::config::{Config, Home};
use crate::inventory::Inventory;
use crate::{Error, Result};
use crate::{ledger, links, policy, yaml};

/// `<data dir>/store`, the skill root after the source.
pub fn dir(data: &Path) -> PathBuf {
    data.join("store")
}

/// The inventory over the source and the store, with each store skill's
/// install date from the lockfile.
pub fn inventory(source: &Path, data: &Path) -> Result<Inventory> {
    let store = dir(data);
    // Like the source, canonical: event paths are compared resolved.
    let store = fs::canonicalize(&store).unwrap_or(store);
    let mut inv = Inventory::load_roots(source, &[source, &store])?;
    inv.installed = Lock::load(data)?.skills.into_iter().map(|l| (l.name, l.installed_at)).collect();
    Ok(inv)
}

/// `<data dir>/mazo.lock`: one `[[skill]]` per installed skill.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    #[serde(default, rename = "skill")]
    pub skills: Vec<Locked>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Locked {
    pub name: String,
    /// Where the skill came from; for a local path, the physical directory.
    pub origin: String,
    /// The skill's directory inside `origin`; `.` when `origin` is the skill.
    pub subpath: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// [`content_hash`] of the installed copy.
    pub content_hash: String,
    pub trusted: bool,
    /// RFC 3339.
    pub installed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
}

impl Lock {
    /// The lockfile under `data`; empty when there is none.
    pub fn load(data: &Path) -> Result<Lock> {
        let path = data.join("mazo.lock");
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()).into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
            Err(e) => Err(format!("{}: {e}", path.display()).into()),
        }
    }

    /// Drops `name`'s entry from the lockfile under `data`, if it has one.
    pub fn remove(data: &Path, name: &str) -> Result<()> {
        let mut lock = Lock::load(data)?;
        lock.skills.retain(|s| s.name != name);
        lock.save(data)
    }

    fn save(&self, data: &Path) -> Result<()> {
        ledger::write_atomic(&data.join("mazo.lock"), toml::to_string(self).map_err(|e| e.to_string())?.as_bytes())
    }
}

/// Where `mazo add` reads a skill from.
#[derive(Debug, PartialEq)]
enum Source {
    Local(PathBuf),
    Git(Remote),
}

#[derive(Debug, PartialEq)]
struct Remote {
    /// The clone URL, without the subpath.
    url: String,
    /// The URL's path segments, `.git` dropped from the last: owner, repo, ….
    path: Vec<String>,
    /// The directory inside the repository; empty for its root.
    subpath: PathBuf,
}

impl Source {
    /// A path that exists or starts with `.`, `/` or `~` is local. Otherwise
    /// `scheme://…` and `[user@]host:path` are git URLs, and `owner/repo[/sub]`
    /// is a GitHub repository. In a URL the subpath follows the first
    /// segment ending in `.git`.
    fn parse(arg: &str) -> std::result::Result<Source, String> {
        if arg.starts_with('-') {
            return Err("a source must not start with -".into());
        }
        if arg.starts_with(['.', '/', '~']) || Path::new(arg).exists() {
            return Ok(Source::Local(arg.into()));
        }
        let bad = || format!("not a local path, a git URL or owner/repo[/path]: {arg:?}");
        let (base, segments): (String, Vec<&str>) = if let Some((scheme, rest)) = arg.split_once("://") {
            let (host, path) = rest.split_once('/').ok_or_else(bad)?;
            (format!("{scheme}://{host}/"), path.split('/').collect())
        } else if let Some((host, path)) = arg.split_once(':').filter(|(host, _)| !host.contains('/')) {
            (format!("{host}:"), path.split('/').collect())
        } else {
            let segments: Vec<&str> = arg.trim_end_matches('/').split('/').collect();
            let repo = segments.get(1).ok_or_else(bad)?;
            let repo = repo.strip_suffix(".git").unwrap_or(repo);
            let path = vec![segments[0].to_string(), repo.to_string()];
            let url = format!("https://github.com/{}/{repo}.git", segments[0]);
            return Remote::new(url, path, &segments[2..]).map(Source::Git).ok_or_else(bad);
        };
        let segments: Vec<&str> = segments.into_iter().filter(|s| !s.is_empty()).collect();
        let cut = segments.iter().position(|s| s.ends_with(".git")).map_or(segments.len(), |i| i + 1);
        let mut url = format!("{base}{}", segments[..cut].join("/"));
        // One origin per repository; a `file://` path names the directory as it is.
        if !url.ends_with(".git") && !url.starts_with("file://") {
            url.push_str(".git");
        }
        // Only the last segment of the repository's path can end in `.git`.
        let path = segments[..cut].iter().map(|s| s.strip_suffix(".git").unwrap_or(s).to_string()).collect();
        Remote::new(url, path, &segments[cut..]).map(Source::Git).ok_or_else(bad)
    }
}

impl Remote {
    /// `None` when the path names no repository or a segment is empty, `.` or `..`.
    fn new(url: String, path: Vec<String>, subpath: &[&str]) -> Option<Remote> {
        let ok = |s: &str| !s.is_empty() && s != "." && s != "..";
        let valid = path.len() >= 2 && path.iter().all(|s| ok(s)) && subpath.iter().all(|s| ok(s));
        valid.then(|| Remote { url, path, subpath: subpath.iter().collect() })
    }

    /// An entry equal to the owner (the first segment) or to owner/repo (the
    /// first two), ignoring ASCII case as GitHub does, trusts the repository,
    /// on any host.
    fn trusted_by(&self, trusted: &[String]) -> bool {
        let repo = format!("{}/{}", self.path[0], self.path[1]);
        trusted.iter().any(|t| t.eq_ignore_ascii_case(&self.path[0]) || t.eq_ignore_ascii_case(&repo))
    }

    /// The name of the skill at the subpath: its last segment, else the repository's.
    fn name(&self) -> String {
        self.subpath.file_name().map_or_else(|| self.path[self.path.len() - 1].clone(), |n| n.to_string_lossy().into())
    }

    /// Shallow-clones the repository into `into` and returns the commit.
    /// Neither git nor ssh may prompt, unless the user's `GIT_SSH_COMMAND` says so.
    fn clone_into(&self, into: &Path) -> Result<String> {
        let mut git = std::process::Command::new("git");
        git.args(["clone", "--depth", "1", "--quiet", "--"]).arg(&self.url).arg(into).env("GIT_TERMINAL_PROMPT", "0");
        if std::env::var_os("GIT_SSH_COMMAND").is_none() {
            git.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        let out = git.output().map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(format!("git clone {}: {}", self.url, String::from_utf8_lossy(&out.stderr).trim()).into());
        }
        let head = crate::inventory::git(into, &["rev-parse", "HEAD"]);
        head.map(|h| h.trim().to_string()).ok_or_else(|| format!("{}: no HEAD commit", self.url).into())
    }
}

/// A directory removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `mazo add <source> [--skill <name>] [--as <name>] [--trust]`: copies the
/// skill at `source` (a local path, or a repository shallow-cloned into
/// `<data dir>/.clone-<pid>`) into the store, records it in the lockfile and
/// links it into every home. A bad skill, a name taken in any root, several
/// skills and no `--skill`, or an untrusted source is a usage error, and a
/// sync that would fail with the skill installed is a refusal; either way the
/// store and the lockfile are left as they were.
pub fn add(
    cfg: &Config,
    inv: &Inventory,
    data: &Path,
    arg: &str,
    rename: Option<&str>,
    pick: Option<&str>,
    trust: bool,
) -> Result<ExitCode> {
    let usage = |e: String| Error::Usage(format!("add {arg}: {e}"));
    let source = Source::parse(arg).map_err(usage)?;
    let trusted = trust || matches!(&source, Source::Git(r) if r.trusted_by(&cfg.trusted));
    if !trusted {
        return Err(Error::Usage(format!(
            "add refused: {arg} is not on the trust list and vetting is not available yet; \
             pass --trust to install it without vetting"
        )));
    }
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let mut scratch = None;
    let (root, origin, subpath, commit, own) = match &source {
        Source::Local(path) => {
            let root = fs::canonicalize(path).map_err(|e| usage(e.to_string()))?;
            let own =
                root.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| usage("no name".into()))?;
            (root.clone(), root.to_string_lossy().into_owned(), PathBuf::new(), None, own)
        }
        Source::Git(remote) => {
            fs::create_dir_all(data).map_err(|e| err(data, e))?;
            let clone = &scratch.insert(Scratch(data.join(format!(".clone-{}", std::process::id())))).0;
            let _ = fs::remove_dir_all(clone);
            let commit = remote.clone_into(clone)?;
            (clone.join(&remote.subpath), remote.url.clone(), remote.subpath.clone(), Some(commit), remote.name())
        }
    };
    let (src, child) = skill_in(&root, &own, pick).map_err(usage)?;
    // A symlink in the repository must not lead the copy outside the clone.
    let src = fs::canonicalize(&src).map_err(|e| usage(e.to_string()))?;
    if let Some(Scratch(clone)) = &scratch
        && !src.starts_with(fs::canonicalize(clone).map_err(|e| err(clone, e))?)
    {
        return Err(usage(format!("{} leaves the repository", src.display())));
    }
    let subpath = match &child {
        Some(c) => subpath.join(c),
        None => subpath,
    };
    let skill_md = fs::read_to_string(src.join("SKILL.md")).map_err(|e| usage(format!("SKILL.md: {e}")))?;
    let name = rename.map(str::to_string).or(child).unwrap_or(own);
    let skill_md = if rename.is_some() { renamed(&skill_md, &name) } else { skill_md };
    yaml::frontmatter(&skill_md).and_then(|meta| policy::always_on(&meta, &name)).map_err(|e| usage(e.to_string()))?;
    let files = tree(&src).map_err(usage)?;

    let store = dir(data);
    let to = store.join(&name);
    if let Some(p) = [&cfg.source, &store].map(|r| r.join(&name)).into_iter().find(|p| p.symlink_metadata().is_ok()) {
        return Err(Error::Usage(format!(
            "add refused: {} already exists; pass --as <name> to install it under another name",
            p.display()
        )));
    }

    let installed_at = jiff::Timestamp::now().round(jiff::Unit::Second).map_err(|e| e.to_string())?.to_string();
    let _lock = ledger::lock(data)?;
    // Copied under a hidden name, which no root lists, then renamed into place.
    let staged = store.join(format!(".add-{name}"));
    let _ = fs::remove_dir_all(&staged);
    fs::create_dir_all(&staged).map_err(|e| err(&staged, e))?;
    let copied = files.iter().try_for_each(|rel| {
        let (from, dest) = (src.join(rel), staged.join(rel));
        if from.is_dir() {
            fs::create_dir(&dest).map_err(|e| err(&dest, e))
        } else if rel == Path::new("SKILL.md") {
            fs::write(&dest, &skill_md).map_err(|e| err(&dest, e))
        } else {
            fs::copy(&from, &dest).map(drop).map_err(|e| err(&dest, e))
        }
    });
    let hashed = copied.map_err(Error::from).and_then(|()| content_hash(&staged));
    let installed = hashed.and_then(|hash| fs::rename(&staged, &to).map(|()| hash).map_err(|e| err(&to, e).into()));
    let content_hash = installed.inspect_err(|_| drop(fs::remove_dir_all(&staged)))?;

    // From here until the lockfile is written, `store/<name>` has no lock
    // entry. A crash in that window leaves it there unrecorded: a re-run of
    // `add` refuses the name as taken, and `sync` links it like any store skill.
    let mut with = inv.clone();
    with.skills.insert(name.clone());
    with.known.insert(name.clone());
    with.origins.insert(name.clone(), vec![store.clone()]);
    let homes: Vec<&Home> = cfg.homes.iter().collect();
    let plan = links::plan(cfg, &with, &homes, None);
    let errors = links::sync_errors(cfg, &with, &homes, &plan);
    let recorded = if errors.is_empty() {
        let subpath = if subpath.as_os_str().is_empty() { ".".into() } else { subpath.to_string_lossy().into() };
        let locked = Locked { name, origin, subpath, commit, content_hash, trusted, installed_at, verdict: None };
        Lock::load(data).and_then(|mut lock| {
            lock.skills.push(locked);
            lock.save(data)
        })
    } else {
        Err("add refused; nothing was installed".into())
    };
    if let Err(e) = recorded {
        fs::remove_dir_all(&to).map_err(|e| err(&to, e))?;
        for e in &errors {
            eprintln!("{e}");
        }
        return Err(e);
    }
    println!("installed {}", to.display());
    for line in links::apply(&plan)? {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The skill under `root` and the child it is in: `root` itself, named
/// `own`, when it holds a `SKILL.md`; else its one child skill, or the one
/// `pick` names.
fn skill_in(root: &Path, own: &str, pick: Option<&str>) -> std::result::Result<(PathBuf, Option<String>), String> {
    if root.join("SKILL.md").is_file() {
        return match pick {
            Some(p) if p != own => Err(format!("no skill named {p}; the only skill here is {own}")),
            _ => Ok((root.to_path_buf(), None)),
        };
    }
    let names = crate::inventory::canonical_skills(root).map_err(|e| e.to_string())?;
    let list = names.iter().map(String::as_str).collect::<Vec<_>>().join(", ");
    let name = match pick {
        Some(p) if names.contains(p) => p.to_string(),
        Some(p) => return Err(format!("no skill named {p}; the skills here are: {list}")),
        None if names.len() == 1 => list,
        None if names.is_empty() => return Err("no SKILL.md here or in any child directory".into()),
        None => return Err(format!("several skills here; pass --skill with one of: {list}")),
    };
    Ok((root.join(&name), Some(name)))
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
        for e in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let e = e.map_err(|e| format!("{}: {e}", dir.display()))?;
            if e.file_name() == ".git" {
                continue;
            }
            let path = rel.join(e.file_name());
            let ft = e.file_type().map_err(|err| format!("{}: {err}", e.path().display()))?;
            if ft.is_dir() {
                stack.push(path.clone());
            } else if !ft.is_file() {
                return Err(format!("{} is not a regular file or directory", e.path().display()));
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
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        sha.update(rel.as_os_str().as_encoded_bytes());
        sha.update(format!("\0{}\0", bytes.len()).as_bytes());
        sha.update(&bytes);
    }
    Ok(sha.digest().to_string())
}

#[cfg(test)]
mod tests {
    use super::{Remote, Source, content_hash, renamed};

    fn remote(arg: &str) -> Remote {
        match Source::parse(arg) {
            Ok(Source::Git(r)) => r,
            other => panic!("{arg}: {other:?}"),
        }
    }

    #[test]
    fn shorthand_resolves_to_github_with_a_subpath() {
        let r = remote("owner/repo");
        assert_eq!(
            (r.url.as_str(), r.path.clone(), r.subpath.to_str()),
            ("https://github.com/owner/repo.git", vec!["owner".into(), "repo".into()], Some(""))
        );
        assert_eq!(r.name(), "repo");
        let r = remote("owner/repo.git/a/b/");
        assert_eq!((r.url.as_str(), r.subpath.to_str()), ("https://github.com/owner/repo.git", Some("a/b")));
        assert_eq!(r.name(), "b");
        for same in
            ["https://github.com/owner/repo", "https://github.com/owner/repo/", "https://github.com/owner/repo.git"]
        {
            assert_eq!(remote(same).url, remote("owner/repo").url, "{same}");
        }
    }

    #[test]
    fn git_urls_split_the_subpath_after_the_git_segment() {
        for (arg, url, path, sub) in [
            ("https://github.com/o/r", "https://github.com/o/r.git", "o/r", ""),
            ("https://github.com/o/r/", "https://github.com/o/r.git", "o/r", ""),
            ("ssh://h/o/r", "ssh://h/o/r.git", "o/r", ""),
            ("git@h:o/r/", "git@h:o/r.git", "o/r", ""),
            ("file:///srv/git/r", "file:///srv/git/r", "srv/git/r", ""),
            ("https://gitlab.com/o/r.git/skills/x", "https://gitlab.com/o/r.git", "o/r", "skills/x"),
            ("ssh://git@host:22/o/r.git", "ssh://git@host:22/o/r.git", "o/r", ""),
            ("git@host.org:o/r.git/a", "git@host.org:o/r.git", "o/r", "a"),
            ("file:///srv/git/r.git/a/b", "file:///srv/git/r.git", "srv/git/r", "a/b"),
        ] {
            let r = remote(arg);
            assert_eq!((r.url.as_str(), r.path.join("/"), r.subpath.to_str()), (url, path.into(), Some(sub)), "{arg}");
        }
    }

    #[test]
    fn local_paths_stay_local_and_bad_sources_are_refused() {
        for arg in ["./x", "/x/y", "~/x", ".", "src"] {
            assert_eq!(Source::parse(arg), Ok(Source::Local(arg.into())), "{arg}");
        }
        for arg in [
            "-uhttps://x/o/r.git",
            "--upload-pack=x",
            "owner",
            "owner//repo",
            "o/r/../x",
            "https://host",
            "git@host:r.git",
        ] {
            assert!(Source::parse(arg).is_err(), "{arg}");
        }
    }

    #[test]
    fn trust_matches_the_owner_or_owner_and_repo_on_any_host() {
        let list = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for arg in [
            "owner/repo/a",
            "https://github.com/owner/repo.git",
            "git@codeberg.org:owner/repo.git",
            "ssh://h/owner/repo",
        ] {
            let r = remote(arg);
            assert!(r.trusted_by(&list(&["owner"])), "{arg}");
            assert!(r.trusted_by(&list(&["x", "owner/repo"])), "{arg}");
            assert!(!r.trusted_by(&list(&["repo", "owner/other", "other/repo", "github.com"])), "{arg}");
            assert!(!r.trusted_by(&[]), "{arg}");
            assert!(r.trusted_by(&list(&["OWNER"])) && r.trusted_by(&list(&["Owner/Repo"])), "{arg}");
        }
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
