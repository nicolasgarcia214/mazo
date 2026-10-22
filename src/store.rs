//! The store of installed third-party skills, `<data dir>/store/`, and its
//! lockfile `<data dir>/mazo.lock`: `mazo add`, `mazo update`, and the entry
//! `mazo retire` drops.

use std::cell::OnceCell;
use std::fs;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::audit::{self, Backend, Caller, Pane, Vet, VetVerdict};
use crate::config::{Config, Home, Judge};
use crate::inventory::Inventory;
use crate::ledger::Ledger;
use crate::{Error, Result};
use crate::{ledger, links, policy, work, yaml};

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
    inv.installed = Lock::load(data)?.skills.into_iter().map(|l| (l.name.clone(), l)).collect();
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

impl Locked {
    /// The repository `mazo update` fetches it from; `None` when it was
    /// installed from a local path.
    fn remote(&self) -> std::result::Result<Option<Remote>, String> {
        Ok(match Source::parse(&self.origin)? {
            Source::Git(remote) => Some(remote),
            Source::Local(_) => None,
        })
    }

    /// Whether `mazo update` can fetch it again.
    pub fn updatable(&self) -> bool {
        !matches!(self.remote(), Ok(None))
    }
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
    /// The clone is killed once `cancel` turns true, or after [`CLONE_LIMIT`].
    fn clone_into(&self, into: &Path, cancel: &dyn Fn() -> bool) -> Result<String> {
        let mut git = std::process::Command::new("git");
        git.args(["clone", "--depth", "1", "--quiet", "--"]).arg(&self.url).arg(into).env("GIT_TERMINAL_PROMPT", "0");
        if std::env::var_os("GIT_SSH_COMMAND").is_none() {
            git.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        git.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let spawned = git.spawn().and_then(|child| audit::wait(child, None, CLONE_LIMIT, cancel));
        let Some(out) = spawned.map_err(|e| format!("git: {e}"))? else {
            if cancel() {
                return Err(audit::CANCELLED.into());
            }
            let minutes = CLONE_LIMIT.as_secs() / 60;
            return Err(format!("git clone {}: not done within {minutes} minutes; it was killed", self.url).into());
        };
        if !out.status.success() {
            return Err(format!("git clone {}: {}", self.url, String::from_utf8_lossy(&out.stderr).trim()).into());
        }
        let head = crate::inventory::git(into, &["rev-parse", "HEAD"]);
        head.map(|h| h.trim().to_string()).ok_or_else(|| format!("{}: no HEAD commit", self.url).into())
    }
}

/// How long a clone may run before it is killed.
const CLONE_LIMIT: Duration = Duration::from_secs(10 * 60);

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
    /// `--all`.
    pub all: bool,
    /// Install without vetting.
    pub trust: bool,
    /// Accept a vetted skill without asking.
    pub yes: bool,
    /// Install a skill with a security finding.
    pub force: bool,
}

impl Add<'_> {
    /// What the vet gate may assume for this add.
    fn consent(&self) -> Consent {
        Consent { update: false, yes: self.yes, force: self.force }
    }
}

/// What `add` reads skills from: the source resolved, and cloned when it is a repository.
struct Fetched<'a> {
    /// The source at its subpath.
    root: PathBuf,
    origin: String,
    subpath: PathBuf,
    commit: Option<String>,
    /// The name of a skill at `root` itself.
    own: String,
    /// The clone a skill must stay inside.
    clone: Option<&'a Path>,
    trusted: bool,
}

/// `mazo add <source> [--skill <name> | --all] [--as <name>] [--trust]
/// [--yes] [--force]`: copies the skills chosen at `source` (a local path, or
/// a repository shallow-cloned into `<data dir>/.clone-<pid>`) into the store,
/// each recorded in the lockfile and linked into every home ([`install`]). A
/// source that is not trusted is first vetted by the judge ([`gate`]),
/// with `--all` once its judge calls are agreed to ([`confirm_calls`]). A
/// source of several skills installs the one `--skill` names, every one with
/// `--all`, else the ones picked at a terminal ([`skills_in`]); none picked
/// installs nothing. A refused skill leaves the rest to install; the exit
/// code is the highest of the refusals', 0 when there were none.
pub fn add(cfg: &Config, inv: &Inventory, data: &Path, args: &Add) -> Result<ExitCode> {
    let arg = args.source;
    let usage = |e: String| Error::Usage(format!("add {arg}: {e}"));
    let source = Source::parse(arg).map_err(usage)?;
    let trusted = args.trust || matches!(&source, Source::Git(r) if r.trusted_by(&cfg.trusted));
    // The judge is set up, and its login checked, before anything is cloned.
    let backend = match &cfg.judge {
        _ if trusted => None,
        None => {
            return Err(Error::Usage(format!(
                "add refused: {arg} is not on the trust list and vetting it needs a [judge], which the config lacks; \
                 pass --trust to install it without vetting\n{}",
                no_judge()
            )));
        }
        Some(judge) => Some((judge, audit::backend(judge, &cfg.homes)?)),
    };
    let caller = backend.as_ref().map(|(judge, backend)| Caller { data, judge, backend: backend.as_ref(), pane: None });
    if let Some(c) = &caller {
        // `--all` shows its call count first, so it asks once the source is read.
        if !args.all {
            args.consent().answerable().map_err(Error::Usage)?;
        }
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
            let commit = remote.clone_into(clone, &|| false)?;
            (clone.join(&remote.subpath), remote.url.clone(), remote.subpath.clone(), Some(commit), remote.name())
        }
    };
    let clone = scratch.as_ref().map(|s| s.0.as_path());
    let from = Fetched { root, origin, subpath, commit, own, clone, trusted };
    let children = skills_in(&from.root, &from.own, args)?;
    if children.is_empty() {
        println!("nothing selected; nothing was installed");
    }
    if caller.is_some() && args.all {
        confirm_calls(children.len(), args.consent())?;
    }
    let mut inv = inv.clone();
    let mut code = 0;
    for child in children {
        if let Err(e) = install(cfg, &mut inv, data, args, caller.as_ref(), &from, child) {
            eprintln!("ERROR: {e}");
            code = code.max(e.exit_code());
        }
    }
    Ok(ExitCode::from(code))
}

/// Installs the skill of `from` in `child` (`None` for `from.root` itself)
/// and adds it to `inv`. A bad skill or a name taken in any root is a usage
/// error, and a refused vet or a sync that would fail with the skill
/// installed is a refusal; either way the store and the lockfile are left as
/// they were.
fn install(
    cfg: &Config,
    inv: &mut Inventory,
    data: &Path,
    args: &Add,
    caller: Option<&Caller>,
    from: &Fetched,
    child: Option<String>,
) -> Result<()> {
    let (arg, rename) = (args.source, args.rename);
    let usage = |e: String| Error::Usage(format!("add {arg}: {e}"));
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let (src, subpath) = match &child {
        Some(c) => (from.root.join(c), from.subpath.join(c)),
        None => (from.root.clone(), from.subpath.clone()),
    };
    let src = resolved(&src, from.clone).map_err(usage)?;
    let name = rename.map(str::to_string).or(child).unwrap_or_else(|| from.own.clone());
    let (skill_md, files) = checked(&src, &name, rename.is_some()).map_err(usage)?;

    let store = dir(data);
    let to = store.join(&name);
    if let Some(p) = cfg.source.iter().chain([&store]).map(|r| r.join(&name)).find(|p| p.symlink_metadata().is_ok()) {
        return Err(Error::Usage(format!(
            "add refused: {} already exists; pass --as <name> to install it under another name",
            p.display()
        )));
    }

    let vetted = match caller {
        Some(c) => Some((gate(c, inv, args.consent(), &name, contents(&src, &files, &skill_md)?, None)?, now()?)),
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
            origin: from.origin.clone(),
            subpath,
            commit: from.commit.clone(),
            content_hash,
            trusted: from.trusted,
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
    // Recorded: the next skill of this add plans its links with this one.
    *inv = with;
    println!("installed {}", to.display());
    for line in links::apply(&plan)? {
        println!("{line}");
    }
    Ok(())
}

/// `mazo update`'s arguments.
pub struct Update<'a> {
    /// The store skills to update; every one when empty.
    pub names: &'a [String],
    /// Accept a re-vetted update without asking.
    pub yes: bool,
    /// Apply an update with a security finding.
    pub force: bool,
    /// The TUI's job pane: what the update reports, and its judge's
    /// progress, go there instead of to stdout and stderr.
    pub pane: Option<&'a Pane>,
}

/// Where an update reports: the TUI's job pane, else stdout, or stderr for
/// an error.
#[derive(Clone, Copy)]
struct Out<'a>(Option<&'a Pane>);

impl Out<'_> {
    fn line(self, line: &str) {
        match self.0 {
            Some(pane) => _ = pane.lines.send(line.into()),
            None => println!("{line}"),
        }
    }

    fn error(self, line: &str) {
        match self.0 {
            Some(pane) => _ = pane.lines.send(line.into()),
            None => eprintln!("{line}"),
        }
    }

    /// Whether the pane asked the job to stop.
    fn cancelled(self) -> bool {
        self.0.is_some_and(|p| p.cancel.load(Ordering::Relaxed))
    }
}

/// `mazo update [names…] [--yes] [--force]`: re-clones each git-sourced
/// store skill (every one when `names` is empty) and, when the copy at its
/// subpath hashes differently, swaps it into the store and records the new
/// commit and hash. A skill whose source is not trusted is first re-vetted
/// by the judge on the diff from the installed copy ([`gate`]), and refused
/// (exit 2, with [`no_judge`] once) when no judge is configured; a local-path one is
/// not updatable. A skill that fails or is refused is left as it was and the
/// rest still run; the exit code is the highest of the failures', 2 for no
/// judge and 1 for the rest. A name that is not a store skill is a usage error.
pub fn update(cfg: &Config, inv: &Inventory, data: &Path, args: &Update) -> Result<ExitCode> {
    let (names, out) = (args.names, Out(args.pane));
    let lock = Lock::load(data)?;
    if let Some(n) = names.iter().find(|n| !lock.skills.iter().any(|l| &l.name == *n)) {
        return Err(Error::Usage(format!("update: not a store skill: {n}")));
    }
    let consent = Consent { update: true, yes: args.yes, force: args.force };
    let revet = cfg.judge.as_ref().map(|judge| Revet {
        data,
        judge,
        homes: &cfg.homes,
        consent,
        pane: args.pane,
        backend: OnceCell::new(),
    });
    let (mut updated, mut unjudged, mut code) = (false, false, 0);
    for l in lock.skills.iter().filter(|l| names.is_empty() || names.contains(&l.name)) {
        // A cancelled job starts nothing more.
        if out.cancelled() {
            return Err(audit::CANCELLED.into());
        }
        let mut remote = match l.remote() {
            Ok(Some(remote)) => remote,
            Ok(None) => {
                out.line(&format!("{}: not updatable; installed from a local path", l.name));
                continue;
            }
            Err(e) => {
                out.error(&format!("ERROR: update {}: {e}", l.name));
                code = code.max(1);
                continue;
            }
        };
        let vet = match &revet {
            _ if l.trusted || remote.trusted_by(&cfg.trusted) => None,
            None => {
                out.error(&format!(
                    "ERROR: update {}: update refused: {} is not trusted and re-vetting it needs a [judge], \
                     which the config lacks; add it to trusted to update it without vetting",
                    l.name, l.origin
                ));
                (unjudged, code) = (true, 2);
                continue;
            }
            Some(r) => Some(r),
        };
        remote.subpath = if l.subpath == "." { PathBuf::new() } else { l.subpath.clone().into() };
        match refresh(cfg, inv, data, l, &remote, vet, out) {
            Ok(Some(commit)) => {
                out.line(&format!("updated {} to {commit}", dir(data).join(&l.name).display()));
                updated = true;
            }
            Ok(None) => out.line(&format!("{}: up to date", l.name)),
            Err(e) => {
                out.error(&format!("ERROR: update {}: {e}", l.name));
                code = code.max(1);
            }
        }
    }
    if updated {
        let homes: Vec<&Home> = cfg.homes.iter().collect();
        let plan = links::plan(cfg, inv, &homes, None);
        let errors = links::sync_errors(cfg, inv, &homes, &plan);
        for e in &errors {
            out.error(e);
        }
        if errors.is_empty() {
            for line in links::apply(&plan)? {
                out.line(&line);
            }
        }
        if !errors.is_empty() {
            code = code.max(1);
        }
    }
    if unjudged {
        out.error(&no_judge());
    }
    Ok(ExitCode::from(code))
}

/// Clones `remote` into `<data>/.clone-<pid>` and, when the skill at its
/// subpath hashes unlike `l`, passes it through the `vet` gate if there is
/// one, then swaps the copy in for `store/<name>` under the data lock and
/// records the commit and hash, and any new verdict: the new commit, or
/// `None` when up to date. A refused vet, a sync that would fail with the new
/// copy, or any failed write leaves the old copy and the lockfile as they
/// were; the sync's errors go to `out`. A cancel stops it before the swap,
/// and a copy an interrupted swap set aside is put back first ([`recover`]).
fn refresh(
    cfg: &Config,
    inv: &Inventory,
    data: &Path,
    l: &Locked,
    remote: &Remote,
    vet: Option<&Revet>,
    out: Out,
) -> Result<Option<String>> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let store = dir(data);
    let (to, old) = (store.join(&l.name), store.join(format!(".old-{}", l.name)));
    {
        let _lock = ledger::lock(data)?;
        recover(&to, &old).map_err(|e| err(&old, e))?;
    }
    let scratch = Scratch(data.join(format!(".clone-{}", std::process::id())));
    let clone = scratch.0.as_path();
    let _ = fs::remove_dir_all(clone);
    let commit = remote.clone_into(clone, &|| out.cancelled())?;
    let src = resolved(&clone.join(&remote.subpath), Some(clone))?;
    let (skill_md, files) = checked(&src, &l.name, l.name != remote.name())?;

    // Staged under a name of this process's own, so that the judge and the
    // y/N prompt run without the data lock.
    let staged = Scratch(store.join(format!(".update-{}-{}", l.name, std::process::id())));
    let content_hash = stage(&src, &files, &skill_md, &staged.0)?;
    if content_hash == l.content_hash {
        return Ok(None);
    }
    let vetted = match vet {
        Some(r) => Some((r.vet(inv, &l.name, &skill_md, &store, &staged.0)?, now()?)),
        None => None,
    };

    let _lock = ledger::lock(data)?;
    // The judge saw the diff from the copy then installed, and ran unlocked.
    if let Some(((_, diffed), _)) = &vetted
        && self::content_hash(&to)? != *diffed
    {
        return Err(format!(
            "update refused: {} changed while it was vetted; the skill was left as it was",
            to.display()
        )
        .into());
    }
    if out.cancelled() {
        return Err(audit::CANCELLED.into());
    }
    recover(&to, &old).map_err(|e| err(&old, e))?;
    let swapped = fs::rename(&to, &old).map_err(|e| err(&to, e)).and_then(|()| {
        fs::rename(&staged.0, &to).map_err(|e| err(&to, e)).inspect_err(|_| drop(fs::rename(&old, &to)))
    });
    swapped?;

    let homes: Vec<&Home> = cfg.homes.iter().collect();
    let errors = links::sync_errors(cfg, inv, &homes, &links::plan(cfg, inv, &homes, None));
    let recorded = if errors.is_empty() {
        Lock::load(data).and_then(|mut lock| {
            let entry = lock.skills.iter_mut().find(|s| s.name == l.name).ok_or("no longer in the lockfile")?;
            entry.commit = Some(commit.clone());
            entry.content_hash = content_hash;
            if let Some(((vet, _), at)) = vetted {
                (entry.vetted_at, entry.r#for, entry.r#try) = (Some(at), Some(vet.r#for), vet.r#try);
                entry.verdict = Some(vet.verdict);
            }
            lock.save(data)
        })
    } else {
        Err("update refused; the skill was left as it was".into())
    };
    if let Err(e) = recorded {
        fs::remove_dir_all(&to).and_then(|()| fs::rename(&old, &to)).map_err(|e| err(&to, e))?;
        for e in &errors {
            out.error(e);
        }
        return Err(e);
    }
    // Installed and recorded; a hidden leftover is listed by no root.
    let _ = recover(&to, &old);
    Ok(Some(commit))
}

/// Undoes a swap interrupted after it set `store/<name>` aside as `old`:
/// `old` goes back to `to` when `to` is missing, and is removed only when
/// `to` is there.
fn recover(to: &Path, old: &Path) -> std::io::Result<()> {
    match (old.symlink_metadata(), to.symlink_metadata()) {
        (Err(_), _) => Ok(()),
        (Ok(_), Ok(_)) => fs::remove_dir_all(old),
        (Ok(_), Err(_)) => fs::rename(old, to),
    }
}

/// `update`'s re-vets: the judge, set up and its login checked the first
/// time a changed untrusted skill needs it, so that an update of trusted
/// skills alone never touches it.
struct Revet<'a> {
    data: &'a Path,
    judge: &'a Judge,
    homes: &'a [Home],
    consent: Consent,
    pane: Option<&'a Pane>,
    backend: OnceCell<Box<dyn Backend>>,
}

impl Revet<'_> {
    fn caller(&self) -> Result<Caller<'_>> {
        if self.backend.get().is_none() {
            let backend = audit::backend(self.judge, self.homes)?;
            self.with(backend.as_ref()).login()?;
            let _ = self.backend.set(backend);
        }
        Ok(self.with(self.backend.get().expect("set above").as_ref()))
    }

    fn with<'s>(&'s self, backend: &'s dyn Backend) -> Caller<'s> {
        Caller { data: self.data, judge: self.judge, backend, pane: self.pane }
    }

    /// The vet gate on an update of the store skill `name` to the copy
    /// staged in `staged`: its new `skill_md` and the `git diff --no-index`
    /// from `store/<name>`, with the staged copy's paths shown as the
    /// installed one's. Binary files are diffed as text, so that none hides
    /// behind "Binary files differ": a diff that is not text is withheld
    /// whole. The verdict, and the [`content_hash`] of the copy diffed from.
    fn vet(&self, inv: &Inventory, name: &str, skill_md: &str, store: &Path, staged: &Path) -> Result<(Vet, String)> {
        self.consent.answerable()?;
        let c = self.caller()?;
        let diffed = content_hash(&store.join(name))?;
        let staged = staged.file_name().ok_or("no staged copy")?.to_string_lossy().into_owned();
        // Every flag that user config could otherwise change is set.
        let out = std::process::Command::new("git")
            .current_dir(store)
            .args(["diff", "--no-index", "--text", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames"])
            .args(["--src-prefix=a/", "--dst-prefix=b/", "--", name, &staged])
            .output()
            .map_err(|e| format!("git: {e}"))?;
        // 1 means the copies differ, which the hash already said.
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(format!("git diff: {}", String::from_utf8_lossy(&out.stderr).trim()).into());
        }
        let (from, to) = (format!("/{staged}/"), format!("/{name}/"));
        let diff = out.stdout.split_inclusive(|b| *b == b'\n').flat_map(|line| {
            let header = [&b"diff --git "[..], b"--- ", b"+++ "].iter().any(|h| line.starts_with(h));
            match std::str::from_utf8(line) {
                Ok(text) if header => text.replace(&from, &to).into_bytes(),
                _ => line.to_vec(),
            }
        });
        let vet = gate(
            &c,
            inv,
            self.consent,
            name,
            vec![("SKILL.md".into(), skill_md.as_bytes().to_vec())],
            Some(diff.collect()),
        )?;
        Ok((vet, diffed))
    }
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

/// The most bytes of one file, and of all files, the vet judge is sent; a
/// file past either, or one that is not text, is withheld.
const VET_FILE_BYTES: u64 = 64 * 1024;
const VET_TOTAL_BYTES: u64 = 256 * 1024;

/// What the vet gate guards, an add or an update, which words its prompt and
/// refusals, and the flags that answer for the user.
#[derive(Clone, Copy)]
struct Consent {
    update: bool,
    /// `--yes`.
    yes: bool,
    /// `--force`.
    force: bool,
}

impl Consent {
    fn verb(self) -> &'static str {
        if self.update { "update" } else { "add" }
    }

    fn act(self) -> &'static str {
        if self.update { "update" } else { "install" }
    }

    fn kept(self) -> &'static str {
        if self.update { "the skill was left as it was" } else { "nothing was installed" }
    }

    /// Refused before the judge is paid for an answer nobody can accept.
    fn answerable(self) -> std::result::Result<(), String> {
        if self.yes || std::io::stdin().is_terminal() {
            return Ok(());
        }
        Err(format!(
            "{} refused: a vetted {} asks y/N; pass --yes when stdin is not a terminal",
            self.verb(),
            self.act()
        ))
    }
}

/// The judges any setup can configure, each as a `[judge]` table to paste
/// into the config, after a TOML comment saying what it calls.
const JUDGE_EXAMPLES: [&str; 2] = [
    "# command: any CLI that reads the prompt on stdin\n[judge]\nagent = \"command\"\nargv = [\"llm\", \"-m\", \"gpt-5\"]\n",
    "# http: an OpenAI-compatible endpoint\n[judge]\nagent = \"http\"\nurl = \"https://api.openai.com/v1\"\n\
     model = \"gpt-5\"\napi_key_env = \"OPENAI_API_KEY\"\n",
];

/// What to do with no `[judge]`, after an untrusted add or update is
/// refused for want of one: [`JUDGE_EXAMPLES`], and a prefilled issue asking
/// for a judge for the user's harness.
fn no_judge() -> String {
    let body = "mazo refuses an untrusted add or update without a [judge], and none of its judges fits the harness I use.\n\n\
                Harness: <name and version>\n\
                How it runs one prompt without a terminal: <command or API>\n";
    let issue = url::Url::parse_with_params(
        "https://github.com/nicolasgarcia214/mazo/issues/new",
        [("title", "Support <harness> as a judge"), ("body", body)],
    )
    .expect("a valid URL");
    format!(
        "add one of these judges to the config:\n{}or ask for a judge for your harness: {issue}",
        JUDGE_EXAMPLES.join("")
    )
}

/// `add --all` of an untrusted source: the `n` judge calls, one per skill,
/// shown and agreed to y/N before the first; `--yes` agrees, and without a
/// terminal it is required.
fn confirm_calls(n: usize, consent: Consent) -> Result<()> {
    let s = if n == 1 { "" } else { "s" };
    println!("vetting {n} skill{s} makes {n} judge call{s}");
    consent.answerable().map_err(Error::Usage)?;
    if consent.yes {
        return Ok(());
    }
    eprint!("make {n} judge call{s}? [y/N] ");
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("stdin: {e}"))?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        return Err("add declined; nothing was installed".into());
    }
    Ok(())
}

/// Every file of `files` under `src` as `(relative path, bytes)`, with
/// `skill_md` as its `SKILL.md`.
fn contents(src: &Path, files: &[PathBuf], skill_md: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let files = files.iter().filter(|rel| !src.join(rel).is_dir());
    files
        .map(|rel| {
            let bytes = if rel == Path::new("SKILL.md") {
                skill_md.as_bytes().to_vec()
            } else {
                let path = src.join(rel);
                fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?
            };
            Ok((rel.to_string_lossy().into_owned(), bytes))
        })
        .collect()
}

/// The vet gate, before anything is installed: one judge call on `files` of
/// the skill `name` and, for an update, the `diff` from the installed copy,
/// with the names and descriptions of every other source and store skill
/// and the [`work::summary`] of the user's recent work; its verdict shown,
/// then obeyed. A file or diff withheld from the judge (not text, or past
/// [`VET_FILE_BYTES`] or [`VET_TOTAL_BYTES`]) was never reviewed, so it is a
/// security finding of mazo's own. A security finding
/// refuses (exit 1) unless `--force`, a withheld one before the judge is
/// called; then the user is asked y/N, defaulting to the verdict's
/// [`VetVerdict::default_yes`], unless `--yes`. A judge that gives no valid
/// answer, or a No, refuses.
fn gate(
    c: &Caller,
    inv: &Inventory,
    consent: Consent,
    name: &str,
    files: Vec<(String, Vec<u8>)>,
    diff: Option<Vec<u8>>,
) -> Result<Vet> {
    let (verb, act, kept) = (consent.verb(), consent.act(), consent.kept());
    let (mut withheld, mut total) = (Vec::new(), 0);
    let mut sent = |label: &str, bytes: Vec<u8>| {
        let size = bytes.len() as u64;
        let why = if size > VET_FILE_BYTES {
            Err("over the per-file cap")
        } else if total + size > VET_TOTAL_BYTES {
            Err("over the total cap")
        } else {
            String::from_utf8(bytes).map_err(|_| "not text")
        };
        match why {
            Ok(text) => {
                total += size;
                text
            }
            Err(why) => {
                withheld.push(format!("{label} ({size} bytes, {why})"));
                format!("(withheld: {size} bytes, {why})")
            }
        }
    };
    let files: Vec<(String, String)> = files.into_iter().map(|(rel, bytes)| (rel.clone(), sent(&rel, bytes))).collect();
    let diff = diff.map(|d| sent("the diff", d));
    let withheld = (!withheld.is_empty()).then(|| format!("not sent to the judge in full: {}", withheld.join(", ")));
    if let Some(withheld) = &withheld
        && !consent.force
    {
        return Err(format!("{verb} refused: {withheld}; pass --force to {act} it anyway").into());
    }
    let others = inv.skills.iter().filter(|s| *s != name);
    let existing: Vec<(String, String)> =
        others.clone().map(|s| (s.clone(), inv.description(s).unwrap_or_default())).collect();
    let work = work::summary(Ledger::load(c.data)?.events(), jiff::Timestamp::now());
    let input = audit::vet_input(name, &files, diff.as_deref(), &existing, &work);
    let names: Vec<&str> = others.map(String::as_str).collect();
    let mut vet = audit::vet(c, name, &input, &names)?
        .map_err(|why| format!("{verb} refused: vetting {name} failed: {why}; {kept}"))?;
    if let Some(withheld) = withheld {
        let found = vet.verdict.security.take();
        vet.verdict.security = Some(found.map_or_else(|| withheld.clone(), |f| format!("{f}; {withheld}")));
    }
    vet.text(name).lines().for_each(|l| Out(c.pane).line(l));
    if vet.verdict.security.is_some() && !consent.force {
        return Err(format!("{verb} refused: the judge found a security issue; pass --force to {act} it anyway").into());
    }
    if consent.yes {
        return Ok(vet);
    }
    let default = vet.verdict.default_yes();
    eprint!("{act} {name}? {} ", if default { "[Y/n]" } else { "[y/N]" });
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("stdin: {e}"))?;
    let yes = match answer.trim() {
        "" => default,
        a => matches!(a, "y" | "Y" | "yes"),
    };
    if !yes {
        return Err(format!("{verb} declined; {kept}").into());
    }
    Ok(vet)
}

/// The skills under `root` to install, each by the child it is in: `root`
/// itself (`None`), named `own`, when it holds a `SKILL.md`; else its one
/// child skill, the one `--skill` names, every one with `--all`, or those
/// picked when stdin and stdout are a terminal. Off a terminal, several
/// skills and neither flag is a usage error listing them.
fn skills_in(root: &Path, own: &str, args: &Add) -> Result<Vec<Option<String>>> {
    let usage = |e: String| Error::Usage(format!("add {}: {e}", args.source));
    if root.join("SKILL.md").is_file() {
        return match args.pick {
            Some(p) if p != own => Err(usage(format!("no skill named {p}; the only skill here is {own}"))),
            _ => Ok(vec![None]),
        };
    }
    let found = Inventory::load_roots(None, &[root]).map_err(|e| usage(e.to_string()))?;
    let list = found.skills.iter().map(String::as_str).collect::<Vec<_>>().join(", ");
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let names: Vec<String> = match args.pick {
        Some(p) if found.skills.contains(p) => vec![p.to_string()],
        Some(p) => return Err(usage(format!("no skill named {p}; the skills here are: {list}"))),
        None if found.skills.is_empty() => return Err(usage("no SKILL.md here or in any child directory".into())),
        None if args.all || found.skills.len() == 1 => found.skills.iter().cloned().collect(),
        None if tty => {
            // One line each: a folded description keeps its words, not its breaks.
            let line = |d: String| d.split_whitespace().collect::<Vec<_>>().join(" ");
            let items = found.skills.iter().map(|s| (s.clone(), found.description(s).map(line).unwrap_or_default()));
            crate::pick::pick(items.collect()).map_err(|e| format!("skill picker: {e}"))?
        }
        None => return Err(usage(format!("several skills here; pass --skill with one of: {list}, or --all"))),
    };
    if args.rename.is_some() && names.len() > 1 {
        return Err(usage("--as names one skill; pick one with --skill".into()));
    }
    Ok(names.into_iter().map(Some).collect())
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
    use super::{JUDGE_EXAMPLES, Remote, Source, content_hash, no_judge, renamed};

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
    fn each_judge_example_is_printed_and_loads_as_a_valid_config_judge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for example in JUDGE_EXAMPLES {
            assert!(no_judge().contains(example), "{example}");
            std::fs::write(&path, format!("source = \".\"\n{example}")).unwrap();
            let cfg = crate::config::load(&path).unwrap_or_else(|e| panic!("{example}: {e}")).unwrap();
            assert!(cfg.judge.is_some(), "{example}");
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
