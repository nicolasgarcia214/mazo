//! The config (SPEC §6.1) with its `[policy]` table: discovery, `skima init`, and path expansion,
//! including the Pi directory normalisation that `install.sh` did in Node (SPEC §5.6).

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::ledger::Agent;

#[derive(Debug, Clone)]
pub struct Config {
    /// The physical skills source directory.
    pub source: PathBuf,
    pub loose: Vec<String>,
    pub homes: Vec<Home>,
    pub policy: Policy,
    pub judge: Judge,
}

/// The optional `[judge]` table (SPEC §6.4, D28).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Judge {
    pub agent: Agent,
    /// Passed as `--model` (`-m` for Codex); unset means the agent's own default.
    pub model: Option<String>,
}

impl Default for Judge {
    fn default() -> Judge {
        Judge { agent: Agent::Claude, model: None }
    }
}

/// The `[policy]` table (SPEC §5.4): every rule off and no authoring skill by default.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub no_disable_model_invocation: bool,
    pub codex_manifest: bool,
    pub claude_name_only: bool,
    /// Skills whose strong load marks a root session as authoring (SPEC §4.1).
    pub authoring_skills: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Home {
    pub name: String,
    pub agent: Agent,
    pub path: PathBuf,
    /// Extra files to link: key relative to the home, value relative to the source.
    pub links: BTreeMap<String, String>,
}

impl Home {
    pub fn skills_dir(&self) -> PathBuf {
        self.path.join("skills")
    }

    pub fn settings(&self) -> PathBuf {
        self.path.join("settings.json")
    }

    /// Where this home's transcripts live.
    pub fn transcripts(&self) -> PathBuf {
        match self.agent {
            Agent::Claude => self.path.join("projects"),
            Agent::Codex => self.path.join("sessions"),
            Agent::Pi => match std::env::var_os("PI_CODING_AGENT_SESSION_DIR").filter(|v| !v.is_empty()) {
                Some(dir) => PathBuf::from(dir),
                None => self.path.join("sessions"),
            },
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    source: String,
    #[serde(default)]
    loose: Vec<String>,
    #[serde(default)]
    policy: Policy,
    #[serde(default, skip_serializing)]
    judge: Judge,
    #[serde(default, rename = "home")]
    homes: Vec<RawHome>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawHome {
    name: String,
    agent: Agent,
    path: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    links: BTreeMap<String, String>,
}

/// `$SKIMA_CONFIG`, else `$XDG_CONFIG_HOME/skima/config.toml` with
/// `XDG_CONFIG_HOME` defaulting to `~/.config` (SPEC §6.1). Nothing else.
pub fn path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("SKIMA_CONFIG").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let dir = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(home_dir()?).join(".config"),
    };
    Ok(dir.join("skima/config.toml"))
}

/// The config at `path`, or `None` when no file is there.
pub fn load(path: &Path) -> Result<Option<Config>> {
    // The physical file, so a config symlinked from a skills repo resolves
    // `source` against that repo (SPEC §6.1).
    let path = match fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw: RawConfig = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new("/"));
    // `source` is relative to the config file and resolved physically, as
    // install.sh's `pwd -P` did (SPEC §6.1, test_install.py:130).
    let joined = base.join(&raw.source);
    let source = fs::canonicalize(&joined).map_err(|e| format!("{}: {e}", joined.display()))?;
    let home = home_dir()?;
    let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
    let mut homes = Vec::new();
    for h in raw.homes {
        let path = match h.agent {
            Agent::Pi => {
                let env = std::env::var("PI_CODING_AGENT_DIR").ok().filter(|v| !v.is_empty());
                let value = env.unwrap_or(h.path);
                pi_agent_dir(&value, &home, &cwd)?
            }
            _ => resolve(&cwd, &expand_tilde(&h.path, &home)),
        };
        homes.push(Home { name: h.name, agent: h.agent, path, links: h.links });
    }
    Ok(Some(Config { source, loose: raw.loose, homes, policy: raw.policy, judge: raw.judge }))
}

/// `skima init` (SPEC F11): a config at `path` naming `source` absolutely, a
/// home for each agent directory under `~` that exists, and every rule off.
/// Refuses to overwrite.
pub fn init(path: &Path, source: &Path) -> Result<()> {
    let abs = fs::canonicalize(source).map_err(|e| format!("{}: {e}", source.display()))?;
    let source = abs.to_str().ok_or_else(|| format!("{}: not UTF-8", abs.display()))?.to_string();
    let home = PathBuf::from(home_dir()?);
    let homes =
        [("claude", Agent::Claude, ".claude"), ("codex", Agent::Codex, ".codex"), ("pi", Agent::Pi, ".pi/agent")]
            .into_iter()
            .filter(|(_, _, rel)| home.join(rel).exists())
            .map(|(name, agent, rel)| RawHome {
                name: name.into(),
                agent,
                path: format!("~/{rel}"),
                links: BTreeMap::new(),
            })
            .collect();
    let raw = RawConfig { source, loose: Vec::new(), policy: Policy::default(), judge: Judge::default(), homes };
    let text = toml::to_string(&raw).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| match e.kind() {
        ErrorKind::AlreadyExists => format!("{} exists; not overwriting it", path.display()),
        _ => format!("{}: {e}", path.display()),
    })?;
    file.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn home_dir() -> Result<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty()).ok_or_else(|| "HOME is not set".to_string())
}

/// `~` alone is `$HOME`; `~/x` is the string `$HOME/x`. No other form expands.
pub(crate) fn expand_tilde(value: &str, home: &str) -> String {
    if value == "~" {
        home.to_string()
    } else if let Some(rest) = value.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        value.to_string()
    }
}

/// Node's `path.resolve(base, p)` for POSIX: join, then drop `.`/empty segments
/// and pop on `..`, without touching the filesystem.
pub fn resolve(base: &Path, p: &str) -> PathBuf {
    let joined = if p.starts_with('/') { PathBuf::from(p) } else { base.join(p) };
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// The Pi agent directory from a `PI_CODING_AGENT_DIR`-style value (SPEC §5.6).
/// The skills directory is `<result>/skills`.
fn pi_agent_dir(value: &str, home: &str, cwd: &Path) -> Result<PathBuf> {
    let invalid = |reason: &str| format!("invalid PI_CODING_AGENT_DIR: {reason}");
    let mut value = expand_tilde(value, home);
    if value.starts_with("file://") {
        value = file_url_to_path(&value).map_err(|r| invalid(&r))?;
    }
    if value.contains('\0') {
        return Err(invalid("agent directory cannot contain NUL"));
    }
    Ok(resolve(cwd, &value))
}

/// Node's `fileURLToPath` on POSIX: WHATWG parse, empty or `localhost` host,
/// no encoded `/`, strict percent-decoding to UTF-8.
fn file_url_to_path(value: &str) -> std::result::Result<String, String> {
    let url = url::Url::parse(value).map_err(|_| "Invalid URL".to_string())?;
    if url.scheme() != "file" {
        return Err("The URL must be of scheme file".into());
    }
    match url.host_str() {
        None | Some("") | Some("localhost") => {}
        Some(_) => return Err("File URL host must be \"localhost\" or empty on linux".into()),
    }
    let path = url.path();
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2f") {
        return Err("File URL path must not include encoded / characters".into());
    }
    percent_decode_strict(path).ok_or_else(|| "URI malformed".to_string())
}

fn percent_decode_strict(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pi(v: &str) -> std::result::Result<PathBuf, String> {
        pi_agent_dir(v, "/r/home", Path::new("/r/cwd"))
    }

    fn policy(table: &str) -> std::result::Result<Policy, String> {
        toml::from_str::<RawConfig>(&format!("source = \".\"\n{table}")).map(|r| r.policy).map_err(|e| e.to_string())
    }

    #[test]
    fn policy_keys_are_optional_and_off_by_default() {
        for table in ["", "[policy]\n"] {
            let p = policy(table).unwrap();
            assert!(!p.no_disable_model_invocation && !p.codex_manifest && !p.claude_name_only, "{table:?}");
            assert!(p.authoring_skills.is_empty(), "{table:?}");
        }
        let p = policy("[policy]\ncodex_manifest = true\nauthoring_skills = [\"grade\"]\n").unwrap();
        assert!(p.codex_manifest && !p.no_disable_model_invocation && !p.claude_name_only);
        assert_eq!(p.authoring_skills, ["grade"]);
        let p = policy("[policy]\nno_disable_model_invocation = true\nclaude_name_only = true\n").unwrap();
        assert!(p.no_disable_model_invocation && p.claude_name_only && !p.codex_manifest);
    }

    fn judge(table: &str) -> std::result::Result<Judge, String> {
        toml::from_str::<RawConfig>(&format!("source = \".\"\n{table}")).map(|r| r.judge).map_err(|e| e.to_string())
    }

    #[test]
    fn judge_table_takes_agent_and_model_only() {
        for table in ["", "[judge]\n"] {
            let j = judge(table).unwrap();
            assert_eq!((j.agent, j.model), (Agent::Claude, None), "{table:?}");
        }
        let j = judge("[judge]\nagent = \"codex\"\nmodel = \"gpt-5\"\n").unwrap();
        assert_eq!((j.agent, j.model.as_deref()), (Agent::Codex, Some("gpt-5")));
        assert_eq!(judge("[judge]\nagent = \"pi\"\n").unwrap().agent, Agent::Pi);
        let err = judge("[judge]\nmodel = \"sonnet\"\ntemperature = 1\n").unwrap_err();
        assert!(err.contains("unknown field `temperature`"), "{err}");
        assert!(judge("[judge]\nagent = \"gemini\"\n").is_err());
    }

    #[test]
    fn tilde_and_relative_values_resolve_lexically() {
        assert_eq!(pi("~").unwrap(), PathBuf::from("/r/home"));
        assert_eq!(pi("~/a b").unwrap(), PathBuf::from("/r/home/a b"));
        assert_eq!(pi("~//../a").unwrap(), PathBuf::from("/r/a"));
        assert_eq!(pi("~other").unwrap(), PathBuf::from("/r/cwd/~other"));
        assert_eq!(pi("-agent").unwrap(), PathBuf::from("/r/cwd/-agent"));
        assert_eq!(pi("alias/../x").unwrap(), PathBuf::from("/r/cwd/x"));
    }

    #[test]
    fn file_urls_follow_whatwg_and_node_checks() {
        assert_eq!(pi("file:///r/x/pi%20agent").unwrap(), PathBuf::from("/r/x/pi agent"));
        assert_eq!(pi("file://LOCALHOST/r/x?ignored#ignored").unwrap(), PathBuf::from("/r/x"));
        assert_eq!(pi("file:///r/x/unused/%2e%2e/a").unwrap(), PathBuf::from("/r/x/a"));
        assert_eq!(pi("file:///r/empty//../agent").unwrap(), PathBuf::from("/r/empty/agent"));
        assert_eq!(pi("file:///r/b \t\r\n").unwrap(), PathBuf::from("/r/b"));
        for bad in [
            "file://remote.example/r/x",
            "file:///r/x%2Fchild",
            "file:///r/x%",
            "file:///r/x%FF",
            "file:///r/x%00",
            "file://localhost:80/r/x",
        ] {
            let err = pi(bad).unwrap_err();
            assert!(err.starts_with("invalid PI_CODING_AGENT_DIR"), "{bad}: {err}");
        }
    }
}
