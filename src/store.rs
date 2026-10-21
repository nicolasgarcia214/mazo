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

/// `mazo add <path> [--as <name>] [--trust]`: copies the skill directory at
/// `path` into the store, records it in the lockfile and links it into every
/// home. A bad skill, a name taken in any root, or an untrusted source is a
/// usage error, and a sync that would fail with the skill installed is a
/// refusal; either way the store and the lockfile are left as they were.
pub fn add(
    cfg: &Config,
    inv: &Inventory,
    data: &Path,
    path: &Path,
    rename: Option<&str>,
    trust: bool,
) -> Result<ExitCode> {
    let usage = |e: String| Error::Usage(format!("add {}: {e}", path.display()));
    let src = fs::canonicalize(path).map_err(|e| usage(e.to_string()))?;
    let skill_md = fs::read_to_string(src.join("SKILL.md")).map_err(|e| usage(format!("SKILL.md: {e}")))?;
    let name = match rename {
        Some(n) => n.to_string(),
        None => src.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| usage("no name".into()))?,
    };
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
    if !trust {
        return Err(Error::Usage(format!(
            "add refused: {} is not on the trust list and vetting is not available yet; \
             pass --trust to install it without vetting",
            src.display()
        )));
    }

    let installed_at = jiff::Timestamp::now().round(jiff::Unit::Second).map_err(|e| e.to_string())?.to_string();
    let _lock = ledger::lock(data)?;
    // Copied under a hidden name, which no root lists, then renamed into place.
    let staged = store.join(format!(".add-{name}"));
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
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
        let origin = src.to_string_lossy().into_owned();
        let locked = Locked {
            name,
            origin,
            subpath: ".".into(),
            commit: None,
            content_hash,
            trusted: trust,
            installed_at,
            verdict: None,
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
    use super::{content_hash, renamed};

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
