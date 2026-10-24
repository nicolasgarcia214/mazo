//! The store of installed third-party skills, `<data dir>/store/`, and its
//! lockfile `<data dir>/mazo.lock`: `mazo add`, `mazo update`, and the entry
//! `mazo retire` drops. With `--project`, `add`, `update` and `retire` act on
//! a project's own store, `<git root>/.mazo/skills/`, and its own
//! `<git root>/mazo.lock`.
//!
//! Here `add`, its install into a [`Target`], and that target's removal;
//! [`update`] re-fetches, and [`bundled`] installs mazo's own skills. A
//! source is parsed, trusted and cloned in [`source`], copied, hashed and
//! recorded in [`lock`], judged in [`vet`], asked about in [`ask`], and a
//! project's root and links are [`project`]'s.

mod ask;
mod bundled;
mod lock;
mod project;
mod source;
mod update;
mod vet;

use std::fs;
use std::path::{Path, PathBuf};

use crate::audit::{self, Caller};
use crate::config::{self, Config, Home};
use crate::inventory::Inventory;
use crate::{Error, Result};
use crate::{ledger, links};

pub use ask::{Io, Terminal};
pub use bundled::{install as install_bundled, missing as missing_bundled};
pub use lock::{Lock, Locked, content_hash};
pub use project::{Project, project_dir, project_hint, project_inventory, project_root};
pub use source::github_repo;
pub use update::{Update, update};

use ask::{Consent, confirm_calls, name as skill_name, skills_in};
use lock::{checked, now, resolved, stage};
use project::{project_links, unlink_project};
use source::{Scratch, Source, local};
use vet::{contents, gate, no_judge};

/// A file error as the store reports it: the path, then the error. The path
/// is [`inert`](crate::render::inert), since a skill's author names its files.
fn err(p: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", crate::render::inert(&p.display().to_string()))
}

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
    /// Install into the project at the current directory's git root.
    pub project: bool,
    /// Where the add reports and asks.
    pub io: &'a dyn Io,
}

impl Add<'_> {
    /// What the vet gate may assume for this add.
    fn consent(&self) -> Consent<'_> {
        Consent { update: false, yes: self.yes, force: self.force, io: self.io }
    }
}

/// `arg` parsed, and whether it installs without vetting: with `trust`, or
/// as a git source on the `trusted` list. An untrusted source with no
/// `[judge]` is refused, naming the judges to add ([`no_judge`]).
fn resolve(cfg: &Config, arg: &str, trust: bool) -> Result<(Source, bool)> {
    let source = Source::parse(arg).map_err(|e| Error::Usage(format!("add {arg}: {e}")))?;
    let trusted = trust || matches!(&source, Source::Git(r) if r.trusted_by(&cfg.trusted));
    if !trusted && cfg.judge.is_none() {
        return Err(Error::Usage(format!(
            "add refused: {arg} is not on the trust list and vetting it needs a [judge], which the config lacks; \
             pass --trust to install it without vetting\n{}",
            no_judge()
        )));
    }
    Ok((source, trusted))
}

/// The refusal `mazo add <arg>` gives before reading the source, if any.
pub fn precheck(cfg: &Config, arg: &str) -> Result<()> {
    resolve(cfg, arg, false).map(drop)
}

/// How an add or update ended, its failures reported as they happened: the
/// worst of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Done,
    /// A skill failed or was refused.
    Failed,
    /// A skill was refused as a usage error, or for want of a `[judge]`.
    Usage,
}

/// What `add` reads skills from: the source resolved, and cloned when it is a repository.
struct Fetched<'a> {
    /// The source at its subpath.
    root: PathBuf,
    origin: String,
    subpath: PathBuf,
    r#ref: Option<String>,
    commit: Option<String>,
    /// The name of a skill at `root` itself.
    own: String,
    /// The clone a skill must stay inside.
    clone: Option<&'a Path>,
    trusted: bool,
}

/// `mazo add <source> [--skill <name> | --all] [--as <name>] [--trust]
/// [--yes] [--force]`: copies the skills chosen at `source` (a local path, or
/// a repository shallow-cloned at its ref, resolved first, into
/// `<data dir>/.clone-<pid>`) into the store, each recorded in the lockfile and linked into every home ([`install`]). A
/// source that is not trusted is first vetted by the judge ([`gate`]),
/// with `--all` once its judge calls are agreed to ([`confirm_calls`]). A
/// source of several skills installs the one `--skill` names, every one with
/// `--all`, else the ones picked at a terminal ([`skills_in`]); none picked
/// installs nothing. A refused skill leaves the rest to install; the
/// outcome is the worst of the refusals'. With
/// `--project`, outside a git repository is a usage error before anything
/// else, and the vet sees the project's skills too.
pub fn add(cfg: &Config, inv: &Inventory, data: &Path, args: &Add) -> Result<Outcome> {
    let project = if args.project { Some(project_root("add")?) } else { None };
    let (arg, out) = (args.source, args.io);
    let usage = |e: String| Error::Usage(format!("add {arg}: {e}"));
    let (mut source, trusted) = resolve(cfg, arg, args.trust)?;
    // The judge is set up, and its login checked, before anything is cloned.
    let backend = match &cfg.judge {
        Some(judge) if !trusted => Some((judge, audit::backend(judge, &cfg.homes)?)),
        _ => None,
    };
    let caller =
        backend.as_ref().map(|(judge, backend)| Caller { data, judge, backend: backend.as_ref(), pane: out.pane() });
    if let Some(c) = &caller {
        // `--all` shows its call count first, so it asks once the source is read.
        if !args.all {
            args.consent().answerable().map_err(Error::Usage)?;
        }
        c.login()?;
    }
    let mut scratch = None;
    let (root, origin, subpath, r#ref, commit, own) = match &mut source {
        Source::Local(path) => {
            let root = fs::canonicalize(local(path, &config::home_dir()?)?).map_err(|e| usage(e.to_string()))?;
            let own =
                root.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| usage("no name".into()))?;
            (root.clone(), root.to_string_lossy().into_owned(), PathBuf::new(), None, None, own)
        }
        Source::Git(remote) => {
            remote.resolve(&|| out.cancelled())?;
            fs::create_dir_all(data).map_err(|e| err(data, e))?;
            let clone = &scratch.insert(Scratch(data.join(format!(".clone-{}", std::process::id())))).0;
            let _ = fs::remove_dir_all(clone);
            let commit = remote.clone_into(clone, &|| out.cancelled())?;
            let (url, subpath, r#ref) = (remote.url.clone(), remote.subpath.clone(), remote.r#ref.clone());
            (clone.join(&subpath), url, subpath, r#ref, Some(commit), remote.name())
        }
    };
    let clone = scratch.as_ref().map(|s| s.0.as_path());
    let from = Fetched { root, origin, subpath, r#ref, commit, own, clone, trusted };
    let children = skills_in(&from.root, &from.own, args)?;
    if children.is_empty() {
        out.line("nothing selected; nothing was installed");
    }
    if caller.is_some() && !children.is_empty() && (args.all || out.confirms_calls()) {
        confirm_calls(children.len(), args.consent())?;
    }
    let mut inv = match &project {
        Some(p) => project_inventory(inv, p)?,
        None => inv.clone(),
    };
    let target = Target { data, project: project.as_deref() };
    let mut outcome = Outcome::Done;
    for child in children {
        // A cancelled job starts nothing more.
        if out.cancelled() {
            return Err(audit::CANCELLED.into());
        }
        if let Err(e) = install(cfg, &mut inv, target, args, caller.as_ref(), &from, child) {
            out.error(&format!("ERROR: {e}"));
            outcome = outcome.max(if let Error::Usage(_) = e { Outcome::Usage } else { Outcome::Failed });
        }
    }
    Ok(outcome)
}

/// The store a command acts on: the data directory's, or a project's.
#[derive(Clone, Copy)]
pub struct Target<'a> {
    pub data: &'a Path,
    /// The project's git root, for `--project`.
    pub project: Option<&'a Path>,
}

impl Target<'_> {
    /// The sync of every home after a change to this store, over `inv`
    /// without `gone`: its plan and the errors that stop it. Empty for a
    /// project's store, which the homes do not link.
    pub fn home_sync(self, cfg: &Config, inv: &Inventory, gone: Option<&Path>) -> (links::Plan, Vec<String>) {
        if self.project.is_some() {
            return Default::default();
        }
        let homes: Vec<&Home> = cfg.homes.iter().collect();
        let plan = links::plan(cfg, inv, &homes, gone);
        let errors = links::sync_errors(cfg, inv, &homes, &plan);
        (plan, errors)
    }

    /// Removes the skill `name` at `from` in this store: its links (`plan`'s,
    /// and a project's own), then its lock entry, then its directory, so
    /// that a failure leaves at worst an unrecorded skill, never an entry for
    /// a missing one. The lines reporting it.
    pub fn remove(self, from: &Path, name: &str, plan: &links::Plan) -> Result<Vec<String>> {
        let mut out = links::apply(plan)?;
        if let Some(p) = self.project {
            out.extend(unlink_project(p, name)?);
        }
        Lock::remove(&self.lock_dir(), name)?;
        fs::remove_dir_all(from).map_err(|e| err(from, e))?;
        out.push(format!("removed   {}", from.display()));
        Ok(out)
    }

    fn store(self) -> PathBuf {
        self.project.map_or_else(|| dir(self.data), project_dir)
    }

    /// Where its `mazo.lock` is.
    fn lock_dir(self) -> PathBuf {
        self.project.unwrap_or(self.data).to_path_buf()
    }
}

/// Refuses the name `add` would install when any of its `paths` exists.
fn unclaimed(paths: impl IntoIterator<Item = PathBuf>) -> Result<()> {
    match paths.into_iter().find(|p| p.symlink_metadata().is_ok()) {
        Some(p) => Err(Error::Usage(format!(
            "add refused: {} already exists; pass --as <name> to install it under another name",
            p.display()
        ))),
        None => Ok(()),
    }
}

/// Directories, deepest first, each removed when dropped if it is empty: what
/// staging made of a store, gone again when the add is refused.
struct Fresh(Vec<PathBuf>);

impl Drop for Fresh {
    fn drop(&mut self) {
        for dir in &self.0 {
            let _ = fs::remove_dir(dir);
        }
    }
}

/// Installs the skill of `from` at the path `child` below its root (`None`
/// for `from.root` itself), named for its last segment, and adds it to
/// `inv`. A bad skill or a name taken in any root, or at a
/// project link, is a usage error, and a refused vet or a sync that would
/// fail with the skill installed is a refusal; either way the store and the
/// lockfile are left as they were. A project skill is linked into the
/// project skill dirs ([`project_links`]), not the homes.
fn install(
    cfg: &Config,
    inv: &mut Inventory,
    target: Target,
    args: &Add,
    caller: Option<&Caller>,
    from: &Fetched,
    child: Option<String>,
) -> Result<()> {
    let (arg, rename, out) = (args.source, args.rename, args.io);
    let usage = |e: String| Error::Usage(format!("add {arg}: {e}"));
    let (src, subpath) = match &child {
        Some(c) => (from.root.join(c), from.subpath.join(c)),
        None => (from.root.clone(), from.subpath.clone()),
    };
    let src = resolved(&src, from.clone).map_err(usage)?;
    let name = rename.or(child.as_deref().map(skill_name)).unwrap_or(&from.own).to_string();
    let (skill_md, files) = checked(&src, &name, rename.is_some()).map_err(usage)?;

    let Target { data, project } = target;
    let store = target.store();
    let to = store.join(&name);
    let links: Vec<(PathBuf, PathBuf)> = project.into_iter().flat_map(|p| project_links(p, &name)).collect();
    let claimed = || {
        let roots = cfg.source.iter().cloned().chain([dir(data)]).chain(project.map(|_| store.clone()));
        roots.map(|r| r.join(&name)).chain(links.iter().map(|(l, _)| l.clone()))
    };
    // Checked before the judge is paid for, and again under the lock.
    unclaimed(claimed())?;

    // Copied under a hidden name of this process's own, which no root lists,
    // so that the judge and the y/N prompt run without the data lock. Declared
    // first, the store's new directories are dropped last.
    let _fresh = Fresh(store.ancestors().take_while(|d| !d.exists()).map(Path::to_path_buf).collect());
    let (staged, content_hash) =
        stage(&src, &files, &skill_md, store.join(format!(".add-{name}-{}", std::process::id())))?;
    let vetted = match caller {
        Some(c) => Some((gate(c, inv, args.consent(), &name, contents(&staged.0, &files)?, None)?, now()?)),
        None => None,
    };

    let installed_at = now()?;
    // The data lock guards the project's store and `mazo.lock` too, so two
    // adds into one project are serialized only when they share a data dir.
    let _lock = ledger::lock(data)?;
    unclaimed(claimed())?;
    fs::rename(&staged.0, &to).map_err(|e| err(&to, e))?;

    // From here until the lockfile is written, `store/<name>` has no lock
    // entry. A crash in that window leaves it there unrecorded: a re-run of
    // `add` refuses the name as taken, and `sync` links it like any store skill.
    let subpath = if subpath.as_os_str().is_empty() { ".".into() } else { subpath.to_string_lossy().into() };
    let (vetted_at, r#for, r#try, verdict) = match vetted {
        Some((vet, at)) => (Some(at), Some(vet.r#for), vet.r#try, Some(vet.verdict)),
        None => (None, None, Vec::new(), None),
    };
    let locked = Locked {
        name: name.clone(),
        origin: from.origin.clone(),
        subpath,
        r#ref: from.r#ref.clone(),
        commit: from.commit.clone(),
        content_hash,
        trusted: from.trusted,
        installed_at,
        vetted_at,
        r#for,
        r#try,
        verdict,
    };
    let lock_dir = target.lock_dir();
    let record = |locked| {
        Lock::load(&lock_dir).and_then(|mut lock| {
            lock.skills.push(locked);
            lock.save(&lock_dir)
        })
    };
    // A store skill joins the homes' links; a project skill gets its own
    // links in the project skill dirs, and the homes' plan for its store is empty.
    let mut with = inv.clone();
    with.skills.insert(name.clone());
    with.known.insert(name.clone());
    with.origins.insert(name.clone(), vec![store]);
    let (plan, errors) = target.home_sync(cfg, &with, None);
    let mut made = Vec::new();
    let recorded = if errors.is_empty() {
        let linked = links.iter().try_for_each(|(link, target)| {
            let parent = link.parent().expect("a project link has a parent");
            fs::create_dir_all(parent).map_err(|e| err(parent, e))?;
            std::os::unix::fs::symlink(target, link).map_err(|e| err(link, e))?;
            made.push(link);
            Ok(())
        });
        linked.and_then(|()| record(locked))
    } else {
        Err("add refused; nothing was installed".into())
    };
    if let Err(e) = recorded {
        for link in &made {
            fs::remove_file(link).map_err(|e| err(link, e))?;
        }
        fs::remove_dir_all(&to).map_err(|e| err(&to, e))?;
        for e in &errors {
            out.error(e);
        }
        return Err(e);
    }
    // Recorded: the next skill of this add plans its links with this one.
    *inv = with;
    out.line(&format!("installed {}", to.display()));
    for link in made {
        out.line(&format!("linked    {}", link.display()));
    }
    for line in links::apply(&plan)? {
        out.line(&line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::unclaimed;
    use crate::Error;

    #[test]
    fn a_name_claimed_by_a_dir_or_a_dangling_link_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, link) = (tmp.path().join("dir"), tmp.path().join("link"));
        assert!(unclaimed([dir.clone(), link.clone()]).is_ok());
        std::os::unix::fs::symlink(tmp.path().join("gone"), &link).unwrap();
        std::fs::create_dir(&dir).unwrap();
        for (paths, taken) in [([link.clone(), dir.clone()], &link), ([tmp.path().join("free"), dir.clone()], &dir)] {
            match unclaimed(paths) {
                Err(Error::Usage(e)) => assert!(e.contains(&format!("{} already exists", taken.display())), "{e}"),
                other => panic!("{other:?}"),
            }
        }
    }
}
