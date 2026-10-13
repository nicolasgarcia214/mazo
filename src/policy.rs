//! Policy rules moved from `install.sh`'s `validate_policy` (SPEC §5.4), and
//! the scan-freshness warning.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::{Config, Home};
use crate::inventory::Inventory;
use crate::ledger::{Agent, Coverage};
use crate::strict_yaml::{self, Yaml};

/// Per-skill rules: run on every `sync` and `status`, whatever `--agent` says.
/// Returns `ERROR: …` lines.
pub fn per_skill(cfg: &Config, inv: &Inventory) -> Vec<String> {
    if inv.skills.is_empty() {
        return vec![format!("ERROR: no canonical skills found in {}", cfg.source.display())];
    }
    inv.skills
        .iter()
        .filter_map(|name| {
            let dir = cfg.source.join(name);
            check_skill(&dir, name).err().map(|e| format!("ERROR: {}: {e}", dir.display()))
        })
        .collect()
}

fn check_skill(dir: &Path, name: &str) -> crate::Result<()> {
    let skill_md = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&skill_md).map_err(|e| format!("{}: {e}", skill_md.display()))?;
    let meta = strict_yaml::frontmatter(&text)?;
    if !matches!(meta, Yaml::Map(_)) {
        return Err("SKILL.md frontmatter must be a mapping".into());
    }
    if meta.get("disable-model-invocation").is_some() {
        return Err("SKILL.md must omit disable-model-invocation; Claude invocation lives in skillOverrides".into());
    }
    let description = match meta.get("description") {
        Some(Yaml::Str(d)) if !d.trim_matches(python_space).is_empty() => d,
        _ => return Err("SKILL.md must declare its skill name and a nonempty description".into()),
    };
    if !matches!(meta.get("name"), Some(Yaml::Str(n)) if n == name) {
        return Err("SKILL.md must declare its skill name and a nonempty description".into());
    }
    if name.chars().count() > 64 || !valid_name(name) || description.chars().count() > 1024 {
        return Err("skill name or description exceeds the Agent Skills format limits".into());
    }
    let manifest_path = dir.join("agents/openai.yaml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest = strict_yaml::load(&text)?;
    match manifest.get("policy") {
        Some(policy @ Yaml::Map(_)) if policy.get("allow_implicit_invocation") == Some(&Yaml::Bool(false)) => Ok(()),
        _ => Err("agents/openai.yaml must set policy.allow_implicit_invocation: false".into()),
    }
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

/// Per-home rules for the selected homes. Returns `ERROR: …` lines.
pub fn per_home(inv: &Inventory, home: &Home) -> Vec<String> {
    if home.agent != Agent::Claude {
        return Vec::new();
    }
    let path = home.settings();
    if !path.exists() {
        return vec![format!("ERROR: {}: no Claude settings profile", home.path.display())];
    }
    let p = path.display();
    let settings: serde_json::Value = match std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
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

/// Retention in days for a Claude home: `cleanupPeriodDays`, or 30 when unset.
pub fn retention_days(home: &Home) -> i64 {
    std::fs::read(home.settings())
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("cleanupPeriodDays").and_then(|d| d.as_i64()))
        .unwrap_or(30)
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
        let cfg = Config { source: dir.path().into(), loose: vec![], homes: vec![home] };
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
