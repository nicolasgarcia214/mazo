//! `mazo update`: each skill re-cloned, re-vetted on its diff when its
//! source is untrusted, and swapped in under the data lock.

use super::ask::{Consent, Io};
use super::bundled;
use super::lock::{checked, now, resolved, stage};
use super::source::{Remote, Scratch};
use super::vet::{gate, no_judge};
use super::{Lock, Locked, Outcome, Target, content_hash, inventory, project_hint, project_root, scoped_inventory};
use crate::audit::{self, Backend, Caller, Vet};
use crate::config::{Config, Home, Judge};
use crate::inventory::Inventory;
use crate::{Error, Result};
use crate::{ledger, links};
use std::cell::OnceCell;
use std::fs;
use std::path::{Path, PathBuf};

/// `mazo update`'s arguments.
pub struct Update<'a> {
    /// The store skills to update; every one when empty.
    pub names: &'a [String],
    /// Accept a re-vetted update without asking.
    pub yes: bool,
    /// Apply an update with a security finding.
    pub force: bool,
    /// Update the project skills at the current directory's git root.
    pub project: bool,
    /// Where the update reports and asks.
    pub io: &'a dyn Io,
}

/// `mazo update [names…] [--yes] [--force]`: re-clones each git-sourced
/// store skill (every one when `names` is empty) and, when the copy at its
/// subpath hashes differently, swaps it into the store and records the new
/// commit and hash. A skill whose source is neither on the `trusted` list
/// nor [`bundled::bundled`], whatever its lock entry says, is first re-vetted
/// by the judge on the diff from the installed copy ([`gate`]), and refused
/// (exit 2, with [`no_judge`] once) when no judge is configured; a local-path one is
/// not updatable. A skill that fails or is refused is left as it was and the
/// rest still run; the outcome is the worst of the failures', a usage one
/// for no judge. A name that is not a store skill is a usage error.
/// With `--project` it updates the project's skills and lockfile instead, the
/// judge seeing the project's skills too; their links, relative to the
/// project store, need no change. A bundled skill [`bundled::behind`] the
/// running version moves to its tag, unvetted, once the remote has that tag.
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
    // Whether the remote has the running version's tag, asked once.
    let tagged = OnceCell::new();
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
                outcome = outcome.max(Outcome::Failed);
                continue;
            }
        };
        let repin = bundled::behind(l)
            && match tagged.get_or_init(|| bundled::listed(&remote, &|| out.cancelled()).map_err(|e| e.to_string())) {
                Ok((tagged, _)) => *tagged,
                Err(e) => {
                    out.error(&format!("ERROR: update {}: {e}", l.name));
                    outcome = outcome.max(Outcome::Failed);
                    continue;
                }
            };
        if repin {
            remote.r#ref = Some(bundled::tag());
        }
        let vet = match &revet {
            // Re-derived on each run: a lockfile, a project's anyone's commit, grants nothing,
            // and no project holds a bundled skill.
            _ if (project.is_none() && bundled::bundled(l)) || remote.trusted_by(&cfg.trusted) => None,
            None => {
                out.error(&format!(
                    "ERROR: update {}: update refused: {} is not trusted and re-vetting it needs a [judge], \
                     which the config lacks; add it to trusted to update it without vetting",
                    l.name, l.origin
                ));
                (unjudged, outcome) = (true, Outcome::Usage);
                continue;
            }
            Some(r) => Some(r),
        };
        remote.subpath = if l.subpath == "." { PathBuf::new() } else { l.subpath.clone().into() };
        match refresh(cfg, inv, target, l, &remote, vet, out) {
            Ok(Some(commit)) => {
                let at = if repin { format!("{} ({commit})", bundled::tag()) } else { commit };
                out.line(&format!("updated {} to {at}", target.store().join(&l.name).display()));
                updated = true;
            }
            Ok(None) => out.line(&format!("{}: up to date", l.name)),
            Err(e) => {
                out.error(&format!("ERROR: update {}: {e}", l.name));
                outcome = outcome.max(Outcome::Failed);
            }
        }
    }
    // A project's skills are linked relative to its store, so need no sync.
    if updated && target.project.is_none() {
        // Under the lock, on the inventory read under it, as `mazo sync` does.
        let _lock = ledger::lock(data)?;
        let (plan, errors) = target.home_sync(cfg, &inventory(cfg.source.as_deref(), data)?, None);
        for e in &errors {
            out.error(e);
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
        out.error(&no_judge());
    }
    Ok(outcome)
}

/// Clones `remote` into `<data>/.clone-<pid>` and, when the skill at its
/// subpath hashes unlike `l` or `remote` has another ref than `l`, passes it
/// through the `vet` gate if there is one, then swaps the copy in for
/// `<store>/<name>` of `target` under the data lock and records the ref,
/// commit and hash, and any new verdict, in its lockfile: the new commit, or
/// `None` when up to date. A refused vet, a
/// sync that would fail with the new copy (for the data directory's store),
/// or any failed write leaves the old copy and the lockfile as they were;
/// the sync's errors go to `out`. A cancel stops it before the swap, and a
/// copy an interrupted swap set aside is put back first ([`recover`]).
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
        let _lock = ledger::lock(data)?;
        recover(&to, &old).map_err(crate::at(&old))?;
    }
    let (scratch, commit) = remote.clone_in(data, &|| out.cancelled())?;
    let clone = scratch.0.as_path();
    let src = resolved(&clone.join(&remote.subpath), Some(clone))?;
    let (skill_md, files) = checked(&src, &l.name, l.name != remote.name())?;

    // Staged under a name of this process's own, so that the judge and the
    // y/N prompt run without the data lock.
    let staged = Scratch::new(&store, &format!(".update-{}", l.name));
    let (staged, content_hash) = stage(&src, &files, &skill_md, staged)?;
    if content_hash == l.content_hash && remote.r#ref == l.r#ref {
        return Ok(None);
    }
    let vetted = match vet {
        Some(r) => Some((r.vet(inv, &l.name, &skill_md, &store, &staged.0)?, now()?)),
        None => None,
    };

    let _lock = ledger::lock(data)?;
    // The judge saw the diff from the copy then installed, and ran unlocked.
    if let Some(((_, diffed), _)) = &vetted
        && super::content_hash(&to)? != *diffed
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
    recover(&to, &old).map_err(crate::at(&old))?;
    // Read under the lock, so a concurrent `retire` or `add` cannot stale the check.
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
            out.error(e);
        }
        // A failed undo is the error to report, after why it ran.
        if let Err(c) = fs::remove_dir_all(&to).and_then(|()| fs::rename(&old, &to)) {
            let why = if errors.is_empty() { e.to_string() } else { "the sync refused it".into() };
            return Err(format!("the update failed ({why}), and undoing it failed: {}", crate::at(&to)(c)).into());
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
