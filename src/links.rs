//! Drift classification and sync: `plan()` is pure and
//! `apply()` is the only writer, so every check happens before any write.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{Config, Home};
use crate::inventory::{Inventory, Scope};
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
    pub scope: Scope,
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

/// One directory mazo links into, with the canonical entries it should hold.
struct Target {
    home: String,
    dir: PathBuf,
    /// `(name in dir, absolute source)` pairs.
    entries: Vec<(String, PathBuf)>,
}

impl Target {
    /// Adds the entry `name` linking to `src`, or the conflict when `name`
    /// already links to another source.
    fn claim(&mut self, name: String, src: PathBuf) -> Option<String> {
        match self.entries.iter().find(|(n, _)| *n == name) {
            Some((_, other)) if *other == src => None,
            Some((_, other)) => Some(format!(
                "ERROR: {} is claimed by two sources: {} and {}",
                self.dir.join(&name).display(),
                other.display(),
                src.display()
            )),
            None => {
                self.entries.push((name, src));
                None
            }
        }
    }
}

fn targets(
    cfg: &Config,
    inv: &Inventory,
    homes: &[&Home],
    present: &dyn Fn(&Path) -> bool,
    conflicts: &mut Vec<(String, String)>,
) -> Vec<Target> {
    let (single, shared): (Vec<_>, Vec<_>) = inv.origins.iter().partition(|(_, roots)| roots.len() == 1);
    let mut common: Vec<(String, PathBuf)> =
        single.iter().map(|(name, roots)| (name.to_string(), roots[0].join(name))).collect();
    // `load` refuses `loose` and `links` without a source.
    for (loose, src) in cfg.source.iter().flat_map(|s| cfg.loose.iter().map(move |l| (l, s.join(l)))) {
        if src.is_file() {
            common.push((loose.clone(), src));
        }
    }
    common.sort();
    let mut out = Vec::new();
    for home in homes {
        for (name, roots) in &shared {
            let roots: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
            let path = home.skills_dir().join(name);
            conflicts.push((
                home.name.clone(),
                format!("ERROR: {} is claimed by several roots: {}", path.display(), roots.join(", ")),
            ));
        }
        out.push(Target { home: home.name.clone(), dir: home.skills_dir(), entries: common.clone() });
        for (key, src) in cfg.source.iter().flat_map(|s| home.links.iter().map(move |(k, v)| (k, s.join(v)))) {
            let path = home.path.join(key);
            if !present(&src) {
                conflicts.push((home.name.clone(), format!("ERROR: {} is missing", src.display())));
                continue;
            }
            let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { continue };
            let name = name.to_string_lossy().into_owned();
            match out.iter_mut().find(|t: &&mut Target| t.home == home.name && t.dir == dir) {
                Some(t) => conflicts.extend(t.claim(name, src).map(|e| (home.name.clone(), e))),
                None => {
                    out.push(Target { home: home.name.clone(), dir: dir.to_path_buf(), entries: vec![(name, src)] })
                }
            }
        }
    }
    out
}

/// The `(home, skill)` pairs that [`plan`] links over `homes`, whatever is
/// on disk: each skill of `inv` that is an entry of a home's skills directory.
pub fn linked<'h>(cfg: &Config, inv: &Inventory, homes: &[&'h Home]) -> Vec<(&'h Home, String)> {
    let targets = targets(cfg, inv, homes, &|p: &Path| p.exists(), &mut Vec::new());
    let entries = |h: &'h Home| {
        let dir = h.skills_dir();
        let of = targets.iter().filter(move |t| t.home == h.name && t.dir == dir).flat_map(|t| &t.entries);
        of.filter(|(name, _)| inv.skills.contains(name)).map(move |(name, _)| (h, name.clone()))
    };
    homes.iter().flat_map(|h| entries(h)).collect()
}

/// Classifies every entry of the selected homes and plans the writes that
/// `sync` would make. Never touches the filesystem except to read it.
/// With `gone` (for `retire`), the plan is the one `sync` would make if `gone`
/// did not exist: links into it plan as dangling and a `links` entry under it
/// is missing. `inv` must already leave out the skill at `gone`.
pub fn plan(cfg: &Config, inv: &Inventory, homes: &[&Home], gone: Option<&Path>) -> Plan {
    let present = |p: &Path| gone.is_none_or(|g| !p.starts_with(g)) && p.exists();
    let mut plan = Plan::default();
    let targets = targets(cfg, inv, homes, &present, &mut plan.conflicts);
    // A link to `<root>/<its name>` for a root of `inv`, whose target is gone:
    // mazo made it, so mazo may remove it.
    let owned_dangling = |path: &Path| {
        fs::read_link(path).is_ok_and(|text| {
            path.file_name().is_some_and(|n| inv.roots.iter().any(|r| text == r.join(n))) && !present(&text)
        })
    };
    // Two homes can share a directory, directly or through a symlink
    // (PI_CODING_AGENT_DIR pointing at another home): plan each physical one
    // once, with the entries of every target there.
    let mut merged: Vec<(PathBuf, Target)> = Vec::new();
    for t in targets {
        let physical = physical(&t.dir);
        match merged.iter_mut().find(|(p, _)| *p == physical) {
            Some((_, into)) => {
                for (name, src) in t.entries {
                    plan.conflicts.extend(into.claim(name, src).map(|e| (t.home.clone(), e)));
                }
            }
            None => merged.push((physical, t)),
        }
    }
    for (physical, t) in &merged {
        // Every skill there would look like a real file, or a link written into the root.
        if let Some(root) = inv.roots.iter().find(|r| *r == physical) {
            plan.conflicts.push((
                t.home.clone(),
                format!("ERROR: {} resolves to {}; replace it with a directory", t.dir.display(), root.display()),
            ));
            continue;
        }
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
            let state = match state(&path, src) {
                State::WrongLink if owned_dangling(&path) => State::Dangling,
                s => s,
            };
            match state {
                State::Missing => plan.actions.push(Action::Link { source: src.clone(), path: path.clone() }),
                // A copy deleted from one root and installed into another.
                State::Dangling => {
                    plan.actions.push(Action::Remove(path.clone()));
                    plan.actions.push(Action::Link { source: src.clone(), path: path.clone() });
                }
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
            plan.entries.push(Entry { home: t.home.clone(), path, state, scope: Scope::Global });
        }
        if !exists {
            continue;
        }
        let Ok(dir) = fs::read_dir(&t.dir) else { continue };
        let mut others: Vec<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| !t.entries.iter().any(|(n, _)| p.file_name().is_some_and(|f| f == n.as_str())))
            // Hidden names are never pruned.
            .filter(|p| p.file_name().is_some_and(|f| !f.to_string_lossy().starts_with('.')))
            .collect();
        others.sort();
        for path in others {
            let owned = owned_dangling(&path);
            let state = if owned { State::Dangling } else { State::Foreign };
            if owned {
                plan.actions.push(Action::Remove(path.clone()));
            }
            plan.entries.push(Entry { home: t.home.clone(), path, state, scope: Scope::Global });
        }
    }
    plan
}

/// The state of the entry at `path` that should be a link to `src`.
pub fn state(path: &Path, src: &Path) -> State {
    match fs::symlink_metadata(path) {
        Err(_) => State::Missing,
        Ok(m) if m.file_type().is_symlink() => match fs::read_link(path) {
            Ok(text) if text == src => State::Ok,
            _ => State::WrongLink,
        },
        Ok(_) => State::Obstructed,
    }
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

/// Every line that stops `sync`: the per-skill rules, the
/// per-home rules of the selected homes, then the plan's conflicts.
pub fn sync_errors(cfg: &Config, inv: &Inventory, homes: &[&Home], plan: &Plan) -> Vec<String> {
    policy::per_skill(cfg, inv)
        .into_iter()
        .chain(policy::home_errors(cfg, inv, homes).into_iter().chain(plan.conflicts.iter().cloned()).map(|(_, e)| e))
        .collect()
}

/// `sync`'s plan over `homes`, or the [`sync_errors`] that stop it.
pub fn checked(cfg: &Config, inv: &Inventory, homes: &[&Home]) -> Result<Plan, Vec<String>> {
    let plan = plan(cfg, inv, homes, None);
    let errors = sync_errors(cfg, inv, homes, &plan);
    if errors.is_empty() { Ok(plan) } else { Err(errors) }
}

/// The line `sync --dry-run` prints for each action of `plan`.
pub fn planned(plan: &Plan) -> Vec<String> {
    plan.actions
        .iter()
        .map(|a| match a {
            Action::Mkdir(d) => format!("would create {}", d.display()),
            Action::Link { path, .. } => format!("would link  {}", path.display()),
            Action::Remove(p) => format!("would prune {}", p.display()),
        })
        .collect()
}

/// Performs a conflict-free plan. Returns the lines to print.
pub fn apply(plan: &Plan) -> crate::Result<Vec<String>> {
    apply_after(Vec::new(), plan)
}

/// [`apply`], continuing `out`, the lines of writes already made. A failed
/// write's error lists every link and prune done before it, `out`'s
/// included; none is undone.
pub fn apply_after(mut out: Vec<String>, plan: &Plan) -> crate::Result<Vec<String>> {
    assert!(plan.conflicts.is_empty(), "apply() is only called on a conflict-free plan");
    for a in &plan.actions {
        let (path, done) = match a {
            Action::Mkdir(d) => (d, fs::create_dir_all(d).map(|()| None)),
            Action::Link { source, path } => {
                (path, std::os::unix::fs::symlink(source, path).map(|()| Some(format!("linked    {}", path.display()))))
            }
            Action::Remove(p) => {
                (p, fs::remove_file(p).map(|()| Some(format!("pruned    {} (dangling)", p.display()))))
            }
        };
        if let Err(e) = done.map(|line| out.extend(line)) {
            let done = if out.is_empty() {
                String::new()
            } else {
                format!("; done before it:{}", out.iter().map(|l| format!("\n  {l}")).collect::<String>())
            };
            return Err(crate::at(path)(format!("{e}{done}")).into());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Action, Plan, State, plan};
    use crate::config::{Config, Home};
    use crate::inventory::Inventory;
    use crate::ledger::Agent;

    fn skill(root: &Path, name: &str) {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
    }

    fn setup(tmp: &Path) -> (Config, Inventory) {
        let (source, store) = (tmp.join("source"), tmp.join("store"));
        skill(&source, "mine");
        skill(&store, "theirs");
        let home =
            Home { name: "claude".into(), agent: Agent::Claude, path: tmp.join("home"), links: Default::default() };
        let cfg = Config {
            source: Some(source.clone()),
            loose: vec![],
            trusted: vec![],
            evals_dir: Default::default(),
            context_window: crate::config::CONTEXT_WINDOW,
            homes: vec![home],
            policy: Default::default(),
            judge: None,
        };
        let inv = Inventory::load_roots(Some(&source), &[&source, &store]).unwrap();
        (cfg, inv)
    }

    #[test]
    fn each_skill_links_to_its_own_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, inv) = setup(tmp.path());
        let skills = tmp.path().join("home/skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("store/gone"), skills.join("gone")).unwrap();
        let p = plan(&cfg, &inv, &cfg.homes.iter().collect::<Vec<_>>(), None);
        assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
        let gone = p.entries.iter().find(|e| e.path == skills.join("gone")).unwrap();
        assert_eq!(gone.state, State::Dangling, "a link into a non-source root is owned");
        assert!(p.actions.contains(&Action::Remove(skills.join("gone"))));
        let links: Vec<&Action> = p.actions.iter().filter(|a| matches!(a, Action::Link { .. })).collect();
        assert_eq!(
            links,
            [
                &Action::Link { source: tmp.path().join("source/mine"), path: skills.join("mine") },
                &Action::Link { source: tmp.path().join("store/theirs"), path: skills.join("theirs") },
            ]
        );
    }

    #[test]
    fn a_dangling_link_into_another_root_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, inv) = setup(tmp.path());
        let homes: Vec<&Home> = cfg.homes.iter().collect();
        let skills = tmp.path().join("home/skills");
        std::fs::create_dir_all(&skills).unwrap();
        // `theirs` was a source skill, deleted there and installed into the store.
        std::os::unix::fs::symlink(tmp.path().join("source/theirs"), skills.join("theirs")).unwrap();
        let p = plan(&cfg, &inv, &homes, None);
        assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
        assert_eq!(p.entries.iter().find(|e| e.path == skills.join("theirs")).unwrap().state, State::Dangling);
        super::apply(&p).unwrap();
        assert_eq!(std::fs::read_link(skills.join("theirs")).unwrap(), tmp.path().join("store/theirs"));

        // A wrong link whose target exists is still the user's to resolve.
        std::fs::remove_file(skills.join("mine")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("store/theirs"), skills.join("mine")).unwrap();
        let p = plan(&cfg, &inv, &homes, None);
        let errors: Vec<&String> = p.conflicts.iter().map(|(_, e)| e).collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains(&skills.join("mine").display().to_string()), "{}", errors[0]);
    }

    #[test]
    fn a_name_in_two_roots_is_a_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        skill(&tmp.path().join("store"), "mine");
        let (cfg, inv) = setup(tmp.path());
        let p = plan(&cfg, &inv, &cfg.homes.iter().collect::<Vec<_>>(), None);
        let errors: Vec<&String> = p.conflicts.iter().map(|(_, e)| e).collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("ERROR: ") && errors[0].contains("mine"), "{}", errors[0]);
        assert!(!p.actions.iter().any(|a| matches!(a, Action::Link { path, .. } if path.ends_with("mine"))));
    }

    #[test]
    fn a_home_sharing_a_skills_dir_keeps_its_links_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = std::fs::canonicalize(tmp.path()).unwrap();
        let (mut cfg, inv) = setup(&tmp);
        std::fs::write(tmp.join("source/extra.md"), "extra\n").unwrap();
        let skills = tmp.join("home/skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::create_dir_all(tmp.join("pi")).unwrap();
        std::os::unix::fs::symlink(&skills, tmp.join("pi/skills")).unwrap();
        let links = [("skills/extra.md".to_string(), "extra.md".to_string())].into();
        cfg.homes.push(Home { name: "pi".into(), agent: Agent::Pi, path: tmp.join("pi"), links });
        let p = plan(&cfg, &inv, &cfg.homes.iter().collect::<Vec<_>>(), None);
        assert!(p.conflicts.is_empty(), "{:?}", p.conflicts);
        let extra = Action::Link { source: tmp.join("source/extra.md"), path: skills.join("extra.md") };
        assert!(p.actions.contains(&extra), "{:?}", p.actions);

        // The same name to another source is a conflict, whichever home claims it.
        cfg.homes[1].links = [("skills/mine".to_string(), "extra.md".to_string())].into();
        let p = plan(&cfg, &inv, &cfg.homes.iter().collect::<Vec<_>>(), None);
        let errors: Vec<&String> = p.conflicts.iter().map(|(_, e)| e).collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("claimed by two sources"), "{}", errors[0]);
    }

    #[test]
    fn a_skills_dir_that_resolves_to_a_root_is_one_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp = std::fs::canonicalize(tmp.path()).unwrap();
        let (cfg, inv) = setup(&tmp);
        std::fs::create_dir_all(tmp.join("home")).unwrap();
        std::os::unix::fs::symlink(tmp.join("source"), tmp.join("home/skills")).unwrap();
        let p = plan(&cfg, &inv, &cfg.homes.iter().collect::<Vec<_>>(), None);
        let errors: Vec<&String> = p.conflicts.iter().map(|(_, e)| e).collect();
        let want = format!("ERROR: {} resolves to {}", tmp.join("home/skills").display(), tmp.join("source").display());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with(&want), "{}", errors[0]);
        assert!(p.actions.is_empty(), "{:?}", p.actions);
    }

    #[test]
    fn a_failed_write_reports_the_writes_before_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (made, gone) = (tmp.path().join("made"), tmp.path().join("no/such/dir/link"));
        let p = Plan {
            actions: vec![
                Action::Link { source: tmp.path().into(), path: made.clone() },
                Action::Link { source: tmp.path().into(), path: gone.clone() },
            ],
            ..Plan::default()
        };
        let e = super::apply_after(vec!["archived  a -> b".into()], &p).unwrap_err().to_string();
        assert!(e.starts_with(&format!("{}: ", gone.display())), "{e}");
        assert!(e.ends_with(&format!("; done before it:\n  archived  a -> b\n  linked    {}", made.display())), "{e}");
    }
}
