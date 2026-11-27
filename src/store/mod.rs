mod ask;
mod bundled;
mod lock;
mod project;
mod source;
mod update;
mod vet;
mod work;

use std::fs;
use std::path::{Path, PathBuf};

use crate::audit::{self, Caller};
use crate::config::{self, Config, Home};
use crate::inventory::Inventory;
use crate::render::{Diag, PathExt};
use crate::{Error, Result};
use crate::{durable, links};

pub use ask::{Io, Terminal};
pub use bundled::{install as install_bundled, missing as missing_bundled};
pub use lock::{Lock, Locked, content_hash};
pub use project::{Project, project_dir, project_hint, project_root, scoped_inventory};
pub use source::github_repo;
pub use update::{Update, update};

use ask::{Consent, confirm_calls, name as skill_name, skills_in};
use lock::{checked, now, resolved, stage};
use project::{project_links, unlink_project};
use source::{Remote, Scratch, Source, local};
use vet::{contents, gate, no_judge};

pub fn dir(data: &Path) -> PathBuf {
    data.join("store")
}

pub fn inventory(source: Option<&Path>, data: &Path) -> Result<Inventory> {
    let store = dir(data);
    let store = fs::canonicalize(&store).unwrap_or(store);
    let roots: Vec<&Path> = source.into_iter().chain([store.as_path()]).collect();
    let mut inv = Inventory::load_roots(source, &roots)?;
    inv.installed = Lock::installed(data)?;
    Ok(inv)
}

pub struct Add<'a> {
    pub source: &'a str,
    pub rename: Option<&'a str>,
    pub pick: Option<&'a str>,
    pub all: bool,
    pub trust: bool,
    pub yes: bool,
    pub force: bool,
    pub project: bool,
    pub io: &'a dyn Io,
}

impl Add<'_> {
    fn consent(&self) -> Consent<'_> {
        Consent { update: false, yes: self.yes, force: self.force, io: self.io }
    }
}

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

pub fn precheck(cfg: &Config, arg: &str) -> Result<()> {
    resolve(cfg, arg, false).map(drop)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Done,
    Failed,
    Usage,
}

struct Fetched<'a> {
    root: PathBuf,
    origin: String,
    subpath: PathBuf,
    r#ref: Option<String>,
    commit: Option<String>,
    own: String,
    clone: Option<&'a Path>,
    trusted: bool,
}

pub fn add(cfg: &Config, inv: &Inventory, data: &Path, args: &Add) -> Result<Outcome> {
    let project = if args.project { Some(project_root("add")?) } else { None };
    let (arg, out) = (args.source, args.io);
    let usage = |e: String| Error::Usage(format!("add {arg}: {e}"));
    let (source, trusted) = resolve(cfg, arg, args.trust)?;
    let backend = match &cfg.judge {
        Some(judge) if !trusted => Some((judge, audit::backend(judge, &cfg.homes)?)),
        _ => None,
    };
    let caller =
        backend.as_ref().map(|(judge, backend)| Caller { data, judge, backend: backend.as_ref(), pane: out.pane() });
    if let Some(c) = &caller {
        if !args.all {
            args.consent().answerable().map_err(Error::Usage)?;
        }
        c.login()?;
    }
    let mut scratch = None;
    let from = match source {
        Source::Local(path) => {
            let root = fs::canonicalize(local(&path, &config::home_dir()?)?).map_err(|e| usage(e.to_string()))?;
            let own =
                root.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| usage("no name".into()))?;
            let origin = root.to_string_lossy().into_owned();
            Fetched { root, origin, subpath: PathBuf::new(), r#ref: None, commit: None, own, clone: None, trusted }
        }
        Source::Git(mut remote) => {
            remote.resolve(&|| out.cancelled())?;
            let (clone, commit) = remote.clone_in(data, &|| out.cancelled())?;
            let (clone, commit) = (&scratch.insert(clone).0, Some(commit));
            let own = remote.name();
            let Remote { url: origin, subpath, r#ref, .. } = remote;
            Fetched { root: clone.join(&subpath), origin, subpath, r#ref, commit, own, clone: Some(clone), trusted }
        }
    };
    let children = skills_in(&from.root, &from.own, args)?;
    if children.is_empty() {
        out.line("nothing selected; nothing was installed");
    }
    if caller.is_some() && !children.is_empty() && (args.all || out.confirms_calls()) {
        confirm_calls(children.len(), args.consent())?;
    }
    let mut inv = scoped_inventory(inv.clone(), project.as_deref())?;
    let target = Target { data, project: project.as_deref() };
    let mut outcome = Outcome::Done;
    for child in children {
        if out.cancelled() {
            return Err(Error::Cancelled);
        }
        if let Err(e) = install(cfg, &mut inv, target, args, caller.as_ref(), &from, child) {
            out.error(Diag::error(e.to_string()).into());
            outcome = outcome.max(if let Error::Usage(_) = e { Outcome::Usage } else { Outcome::Failed });
        }
    }
    Ok(outcome)
}

#[derive(Clone, Copy)]
pub struct Target<'a> {
    pub data: &'a Path,
    pub project: Option<&'a Path>,
}

impl Target<'_> {
    pub fn home_sync(self, cfg: &Config, inv: &Inventory, gone: Option<&Path>) -> (links::Plan, Vec<Diag>) {
        if self.project.is_some() {
            return Default::default();
        }
        let homes: Vec<&Home> = cfg.homes.iter().collect();
        let plan = links::plan(cfg, inv, &homes, gone);
        let errors = links::sync_errors(cfg, inv, &homes, &plan);
        (plan, errors)
    }

    pub fn remove(self, from: &Path, name: &str, plan: &links::Plan) -> Result<Vec<String>> {
        let mut out = links::apply(plan)?;
        if let Some(p) = self.project {
            out.extend(unlink_project(p, name)?);
        }
        Lock::remove(&self.lock_dir(), name)?;
        fs::remove_dir_all(from).map_err(crate::at(from))?;
        out.push(format!("removed   {}", from.inert()));
        Ok(out)
    }

    fn store(self) -> PathBuf {
        self.project.map_or_else(|| dir(self.data), project_dir)
    }

    fn lock_dir(self) -> PathBuf {
        self.project.unwrap_or(self.data).to_path_buf()
    }
}

fn unclaimed(paths: impl IntoIterator<Item = PathBuf>) -> Result<()> {
    match paths.into_iter().find(|p| p.symlink_metadata().is_ok()) {
        Some(p) => Err(Error::Usage(format!(
            "add refused: {} already exists; pass --as <name> to install it under another name",
            p.inert()
        ))),
        None => Ok(()),
    }
}

struct Fresh(Vec<PathBuf>);

impl Drop for Fresh {
    fn drop(&mut self) {
        for dir in &self.0 {
            let _ = fs::remove_dir(dir);
        }
    }
}

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
    unclaimed(claimed())?;

    let _fresh = Fresh(store.ancestors().take_while(|d| !d.exists()).map(Path::to_path_buf).collect());
    let (staged, content_hash) = stage(&src, &files, &skill_md, Scratch::new(&store, &format!(".add-{name}")))?;
    let vetted = match caller {
        Some(c) => Some((gate(c, inv, args.consent(), &name, contents(&staged.0, &files)?, None)?, now()?)),
        None => None,
    };

    let installed_at = now()?;
    let _lock = durable::lock(data)?;
    unclaimed(claimed())?;
    let mut with = scoped_inventory(inventory(cfg.source.as_deref(), data)?, project)?;
    fs::rename(&staged.0, &to).map_err(crate::at(&to))?;

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
    with.skills.insert(name.clone());
    with.known.insert(name.clone());
    with.origins.insert(name.clone(), vec![store]);
    let (plan, errors) = target.home_sync(cfg, &with, None);
    let mut made = Vec::new();
    let recorded = if errors.is_empty() {
        let linked = links.iter().try_for_each(|(link, target)| {
            let parent = link.parent().expect("a project link has a parent");
            fs::create_dir_all(parent).map_err(crate::at(parent))?;
            std::os::unix::fs::symlink(target, link).map_err(crate::at(link))?;
            made.push(link);
            Ok(())
        });
        linked.and_then(|()| record(locked))
    } else {
        Err("add refused; nothing was installed".into())
    };
    if let Err(e) = recorded {
        for e in &errors {
            out.error(e.clone().into());
        }
        let undone = made
            .iter()
            .try_for_each(|link| fs::remove_file(link).map_err(crate::at(link)))
            .and_then(|()| fs::remove_dir_all(&to).map_err(crate::at(&to)));
        if let Err(c) = undone {
            let why = if errors.is_empty() { e.to_string() } else { "the sync refused it".into() };
            return Err(format!("add {arg}: the install failed ({why}), and undoing it failed: {c}").into());
        }
        return Err(e);
    }
    *inv = with;
    out.line(&format!("installed {}", to.inert()));
    for link in made {
        out.line(&format!("linked    {}", link.inert()));
    }
    for line in links::apply(&plan)? {
        out.line(&line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;

    #[test]
    fn a_name_claimed_by_a_dir_or_a_dangling_link_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, link) = (tmp.path().join("dir"), tmp.path().join("link"));
        assert!(unclaimed([dir.clone(), link.clone()]).is_ok());
        std::os::unix::fs::symlink(tmp.path().join("gone"), &link).unwrap();
        fs::create_dir(&dir).unwrap();
        for (paths, taken) in [([link.clone(), dir.clone()], &link), ([tmp.path().join("free"), dir.clone()], &dir)] {
            match unclaimed(paths) {
                Err(Error::Usage(e)) => assert!(e.contains(&format!("{} already exists", taken.display())), "{e}"),
                other => panic!("{other:?}"),
            }
        }
    }
}
