//! Lifecycle of a skill in the source: `prune` (SPEC F17).

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use crate::Result;
use crate::inventory::git;

/// `skima prune [<name>] [--yes] [--force]` (SPEC F17, D21): permanently deletes
/// `<source>/.archive/<name>`, or every archived entry. Exit 2 for a usage error,
/// 1 for a refusal; a refusal or a declined confirmation deletes nothing.
pub fn prune(source: &Path, name: Option<&str>, yes: bool, force: bool, tty: bool) -> Result<ExitCode> {
    let archive = source.join(".archive");
    let names = match name {
        Some(n) if is_entry(&archive, n) => vec![n.to_string()],
        Some(n) => return usage(&format!("{n} is not in {}", archive.display())),
        None => entries(&archive),
    };
    if !yes && !tty {
        return usage("prune deletes permanently; pass --yes when stdin is not a terminal");
    }
    if names.is_empty() {
        println!("nothing to prune in {}", archive.display());
        return Ok(ExitCode::SUCCESS);
    }
    if !force {
        let unsaved = unsaved(source, &names)?;
        if !unsaved.is_empty() {
            for f in &unsaved {
                eprintln!("{f}: content is in no HEAD path");
            }
            eprintln!("ERROR: prune refused: that work exists nowhere else; commit it or pass --force");
            return Ok(ExitCode::from(1));
        }
    }
    for n in &names {
        println!("delete {}", archive.join(n).display());
    }
    if !yes {
        eprint!("delete {} archived skill(s) permanently? [y/N] ", names.len());
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("stdin: {e}"))?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            return Ok(ExitCode::from(1));
        }
    }
    for n in &names {
        let dir = archive.join(n);
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn usage(msg: &str) -> Result<ExitCode> {
    eprintln!("ERROR: {msg}");
    Ok(ExitCode::from(2))
}

/// A single plain component naming a directory in the archive.
fn is_entry(archive: &Path, name: &str) -> bool {
    matches!(Path::new(name).components().collect::<Vec<_>>()[..], [Component::Normal(_)])
        && archive.join(name).symlink_metadata().is_ok_and(|m| m.is_dir())
}

fn entries(archive: &Path) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(archive) else { return Vec::new() };
    let mut names: Vec<String> = dir
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Archived files, relative to the source, whose blob (`git hash-object`, which
/// ignores `.gitignore`) equals neither `HEAD:./<name>/<file>` nor
/// `HEAD:./.archive/<name>/<file>`. Empty outside a git repository.
fn unsaved(source: &Path, names: &[String]) -> Result<Vec<String>> {
    if git(source, &["rev-parse", "--is-inside-work-tree"]).is_none() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for n in names {
        walk(source, &source.join(".archive").join(n), &mut files)?;
    }
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["hash-object", "--"];
    args.extend(files.iter().map(String::as_str));
    let hashes = git(source, &args).ok_or("git hash-object failed")?;
    let mut ls = vec!["ls-tree", "-r", "-z", "HEAD", "--"];
    let roots: Vec<String> = names.iter().flat_map(|n| [n.clone(), format!(".archive/{n}")]).collect();
    ls.extend(roots.iter().map(String::as_str));
    // No HEAD (no commit yet): nothing is saved.
    let tree = git(source, &ls).unwrap_or_default();
    let head: BTreeMap<&str, &str> = tree
        .split('\0')
        .filter_map(|rec| {
            let (meta, path) = rec.split_once('\t')?;
            Some((path, meta.split(' ').nth(2)?))
        })
        .collect();
    Ok(files
        .iter()
        .zip(hashes.lines())
        .filter(|(f, hash)| {
            let live = f.strip_prefix(".archive/").unwrap_or(f);
            head.get(f.as_str()) != Some(hash) && head.get(live) != Some(hash)
        })
        .map(|(f, _)| f.clone())
        .collect())
}

/// Every non-directory under `dir`, as a path relative to `source`.
fn walk(source: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            walk(source, &p, out)?;
        } else {
            out.push(p.strip_prefix(source).unwrap_or(&p).to_string_lossy().into_owned());
        }
    }
    Ok(())
}
