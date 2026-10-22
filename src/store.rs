//! The store of installed third-party skills, `<data dir>/store/`, and its
//! lockfile `<data dir>/mazo.lock`: `mazo add`, `mazo update`, and the entry
//! `mazo retire` drops.

use std::fs;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

use crate::audit::{self, Caller, Vet, VetVerdict};
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
pub fn inventory(source: Option<&Path>, data: &Path) -> Result<Inventory> {
    let store = dir(data);
    // Like the source, canonical: event paths are compared resolved.
    let store = fs::canonicalize(&store).unwrap_or(store);
    let roots: Vec<&Path> = source.into_iter().chain([store.as_path()]).collect();
    let mut inv = Inventory::load_roots(source, &roots)?;
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

/// `mazo add`'s arguments.
pub struct Add<'a> {
    /// A local path, a git URL or GitHub `owner/repo[/path]`.
    pub source: &'a str,
    /// `--as`.
    pub rename: Option<&'a str>,
    /// `--skill`.
    pub pick: Option<&'a str>,
    /// Install without vetting.
    pub trust: bool,
    /// Accept a vetted skill without asking.
    pub yes: bool,
    /// Install a skill with a security finding.
    pub force: bool,
}

/// `mazo add <source> [--skill <name>] [--as <name>] [--trust] [--yes]
/// [--force]`: copies the skill at `source` (a local path, or a repository
/// shallow-cloned into `<data dir>/.clone-<pid>`) into the store, records it
/// in the lockfile and links it into every home. A source that is not
/// trusted is first vetted by the judge ([`gate`]). A bad skill, a name taken
/// in any root, several skills and no `--skill`, or an untrusted source with
/// no judge is a usage error, and a refused vet or a sync that would fail
/// with the skill installed is a refusal; either way the store and the
/// lockfile are left as they were.
pub fn add(cfg: &Config, inv: &Inventory, data: &Path, args: &Add) -> Result<ExitCode> {
    let (arg, rename) = (args.source, args.rename);
    let usage = |e: String| Error::Usage(format!("add {arg}: {e}"));
    let source = Source::parse(arg).map_err(usage)?;
    let trusted = args.trust || matches!(&source, Source::Git(r) if r.trusted_by(&cfg.trusted));
    // The judge is set up, and its login checked, before anything is cloned.
    let backend = match &cfg.judge {
        _ if trusted => None,
        None => {
            return Err(Error::Usage(format!(
                "add refused: {arg} is not on the trust list and vetting is not available yet; \
                 pass --trust to install it without vetting"
            )));
        }
        Some(judge) => Some((judge, audit::backend(judge, &cfg.homes)?)),
    };
    let caller = backend.as_ref().map(|(judge, backend)| Caller { data, judge, backend: backend.as_ref() });
    if let Some(c) = &caller {
        c.login()?;
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
    let (src, child) = skill_in(&root, &own, args.pick).map_err(usage)?;
    let src = resolved(&src, scratch.as_ref().map(|s| s.0.as_path())).map_err(usage)?;
    let subpath = match &child {
        Some(c) => subpath.join(c),
        None => subpath,
    };
    let name = rename.map(str::to_string).or(child).unwrap_or(own);
    let (skill_md, files) = checked(&src, &name, rename.is_some()).map_err(usage)?;

    let store = dir(data);
    let to = store.join(&name);
    if let Some(p) = cfg.source.iter().chain([&store]).map(|r| r.join(&name)).find(|p| p.symlink_metadata().is_ok()) {
        return Err(Error::Usage(format!(
            "add refused: {} already exists; pass --as <name> to install it under another name",
            p.display()
        )));
    }

    let vetted = match &caller {
        Some(c) => Some((gate(c, inv, args, &name, &src, &files, &skill_md)?, now()?)),
        None => None,
    };

    let installed_at = now()?;
    let _lock = ledger::lock(data)?;
    // Copied under a hidden name, which no root lists, then renamed into place.
    let staged = store.join(format!(".add-{name}"));
    let installed = stage(&src, &files, &skill_md, &staged)
        .and_then(|hash| fs::rename(&staged, &to).map(|()| hash).map_err(|e| err(&to, e).into()));
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
        let (vetted_at, r#for, r#try, verdict) = match vetted {
            Some((vet, at)) => (Some(at), Some(vet.r#for), vet.r#try, Some(vet.verdict)),
            None => (None, None, Vec::new(), None),
        };
        let locked = Locked {
            name,
            origin,
            subpath,
            commit,
            content_hash,
            trusted,
            installed_at,
            vetted_at,
            r#for,
            r#try,
            verdict,
        };
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

/// `mazo update [names…]`: re-clones each git-sourced store skill (every one
/// when `names` is empty) and, when the copy at its subpath hashes
/// differently, swaps it into the store and records the new commit and hash.
/// A skill whose source is not trusted is skipped, since re-vetting is not
/// available yet, and a local-path one is not updatable. A skill that fails
/// is left as it was and the rest still run; any failure exits 1. A name that
/// is not a store skill is a usage error.
pub fn update(cfg: &Config, inv: &Inventory, data: &Path, names: &[String]) -> Result<ExitCode> {
    let lock = Lock::load(data)?;
    if let Some(n) = names.iter().find(|n| !lock.skills.iter().any(|l| &l.name == *n)) {
        return Err(Error::Usage(format!("update: not a store skill: {n}")));
    }
    let clone = Scratch(data.join(format!(".clone-{}", std::process::id())));
    let (mut updated, mut failed) = (false, false);
    for l in lock.skills.iter().filter(|l| names.is_empty() || names.contains(&l.name)) {
        let mut remote = match Source::parse(&l.origin) {
            Ok(Source::Git(remote)) => remote,
            Ok(Source::Local(_)) => {
                println!("{}: not updatable; installed from a local path", l.name);
                continue;
            }
            Err(e) => {
                eprintln!("ERROR: update {}: {e}", l.name);
                failed = true;
                continue;
            }
        };
        if !l.trusted && !remote.trusted_by(&cfg.trusted) {
            println!("{}: skipped; {} is not trusted and re-vetting is not available yet", l.name, l.origin);
            continue;
        }
        remote.subpath = if l.subpath == "." { PathBuf::new() } else { l.subpath.clone().into() };
        match refresh(cfg, inv, data, l, &remote, &clone.0) {
            Ok(Some(commit)) => {
                println!("updated {} to {commit}", dir(data).join(&l.name).display());
                updated = true;
            }
            Ok(None) => println!("{}: up to date", l.name),
            Err(e) => {
                eprintln!("ERROR: update {}: {e}", l.name);
                failed = true;
            }
        }
    }
    if updated {
        let homes: Vec<&Home> = cfg.homes.iter().collect();
        let plan = links::plan(cfg, inv, &homes, None);
        let errors = links::sync_errors(cfg, inv, &homes, &plan);
        for e in &errors {
            eprintln!("{e}");
        }
        if errors.is_empty() {
            for line in links::apply(&plan)? {
                println!("{line}");
            }
        }
        failed |= !errors.is_empty();
    }
    Ok(if failed { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

/// Clones `remote` into `clone` and, when the skill at its subpath hashes
/// unlike `l`, swaps the copy in for `store/<name>` under the data lock and
/// records the commit and hash: the new commit, or `None` when up to date. A
/// sync that would fail with the new copy, or any failed write, puts the old
/// one back and leaves the lockfile as it was.
fn refresh(
    cfg: &Config,
    inv: &Inventory,
    data: &Path,
    l: &Locked,
    remote: &Remote,
    clone: &Path,
) -> Result<Option<String>> {
    let _ = fs::remove_dir_all(clone);
    let commit = remote.clone_into(clone)?;
    let src = resolved(&clone.join(&remote.subpath), Some(clone))?;
    let (skill_md, files) = checked(&src, &l.name, l.name != remote.name())?;

    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let store = dir(data);
    let (to, staged, old) =
        (store.join(&l.name), store.join(format!(".update-{}", l.name)), store.join(format!(".old-{}", l.name)));
    let _lock = ledger::lock(data)?;
    let content_hash = stage(&src, &files, &skill_md, &staged)?;
    if content_hash == l.content_hash {
        fs::remove_dir_all(&staged).map_err(|e| err(&staged, e))?;
        return Ok(None);
    }
    let _ = fs::remove_dir_all(&old);
    let swapped = fs::rename(&to, &old)
        .map_err(|e| err(&to, e))
        .and_then(|()| fs::rename(&staged, &to).map_err(|e| err(&to, e)).inspect_err(|_| drop(fs::rename(&old, &to))));
    if let Err(e) = swapped {
        let _ = fs::remove_dir_all(&staged);
        return Err(e.into());
    }

    let homes: Vec<&Home> = cfg.homes.iter().collect();
    let errors = links::sync_errors(cfg, inv, &homes, &links::plan(cfg, inv, &homes, None));
    let recorded = if errors.is_empty() {
        Lock::load(data).and_then(|mut lock| {
            let entry = lock.skills.iter_mut().find(|s| s.name == l.name).ok_or("no longer in the lockfile")?;
            entry.commit = Some(commit.clone());
            entry.content_hash = content_hash;
            lock.save(data)
        })
    } else {
        Err("update refused; the skill was left as it was".into())
    };
    if let Err(e) = recorded {
        fs::remove_dir_all(&to).and_then(|()| fs::rename(&old, &to)).map_err(|e| err(&to, e))?;
        for e in &errors {
            eprintln!("{e}");
        }
        return Err(e);
    }
    // Installed and recorded; a hidden leftover is listed by no root.
    let _ = fs::remove_dir_all(&old);
    Ok(Some(commit))
}

/// `src` resolved; inside `clone` when there is one, so that a symlink in the
/// repository cannot lead the copy outside it.
fn resolved(src: &Path, clone: Option<&Path>) -> std::result::Result<PathBuf, String> {
    let src = fs::canonicalize(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if let Some(clone) = clone
        && !src.starts_with(fs::canonicalize(clone).map_err(|e| format!("{}: {e}", clone.display()))?)
    {
        return Err(format!("{} leaves the repository", src.display()));
    }
    Ok(src)
}

/// The `SKILL.md` of the skill at `src` installed as `name`, its `name:`
/// rewritten when `rename`, once it passes the always-on rules; and every
/// entry under `src`.
fn checked(src: &Path, name: &str, rename: bool) -> std::result::Result<(String, Vec<PathBuf>), String> {
    let skill_md = fs::read_to_string(src.join("SKILL.md")).map_err(|e| format!("SKILL.md: {e}"))?;
    let skill_md = if rename { renamed(&skill_md, name) } else { skill_md };
    yaml::frontmatter(&skill_md).and_then(|meta| policy::always_on(&meta, name)).map_err(|e| e.to_string())?;
    Ok((skill_md, tree(src)?))
}

/// Copies `files` of `src` into a fresh `staged`, `SKILL.md` as `skill_md`,
/// and returns the copy's [`content_hash`]. On failure `staged` is removed.
fn stage(src: &Path, files: &[PathBuf], skill_md: &str, staged: &Path) -> Result<String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let _ = fs::remove_dir_all(staged);
    fs::create_dir_all(staged).map_err(|e| err(staged, e))?;
    let copied = files.iter().try_for_each(|rel| {
        let (from, dest) = (src.join(rel), staged.join(rel));
        if from.is_dir() {
            fs::create_dir(&dest).map_err(|e| err(&dest, e))
        } else if rel == Path::new("SKILL.md") {
            fs::write(&dest, skill_md).map_err(|e| err(&dest, e))
        } else {
            fs::copy(&from, &dest).map(drop).map_err(|e| err(&dest, e))
        }
    });
    copied.map_err(Error::from).and_then(|()| content_hash(staged)).inspect_err(|_| drop(fs::remove_dir_all(staged)))
}

/// Now, to the second, RFC 3339.
fn now() -> Result<String> {
    Ok(jiff::Timestamp::now().round(jiff::Unit::Second).map_err(|e| e.to_string())?.to_string())
}

/// The vet gate, before anything is copied: one judge call on the files of
/// the skill at `src` (`files`, with `skill_md` as its `SKILL.md`) and the
/// names and descriptions of every source and store skill, its verdict
/// shown, then obeyed. A security finding refuses the install (exit 1)
/// unless `--force`; then the user is asked y/N, defaulting to the verdict's
/// [`VetVerdict::default_yes`], unless `--yes`. With no terminal on stdin and
/// no `--yes` it is a usage error. A judge that gives no valid answer, or a
/// No, refuses the install.
fn gate(
    c: &Caller,
    inv: &Inventory,
    args: &Add,
    name: &str,
    src: &Path,
    files: &[PathBuf],
    skill_md: &str,
) -> Result<Vet> {
    let read = |rel: &PathBuf| -> Result<Option<(String, String)>> {
        let path = src.join(rel);
        if path.is_dir() {
            return Ok(None);
        }
        let text = if rel == Path::new("SKILL.md") {
            skill_md.to_string()
        } else {
            let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            String::from_utf8(bytes).unwrap_or_else(|e| format!("(not text, {} bytes)", e.as_bytes().len()))
        };
        Ok(Some((rel.to_string_lossy().into_owned(), text)))
    };
    let files: Vec<(String, String)> = files.iter().map(read).filter_map(Result::transpose).collect::<Result<_>>()?;
    let existing: Vec<(String, String)> =
        inv.skills.iter().map(|s| (s.clone(), inv.description(s).unwrap_or_default())).collect();
    let input = audit::vet_input(name, &files, &existing);
    let names: Vec<&str> = inv.skills.iter().map(String::as_str).collect();
    let vet = audit::vet(c, name, &input, &names)?
        .map_err(|why| format!("add refused: vetting {name} failed: {why}; nothing was installed"))?;
    print!("{}", vet.text(name));
    if vet.verdict.security.is_some() && !args.force {
        return Err("add refused: the judge found a security issue; pass --force to install it anyway".into());
    }
    if args.yes {
        return Ok(vet);
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::Usage(
            "add refused: a vetted install asks y/N; pass --yes when stdin is not a terminal".into(),
        ));
    }
    let default = vet.verdict.default_yes();
    eprint!("install {name}? {} ", if default { "[Y/n]" } else { "[y/N]" });
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("stdin: {e}"))?;
    let yes = match answer.trim() {
        "" => default,
        a => matches!(a, "y" | "Y" | "yes"),
    };
    if !yes {
        return Err("add declined; nothing was installed".into());
    }
    Ok(vet)
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
