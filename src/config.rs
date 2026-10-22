//! The config with its `[policy]` table: discovery, `mazo init`, and path expansion,
//! including the Pi directory normalisation that `install.sh` did in Node.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder};
use std::io::{BufRead, ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ledger::Agent;
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct Config {
    /// The physical skills source directory; `None` for a user with no skills of their own.
    pub source: Option<PathBuf>,
    pub loose: Vec<String>,
    /// Owners (`owner`) and repositories (`owner/repo`) whose skills install
    /// without vetting.
    pub trusted: Vec<String>,
    pub homes: Vec<Home>,
    pub policy: Policy,
    /// `None` without a `[judge]` table: no agent is assumed.
    pub judge: Option<Judge>,
}

/// The `[judge]` table.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Judge {
    pub agent: JudgeAgent,
    /// Passed as `--model` (`-m` for Codex); unset means the agent's own default.
    pub model: Option<String>,
    pub billing: Billing,
    /// The `command` judge's program and arguments; no other judge takes one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    /// The `http` judge's base URL, posted to at `<url>/chat/completions`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The variable holding the `http` judge's API key; unset sends no key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

impl Judge {
    /// Why the table is invalid: `command` without an `argv`, `http` without
    /// a `url` or a `model` or with a blank `api_key_env`, or another agent's
    /// fields.
    fn check(&self) -> std::result::Result<(), &'static str> {
        let (command, http) = (self.agent == JudgeAgent::Command, self.agent == JudgeAgent::Http);
        let blank = |v: &Option<String>| v.as_deref().is_none_or(str::is_empty);
        if command && self.argv.is_empty() {
            Err("[judge] agent = \"command\" needs a non-empty argv")
        } else if !command && !self.argv.is_empty() {
            Err("[judge] argv is for agent = \"command\" only")
        } else if http && (blank(&self.url) || blank(&self.model)) {
            Err("[judge] agent = \"http\" needs a url and a model")
        } else if http && self.api_key_env.as_deref() == Some("") {
            Err("[judge] api_key_env must name a variable")
        } else if !http && (self.url.is_some() || self.api_key_env.is_some()) {
            Err("[judge] url and api_key_env are for agent = \"http\" only")
        } else {
            Ok(())
        }
    }
}

/// The agent mazo audit calls: one of the homes' agents, any CLI that
/// reads a prompt on stdin and prints its answer, or an OpenAI-compatible
/// chat completions endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeAgent {
    #[default]
    Claude,
    Codex,
    Pi,
    Command,
    Http,
}

/// Who pays for the judge's calls. Under `Subscription` every call runs
/// without the variables that would bill an API key or a cloud account, after
/// a login check; under `Api` the environment passes unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Billing {
    #[default]
    Subscription,
    Api,
}

/// The `[policy]` table: every rule off and no authoring skill by default.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub no_disable_model_invocation: bool,
    pub codex_manifest: bool,
    pub claude_name_only: bool,
    /// Skills whose strong load marks a root session as authoring.
    pub authoring_skills: Vec<String>,
}

impl Config {
    /// The source, or a usage error saying `what` needs it.
    pub fn need_source(&self, what: &str) -> Result<&Path> {
        self.source.as_deref().ok_or_else(|| Error::Usage(format!("{what} needs `source` in the config")))
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default)]
    loose: Vec<String>,
    /// Owners (`owner`) and repositories (`owner/repo`) whose skills install
    /// without vetting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    trusted: Vec<String>,
    #[serde(default)]
    policy: Policy,
    judge: Option<Judge>,
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

/// `$MAZO_CONFIG`, else `$XDG_CONFIG_HOME/mazo/config.toml` with
/// `XDG_CONFIG_HOME` defaulting to `~/.config`. Nothing else.
pub fn path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("MAZO_CONFIG").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let dir = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(home_dir()?).join(".config"),
    };
    Ok(dir.join("mazo/config.toml"))
}

/// The config at `path`, or `None` when no file is there.
pub fn load(path: &Path) -> Result<Option<Config>> {
    // The physical file, so a config symlinked from a skills repo resolves
    // `source` against that repo.
    let path = match fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display()).into()),
    };
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw: RawConfig = toml::from_str(&text).map_err(|e| Error::Usage(format!("{}: {e}", path.display())))?;
    if let Some(bad) = raw.trusted.iter().find(|t| t.split('/').count() > 2 || t.split('/').any(str::is_empty)) {
        return Err(Error::Usage(format!("{}: trusted entry {bad:?} must be owner or owner/repo", path.display())));
    }
    if let Some(judge) = &raw.judge {
        judge.check().map_err(|e| Error::Usage(format!("{}: {e}", path.display())))?;
    }
    let base = path.parent().unwrap_or(Path::new("/"));
    // `source` is relative to the config file and resolved physically, as
    // install.sh's `pwd -P` did (test_install.py:130).
    let source = match &raw.source {
        Some(s) => {
            let joined = base.join(s);
            Some(fs::canonicalize(&joined).map_err(|e| Error::Usage(format!("{}: {e}", joined.display())))?)
        }
        None if !raw.loose.is_empty() || raw.homes.iter().any(|h| !h.links.is_empty()) => {
            return Err(Error::Usage(format!("{}: loose and links are relative to source; set it", path.display())));
        }
        None => None,
    };
    let home = home_dir()?;
    let mut homes: Vec<Home> = Vec::new();
    for h in raw.homes {
        if homes.iter().any(|o| o.name == h.name) {
            return Err(Error::Usage(format!("{}: two homes are named {:?}", path.display(), h.name)));
        }
        // A relative `path` is relative to this file, like `source`; Pi
        // resolves its own variable against the cwd, so mazo does too.
        let path = match (h.agent, std::env::var("PI_CODING_AGENT_DIR").ok().filter(|v| !v.is_empty())) {
            (Agent::Pi, Some(env)) => {
                let cwd = std::env::current_dir().map_err(|e| format!("current directory: {e}"))?;
                pi_agent_dir(&env, &home, &cwd)?
            }
            (Agent::Pi, None) => pi_agent_dir(&h.path, &home, base)?,
            _ => resolve(base, &expand_tilde(&h.path, &home)),
        };
        homes.push(Home { name: h.name, agent: h.agent, path, links: h.links });
    }
    Ok(Some(Config { source, loose: raw.loose, trusted: raw.trusted, homes, policy: raw.policy, judge: raw.judge }))
}

/// `mazo init`: a config at `path` naming `source`, if given, absolutely, a
/// home for each agent directory under `~` that exists, every rule off, and
/// the [`detect_judge`] judge, else the one [`choose_judge`] reads from `ask`.
/// Refuses to overwrite, before detecting or asking.
pub fn init(path: &Path, source: Option<&Path>, ask: Option<&mut dyn BufRead>) -> Result<Option<InitJudge>> {
    let exists = || format!("{} exists; not overwriting it", path.display());
    if path.symlink_metadata().is_ok() {
        return Err(exists().into());
    }
    let source = match source {
        Some(source) => {
            let abs = fs::canonicalize(source).map_err(|e| format!("{}: {e}", source.display()))?;
            Some(abs.to_str().ok_or_else(|| format!("{}: not UTF-8", abs.display()))?.to_string())
        }
        None => None,
    };
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
    let judge = match (detect_judge()?, ask) {
        (Some(agent), _) => Some(InitJudge::Detected(agent)),
        (None, Some(input)) => choose_judge(input, &mut std::io::stderr())?.map(InitJudge::Chosen),
        (None, None) => None,
    };
    let text = config_text(source, homes, judge.map(|(InitJudge::Detected(a) | InitJudge::Chosen(a))| a))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| match e.kind() {
        ErrorKind::AlreadyExists => exists(),
        _ => format!("{}: {e}", path.display()),
    })?;
    file.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(judge)
}

/// The judge `init` wrote: one it found logged in, or one the user chose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InitJudge {
    Detected(JudgeAgent),
    Chosen(JudgeAgent),
}

/// The line `init` prints after the config path: the judge it wrote and
/// what is left to do before `mazo audit` can use it.
pub fn init_note(judge: Option<InitJudge>) -> String {
    let name = |a: JudgeAgent| JUDGE_AGENTS.iter().find(|(_, b)| *b == a).map_or("", |(name, _)| *name);
    match judge {
        Some(InitJudge::Chosen(a @ (JudgeAgent::Command | JudgeAgent::Http))) => format!(
            "the [judge] table for agent = \"{}\" is commented out: fill in its placeholders and uncomment it",
            name(a)
        ),
        Some(InitJudge::Chosen(a)) => {
            format!(
                "judge: {0}, which was not found on PATH or not logged in; log in to {0} before mazo audit",
                name(a)
            )
        }
        Some(InitJudge::Detected(a)) => format!("judge: {}", name(a)),
        None => "no judge: log in to claude, codex or pi and add [judge] agent = \"<agent>\" to the config, \
                 or a command or http judge (see the README)"
            .into(),
    }
}

/// Each judge agent by its config name, in the order `init` tries them.
pub const JUDGE_AGENTS: [(&str, JudgeAgent); 5] = [
    ("claude", JudgeAgent::Claude),
    ("codex", JudgeAgent::Codex),
    ("pi", JudgeAgent::Pi),
    ("command", JudgeAgent::Command),
    ("http", JudgeAgent::Http),
];

/// The `[judge]` tables `init` writes commented out, since they are invalid
/// until filled in.
const COMMAND_PLACEHOLDER: &str = "\n# [judge]\n# agent = \"command\"\n# argv = [\"<program>\", \"<argument>\"]\n";
const HTTP_PLACEHOLDER: &str = "\n# [judge]\n# agent = \"http\"\n# url = \"<base url; mazo posts to <url>/chat/completions>\"\n\
# model = \"<model>\"\n# api_key_env = \"<variable holding the API key; delete for none>\"\n";

/// The first of claude, codex and pi whose binary is on `PATH` and whose
/// subscription login check passes; Pi has no check. The checks read local
/// state only: `claude auth status` and Codex's `auth.json`. A check's
/// scratch directory is a fresh one under the temporary directory, removed
/// after, so detection leaves nothing behind.
fn detect_judge() -> Result<Option<JudgeAgent>> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let scratch = std::env::temp_dir().join(format!(
        "mazo-init-{}-{}",
        std::process::id(),
        jiff::Timestamp::now().as_nanosecond()
    ));
    DirBuilder::new().mode(0o700).create(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let detected = JUDGE_AGENTS[..3].iter().find_map(|&(name, agent)| {
        let found = std::env::split_paths(&path)
            .any(|dir| dir.join(name).metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0));
        let judge = Judge { agent, ..Judge::default() };
        (found && crate::audit::backend(&judge, &[]).and_then(|b| b.login(&scratch, &|| false)).is_ok())
            .then_some(agent)
    });
    fs::remove_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    Ok(detected)
}

/// The judge agent named on a line of `input`, asked for on `prompt` until
/// one is; `None` when `input` ends first.
pub fn choose_judge(input: &mut dyn BufRead, prompt: &mut dyn Write) -> Result<Option<JudgeAgent>> {
    let names: Vec<&str> = JUDGE_AGENTS.iter().map(|(name, _)| *name).collect();
    let ask = |prompt: &mut dyn Write| {
        write!(prompt, "no claude, codex or pi login found; judge for mazo audit [{}]: ", names.join("/"))
            .and_then(|()| prompt.flush())
            .map_err(|e| format!("stderr: {e}"))
    };
    loop {
        ask(prompt)?;
        let mut answer = String::new();
        if input.read_line(&mut answer).map_err(|e| format!("stdin: {e}"))? == 0 {
            return Ok(None);
        }
        if let Some((_, agent)) = JUDGE_AGENTS.iter().find(|(name, _)| *name == answer.trim()) {
            return Ok(Some(*agent));
        }
    }
}

/// The config `init` writes: `judge` as a `[judge]` table, or for `command`
/// and `http` as a commented-out one to fill in.
fn config_text(source: Option<String>, homes: Vec<RawHome>, judge: Option<JudgeAgent>) -> Result<String> {
    let table = judge
        .filter(|a| !matches!(a, JudgeAgent::Command | JudgeAgent::Http))
        .map(|agent| Judge { agent, ..Judge::default() });
    let raw =
        RawConfig { source, loose: Vec::new(), trusted: Vec::new(), policy: Policy::default(), judge: table, homes };
    let mut text = toml::to_string(&raw).map_err(|e| e.to_string())?;
    match judge {
        Some(JudgeAgent::Command) => text.push_str(COMMAND_PLACEHOLDER),
        Some(JudgeAgent::Http) => text.push_str(HTTP_PLACEHOLDER),
        _ => {}
    }
    Ok(text)
}

pub(crate) fn home_dir() -> Result<String> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty()).ok_or_else(|| "HOME is not set".into())
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

/// The Pi agent directory from a `PI_CODING_AGENT_DIR`-style value.
/// The skills directory is `<result>/skills`.
fn pi_agent_dir(value: &str, home: &str, cwd: &Path) -> Result<PathBuf> {
    let invalid = |reason: &str| Error::Usage(format!("invalid PI_CODING_AGENT_DIR: {reason}"));
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

    fn judge(table: &str) -> std::result::Result<Option<Judge>, String> {
        toml::from_str::<RawConfig>(&format!("source = \".\"\n{table}")).map(|r| r.judge).map_err(|e| e.to_string())
    }

    #[test]
    fn no_judge_table_is_no_judge() {
        assert!(judge("").unwrap().is_none());
    }

    #[test]
    fn judge_table_takes_agent_model_and_billing_only() {
        let judge = |table: &str| judge(table).map(|j| j.expect("a [judge] table is a judge"));
        let j = judge("[judge]\n").unwrap();
        assert_eq!((j.agent, j.model, j.billing), (JudgeAgent::Claude, None, Billing::Subscription));
        assert_eq!(judge("[judge]\nbilling = \"subscription\"\n").unwrap().billing, Billing::Subscription);
        assert_eq!(judge("[judge]\nbilling = \"api\"\n").unwrap().billing, Billing::Api);
        for bad in ["\"API\"", "\"apikey\"", "\"\"", "true"] {
            assert!(judge(&format!("[judge]\nbilling = {bad}\n")).is_err(), "{bad}");
        }
        let j = judge("[judge]\nagent = \"codex\"\nmodel = \"gpt-5\"\n").unwrap();
        assert_eq!((j.agent, j.model.as_deref()), (JudgeAgent::Codex, Some("gpt-5")));
        assert_eq!(judge("[judge]\nagent = \"pi\"\n").unwrap().agent, JudgeAgent::Pi);
        let err = judge("[judge]\nmodel = \"sonnet\"\ntemperature = 1\n").unwrap_err();
        assert!(err.contains("unknown field `temperature`"), "{err}");
        assert!(judge("[judge]\nagent = \"gemini\"\n").is_err());
    }

    #[test]
    fn trusted_takes_owners_and_repos_only() {
        let tmp = tempfile::tempdir().unwrap();
        let load = |trusted: &str| {
            let path = tmp.path().join("config.toml");
            fs::write(&path, format!("source = \".\"\n{trusted}")).unwrap();
            load(&path).map(|c| c.is_some())
        };
        for ok in ["", "trusted = []\n", "trusted = [\"owner\", \"owner/repo\"]\n"] {
            assert!(load(ok).unwrap(), "{ok:?}");
        }
        for bad in ["", "/", "owner/", "/repo", "owner//repo", "owner/repo/skill"] {
            let err = load(&format!("trusted = [\"owner\", {bad:?}]\n")).unwrap_err();
            assert!(matches!(&err, Error::Usage(m) if m.contains(&format!("{bad:?}"))), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_judge_table_round_trips_through_serialize_and_load() {
        let tables = [
            "[judge]\nagent = \"claude\"\nbilling = \"subscription\"\n",
            "[judge]\nagent = \"codex\"\nmodel = \"gpt-5\"\nbilling = \"api\"\n",
            "[judge]\nagent = \"command\"\nbilling = \"subscription\"\nargv = [\"llm\", \"-m\", \"x\"]\n",
            "[judge]\nagent = \"http\"\nmodel = \"m\"\nbilling = \"api\"\nurl = \"http://h/v1\"\napi_key_env = \"K\"\n",
        ];
        for table in tables {
            let raw: RawConfig = toml::from_str(&format!("source = \".\"\n{table}")).unwrap();
            let text = toml::to_string(&raw).unwrap();
            assert!(text.contains(table), "{text}");
            let again: RawConfig = toml::from_str(&text).unwrap();
            assert_eq!(toml::to_string(&again).unwrap(), text);
        }
    }

    #[test]
    fn a_command_judge_takes_an_argv_and_no_other_judge_does() {
        let dir = tempfile::tempdir().unwrap();
        let load_judge = |table: &str| {
            let path = dir.path().join("config.toml");
            fs::write(&path, format!("source = \".\"\n{table}")).unwrap();
            load(&path).map(|c| c.unwrap().judge.unwrap())
        };
        let j = load_judge("[judge]\nagent = \"command\"\nargv = [\"llm\", \"-m\", \"x\"]\n").unwrap();
        assert_eq!((j.agent, j.argv), (JudgeAgent::Command, vec!["llm".into(), "-m".into(), "x".into()]));
        assert!(load_judge("[judge]\nagent = \"claude\"\n").unwrap().argv.is_empty());
        let usage = |table: &str| match load_judge(table) {
            Err(Error::Usage(e)) => e,
            other => panic!("{table:?}: {other:?}"),
        };
        for table in ["[judge]\nagent = \"command\"\n", "[judge]\nagent = \"command\"\nargv = []\n"] {
            assert!(usage(table).ends_with("[judge] agent = \"command\" needs a non-empty argv"), "{table:?}");
        }
        for table in [
            "[judge]\nargv = [\"llm\"]\n",
            "[judge]\nagent = \"pi\"\nargv = [\"llm\"]\n",
            "[judge]\nagent = \"http\"\nurl = \"u\"\nmodel = \"m\"\nargv = [\"llm\"]\n",
        ] {
            assert!(usage(table).ends_with("[judge] argv is for agent = \"command\" only"), "{table:?}");
        }
        assert!(usage("[judge]\nagent = \"command\"\nargv = \"llm\"\n").contains("invalid type"));
    }

    #[test]
    fn an_http_judge_takes_a_url_a_model_and_an_api_key_env_and_no_other_judge_does() {
        let dir = tempfile::tempdir().unwrap();
        let load_judge = |table: &str| {
            let path = dir.path().join("config.toml");
            fs::write(&path, format!("source = \".\"\n{table}")).unwrap();
            load(&path).map(|c| c.unwrap().judge.unwrap())
        };
        let j = load_judge("[judge]\nagent = \"http\"\nurl = \"http://h/v1\"\nmodel = \"m\"\napi_key_env = \"K\"\n")
            .unwrap();
        assert_eq!(
            (j.agent, j.url.as_deref(), j.model.as_deref(), j.api_key_env.as_deref()),
            (JudgeAgent::Http, Some("http://h/v1"), Some("m"), Some("K"))
        );
        assert_eq!(load_judge("[judge]\nagent = \"http\"\nurl = \"u\"\nmodel = \"m\"\n").unwrap().api_key_env, None);
        let usage = |table: &str| match load_judge(table) {
            Err(Error::Usage(e)) => e,
            other => panic!("{table:?}: {other:?}"),
        };
        for table in [
            "[judge]\nagent = \"http\"\n",
            "[judge]\nagent = \"http\"\nmodel = \"m\"\n",
            "[judge]\nagent = \"http\"\nurl = \"u\"\n",
            "[judge]\nagent = \"http\"\nurl = \"\"\nmodel = \"m\"\n",
            "[judge]\nagent = \"http\"\nurl = \"u\"\nmodel = \"\"\n",
        ] {
            assert!(usage(table).ends_with("[judge] agent = \"http\" needs a url and a model"), "{table:?}");
        }
        let blank = usage("[judge]\nagent = \"http\"\nurl = \"u\"\nmodel = \"m\"\napi_key_env = \"\"\n");
        assert!(blank.ends_with("[judge] api_key_env must name a variable"), "{blank}");
        for table in [
            "[judge]\nurl = \"u\"\n",
            "[judge]\nagent = \"codex\"\napi_key_env = \"K\"\n",
            "[judge]\nagent = \"command\"\nargv = [\"llm\"]\nurl = \"u\"\n",
        ] {
            assert!(usage(table).ends_with("[judge] url and api_key_env are for agent = \"http\" only"), "{table:?}");
        }
    }

    #[test]
    fn choose_judge_asks_until_a_line_names_an_agent() {
        for (name, agent) in JUDGE_AGENTS {
            let mut prompt = Vec::new();
            let chosen = choose_judge(&mut format!("gpt\n\n {name} \n").as_bytes(), &mut prompt).unwrap();
            assert_eq!(chosen, Some(agent), "{name}");
            let ask = "no claude, codex or pi login found; judge for mazo audit [claude/codex/pi/command/http]: ";
            assert_eq!(String::from_utf8(prompt).unwrap(), ask.repeat(3), "{name}");
        }
        assert_eq!(choose_judge(&mut "gpt\n".as_bytes(), &mut Vec::new()).unwrap(), None);
    }

    #[test]
    fn init_writes_command_and_http_judges_commented_out_until_filled_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for (_, agent) in JUDGE_AGENTS {
            let text = config_text(None, Vec::new(), Some(agent)).unwrap();
            fs::write(&path, &text).unwrap();
            let judge = load(&path).unwrap().unwrap().judge;
            let placeholder = matches!(agent, JudgeAgent::Command | JudgeAgent::Http);
            assert_eq!(judge.map(|j| j.agent), (!placeholder).then_some(agent), "{text}");
            let commented = text.lines().filter(|l| l.starts_with("# ")).count();
            assert_eq!(commented, [0, 0, 0, 3, 5][agent as usize], "{text}");
            // Uncommented, the placeholders make a valid table of that agent.
            fs::write(&path, text.replace("# ", "")).unwrap();
            assert_eq!(load(&path).unwrap().unwrap().judge.map(|j| j.agent), Some(agent), "{text}");
        }
    }

    #[test]
    fn init_note_warns_that_a_chosen_agent_is_not_logged_in() {
        assert_eq!(init_note(Some(InitJudge::Detected(JudgeAgent::Codex))), "judge: codex");
        assert_eq!(
            init_note(Some(InitJudge::Chosen(JudgeAgent::Codex))),
            "judge: codex, which was not found on PATH or not logged in; log in to codex before mazo audit"
        );
        for agent in [JudgeAgent::Command, JudgeAgent::Http] {
            assert!(init_note(Some(InitJudge::Chosen(agent))).ends_with("fill in its placeholders and uncomment it"));
        }
    }
}
