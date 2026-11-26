use super::ask::{Consent, Io};
use super::bundled;
use super::lock::{checked, now, resolved, stage};
use super::source::{Remote, Scratch};
use super::vet::{gate, no_judge};
use super::{Lock, Locked, Outcome, Target, content_hash, inventory, project_hint, project_root, scoped_inventory};
use crate::audit::{self, Backend, Caller, Vet};
use crate::config::{Config, Home, Judge};
use crate::inventory::Inventory;
use crate::render::{Diag, PathExt};
use crate::{Error, Result};
use crate::{durable, links};
use std::cell::OnceCell;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Update<'a> {
    pub names: &'a [String],
    pub yes: bool,
    pub force: bool,
    pub project: bool,
    pub io: &'a dyn Io,
}

pub fn update(cfg: &Config, inv: &Inventory, data: &Path, args: &Update) -> Result<Outcome> {
    let (names, out) = (args.names, args.io);
    let project = if args.project { Some(project_root("update")?) } else { None };
    let target = Target { data, project: project.as_deref() };
    let inv = &scoped_inventory(inv.clone(), project.as_deref())?;
    let lock = Lock::load(&target.lock_dir())?;
    if let Some(n) = names.iter().find(|n| !lock.skills.iter().any(|l| &l.name == *n)) {
        return Err(Error::Usage(match project {
            Some(_) => format!("update: not a project skill: {n}"),
            None => format!("update: not a store skill: {n}{}", project_hint(n)),
        }));
    }
    let consent = Consent { update: true, yes: args.yes, force: args.force, io: out };
    let revet =
        cfg.judge.as_ref().map(|judge| Revet { data, judge, homes: &cfg.homes, consent, backend: OnceCell::new() });
    let (mut updated, mut unjudged, mut outcome) = (false, false, Outcome::Done);
    let tagged = OnceCell::new();
    for l in lock.skills.iter().filter(|l| names.is_empty() || names.contains(&l.name)) {
        if out.cancelled() {
            return Err(Error::Cancelled);
        }
        let mut remote = match l.remote() {
            Ok(Some(remote)) => remote,
            Ok(None) => {
                out.line(&format!("{}: not updatable; installed from a local path", l.name));
                continue;
            }
            Err(e) => {
                out.error(Diag::error(format!("update {}: {e}", l.name)).into());
                outcome = outcome.max(Outcome::Failed);
                continue;
            }
        };
        let repin = bundled::behind(l)
            && match tagged.get_or_init(|| bundled::listed(&remote, &|| out.cancelled()).map_err(|e| e.to_string())) {
                Ok((tagged, _)) => *tagged,
                Err(e) => {
                    out.error(Diag::error(format!("update {}: {e}", l.name)).into());
                    outcome = outcome.max(Outcome::Failed);
                    continue;
                }
            };
        if repin {
            remote.r#ref = Some(bundled::tag());
        }
        let vet = match &revet {
            _ if (project.is_none() && bundled::bundled(l)) || remote.trusted_by(&cfg.trusted) => None,
            None => {
                out.error(
                    Diag::error(format!(
                        "update {}: update refused: {} is not trusted and re-vetting it needs a [judge], \
                         which the config lacks; add it to trusted to update it without vetting",
                        l.name, l.origin
                    ))
                    .into(),
                );
                (unjudged, outcome) = (true, Outcome::Usage);
                continue;
            }
            Some(r) => Some(r),
        };
        remote.subpath = if l.subpath == "." { PathBuf::new() } else { l.subpath.clone().into() };
        match refresh(cfg, inv, target, l, &remote, vet, out) {
            Ok(Some(commit)) => {
                let at = if repin { format!("{} ({commit})", bundled::tag()) } else { commit };
                out.line(&format!("updated {} to {at}", target.store().join(&l.name).inert()));
                updated = true;
            }
            Ok(None) => out.line(&format!("{}: up to date", l.name)),
            Err(e) => {
                out.error(Diag::error(format!("update {}: {e}", l.name)).into());
                outcome = outcome.max(Outcome::Failed);
            }
        }
    }
    if updated && target.project.is_none() {
        let _lock = durable::lock(data)?;
        let (plan, errors) = target.home_sync(cfg, &inventory(cfg.source.as_deref(), data)?, None);
        for e in &errors {
            out.error(e.clone().into());
        }
        if errors.is_empty() {
            for line in links::apply(&plan)? {
                out.line(&line);
            }
        } else {
            outcome = outcome.max(Outcome::Failed);
        }
    }
    if unjudged {
        out.error(no_judge().into());
    }
    Ok(outcome)
}

fn refresh(
    cfg: &Config,
    inv: &Inventory,
    target: Target,
    l: &Locked,
    remote: &Remote,
    vet: Option<&Revet>,
    out: &dyn Io,
) -> Result<Option<String>> {
    let (data, store, lock_dir) = (target.data, target.store(), target.lock_dir());
    let (to, old) = (store.join(&l.name), store.join(format!(".old-{}", l.name)));
    {
        let _lock = durable::lock(data)?;
        recover(&to, &old).map_err(crate::at(&old))?;
    }
    let (scratch, commit) = remote.clone_in(data, &|| out.cancelled())?;
    let clone = scratch.0.as_path();
    let src = resolved(&clone.join(&remote.subpath), Some(clone))?;
    let (skill_md, files) = checked(&src, &l.name, l.name != remote.name())?;

    let staged = Scratch::new(&store, &format!(".update-{}", l.name));
    let (staged, content_hash) = stage(&src, &files, &skill_md, staged)?;
    if content_hash == l.content_hash && remote.r#ref == l.r#ref {
        return Ok(None);
    }
    let vetted = match vet {
        Some(r) => Some((r.vet(inv, &l.name, &skill_md, &store, &staged.0)?, now()?)),
        None => None,
    };

    let _lock = durable::lock(data)?;
    if let Some(((_, diffed), _)) = &vetted
        && super::content_hash(&to)? != *diffed
    {
        return Err(format!(
            "update refused: {} changed while it was vetted; the skill was left as it was",
            to.inert()
        )
        .into());
    }
    if out.cancelled() {
        return Err(Error::Cancelled);
    }
    recover(&to, &old).map_err(crate::at(&old))?;
    let fresh = if target.project.is_none() { Some(inventory(cfg.source.as_deref(), data)?) } else { None };
    let swapped = fs::rename(&to, &old)
        .map_err(crate::at(&to))
        .and_then(|()| fs::rename(&staged.0, &to).map_err(crate::at(&to)).inspect_err(|_| drop(fs::rename(&old, &to))));
    swapped?;

    let errors = fresh.map_or_else(Vec::new, |inv| target.home_sync(cfg, &inv, None).1);
    let recorded = if errors.is_empty() {
        Lock::load(&lock_dir).and_then(|mut lock| {
            let entry = lock.skills.iter_mut().find(|s| s.name == l.name).ok_or("no longer in the lockfile")?;
            entry.r#ref.clone_from(&remote.r#ref);
            entry.commit = Some(commit.clone());
            entry.content_hash = content_hash;
            if let Some(((vet, _), at)) = vetted {
                (entry.vetted_at, entry.r#for, entry.r#try) = (Some(at), Some(vet.r#for), vet.r#try);
                entry.verdict = Some(vet.verdict);
            }
            lock.save(&lock_dir)
        })
    } else {
        Err("update refused; the skill was left as it was".into())
    };
    if let Err(e) = recorded {
        for e in &errors {
            out.error(e.clone().into());
        }
        if let Err(c) = fs::remove_dir_all(&to).and_then(|()| fs::rename(&old, &to)) {
            let why = if errors.is_empty() { e.to_string() } else { "the sync refused it".into() };
            return Err(format!("the update failed ({why}), and undoing it failed: {}", crate::at(&to)(c)).into());
        }
        return Err(e);
    }
    let _ = recover(&to, &old);
    Ok(Some(commit))
}

fn recover(to: &Path, old: &Path) -> std::io::Result<()> {
    match (old.symlink_metadata(), to.symlink_metadata()) {
        (Err(_), _) => Ok(()),
        (Ok(_), Ok(_)) => fs::remove_dir_all(old),
        (Ok(_), Err(_)) => fs::rename(old, to),
    }
}

struct Revet<'a> {
    data: &'a Path,
    judge: &'a Judge,
    homes: &'a [Home],
    consent: Consent<'a>,
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
        Caller { data: self.data, judge: self.judge, backend, pane: self.consent.io.pane() }
    }

    fn vet(&self, inv: &Inventory, name: &str, skill_md: &str, store: &Path, staged: &Path) -> Result<(Vet, String)> {
        self.consent.answerable()?;
        let c = self.caller()?;
        let diffed = content_hash(&store.join(name))?;
        let staged = staged.file_name().ok_or("no staged copy")?.to_string_lossy().into_owned();
        let out = std::process::Command::new("git")
            .current_dir(store)
            .args(["diff", "--no-index", "--text", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames"])
            .args(["--src-prefix=a/", "--dst-prefix=b/", "--", name, &staged])
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if !matches!(out.status.code(), Some(0 | 1)) {
            return Err(
                format!("git diff: {}", crate::render::inert(String::from_utf8_lossy(&out.stderr).trim())).into()
            );
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
