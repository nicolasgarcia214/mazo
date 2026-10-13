//! Shared fixture for `tests/sync_port.rs` and `tests/sync.rs` (SPEC §5.7 step 3):
//! a temporary skills source with a `skima.toml` listing four `~`-relative homes,
//! a temporary `HOME`, a Pi directory, and a private `XDG_DATA_HOME`.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::common::real_skills;

pub const PI_EXTENSION: &str = "pi-explicit-skills.ts";

const SKIMA_TOML: &str = r#"source = "."
loose  = ["SKILLS_USAGE.md", "GOAL_SKILLS_USAGE.md"]

[[home]]
name  = "claude"
agent = "claude"
path  = "~/.claude"

[[home]]
name  = "claude-work"
agent = "claude"
path  = "~/.claude-work"

[[home]]
name  = "codex"
agent = "codex"
path  = "~/.codex"

[[home]]
name  = "pi"
agent = "pi"
path  = "~/.pi/agent"
links = { "extensions/pi-explicit-skills.ts" = "pi-explicit-skills.ts" }
"#;

/// `PI_SKILLS`: sorted non-hidden child directories of [`real_skills`] with a
/// `SKILL.md` (test_install.py:12-16 read its own parent).
pub fn pi_skills() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(real_skills())
        .expect("SKIMA_TEST_SKILLS is readable")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && real_skills().join(n).join("SKILL.md").is_file())
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no skills under {}", real_skills().display());
    names
}

pub struct Fixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub source: PathBuf,
    pub agent: PathBuf,
    pub target: PathBuf,
    pub home: PathBuf,
    pub data: PathBuf,
    pub skills: Vec<String>,
    overrides: Vec<String>,
    /// Environment overrides; `None` removes the variable.
    pub env: BTreeMap<String, Option<String>>,
}

impl Fixture {
    pub fn new() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        // The links name the physical source, so compare against a physical root.
        let root = fs::canonicalize(temp.path()).unwrap();
        let source = root.join("repo with spaces").join("skills");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("skima.toml"), SKIMA_TOML).unwrap();
        fs::copy(real_skills().join(PI_EXTENSION), source.join(PI_EXTENSION)).unwrap();
        let agent = root.join("pi agent");
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        let data = root.join("xdg data");
        let mut env = BTreeMap::new();
        env.insert("HOME".into(), Some(s(&home)));
        env.insert("PI_CODING_AGENT_DIR".into(), Some(s(&agent)));
        env.insert("SKIMA_CONFIG".into(), Some(s(&source.join("skima.toml"))));
        env.insert("XDG_DATA_HOME".into(), Some(s(&data)));
        env.insert("PI_CODING_AGENT_SESSION_DIR".into(), None);
        let mut fx = Fixture {
            target: agent.join("skills"),
            _temp: temp,
            root,
            source,
            agent,
            home,
            data,
            skills: pi_skills(),
            overrides: Vec::new(),
            env,
        };
        for name in fx.skills.clone() {
            fx.add_skill(&name);
        }
        fx
    }

    pub fn add_skill(&mut self, name: &str) -> PathBuf {
        let skill = self.source.join(name);
        fs::create_dir_all(skill.join("agents")).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test skill.\n---\n\n# Test skill\n"),
        )
        .unwrap();
        fs::write(skill.join("agents/openai.yaml"), "policy:\n  allow_implicit_invocation: false\n").unwrap();
        self.overrides.push(name.to_string());
        self.write_settings(&self.home.clone());
        skill
    }

    /// The valid settings text: every added skill is `"name-only"`.
    pub fn valid_settings(&self) -> String {
        let map: serde_json::Map<String, serde_json::Value> =
            self.overrides.iter().map(|n| (n.clone(), "name-only".into())).collect();
        serde_json::json!({ "skillOverrides": map }).to_string()
    }

    /// Writes valid `settings.json` into both Claude homes under `home`.
    pub fn write_settings(&self, home: &Path) {
        for dir in [".claude", ".claude-work"] {
            fs::create_dir_all(home.join(dir)).unwrap();
            fs::write(home.join(dir).join("settings.json"), self.valid_settings()).unwrap();
        }
    }

    pub fn settings(&self, dir: &str) -> PathBuf {
        self.home.join(dir).join("settings.json")
    }

    pub fn set_env(&mut self, key: &str, value: Option<&Path>) {
        self.env.insert(key.into(), value.map(s));
    }

    pub fn skima(&self, args: &[&str]) -> Run {
        self.skima_in(&self.root, args)
    }

    pub fn skima_in(&self, cwd: &Path, args: &[&str]) -> Run {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_skima"));
        cmd.args(args).current_dir(cwd);
        for (k, v) in &self.env {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        Run(cmd.output().unwrap())
    }

    /// `skima sync --agent pi` must fail with an `ERROR:` before creating the
    /// Pi directory (test_install.py:59-63).
    pub fn assert_rejected_before_writes(&self) {
        let r = self.skima(&["sync", "--agent", "pi"]);
        assert_ne!(r.code(), 0, "{}", r.stdout());
        assert!(r.stderr().contains("ERROR:"), "{}", r.stderr());
        assert!(!exists(&self.agent), "{}", r.stdout());
    }

    /// A snapshot of `$HOME`, the Pi directory and the data directory.
    pub fn snapshot(&self) -> Snapshot {
        snapshot(&[&self.home, &self.agent, &self.data])
    }
}

pub struct Run(pub Output);

impl Run {
    pub fn code(&self) -> i32 {
        self.0.status.code().expect("exited")
    }
    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.0.stdout).into_owned()
    }
    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.0.stderr).into_owned()
    }
    pub fn assert_ok(&self) -> &Run {
        assert_eq!(self.code(), 0, "stdout:\n{}\nstderr:\n{}", self.stdout(), self.stderr());
        self
    }
}

/// Every path under the roots, with its type, link text, and the SHA-1 of every
/// regular file. A missing root is recorded as missing.
pub type Snapshot = BTreeMap<PathBuf, String>;

pub fn snapshot(roots: &[&Path]) -> Snapshot {
    let mut out = Snapshot::new();
    for root in roots {
        walk(root, &mut out);
    }
    out
}

fn walk(path: &Path, out: &mut Snapshot) {
    let Ok(m) = fs::symlink_metadata(path) else {
        out.insert(path.to_path_buf(), "absent".into());
        return;
    };
    let ft = m.file_type();
    let entry = if ft.is_symlink() {
        format!("link {}", fs::read_link(path).unwrap().display())
    } else if ft.is_file() {
        format!("file {}", sha1_smol::Sha1::from(fs::read(path).unwrap()).digest())
    } else if ft.is_dir() {
        "dir".to_string()
    } else {
        format!("other {:o}", m.mode())
    };
    out.insert(path.to_path_buf(), entry);
    if ft.is_dir() {
        let mut children: Vec<PathBuf> = fs::read_dir(path).unwrap().flatten().map(|e| e.path()).collect();
        children.sort();
        for c in children {
            walk(&c, out);
        }
    }
}

/// `lstat` existence: a dangling link exists.
pub fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

pub fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

pub fn ino(p: &Path) -> u64 {
    fs::symlink_metadata(p).unwrap().ino()
}

pub fn s(p: &Path) -> String {
    p.to_str().expect("utf-8 path").to_string()
}

/// Python's `Path.as_uri()`: `file://` plus the percent-encoded path.
pub fn as_uri(p: &Path) -> String {
    let mut out = String::from("file://");
    for b in s(p).bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `os.path.relpath(path, start)` for absolute, lexically clean paths.
pub fn relpath(path: &Path, start: &Path) -> String {
    let p: Vec<_> = path.components().collect();
    let b: Vec<_> = start.components().collect();
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".into(); b.len() - common];
    parts.extend(p[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() { ".".into() } else { parts.join("/") }
}
