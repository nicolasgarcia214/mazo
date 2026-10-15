//! Always-on and configurable policy rules (SPEC §5.4), and the
//! scan-freshness warning.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{Config, Home, Policy};
use crate::inventory::Inventory;
use crate::ledger::{Agent, Coverage};
use crate::yaml::{self, Yaml};

/// Per-skill rules, always-on then the configured ones: run on every `sync`
/// and `status`, whatever `--agent` says. Returns `ERROR: …` lines.
pub fn per_skill(cfg: &Config, inv: &Inventory) -> Vec<String> {
    if inv.skills.is_empty() {
        return vec![format!("ERROR: no canonical skills found in {}", cfg.source.display())];
    }
    inv.skills
        .iter()
        .filter_map(|name| {
            let dir = cfg.source.join(name);
            check_skill(&cfg.policy, &dir, name).err().map(|e| format!("ERROR: {}: {e}", dir.display()))
        })
        .collect()
}

fn check_skill(policy: &Policy, dir: &Path, name: &str) -> crate::Result<()> {
    let skill_md = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&skill_md).map_err(|e| format!("{}: {e}", skill_md.display()))?;
    let meta = yaml::frontmatter(&text)?;
    always_on(&meta, name)?;
    if policy.no_disable_model_invocation && yaml::get(&meta, "disable-model-invocation").is_some() {
        return Err("SKILL.md must omit disable-model-invocation; Claude invocation lives in skillOverrides".into());
    }
    if policy.codex_manifest {
        let manifest_path = dir.join("agents/openai.yaml");
        let text = std::fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let manifest = yaml::load(&text)?;
        let value = yaml::get(&manifest, "policy").and_then(|p| yaml::get(p, "allow_implicit_invocation"));
        if !value.is_some_and(yaml::is_plain_false) {
            return Err("agents/openai.yaml must set policy.allow_implicit_invocation: false".into());
        }
    }
    Ok(())
}

/// The Agent Skills format rules on a parsed frontmatter.
fn always_on(meta: &Yaml, name: &str) -> crate::Result<()> {
    if !meta.is_mapping() {
        return Err("SKILL.md frontmatter must be a mapping".into());
    }
    let description = match yaml::get(meta, "description").and_then(yaml::string) {
        Some(d) if !d.trim_matches(python_space).is_empty() => d,
        _ => return Err("SKILL.md must declare its skill name and a nonempty description".into()),
    };
    if yaml::get(meta, "name").and_then(yaml::string) != Some(name) {
        return Err("SKILL.md must declare its skill name and a nonempty description".into());
    }
    if name.chars().count() > 64 || !valid_name(name) || description.chars().count() > 1024 {
        return Err("skill name or description exceeds the Agent Skills format limits".into());
    }
    Ok(())
}

/// Python's `str.isspace()`: Unicode whitespace plus the separators U+001C–U+001F.
fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `[a-z0-9]+(-[a-z0-9]+)*`
fn valid_name(name: &str) -> bool {
    let ok = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    name.split('-').all(ok)
}

/// Per-home configurable rules for the selected homes. Returns `ERROR: …` lines.
pub fn per_home(policy: &Policy, inv: &Inventory, home: &Home) -> Vec<String> {
    if !policy.claude_name_only || home.agent != Agent::Claude {
        return Vec::new();
    }
    let path = home.settings();
    if !path.exists() {
        return vec![format!("ERROR: {}: no Claude settings profile", home.path.display())];
    }
    let p = path.display();
    let settings = match settings(home) {
        Ok(v) => v,
        Err(e) => return vec![format!("ERROR: {p}: {e}")],
    };
    let empty = serde_json::Map::new();
    let overrides = match settings.as_object().map(|o| o.get("skillOverrides")) {
        Some(None) => &empty,
        Some(Some(serde_json::Value::Object(o))) => o,
        _ => return vec![format!("ERROR: {p}: skillOverrides must be an object")],
    };
    let missing: Vec<&String> =
        inv.skills.iter().filter(|s| overrides.get(*s).and_then(|v| v.as_str()) != Some("name-only")).collect();
    if missing.is_empty() {
        return Vec::new();
    }
    let entries: Vec<String> = missing.iter().map(|s| format!("  \"{s}\": \"name-only\"")).collect();
    vec![format!("ERROR: {p}: skillOverrides must set every skill to name-only:\n{}", entries.join(",\n"))]
}

/// A Claude home's parsed `settings.json`.
fn settings(home: &Home) -> crate::Result<serde_json::Value> {
    let bytes = std::fs::read(home.settings()).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// Retention in days for a Claude home: `cleanupPeriodDays`, or 30 when unset.
fn retention_days(home: &Home) -> i64 {
    settings(home).ok().and_then(|v| v["cleanupPeriodDays"].as_i64()).unwrap_or(30)
}

/// Scan-freshness warnings (SPEC §5.4): a Claude home never scanned, or last
/// scanned more than retention minus 7 days ago.
pub fn freshness(cfg: &Config, coverage: &BTreeMap<String, Coverage>, now: jiff::Timestamp) -> Vec<String> {
    let mut out = Vec::new();
    for home in cfg.homes.iter().filter(|h| h.agent == Agent::Claude) {
        let days = retention_days(home);
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
            out.push(format!(
                "warning: {}: last scanned {when}; Claude deletes transcripts after {days} days. Run `skima scan`.",
                home.name
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_agent_skills_format() {
        assert!(valid_name("goal-smart"));
        assert!(valid_name("a1-b2"));
        assert!(!valid_name("Goal"));
        assert!(!valid_name("a--b"));
        assert!(!valid_name("-a"));
        assert!(!valid_name(""));
    }

    #[test]
    fn freshness_warns_after_retention_minus_seven_days() {
        let dir = tempfile::tempdir().unwrap();
        let home =
            Home { name: "claude".into(), agent: Agent::Claude, path: dir.path().into(), links: Default::default() };
        let cfg = Config { source: dir.path().into(), loose: vec![], homes: vec![home], policy: Default::default() };
        let now: jiff::Timestamp = "2026-10-31T00:00:00Z".parse().unwrap();
        let at = |days: i64| {
            let t = now.checked_sub(jiff::SignedDuration::from_hours(24 * days)).unwrap();
            BTreeMap::from([("claude".to_string(), Coverage { first: None, last_scan: Some(t.to_string()) })])
        };
        assert_eq!(freshness(&cfg, &at(24), now).len(), 1);
        assert!(freshness(&cfg, &at(22), now).is_empty());
        assert_eq!(freshness(&cfg, &BTreeMap::new(), now).len(), 1, "never scanned warns");
    }
}
