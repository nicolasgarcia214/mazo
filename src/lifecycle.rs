use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::config::Config;
use crate::durable;
use crate::inventory::{Inventory, git};
use crate::ledger::{self, Agent};
use crate::render::{Diag, PathExt};
use crate::{Error, Result, outln};
use crate::{links, policy, report, scan, store, yaml};

pub fn new(cfg: &Config, inv: &Inventory, name: &str, description: &str) -> Result<ExitCode> {
    let skill_md = format!("---\nname: {name}\ndescription: {}\n---\n\n# {name}\n", quoted(description));
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
        return Err(format!("new refused: {} already exists", root.join(name).inert()).into());
    }
    let dir = source.join(name);
    match fs::create_dir(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!("new refused: {} already exists", dir.inert()).into());
        }
        r => r.map_err(crate::at(&dir))?,
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
        fs::create_dir_all(parent).map_err(crate::at(parent))?;
        fs::write(&path, text).map_err(crate::at(&path))?;
        outln!("wrote {}", path.inert());
    }
    if cfg.policy.claude_name_only {
        for h in cfg.homes.iter().filter(|h| h.agent == Agent::Claude) {
            outln!("{}: add to skillOverrides: \"{name}\": \"name-only\"", h.settings().inert());
        }
    }
    Ok(ExitCode::SUCCESS)
}

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

pub fn prune(source: &Path, name: Option<&str>, yes: bool, force: bool, tty: bool) -> Result<ExitCode> {
    let archive = source.join(".archive");
    let names = match name {
        Some(n) if is_entry(&archive, n) => vec![n.to_string()],
        Some(n) => return Err(Error::Usage(format!("{n} is not in {}", archive.inert()))),
        None => entries(&archive),
    };
    if !yes && !tty {
        return Err(Error::Usage("prune deletes permanently; pass --yes when stdin is not a terminal".into()));
    }
    if names.is_empty() {
        outln!("nothing to prune in {}", archive.inert());
        return Ok(ExitCode::SUCCESS);
    }
    if !force {
        let unsaved = unsaved(source, &names)?;
        if !unsaved.is_empty() {
            for f in &unsaved {
                eprintln!("{}: content is in no HEAD path", crate::render::inert_line(f));
            }
            return Err("prune refused: that work exists nowhere else; commit it or pass --force".into());
        }
    }
    for n in &names {
        outln!("delete {}", archive.join(n).inert());
    }
    if !yes {
        eprint!("delete {} permanently? [y/N] ", crate::render::count(names.len(), "archived skill"));
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("stdin: {e}"))?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            return Ok(ExitCode::from(1));
        }
    }
    for n in &names {
        let dir = archive.join(n);
        fs::remove_dir_all(&dir).map_err(crate::at(&dir))?;
    }
    Ok(ExitCode::SUCCESS)
}

fn is_entry(archive: &Path, name: &str) -> bool {
    matches!(Path::new(name).components().collect::<Vec<_>>()[..], [Component::Normal(_)])
        && archive.join(name).symlink_metadata().is_ok_and(|m| m.is_dir())
}

fn entries(archive: &Path) -> Vec<String> {
    let Ok(dir) = fs::read_dir(archive) else { return Vec::new() };
    let mut names: Vec<String> = dir
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

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

fn link_blob(source: &Path, rel: &str) -> Result<String> {
    let path = source.join(rel);
    let target = fs::read_link(&path).map_err(crate::at(&path))?;
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

fn walk(source: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(crate::at(dir))?;
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

const RETIRE_DAYS: u32 = 30;

pub enum Retired {
    Blocked(Vec<String>),
    Refused(Vec<Diag>),
    Done(Vec<String>),
}

pub fn retire(
    cfg: &Config,
    data: &Path,
    name: &str,
    force: bool,
    project: bool,
    warnings: &mut Vec<Diag>,
) -> Result<Retired> {
    let project = if project { Some(store::project_root("retire")?) } else { None };
    let (inv, root) = located(cfg, data, name, project.as_deref())?;
    archive(cfg, &root, name)?;
    let scanned = scan::scan(cfg, &inv, data, false)?;
    warnings.extend(scanned.warnings());
    if !force {
        let coverage = ledger::load_coverage(data)?;
        let now = jiff::Timestamp::now();
        let tz = jiff::tz::TimeZone::system();
        let input = report::Input {
            cfg,
            inv: &inv,
            events: &scanned.events,
            coverage: &coverage,
            audits: &BTreeMap::new(),
            now,
            tz,
        };
        let blockers = report::retire_blockers(&input, name, RETIRE_DAYS);
        if !blockers.is_empty() {
            return Ok(Retired::Blocked(blockers));
        }
    }
    let _lock = durable::lock(data)?;
    let (mut rest, root) = located(cfg, data, name, project.as_deref())?;
    let to = archive(cfg, &root, name)?;
    let from = root.join(name);
    rest.skills.remove(name);
    rest.origins.remove(name);
    let target = store::Target { data, project: project.as_deref() };
    let (plan, errors) = target.home_sync(cfg, &rest, Some(&from));
    if !errors.is_empty() {
        return Ok(Retired::Refused(errors));
    }

    let mut out = match &to {
        Some(to) => {
            let archive = root.join(".archive");
            fs::create_dir_all(&archive).map_err(crate::at(&archive))?;
            if git(&root, &["ls-files", "--", name]).is_some_and(|o| !o.is_empty()) {
                git(&root, &["mv", "--", name, &format!(".archive/{name}")]).ok_or(format!("git mv {name} failed"))?;
            } else {
                fs::rename(&from, to).map_err(crate::at(&from))?;
            }
            links::apply_after(vec![format!("archived  {} -> {}", from.inert(), to.inert())], &plan)?
        }
        None => target.remove(&from, name, &plan)?,
    };
    if cfg.policy.claude_name_only && project.is_none() {
        for h in cfg.homes.iter().filter(|h| h.agent == Agent::Claude) {
            for (path, settings) in policy::settings(h) {
                if let Some(v) = settings.ok().and_then(|s| s.get("skillOverrides")?.get(name).cloned()) {
                    out.push(format!("{} can drop:\n  \"{name}\": {v}", path.inert()));
                }
            }
        }
    }
    Ok(Retired::Done(out))
}

fn located(cfg: &Config, data: &Path, name: &str, project: Option<&Path>) -> Result<(Inventory, PathBuf)> {
    let inv = store::scoped_inventory(store::inventory(cfg.source.as_deref(), data)?, project)?;
    let root = match project {
        Some(p) => {
            let store = store::project_dir(p);
            let roots = inv.origins.get(name).into_iter().flatten();
            let Some(root) = roots.into_iter().find(|r| **r == store) else {
                return Err(Error::Usage(format!("{name} is not a project skill")));
            };
            root.clone()
        }
        None => {
            let Some(root) = inv.root(name) else {
                let hint = store::project_hint(name);
                if hint.is_empty() {
                    cfg.need_source(&format!("retire {name}"))?;
                }
                return Err(Error::Usage(format!("{name} is not a canonical skill{hint}")));
            };
            root.to_path_buf()
        }
    };
    Ok((inv, root))
}

fn archive(cfg: &Config, root: &Path, name: &str) -> Result<Option<PathBuf>> {
    let to = (cfg.source.as_deref() == Some(root)).then(|| root.join(".archive").join(name));
    if let Some(to) = to.as_ref().filter(|t| t.symlink_metadata().is_ok()) {
        return Err(format!("retire refused: {} already exists", to.inert()).into());
    }
    Ok(to)
}
