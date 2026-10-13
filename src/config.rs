//! `skima.toml` (SPEC §6.1) and path expansion, including the Pi directory
//! normalisation that `install.sh` did in Node (SPEC §5.6).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::Result;
use crate::ledger::Agent;

#[derive(Debug, Clone)]
pub struct Config {
    /// The physical skills source directory.
    pub source: PathBuf,
    pub loose: Vec<String>,
    pub homes: Vec<Home>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    source: String,
    #[serde(default)]
    loose: Vec<String>,
    #[serde(default, rename = "home")]
    homes: Vec<RawHome>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHome {
    name: String,
    agent: Agent,
    path: String,
    #[serde(default)]
    links: BTreeMap<String, String>,
}

/// `$SKIMA_CONFIG`, else `~/dotfiles/skills/skima.toml`.
pub fn config_path() -> Result<PathBuf> {
    match std::env::var_os("SKIMA_CONFIG").filter(|v| !v.is_empty()) {
        Some(p) => Ok(PathBuf::from(p)),
        None => Ok(PathBuf::from(home_dir()?).join("dotfiles/skills/skima.toml")),
    }
}

pub fn load() -> Result<Config> {
    load_from(&config_path()?)
}

pub fn load_from(path: &Path) -> Result<Config> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw: RawConfig = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new("."));
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
    Ok(Config { source, loose: raw.loose, homes })
}

pub fn home_dir() -> Result<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty()).ok_or_else(|| "HOME is not set".to_string())
}

/// `~` alone is `$HOME`; `~/x` is the string `$HOME/x`. No other form expands.
pub fn expand_tilde(value: &str, home: &str) -> String {
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
pub fn pi_agent_dir(value: &str, home: &str, cwd: &Path) -> Result<PathBuf> {
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
