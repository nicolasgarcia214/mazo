//! Drift classification and sync (SPEC §5.2, §5.5): `plan()` is pure and
//! `apply()` is the only writer, so every check happens before any write.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{Config, Home};
use crate::inventory::Inventory;
use crate::policy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ok,
    Missing,
    Dangling,
    WrongLink,
    Obstructed,
    Foreign,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Missing => "missing",
            State::Dangling => "dangling",
            State::WrongLink => "wrong-link",
            State::Obstructed => "obstructed",
            State::Foreign => "foreign",
        }
    }

    /// What the state means, for `status`: `<home>: <name> <finding>`.
    pub fn finding(self) -> &'static str {
        match self {
            State::Ok => unreachable!("status prints no line for an ok entry"),
            State::Missing => "is not linked (missing)",
            State::Dangling => "link target is gone (dangling)",
            State::WrongLink => "links somewhere else (wrong-link)",
            State::Obstructed => "is a real file, not a link (obstructed)",
            State::Foreign => "is not from the source; left alone (foreign)",
        }
    }

    pub fn is_error(self) -> bool {
        !matches!(self, State::Ok | State::Foreign)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub home: String,
    pub path: PathBuf,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Mkdir(PathBuf),
    Link { source: PathBuf, path: PathBuf },
    Remove(PathBuf),
}

#[derive(Debug, Default)]
pub struct Plan {
    pub entries: Vec<Entry>,
    pub actions: Vec<Action>,
    /// `(home, "ERROR: …")` pairs, each line with its resolution hint; any
    /// conflict means no writes.
    pub conflicts: Vec<(String, String)>,
}

/// One directory skima links into, with the canonical entries it should hold.
struct Target {
    home: String,
    dir: PathBuf,
    /// `(name in dir, absolute source)` pairs.
    entries: Vec<(String, PathBuf)>,
}

fn targets(
    cfg: &Config,
    inv: &Inventory,
    homes: &[&Home],
    present: &dyn Fn(&Path) -> bool,
    conflicts: &mut Vec<(String, String)>,
) -> Vec<Target> {
    let mut common: Vec<(String, PathBuf)> = inv.skills.iter().map(|s| (s.clone(), cfg.source.join(s))).collect();
    for loose in &cfg.loose {
        let src = cfg.source.join(loose);
        if src.is_file() {
            common.push((loose.clone(), src));
        }
    }
    common.sort();
    let mut out = Vec::new();
    for home in homes {
        out.push(Target { home: home.name.clone(), dir: home.skills_dir(), entries: common.clone() });
        for (key, value) in &home.links {
            let path = home.path.join(key);
            let src = cfg.source.join(value);
            if !present(&src) {
                conflicts.push((home.name.clone(), format!("ERROR: {} is missing", src.display())));
                continue;
            }
            let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { continue };
            let name = name.to_string_lossy().into_owned();
            match out.iter_mut().find(|t: &&mut Target| t.home == home.name && t.dir == dir) {
                Some(t) => match t.entries.iter().find(|(n, _)| *n == name) {
                    Some((_, other)) => conflicts.push((
                        home.name.clone(),
                        format!(
                            "ERROR: {} is claimed by two sources: {} and {}",
                            path.display(),
                            other.display(),
                            src.display()
                        ),
                    )),
                    None => t.entries.push((name, src)),
                },
                None => {
                    out.push(Target { home: home.name.clone(), dir: dir.to_path_buf(), entries: vec![(name, src)] })
                }
            }
        }
    }
    out
}

/// Classifies every entry of the selected homes and plans the writes that
/// `sync` would make. Never touches the filesystem except to read it.
pub fn plan(cfg: &Config, inv: &Inventory, homes: &[&Home]) -> Plan {
    plan_with(cfg, inv, homes, None)
}

/// The plan `sync` would make if `gone` did not exist (SPEC F17 `retire`): links
/// into it plan as dangling and a `links` entry under it is missing. `inv` must
/// already leave out the skill at `gone`.
pub fn plan_without(cfg: &Config, inv: &Inventory, homes: &[&Home], gone: &Path) -> Plan {
    plan_with(cfg, inv, homes, Some(gone))
}

fn plan_with(cfg: &Config, inv: &Inventory, homes: &[&Home], gone: Option<&Path>) -> Plan {
    let present = |p: &Path| gone.is_none_or(|g| !p.starts_with(g)) && p.exists();
    let mut plan = Plan::default();
    let targets = targets(cfg, inv, homes, &present, &mut plan.conflicts);
    // Two homes can share a directory, directly or through a symlink
    // (PI_CODING_AGENT_DIR pointing at another home): plan each physical one once.
    let mut planned: Vec<PathBuf> = Vec::new();
    for t in &targets {
        let physical = physical(&t.dir);
        if planned.contains(&physical) {
            continue;
        }
        planned.push(physical);
        if let Err(bad) = check_dir(&t.dir) {
            plan.conflicts.push((t.home.clone(), format!("ERROR: {} is not a directory", bad.display())));
            continue;
        }
        let exists = fs::symlink_metadata(&t.dir).is_ok();
        if !exists {
            plan.actions.push(Action::Mkdir(t.dir.clone()));
        }
        for (name, src) in &t.entries {
            let path = t.dir.join(name);
            let state = match fs::symlink_metadata(&path) {
                Err(_) => State::Missing,
                Ok(m) if m.file_type().is_symlink() => match fs::read_link(&path) {
                    Ok(text) if text == *src => State::Ok,
                    _ => State::WrongLink,
                },
                Ok(_) => State::Obstructed,
            };
            match state {
                State::Missing => plan.actions.push(Action::Link { source: src.clone(), path: path.clone() }),
                State::WrongLink | State::Obstructed => {
                    plan.conflicts.push((
                        t.home.clone(),
                        format!(
                            "ERROR: {} conflicts with a canonical entry; preserved (resolve manually)\n  {}",
                            path.display(),
                            hint(&path, state)
                        ),
                    ));
                }
                _ => {}
            }
            plan.entries.push(Entry { home: t.home.clone(), path, state });
        }
        if !exists {
            continue;
        }
        let Ok(dir) = fs::read_dir(&t.dir) else { continue };
        let mut others: Vec<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| !t.entries.iter().any(|(n, _)| p.file_name().is_some_and(|f| f == n.as_str())))
            // install.sh pruned `"$dir"/*`, which skips hidden names.
            .filter(|p| p.file_name().is_some_and(|f| !f.to_string_lossy().starts_with('.')))
            .collect();
        others.sort();
        for path in others {
            let owned_dangling = fs::read_link(&path)
                .is_ok_and(|text| path.file_name().is_some_and(|n| text == cfg.source.join(n)) && !present(&text));
            let state = if owned_dangling { State::Dangling } else { State::Foreign };
            if owned_dangling {
                plan.actions.push(Action::Remove(path.clone()));
            }
            plan.entries.push(Entry { home: t.home.clone(), path, state });
        }
    }
    plan
}

/// The directory with its nearest existing ancestor resolved through symlinks.
fn physical(dir: &Path) -> PathBuf {
    let mut existing = dir.to_path_buf();
    let mut rest = Vec::new();
    while fs::symlink_metadata(&existing).is_err() {
        match (existing.file_name().map(|n| n.to_owned()), existing.pop()) {
            (Some(n), true) => rest.push(n),
            _ => break,
        }
    }
    let mut p = fs::canonicalize(&existing).unwrap_or(existing);
    p.extend(rest.iter().rev());
    p
}

/// The directory, or its first existing ancestor, must be a directory
/// (a regular file or a dangling symlink there is a conflict).
fn check_dir(dir: &Path) -> Result<(), PathBuf> {
    let mut p = dir.to_path_buf();
    while fs::symlink_metadata(&p).is_err() {
        if !p.pop() {
            return Ok(());
        }
    }
    if fs::metadata(&p).is_ok_and(|m| m.is_dir()) { Ok(()) } else { Err(p) }
}

fn hint(path: &Path, state: State) -> String {
    let q = shell_quote(&path.to_string_lossy());
    if state == State::WrongLink {
        format!("rm {q}")
    } else {
        format!("mv {q} {}", shell_quote(&format!("{}.local", path.to_string_lossy())))
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Every line that stops `sync` (SPEC §5.4, §5.5): the per-skill rules, the
/// per-home rules of the selected homes, then the plan's conflicts.
pub fn sync_errors(cfg: &Config, inv: &Inventory, homes: &[&Home], plan: &Plan) -> Vec<String> {
    policy::per_skill(cfg, inv)
        .into_iter()
        .chain(policy::home_errors(cfg, inv, homes).into_iter().chain(plan.conflicts.iter().cloned()).map(|(_, e)| e))
        .collect()
}

/// Performs a conflict-free plan. Returns the lines to print.
pub fn apply(plan: &Plan) -> crate::Result<Vec<String>> {
    assert!(plan.conflicts.is_empty(), "apply() is only called on a conflict-free plan");
    let mut out = Vec::new();
    for a in &plan.actions {
        match a {
            Action::Mkdir(d) => fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?,
            Action::Link { source, path } => {
                std::os::unix::fs::symlink(source, path).map_err(|e| format!("{}: {e}", path.display()))?;
                out.push(format!("linked    {}", path.display()));
            }
            Action::Remove(p) => {
                fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display()))?;
                out.push(format!("pruned    {} (dangling)", p.display()));
            }
        }
    }
    Ok(out)
}
