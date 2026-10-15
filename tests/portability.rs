//! A plain Agent Skills directory under the default config: every `[policy]`
//! rule off (SPEC D9, §5.4, §10.8).

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

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
        fs::write(self.source.join("skima.toml"), format!("source = \".\"\n{policy}\n{HOMES}")).unwrap();
    }

    fn skima(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_skima"))
            .args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("SKIMA_CONFIG", self.source.join("skima.toml"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("PI_CODING_AGENT_SESSION_DIR")
            .output()
            .unwrap()
    }

    fn link(&self, home: &str, rel: &str) -> PathBuf {
        fs::read_link(self.home.join(home).join(rel)).unwrap()
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn assert_ok(o: &Output) {
    assert!(o.status.success(), "stdout:\n{}\nstderr:\n{}", String::from_utf8_lossy(&o.stdout), stderr(o));
}

#[test]
fn a_plain_skill_and_a_claude_home_without_settings_pass_with_every_rule_off() {
    for policy in ["", "[policy]\n"] {
        let t = Tree::new(policy);
        assert_ok(&t.skima(&["sync"]));
        assert!(!t.home.join(".claude/settings.json").exists());
        for home in [".claude", ".codex"] {
            assert_eq!(t.link(home, "skills/plain"), t.source.join("plain"));
        }
        assert_eq!(t.link(".codex", "extra.md"), t.source.join("extra.md"));
        assert_ok(&t.skima(&["status"]));
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
            let o = t.skima(&[cmd]);
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
        fs::write(t.source.join("skima.toml"), config).unwrap();
        for cmd in ["sync", "status"] {
            let o = t.skima(&[cmd]);
            assert_eq!(o.status.code(), Some(1), "{key}");
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
        let o = t.skima(&["sync"]);
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
        assert_ok(&t.skima(&["sync", "--dry-run"]));
    }

    skill(&t, "plain", &fm("plain", "D."));
    fs::remove_file(t.source.join("extra.md")).unwrap();
    let o = t.skima(&["sync"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains(&format!("{} is missing", t.source.join("extra.md").display())), "{}", stderr(&o));
    assert_ok(&t.skima(&["sync", "--agent", "claude"]));
    assert!(!t.home.join(".codex").exists(), "the links rule applies to the selected homes only");
}
