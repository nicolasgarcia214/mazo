//! A plain Agent Skills directory under the default config: every `[policy]`
//! rule off, and a fresh `HOME` from config discovery
//! through `init` to `sync`.

mod common;

use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::Output;

use common::{Mazo, stderr, stdout};

/// Every configurable rule's violation at once: `disable-model-invocation` in
/// the frontmatter, no `agents/openai.yaml`, no Claude `settings.json`.
const PLAIN_SKILL: &str =
    "---\nname: plain\ndescription: A plain skill.\ndisable-model-invocation: true\n---\n\nBody.\n";

const HOMES: &str = r#"
[[home]]
name  = "claude"
agent = "claude"
path  = "~/.claude"

[[home]]
name  = "codex"
agent = "codex"
path  = "~/.codex"
links = { "extra.md" = "extra.md" }
"#;

struct Tree {
    _temp: tempfile::TempDir,
    home: PathBuf,
    source: PathBuf,
}

impl Tree {
    /// One skill directory holding only `SKILL.md`, and `policy` as the config's
    /// text before the homes.
    fn new(policy: &str) -> Tree {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let (home, source) = (root.join("home"), root.join("skills"));
        fs::create_dir(&home).unwrap();
        common::write(&source.join("plain/SKILL.md"), PLAIN_SKILL);
        common::write(&source.join("extra.md"), "extra\n");
        let tree = Tree { _temp: temp, home, source };
        tree.config(policy);
        tree
    }

    fn config(&self, policy: &str) {
        fs::write(self.source.join("mazo.toml"), format!("source = \".\"\n{policy}\n{HOMES}")).unwrap();
    }

    fn mazo(&self, args: &[&str]) -> Output {
        let mut run = Mazo::new(&self.home, &self.home, &self.source.join("mazo.toml"), &self.home.join("data"));
        run.set("PI_CODING_AGENT_DIR", None).set("PI_CODING_AGENT_SESSION_DIR", None).output(args)
    }

    fn link(&self, home: &str, rel: &str) -> PathBuf {
        fs::read_link(self.home.join(home).join(rel)).unwrap()
    }
}

fn assert_ok(o: &Output) {
    assert!(o.status.success(), "stdout:\n{}\nstderr:\n{}", stdout(o), stderr(o));
}

#[test]
fn a_plain_skill_and_a_claude_home_without_settings_pass_with_every_rule_off() {
    for policy in ["", "[policy]\n"] {
        let t = Tree::new(policy);
        assert_ok(&t.mazo(&["sync"]));
        assert!(!t.home.join(".claude/settings.json").exists());
        for home in [".claude", ".codex"] {
            assert_eq!(t.link(home, "skills/plain"), t.source.join("plain"));
        }
        assert_eq!(t.link(".codex", "extra.md"), t.source.join("extra.md"));
        assert_ok(&t.mazo(&["status"]));
    }
}

#[test]
fn each_rule_alone_rejects_its_own_violation_and_nothing_else() {
    let messages = [
        ("no_disable_model_invocation", "SKILL.md must omit disable-model-invocation"),
        ("codex_manifest", "agents/openai.yaml"),
        ("claude_name_only", "no Claude settings profile"),
    ];
    for (rule, own) in messages {
        let t = Tree::new(&format!("[policy]\n{rule} = true\n"));
        for cmd in ["sync", "status"] {
            let o = t.mazo(&[cmd]);
            assert_eq!(o.status.code(), Some(1), "{rule} {cmd}");
            let err = stderr(&o);
            assert!(err.contains(own), "{rule} {cmd}: {err}");
            for (other, theirs) in messages.iter().filter(|(r, _)| *r != rule) {
                assert!(!err.contains(theirs), "{rule} {cmd} also fired {other}: {err}");
            }
        }
        assert!(!t.home.join(".claude").exists(), "{rule}: sync wrote before failing");
    }
}

#[test]
fn an_unknown_key_anywhere_is_an_error_naming_it() {
    let t = Tree::new("");
    for (config, key) in [
        ("source = \".\"\n[policy]\ncodex_manfest = true\n", "codex_manfest"),
        ("source = \".\"\nsorce = \".\"\n", "sorce"),
        ("source = \".\"\n[[home]]\nname = \"c\"\nagent = \"claude\"\npath = \"~/.c\"\nlink = {}\n", "link"),
        ("source = \".\"\n[polcy]\n", "polcy"),
    ] {
        fs::write(t.source.join("mazo.toml"), config).unwrap();
        for cmd in ["sync", "status"] {
            let o = t.mazo(&[cmd]);
            assert_eq!(o.status.code(), Some(2), "{key}");
            assert!(stderr(&o).contains(&format!("unknown field `{key}`")), "{key}: {}", stderr(&o));
        }
    }
    assert!(!t.home.join(".claude").exists());
}

/// The skill's `SKILL.md` text, written under `dir`.
fn skill(t: &Tree, dir: &str, text: &str) {
    fs::remove_dir_all(&t.source).unwrap();
    common::write(&t.source.join(dir).join("SKILL.md"), text);
    common::write(&t.source.join("extra.md"), "extra\n");
    t.config("");
}

#[test]
fn always_on_rules_hold_with_every_rule_off() {
    let long_name = "a".repeat(65);
    let max_name = "a".repeat(64);
    let fm = |name: &str, description: &str| format!("---\nname: {name}\ndescription: {description}\n---\n");
    let rejected: [(&str, String); 9] = [
        ("plain", fm("other", "D.")),
        ("Plain", fm("Plain", "D.")),
        ("a--b", fm("a--b", "D.")),
        (long_name.as_str(), fm(&long_name, "D.")),
        ("plain", fm("plain", "\"  \"")),
        ("plain", "---\nname: plain\n---\n".into()),
        ("plain", fm("plain", &"é".repeat(1025))),
        ("plain", "---\nname: plain\ndescription: D.\n".into()),
        ("plain", "\n---\nname: plain\ndescription: D.\n---\n".into()),
    ];
    let t = Tree::new("");
    for (dir, text) in &rejected {
        skill(&t, dir, text);
        let o = t.mazo(&["sync"]);
        assert_eq!(o.status.code(), Some(1), "{dir} {text:?}");
        assert!(stderr(&o).contains("ERROR:"), "{dir} {text:?}: {}", stderr(&o));
        assert!(!t.home.join(".claude").exists(), "{dir} {text:?}: sync wrote before failing");
    }

    let accepted = [
        (max_name.as_str(), fm(&max_name, "D.")),
        ("plain", fm("plain", &"é".repeat(1024))),
        ("plain", "\u{feff}---\r\nname: plain\r\ndescription: D.\r\n---\r\nBody.\r\n".into()),
    ];
    for (dir, text) in &accepted {
        skill(&t, dir, text);
        assert_ok(&t.mazo(&["sync", "--dry-run"]));
    }

    skill(&t, "plain", &fm("plain", "D."));
    fs::remove_file(t.source.join("extra.md")).unwrap();
    let o = t.mazo(&["sync"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains(&format!("{} is missing", t.source.join("extra.md").display())), "{}", stderr(&o));
    assert_ok(&t.mazo(&["sync", "--agent", "claude"]));
    assert!(!t.home.join(".codex").exists(), "the links rule applies to the selected homes only");
}

/// A fresh `HOME` with no config, `MAZO_CONFIG` and `XDG_CONFIG_HOME` unset
/// unless a test sets them.
struct Fresh {
    _temp: tempfile::TempDir,
    home: PathBuf,
}

impl Fresh {
    fn new() -> Fresh {
        let temp = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(temp.path()).unwrap();
        Fresh { _temp: temp, home }
    }

    fn mazo(&self, env: &[(&str, &OsStr)], args: &[&str]) -> Output {
        let mut run = Mazo::new(&self.home, &self.home, &self.home.join("mazo.toml"), &self.home.join("data"));
        for key in ["MAZO_CONFIG", "XDG_CONFIG_HOME", "PI_CODING_AGENT_DIR", "PI_CODING_AGENT_SESSION_DIR"] {
            run.set(key, None);
        }
        run.command(env, args).output().unwrap()
    }

    /// `mazo init args…` with only `~/bin`, where a test writes its stub
    /// agents, on `PATH`.
    fn init(&self, env: &[(&str, &OsStr)], args: &[&str]) -> Output {
        let bin = self.home.join("bin");
        let env: Vec<_> = env.iter().copied().chain([("PATH", bin.as_os_str())]).collect();
        self.mazo(&env, &[&["init"], args].concat())
    }

    /// `~/skills/plain/SKILL.md` with `name` and `description` only.
    fn plain_skill(&self) -> PathBuf {
        let source = self.home.join("skills");
        common::write(&source.join("plain/SKILL.md"), "---\nname: plain\ndescription: A plain skill.\n---\n");
        source
    }

    /// Each discovery case: the environment, and the config path it selects.
    fn discovery(&self) -> [(Vec<(&'static str, PathBuf)>, PathBuf); 3] {
        let (explicit, xdg) = (self.home.join("explicit.toml"), self.home.join("xdg"));
        [
            (vec![], self.home.join(".config/mazo/config.toml")),
            (vec![("XDG_CONFIG_HOME", xdg.clone())], xdg.join("mazo/config.toml")),
            (vec![("XDG_CONFIG_HOME", xdg), ("MAZO_CONFIG", explicit.clone())], explicit),
        ]
    }
}

fn env_refs<'a>(env: &'a [(&'static str, PathBuf)]) -> Vec<(&'static str, &'a OsStr)> {
    env.iter().map(|(k, v)| (*k, v.as_os_str())).collect()
}

#[test]
fn a_missing_config_exits_2_naming_the_discovered_path() {
    let f = Fresh::new();
    // The removed default is not a fallback.
    common::write(&f.home.join("dotfiles/skills/mazo.toml"), "source = \".\"\n");
    for (env, path) in f.discovery() {
        for cmd in ["status", "sync", "usage", "prune"] {
            let o = f.mazo(&env_refs(&env), &[cmd]);
            assert_eq!(o.status.code(), Some(2), "{cmd} {env:?}");
            let want = format!(
                "ERROR: no config at {}; run mazo init (--source <skills dir> for your own skills)\n",
                path.display()
            );
            assert_eq!(stderr(&o), want, "{cmd} {env:?}");
        }
    }
}

#[test]
fn init_writes_the_discovered_path_and_only_existing_homes() {
    let f = Fresh::new();
    let source = f.plain_skill();
    for (env, path) in f.discovery() {
        // A relative `--source` is written absolute.
        assert_ok(&f.init(&env_refs(&env), &["--source", "skills"]));
        let v: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(v["source"].as_str(), Some(source.to_str().unwrap()), "{env:?}");
    }

    fs::create_dir(f.home.join(".claude")).unwrap();
    fs::create_dir_all(f.home.join(".pi/agent")).unwrap();
    let path = f.home.join("homes.toml");
    let env = [("MAZO_CONFIG", path.as_os_str())];
    assert_ok(&f.init(&env, &["--source", "skills"]));
    let text = fs::read_to_string(&path).unwrap();
    let v: toml::Table = text.parse().unwrap();
    assert_eq!(v.keys().collect::<Vec<_>>(), ["home", "loose", "policy", "source"], "{text}");
    assert_eq!(v["loose"], toml::Value::Array(vec![]), "{text}");
    let mut policy = v["policy"].as_table().unwrap().clone();
    assert_eq!(policy.remove("authoring_skills"), Some(toml::Value::Array(vec![])), "{text}");
    assert_eq!(
        policy.keys().collect::<Vec<_>>(),
        ["claude_name_only", "codex_manifest", "no_disable_model_invocation"]
    );
    assert!(policy.values().all(|b| *b == toml::Value::Boolean(false)), "{text}");
    let homes: Vec<[&str; 3]> = v["home"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| [&h["name"], &h["agent"], &h["path"]].map(|s| s.as_str().unwrap()))
        .collect();
    assert_eq!(homes, [["claude", "claude", "~/.claude"], ["pi", "pi", "~/.pi/agent"]], "{text}");

    fs::write(&path, "mine\n").unwrap();
    let o = f.init(&env, &["--source", "skills"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("not overwriting"), "{}", stderr(&o));
    assert_eq!(fs::read_to_string(&path).unwrap(), "mine\n");
}

/// `~/bin/<name>`, a stub agent that prints `out` whatever it is asked.
fn stub(f: &Fresh, name: &str, out: &str) {
    let path = f.home.join("bin").join(name);
    common::write(&path, &format!("#!/bin/sh\necho '{out}'\n"));
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// The `[judge]` table of the config `init` wrote at `path`, if any.
fn judge(path: &std::path::Path) -> Option<toml::Value> {
    fs::read_to_string(path).unwrap().parse::<toml::Table>().unwrap().remove("judge")
}

#[test]
fn init_writes_the_first_of_claude_codex_and_pi_that_is_logged_in() {
    const CLAUDE_AI: &str = r#"{"authMethod":"claude.ai"}"#;
    // Each case: the stubs on PATH with what claude auth status prints, whether
    // ~/.codex/auth.json holds a ChatGPT login, and the judge written.
    let cases = [
        (&[("claude", CLAUDE_AI), ("codex", ""), ("pi", "")][..], true, "claude"),
        (&[("codex", ""), ("pi", "")], true, "codex"),
        (&[("claude", r#"{"authMethod":"api_key"}"#), ("codex", ""), ("pi", "")], true, "codex"),
        (&[("claude", "not json"), ("codex", ""), ("pi", "")], false, "pi"),
        (&[("pi", "")], false, "pi"),
        (&[("claude", CLAUDE_AI)], false, "claude"),
    ];
    for (stubs, codex_login, want) in cases {
        let f = Fresh::new();
        for (name, out) in stubs {
            stub(&f, name, out);
        }
        if codex_login {
            common::write(&f.home.join(".codex/auth.json"), r#"{"auth_mode":"chatgpt"}"#);
        }
        let o = f.init(&[], &[]);
        assert_ok(&o);
        assert_eq!(stdout(&o).lines().nth(1), Some(&*format!("judge: {want}")), "{stubs:?}");
        let path = f.home.join(".config/mazo/config.toml");
        let judge = judge(&path).unwrap_or_else(|| panic!("{stubs:?}: no [judge]"));
        assert_eq!(judge["agent"].as_str(), Some(want), "{stubs:?}");
        // The written config loads.
        assert_ok(&f.mazo(&[], &["status"]));
    }
}

#[test]
fn init_without_a_judge_or_a_terminal_writes_no_judge_and_says_how_to_add_one() {
    let f = Fresh::new();
    // A codex without a ChatGPT login is not a judge.
    stub(&f, "codex", "");
    let o = f.init(&[], &[]);
    assert_ok(&o);
    assert!(judge(&f.home.join(".config/mazo/config.toml")).is_none());
    assert_eq!(
        stdout(&o).lines().nth(1),
        Some(
            "no judge: log in to claude, codex or pi and add [judge] agent = \"<agent>\" to the config, \
             or a command or http judge (see the README)"
        )
    );
}

#[test]
fn a_symlinked_config_resolves_source_against_the_physical_file() {
    let f = Fresh::new();
    let source = f.plain_skill();
    fs::create_dir(f.home.join(".codex")).unwrap();
    common::write(
        &source.join("mazo.toml"),
        "source = \".\"\n[[home]]\nname = \"codex\"\nagent = \"codex\"\npath = \"~/.codex\"\n",
    );
    fs::create_dir_all(f.home.join(".config/mazo")).unwrap();
    std::os::unix::fs::symlink(source.join("mazo.toml"), f.home.join(".config/mazo/config.toml")).unwrap();
    assert_ok(&f.mazo(&[], &["sync"]));
    assert_eq!(fs::read_link(f.home.join(".codex/skills/plain")).unwrap(), source.join("plain"));
}

#[test]
fn a_fresh_home_runs_init_then_sync() {
    let f = Fresh::new();
    let source = f.plain_skill();
    common::write(&f.home.join(".claude/settings.json"), "");
    assert_ok(&f.init(&[], &["--source", source.to_str().unwrap()]));
    assert_ok(&f.mazo(&[], &["sync"]));
    assert_eq!(fs::read_link(f.home.join(".claude/skills/plain")).unwrap(), source.join("plain"));
    assert_ok(&f.mazo(&[], &["status"]));
}

#[test]
fn relative_home_paths_resolve_against_the_config_file_not_the_cwd() {
    let f = Fresh::new();
    let source = f.plain_skill();
    let config = source.join("mazo.toml");
    common::write(
        &config,
        "source = \".\"\n\
         [[home]]\nname = \"codex\"\nagent = \"codex\"\npath = \"../agents/codex\"\n\
         [[home]]\nname = \"pi\"\nagent = \"pi\"\npath = \"../agents/pi\"\n",
    );
    assert_ok(&f.mazo(&[("MAZO_CONFIG", config.as_os_str())], &["sync"]));
    for agent in ["codex", "pi"] {
        let link = f.home.join("agents").join(agent).join("skills/plain");
        assert_eq!(fs::read_link(&link).unwrap(), source.join("plain"), "{agent}");
    }
}

#[test]
fn two_homes_with_one_name_are_an_error() {
    let f = Fresh::new();
    let source = f.plain_skill();
    let config = source.join("mazo.toml");
    let home = "[[home]]\nname = \"codex\"\nagent = \"codex\"\npath = \"~/.codex\"\n";
    common::write(&config, &format!("source = \".\"\n{home}{home}"));
    let o = f.mazo(&[("MAZO_CONFIG", config.as_os_str())], &["status"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("two homes are named \"codex\""), "{}", stderr(&o));
}
