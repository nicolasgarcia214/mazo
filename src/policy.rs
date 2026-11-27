use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::config::{Config, Home, Policy};
use crate::inventory::Inventory;
use crate::ledger::{Agent, Coverage};
use crate::render::{Diag, PathExt};
use crate::yaml::{self, Yaml};

pub fn per_skill(cfg: &Config, inv: &Inventory) -> Vec<Diag> {
    if let (true, Some(source)) = (inv.skills.is_empty(), &cfg.source) {
        return vec![Diag::error(format!("no canonical skills found in {}", source.inert()))];
    }
    inv.origins
        .iter()
        .filter_map(|(name, roots)| {
            let dir = roots[0].join(name);
            check_skill(&cfg.policy, &dir, name).err().map(|e| Diag::error(crate::at(&dir)(e)))
        })
        .collect()
}

fn check_skill(policy: &Policy, dir: &Path, name: &str) -> Result<()> {
    let skill_md = dir.join("SKILL.md");
    let text = fs::read_to_string(&skill_md).map_err(crate::at(&skill_md))?;
    let meta = yaml::frontmatter(&text)?;
    always_on(&meta, name)?;
    if policy.no_disable_model_invocation && yaml::get(&meta, "disable-model-invocation").is_some() {
        return Err("SKILL.md must omit disable-model-invocation; Claude invocation lives in skillOverrides".into());
    }
    if policy.codex_manifest {
        let manifest_path = dir.join("agents/openai.yaml");
        let text = fs::read_to_string(&manifest_path).map_err(crate::at(&manifest_path))?;
        let manifest = yaml::load(&text)?;
        let value = yaml::get(&manifest, "policy").and_then(|p| yaml::get(p, "allow_implicit_invocation"));
        if !value.is_some_and(yaml::is_plain_false) {
            return Err("agents/openai.yaml must set policy.allow_implicit_invocation: false".into());
        }
    }
    Ok(())
}

pub fn always_on(meta: &Yaml, name: &str) -> Result<()> {
    if !meta.is_mapping() {
        return Err("SKILL.md frontmatter must be a mapping".into());
    }
    let description = match yaml::get(meta, "description").and_then(yaml::string) {
        Some(d) if !d.trim_matches(python_space).is_empty() => d,
        _ => return Err("SKILL.md must declare a nonempty description".into()),
    };
    if name.chars().count() > 64 {
        return Err(format!("skill name {name:?} is over 64 characters").into());
    }
    if !valid_name(name) {
        return Err(format!("skill name {name:?} must be lowercase letters and digits joined by single hyphens").into());
    }
    if yaml::get(meta, "name").and_then(yaml::string) != Some(name) {
        return Err(format!("SKILL.md must declare name: {name}").into());
    }
    if description.chars().count() > 1024 {
        return Err("SKILL.md description is over 1024 characters".into());
    }
    Ok(())
}

fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

pub fn valid_name(name: &str) -> bool {
    let ok = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    name.split('-').all(ok)
}

pub fn home_errors(cfg: &Config, inv: &Inventory, homes: &[&Home]) -> Vec<(String, Diag)> {
    homes.iter().flat_map(|h| per_home(&cfg.policy, inv, h).into_iter().map(|e| (h.name.clone(), e))).collect()
}

fn per_home(policy: &Policy, inv: &Inventory, home: &Home) -> Vec<Diag> {
    if !policy.claude_name_only || home.agent != Agent::Claude {
        return Vec::new();
    }
    let profile = home.settings();
    if !profile.exists() {
        return vec![Diag::error(format!("{}: no Claude settings profile", home.path.inert()))];
    }
    let (overrides, errors) = overrides(home);
    if !errors.is_empty() {
        return errors;
    }
    let mut missing: BTreeMap<&Path, Vec<&String>> = BTreeMap::new();
    for s in &inv.skills {
        match overrides.get(s) {
            Some((_, v)) if v == "name-only" => {}
            Some((path, _)) => missing.entry(path).or_default().push(s),
            None => missing.entry(&profile).or_default().push(s),
        }
    }
    let error = |(path, skills): (&Path, Vec<&String>)| {
        let entries: Vec<String> = skills.iter().map(|s| format!("  \"{s}\": \"name-only\"")).collect();
        Diag::error(format!(
            "{}: skillOverrides must set every skill to name-only:\n{}",
            path.inert(),
            entries.join(",\n")
        ))
    };
    missing.into_iter().map(error).collect()
}

pub(crate) fn settings(home: &Home) -> impl Iterator<Item = (PathBuf, std::result::Result<serde_json::Value, String>)> {
    [home.settings(), home.path.join("settings.local.json")].into_iter().filter_map(|path| {
        let parsed = match fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => Err(e.to_string()),
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        };
        Some((path, parsed))
    })
}

pub fn overrides(home: &Home) -> (BTreeMap<String, (PathBuf, serde_json::Value)>, Vec<Diag>) {
    let mut values = BTreeMap::new();
    let mut errors = Vec::new();
    for (path, parsed) in settings(home) {
        let set = match parsed {
            Ok(serde_json::Value::Object(mut o)) => match o.remove("skillOverrides") {
                None => continue,
                Some(serde_json::Value::Object(set)) => Ok(set),
                Some(_) => Err("skillOverrides must be an object".to_string()),
            },
            Ok(_) => Err("skillOverrides must be an object".to_string()),
            Err(e) => Err(e),
        };
        match set {
            Ok(set) => values.extend(set.into_iter().map(|(skill, v)| (skill, (path.clone(), v)))),
            Err(e) => errors.push(Diag::error(crate::at(&path)(e))),
        }
    }
    (values, errors)
}

pub fn freshness(cfg: &Config, coverage: &BTreeMap<String, Coverage>, now: jiff::Timestamp) -> Vec<Diag> {
    let mut out = Vec::new();
    for home in cfg.homes.iter().filter(|h| h.agent == Agent::Claude) {
        let set = settings(home).filter_map(|(_, v)| v.ok()?.get("cleanupPeriodDays")?.as_i64());
        let days = set.last().unwrap_or(30);
        let limit = (days - 7).max(0) * 86_400;
        let last = coverage
            .get(&home.name)
            .and_then(|c| c.last_scan.as_deref())
            .and_then(|t| t.parse::<jiff::Timestamp>().ok());
        let stale = match last {
            None => true,
            Some(t) => days <= 7 || now.as_second() - t.as_second() > limit,
        };
        if stale {
            let when = last.map_or("never".to_string(), |t| t.strftime("%Y-%m-%d").to_string());
            out.push(Diag::warning(format!(
                "{}: last scanned {when}; Claude deletes transcripts after {days} days. Run `mazo scan`.",
                home.name
            )));
        }
    }
    out
}
