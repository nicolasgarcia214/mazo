//! Lifecycle of a skill in the source: `new`, `retire`, `prune`.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::config::{Config, Home};
use crate::inventory::{Inventory, git};
use crate::ledger::{self, Agent, Ledger};
use crate::{Error, Result};
use crate::{links, policy, report, scan, store, yaml};

/// `mazo new <name> --description <text>`: writes
/// `<source>/<name>/SKILL.md`, and `agents/openai.yaml` under `codex_manifest`,
/// then prints the `name-only` line for each Claude home under `claude_name_only`.
/// A name or description the always-on rules reject is a usage error; an
/// existing entry, or a store skill of that name, is a refusal. It never runs `sync`.
pub fn new(cfg: &Config, inv: &Inventory, name: &str, description: &str) -> Result<ExitCode> {
    let skill_md = format!("---\nname: {name}\ndescription: {}\n---\n\n# {name}\n", quoted(description));
    // Parsed back through the rules `sync` runs, so what is written passes them.
    let checked = yaml::frontmatter(&skill_md).and_then(|meta| {
        policy::always_on(&meta, name)?;
        match yaml::get(&meta, "description").and_then(yaml::string) {
            Some(d) if d == description => Ok(()),
            _ => Err("the description does not survive as a YAML string".into()),
        }
    });
    checked.map_err(|e| Error::Usage(format!("new {name}: {e}")))?;
    let source = cfg.need_source("new")?;
    if let Some(root) = inv.root(name).filter(|r| *r != source) {
        return Err(format!("new refused: {} already exists", root.join(name).display()).into());
    }
    let dir = source.join(name);
    match std::fs::create_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!("new refused: {} already exists", dir.display()).into());
        }
        r => r.map_err(|e| format!("{}: {e}", dir.display()))?,
    }
    let mut files = vec![("SKILL.md", skill_md)];
    if cfg.policy.codex_manifest {
        let manifest =
            format!("interface:\n  default_prompt: \"Use ${name}.\"\n\npolicy:\n  allow_implicit_invocation: false\n");
        files.push(("agents/openai.yaml", manifest));
    }
    for (rel, text) in files {
        let path = dir.join(rel);
        let parent = path.parent().unwrap_or(&dir);
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("wrote {}", path.display());
    }
    if cfg.policy.claude_name_only {
        for h in cfg.homes.iter().filter(|h| h.agent == Agent::Claude) {
            println!("{}: add to skillOverrides: \"{name}\": \"name-only\"", h.settings().display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// A YAML double-quoted scalar: `"` and `\` escaped, and every control or
/// line-separating character as `\uXXXX`, so the value stays on one line.
fn quoted(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{feff}') => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `mazo prune [<name>] [--yes] [--force]`: permanently deletes
/// `<source>/.archive/<name>`, or every archived entry. A refusal or a
/// declined confirmation deletes nothing and exits 1.
pub fn prune(source: &Path, name: Option<&str>, yes: bool, force: bool, tty: bool) -> Result<ExitCode> {
    let archive = source.join(".archive");
    let names = match name {
        Some(n) if is_entry(&archive, n) => vec![n.to_string()],
        Some(n) => return Err(Error::Usage(format!("{n} is not in {}", archive.display()))),
        None => entries(&archive),
    };
    if !yes && !tty {
        return Err(Error::Usage("prune deletes permanently; pass --yes when stdin is not a terminal".into()));
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
            return Err("prune refused: that work exists nowhere else; commit it or pass --force".into());
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
/// `HEAD:./.archive/<name>/<file>`. A symlink, dangling or not, is the blob of
/// its link text and matches only a `120000` entry. Empty outside a git repository.
fn unsaved(source: &Path, names: &[String]) -> Result<Vec<String>> {
    if git(source, &["rev-parse", "--is-inside-work-tree"]).is_none() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for n in names {
        walk(source, &source.join(".archive").join(n), &mut files)?;
    }
    let is_link = |f: &str| source.join(f).symlink_metadata().is_ok_and(|m| m.is_symlink());
    let regular: Vec<&str> = files.iter().map(String::as_str).filter(|f| !is_link(f)).collect();
    let hashes = if regular.is_empty() {
        String::new()
    } else {
        let mut args = vec!["hash-object", "--"];
        args.extend(&regular);
        git(source, &args).ok_or("git hash-object failed")?
    };
    let mut hashes = hashes.lines();
    let mut ls = vec!["ls-tree", "-r", "-z", "HEAD", "--"];
    let roots: Vec<String> = names.iter().flat_map(|n| [n.clone(), format!(".archive/{n}")]).collect();
    ls.extend(roots.iter().map(String::as_str));
    // No HEAD (no commit yet): nothing is saved.
    let tree = git(source, &ls).unwrap_or_default();
    let head: BTreeMap<&str, (&str, &str)> = tree
        .split('\0')
        .filter_map(|rec| {
            let (meta, path) = rec.split_once('\t')?;
            let mut meta = meta.split(' ');
            Some((path, (meta.next()?, meta.nth(1)?)))
        })
        .collect();
    let mut unsaved = Vec::new();
    for f in &files {
        let paths = [f.as_str(), f.strip_prefix(".archive/").unwrap_or(f)];
        let saved = if is_link(f) {
            let blob = link_blob(source, f)?;
            paths.iter().any(|p| head.get(p) == Some(&("120000", blob.as_str())))
        } else {
            let hash = hashes.next().ok_or("git hash-object failed")?;
            paths.iter().any(|p| head.get(p).is_some_and(|&(_, h)| h == hash))
        };
        if !saved {
            unsaved.push(f.clone());
        }
    }
    Ok(unsaved)
}

/// The blob git stores for the symlink `<source>/<rel>`: `git hash-object
/// --stdin` of its link text.
fn link_blob(source: &Path, rel: &str) -> Result<String> {
    let path = source.join(rel);
    let target = std::fs::read_link(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fail = |e: std::io::Error| format!("git hash-object --stdin: {e}");
    let mut child = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(source)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(fail)?;
    child.stdin.take().ok_or("git hash-object: no stdin")?.write_all(target.as_os_str().as_bytes()).map_err(fail)?;
    let out = child.wait_with_output().map_err(fail)?;
    if !out.status.success() {
        return Err("git hash-object --stdin failed".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
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

/// A strong event in this many days, the skill's, a holder's or a namer's, makes `retire` refuse.
const RETIRE_DAYS: u32 = 30;

/// What [`retire`] did. Nothing is written unless it is `Done`.
pub enum Retired {
    /// Why it refused without `force`: a strong event in the last 30 days of
    /// the skill, a holder or a namer.
    Blocked(Vec<String>),
    /// The errors of the sync plan without the skill.
    Refused(Vec<String>),
    /// The lines reporting what it wrote.
    Done(Vec<String>),
}

/// `mazo retire <name> [--force]`: scans first, refuses a held, named or
/// recently used skill unless `force`, and refuses any conflict or policy error
/// in the sync plan as if the skill were gone, all before any write. Then
/// moves a source skill `<source>/<name>` to `<source>/.archive/<name>` (`git
/// mv` when tracked) and applies that plan; or, for a store skill, applies
/// that plan, drops its lockfile entry and deletes `<store>/<name>`. With
/// `project`, it retires only a skill of the project at the current
/// directory's git root, its blockers found with the project's skills: its
/// links in the project skill dirs go instead of the homes', then its
/// project lockfile entry and directory. Never commits. Prints nothing: the
/// scan's warnings go to `warnings`, kept even when a later step fails.
pub fn retire(
    cfg: &Config,
    inv: &Inventory,
    data: &Path,
    name: &str,
    force: bool,
    project: bool,
    warnings: &mut Vec<String>,
) -> Result<Retired> {
    let project = if project { Some(store::project_root("retire")?) } else { None };
    let with_project;
    let inv = match &project {
        Some(p) => {
            with_project = store::project_inventory(inv, p)?;
            &with_project
        }
        None => inv,
    };
    if let Some(p) = &project
        && inv.root(name) != Some(store::project_dir(p).as_path())
    {
        return Err(Error::Usage(format!("{name} is not a project skill")));
    }
    let Some(root) = inv.root(name) else {
        let hint = store::project_hint(name);
        // Only a store skill retires without a source.
        if hint.is_empty() {
            cfg.need_source(&format!("retire {name}"))?;
        }
        return Err(Error::Usage(format!("{name} is not a canonical skill{hint}")));
    };
    let from = root.join(name);
    // A source skill is archived; a store skill is removed.
    let to = (cfg.source.as_deref() == Some(root)).then(|| root.join(".archive").join(name));
    if let Some(to) = to.as_ref().filter(|t| t.symlink_metadata().is_ok()) {
        return Err(format!("retire refused: {} already exists", to.display()).into());
    }
    warnings.extend(scan::scan(cfg, inv, data, false)?.warnings());
    if !force {
        let ledger = Ledger::load(data)?;
        let coverage = ledger::load_coverage(data)?;
        let now = jiff::Timestamp::now();
        let tz = jiff::tz::TimeZone::system();
        let input =
            report::Input { cfg, inv, events: ledger.events(), coverage: &coverage, audits: &BTreeMap::new(), now, tz };
        let blockers = report::retire_blockers(&input, name, RETIRE_DAYS);
        if !blockers.is_empty() {
            return Ok(Retired::Blocked(blockers));
        }
    }
    // Held from the plan on, as `add` holds it, so a concurrent `add` cannot stale the plan.
    let _lock = if to.is_some() { None } else { Some(ledger::lock(data)?) };
    let mut out = Vec::new();
    if let Some(p) = &project {
        // The homes link no project skill; the same order as a store skill's.
        out.extend(store::unlink_project(p, name)?);
        store::Lock::remove(p, name)?;
        std::fs::remove_dir_all(&from).map_err(|e| format!("{}: {e}", from.display()))?;
        out.push(format!("removed   {}", from.display()));
        return Ok(Retired::Done(out));
    }
    let mut rest = inv.clone();
    rest.skills.remove(name);
    rest.origins.remove(name);
    let homes: Vec<&Home> = cfg.homes.iter().collect();
    let plan = links::plan(cfg, &rest, &homes, Some(&from));
    let errors = links::sync_errors(cfg, &rest, &homes, &plan);
    if !errors.is_empty() {
        return Ok(Retired::Refused(errors));
    }

    if let Some(to) = &to {
        let archive = root.join(".archive");
        std::fs::create_dir_all(&archive).map_err(|e| format!("{}: {e}", archive.display()))?;
        if git(root, &["ls-files", "--", name]).is_some_and(|o| !o.is_empty()) {
            git(root, &["mv", "--", name, &format!(".archive/{name}")]).ok_or(format!("git mv {name} failed"))?;
        } else {
            std::fs::rename(&from, to).map_err(|e| format!("{}: {e}", from.display()))?;
        }
        out.push(format!("archived  {} -> {}", from.display(), to.display()));
        out.extend(links::apply(&plan)?);
    } else {
        // Links first, then the lock entry, then the directory: a failure
        // leaves at worst an unrecorded store skill, never an entry for a
        // missing one.
        out.extend(links::apply(&plan)?);
        store::Lock::remove(data, name)?;
        std::fs::remove_dir_all(&from).map_err(|e| format!("{}: {e}", from.display()))?;
        out.push(format!("removed   {}", from.display()));
    }
    if cfg.policy.claude_name_only {
        for h in cfg.homes.iter().filter(|h| h.agent == Agent::Claude) {
            if let Some(v) = policy::settings(h).ok().and_then(|s| s["skillOverrides"].get(name).cloned()) {
                out.push(format!("{} can drop:\n  \"{name}\": {v}", h.settings().display()));
            }
        }
    }
    Ok(Retired::Done(out))
}
