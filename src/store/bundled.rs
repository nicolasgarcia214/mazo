//! The skills bundled under mazo's own `skills/`: installed by `mazo init`
//! at the tag of the running version, and re-pinned to it by `mazo update`.

use std::path::Path;
use std::process::ExitCode;

use super::source::{Remote, Source, is_commit};
use super::{Add, Locked, add, inventory};
use crate::config::Config;
use crate::inventory::Inventory;
use crate::{Error, Result, outln};

/// The skills `mazo init` installs, and whose absence the TUI's top bar names.
const BUNDLED: [&str; 2] = ["grade", "retro"];

/// mazo's repository, the only source installed trusted without a trust entry.
const REPO: &str = "https://github.com/nicolasgarcia214/mazo";

/// The bundled skills that neither the source nor the store holds.
pub fn missing(inv: &Inventory) -> Vec<&'static str> {
    BUNDLED.into_iter().filter(|s| !inv.skills.contains(*s)).collect()
}

/// `v<version>` of the running mazo.
pub(super) fn tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn repo() -> Remote {
    match Source::git(REPO) {
        Ok(Source::Git(remote)) => remote,
        other => unreachable!("{REPO}: {other:?}"),
    }
}

/// Whether the remote has [`tag`], and the commit its default branch is at.
pub(super) fn listed(remote: &Remote, cancel: &dyn Fn() -> bool) -> Result<(bool, Option<String>)> {
    let tag = format!("refs/tags/{}", tag());
    let listed = remote.ls_remote(&[], &["HEAD", &tag], cancel)?;
    let head = listed.iter().find(|(_, name)| name == "HEAD").map(|(commit, _)| commit.clone());
    Ok((listed.iter().any(|(_, name)| *name == tag), head))
}

/// `mazo init`'s install of every skill under `skills/` of [`REPO`] through
/// `mazo add --all --trust`, at [`tag`], or at the commit the default
/// branch is at, with a warning, when the remote lacks that tag. The trust
/// is this add's alone: no trust entry is written. A bundled skill that is
/// already installed is skipped and the rest picked one by one. A failure
/// prints the `mazo add` that retries it, and exits 1.
pub fn install(cfg: &Config, inv: &Inventory, data: &Path) -> Result<ExitCode> {
    let missing = missing(inv);
    for name in BUNDLED.iter().filter(|n| !missing.contains(n)) {
        outln!("{name}: already installed; skipped");
    }
    if missing.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    let mut r#ref = tag();
    let failed = |r#ref: &str, e: Option<Error>| {
        if let Some(e) = e {
            eprintln!("ERROR: {e}");
        }
        eprintln!("install them again with: mazo add {REPO}/tree/{}/skills --all --trust", r#ref);
        Ok(ExitCode::from(1))
    };
    match listed(&repo(), &|| false) {
        Ok((true, _)) => {}
        Ok((false, Some(head))) => {
            eprintln!("warning: {REPO} has no tag {}; installing the default branch's commit {head}", r#ref);
            r#ref = head;
        }
        Ok((false, None)) => return failed(&r#ref, Some(format!("{REPO}: no tag {} and no HEAD", r#ref).into())),
        Err(e) => return failed(&r#ref, Some(e)),
    }
    let source = format!("{REPO}/tree/{}/skills", r#ref);
    // Every skill when none is installed, else each missing one.
    let picks = if missing.len() == BUNDLED.len() { vec![None] } else { missing.into_iter().map(Some).collect() };
    let mut ok = true;
    for pick in picks {
        let args = Add {
            source: &source,
            rename: None,
            pick,
            all: pick.is_none(),
            trust: true,
            yes: true,
            force: false,
            project: false,
            pane: None,
            asks: None,
        };
        // Re-read, so that each add plans its links with the one before.
        let added = inventory(cfg.source.as_deref(), data).and_then(|inv| add(cfg, &inv, data, &args));
        match added {
            Ok(code) => ok &= code == ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("ERROR: {e}");
                ok = false;
            }
        }
    }
    if ok { Ok(ExitCode::SUCCESS) } else { failed(&r#ref, None) }
}

/// Whether `l` is a bundled entry: one from [`REPO`]'s `skills/`.
fn bundled(l: &Locked) -> bool {
    l.origin.strip_suffix(".git").unwrap_or(&l.origin) == REPO && l.subpath.starts_with("skills/")
}

/// Whether `mazo update` moves `l` to [`tag`] once the remote has it: a
/// bundled entry at a commit, or at a `v<major>.<minor>.<patch>` tag below
/// the running version. A branch, any other tag, and no ref stay.
pub(super) fn behind(l: &Locked) -> bool {
    let Some(r#ref) = l.r#ref.as_deref().filter(|_| bundled(l)) else { return false };
    is_commit(r#ref) || matches!((semver(r#ref), semver(&tag())), (Some(at), Some(running)) if at < running)
}

/// `v<major>.<minor>.<patch>`'s three numbers.
fn semver(tag: &str) -> Option<(u64, u64, u64)> {
    let number = |p: &str| if p.bytes().all(|b| b.is_ascii_digit()) { p.parse().ok() } else { None };
    let mut parts = tag.strip_prefix('v')?.split('.').map(number);
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::{behind, semver, tag};
    use crate::store::Locked;

    fn entry(origin: &str, subpath: &str, r#ref: Option<&str>) -> Locked {
        Locked {
            name: "grade".into(),
            origin: origin.into(),
            subpath: subpath.into(),
            r#ref: r#ref.map(String::from),
            commit: None,
            content_hash: "h".into(),
            trusted: true,
            installed_at: "2026-01-01T00:00:00Z".into(),
            vetted_at: None,
            r#for: None,
            r#try: vec![],
            verdict: None,
        }
    }

    #[test]
    fn only_a_bundled_commit_or_older_semver_tag_is_behind() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let repo = "https://github.com/nicolasgarcia214/mazo";
        for origin in [repo, &format!("{repo}.git")] {
            assert!(behind(&entry(origin, "skills/grade", Some(sha))), "{origin}");
            assert!(behind(&entry(origin, "skills/grade", Some("v0.0.9"))), "{origin}");
        }
        for (origin, subpath, r#ref) in [
            (repo, "skills/grade", None),
            (repo, "skills/grade", Some("master")),
            (repo, "skills/grade", Some("v1")),
            (repo, "skills/grade", Some("v99.0.0")),
            (repo, "skills/grade", Some(tag().as_str())),
            (repo, "other/grade", Some(sha)),
            ("https://github.com/someone/mazo.git", "skills/grade", Some(sha)),
        ] {
            assert!(!behind(&entry(origin, subpath, r#ref)), "{origin} {subpath} {ref:?}");
        }
    }

    #[test]
    fn semver_reads_three_numbers_only() {
        assert_eq!(semver("v1.2.30"), Some((1, 2, 30)));
        for tag in ["1.2.3", "v1", "v1.2", "v1.2.3.4", "v1.2.3-rc1", "v+1.2.3", "v1..3"] {
            assert_eq!(semver(tag), None, "{tag}");
        }
    }
}
