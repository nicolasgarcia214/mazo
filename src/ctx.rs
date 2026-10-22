//! The ctx column: what each agent a skill is linked into keeps of it in
//! context, from Claude's `skillOverrides` and the SKILL.md's
//! `disable-model-invocation`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::{Config, Home};
use crate::inventory::{Inventory, Scope};
use crate::ledger::Agent;
use crate::{links, store, yaml};

/// What an agent keeps of a skill in context, the costliest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Invocation {
    /// Its name and description: Claude on, every Codex skill, Pi's default.
    On,
    /// Its name: Claude name-only.
    NameOnly,
    /// Nothing until the user invokes it: Claude user-invocable-only, or
    /// `disable-model-invocation` in Claude or Pi.
    UserInvocableOnly,
    /// Nothing: Claude off.
    Off,
}

impl Invocation {
    pub fn glyph(self) -> char {
        match self {
            Invocation::On => '●',
            Invocation::NameOnly => '◐',
            Invocation::UserInvocableOnly => '○',
            Invocation::Off => '×',
        }
    }

    /// A Claude skill's state under its `skillOverrides` value: `off` wins
    /// over `disable-model-invocation`, which wins over any other value.
    fn claude(value: Option<&str>, disabled: bool) -> Invocation {
        match value {
            Some("off") => Invocation::Off,
            _ if disabled => Invocation::UserInvocableOnly,
            Some("name-only") => Invocation::NameOnly,
            Some("user-invocable-only") => Invocation::UserInvocableOnly,
            _ => Invocation::On,
        }
    }
}

/// Each agent's [`Invocation`] of a skill, for the agents it is linked into only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Ctx {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude: Option<Invocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex: Option<Invocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pi: Option<Invocation>,
}

impl Ctx {
    /// Each linked agent's letter and glyph, in the order C, X, P: `C◐X●`.
    pub fn cell(&self) -> String {
        [(Agent::Claude, self.claude), (Agent::Codex, self.codex), (Agent::Pi, self.pi)]
            .into_iter()
            .filter_map(|(agent, state)| state.map(|s| format!("{}{}", agent.letter(), s.glyph())))
            .collect()
    }
}

/// What every skill's [`Ctx`] reads, read once: the homes each skill is
/// linked into and each Claude home's `skillOverrides`.
pub struct Contexts {
    /// Each skill's linked homes, as `(agent, home)`.
    linked: BTreeMap<String, Vec<(Agent, String)>>,
    /// Each Claude home's `skillOverrides`, `settings.local.json`'s over `settings.json`'s.
    overrides: BTreeMap<String, BTreeMap<String, String>>,
}

impl Contexts {
    pub fn new(cfg: &Config, inv: &Inventory) -> Contexts {
        let homes: Vec<&Home> = cfg.homes.iter().collect();
        let mut linked: BTreeMap<String, Vec<(Agent, String)>> = BTreeMap::new();
        for (home, skill) in links::linked(cfg, inv, &homes) {
            linked.entry(skill).or_default().push((home.agent, home.name.clone()));
        }
        let overrides = homes.iter().filter(|h| h.agent == Agent::Claude).map(|h| (h.name.clone(), overrides(h)));
        Contexts { linked, overrides: overrides.collect() }
    }

    /// `skill`'s ctx in `scope`. An agent takes the costliest state among
    /// its homes. A project skill is linked into every agent, its Claude
    /// state taken over every Claude home.
    pub fn of(&self, inv: &Inventory, skill: &str, scope: Scope) -> Ctx {
        let (dir, homes): (Option<PathBuf>, Vec<(Agent, Option<&str>)>) = match scope {
            Scope::Global => {
                let homes = self.linked.get(skill).into_iter().flatten().map(|(a, h)| (*a, Some(h.as_str())));
                (inv.root(skill).map(|r| r.join(skill)), homes.collect())
            }
            Scope::Project => {
                let claude: Vec<Option<&str>> = self.overrides.keys().map(|h| Some(h.as_str())).collect();
                let claude = if claude.is_empty() { vec![None] } else { claude };
                let homes =
                    claude.into_iter().map(|h| (Agent::Claude, h)).chain([(Agent::Codex, None), (Agent::Pi, None)]);
                (inv.project.as_ref().map(|p| store::project_dir(&p.root).join(skill)), homes.collect())
            }
        };
        let disabled = dir.is_some_and(|d| disables_model_invocation(&d));
        let state = |agent: Agent, home: Option<&str>| match agent {
            Agent::Claude => {
                let value = home.and_then(|h| self.overrides.get(h)?.get(skill)).map(String::as_str);
                Invocation::claude(value, disabled)
            }
            Agent::Codex => Invocation::On,
            Agent::Pi if disabled => Invocation::UserInvocableOnly,
            Agent::Pi => Invocation::On,
        };
        let of = |agent: Agent| homes.iter().filter(|(a, _)| *a == agent).map(|(a, h)| state(*a, *h)).min();
        Ctx { claude: of(Agent::Claude), codex: of(Agent::Codex), pi: of(Agent::Pi) }
    }
}

/// A Claude home's `skillOverrides`, merged per skill with
/// `settings.local.json` winning; a file that is missing or does not parse
/// sets none.
fn overrides(home: &Home) -> BTreeMap<String, String> {
    let read = |path: PathBuf| -> Option<BTreeMap<String, String>> {
        let settings: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let o = settings.get("skillOverrides")?.as_object()?;
        Some(o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
    };
    [home.settings(), home.path.join("settings.local.json")].into_iter().filter_map(read).flatten().collect()
}

/// Whether the SKILL.md in `dir` sets `disable-model-invocation: true`.
fn disables_model_invocation(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("SKILL.md")) else { return false };
    yaml::frontmatter(&text).is_ok_and(|m| yaml::get(&m, "disable-model-invocation").is_some_and(yaml::is_true))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Contexts, Ctx, Invocation};
    use crate::config::{Config, Home};
    use crate::inventory::{Inventory, Scope};
    use crate::ledger::Agent;
    use crate::store::Project;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn skill(dir: &Path, extra: &str) {
        let name = dir.file_name().unwrap().to_str().unwrap();
        write(&dir.join("SKILL.md"), &format!("---\nname: {name}\ndescription: D.\n{extra}---\n"));
    }

    fn home(tmp: &Path, name: &str, agent: Agent) -> Home {
        Home { name: name.into(), agent, path: tmp.join(name), links: Default::default() }
    }

    fn config(source: &Path, homes: Vec<Home>) -> Config {
        Config {
            source: Some(source.into()),
            loose: vec![],
            trusted: vec![],
            evals_dir: Default::default(),
            homes,
            policy: Default::default(),
            judge: None,
        }
    }

    const SKILLS: [&str; 6] = ["plain", "named", "user", "off", "local", "manual"];

    /// Two Claude homes, a Codex home and a Pi home over one source.
    /// `a`'s settings set name-only, user-invocable-only and off, and
    /// `settings.local.json` turns `local` off over its name-only; `b` sets
    /// `named` on and `user` off. `manual` sets disable-model-invocation and
    /// `off` sets it too.
    fn setup(tmp: &Path) -> (Config, Inventory) {
        let source = tmp.join("source");
        for name in SKILLS {
            let extra = if matches!(name, "manual" | "off") { "disable-model-invocation: true\n" } else { "" };
            skill(&source.join(name), extra);
        }
        write(
            &tmp.join("a/settings.json"),
            r#"{"skillOverrides": {"named": "name-only", "user": "user-invocable-only", "off": "off", "local": "name-only", "manual": "name-only"}}"#,
        );
        write(&tmp.join("a/settings.local.json"), r#"{"skillOverrides": {"local": "off"}}"#);
        write(&tmp.join("b/settings.json"), r#"{"skillOverrides": {"named": "on", "user": "off"}}"#);
        let homes = vec![
            home(tmp, "a", Agent::Claude),
            home(tmp, "b", Agent::Claude),
            home(tmp, "codex", Agent::Codex),
            home(tmp, "pi", Agent::Pi),
        ];
        (config(&source, homes), Inventory::load(&source).unwrap())
    }

    #[test]
    fn claude_takes_the_costliest_override_and_codex_and_pi_follow_the_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, inv) = setup(tmp.path());
        // A Codex manifest never changes the Codex state.
        write(&tmp.path().join("source/plain/agents/openai.yaml"), "policy:\n  allow_implicit_invocation: false\n");
        let only_a = Config { homes: cfg.homes.iter().filter(|h| h.name != "b").cloned().collect(), ..cfg.clone() };
        let cells = |cfg: &Config| {
            let ctx = Contexts::new(cfg, &inv);
            SKILLS.map(|s| (s, ctx.of(&inv, s, Scope::Global).cell()))
        };
        assert_eq!(
            cells(&only_a),
            [
                ("plain", "C●X●P●".to_string()),
                ("named", "C◐X●P●".into()),
                ("user", "C○X●P●".into()),
                ("off", "C×X●P○".into()),
                ("local", "C×X●P●".into()),
                ("manual", "C○X●P○".into()),
            ]
        );
        // `b` sets named on, which costs more than a's name-only, and user off, which costs less.
        assert_eq!(cells(&cfg)[1..3], [("named", "C●X●P●".into()), ("user", "C○X●P●".into())]);
    }

    #[test]
    fn only_the_agents_whose_homes_link_it_show() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, inv) = setup(tmp.path());
        let codex = Config { homes: cfg.homes.iter().filter(|h| h.agent == Agent::Codex).cloned().collect(), ..cfg };
        let ctx = Contexts::new(&codex, &inv).of(&inv, "named", Scope::Global);
        assert_eq!(ctx, Ctx { codex: Some(Invocation::On), ..Ctx::default() });
        assert_eq!(serde_json::to_value(&ctx).unwrap(), serde_json::json!({"codex": "on"}));
        let names = ["on", "name-only", "user-invocable-only", "off"];
        let all = [Invocation::On, Invocation::NameOnly, Invocation::UserInvocableOnly, Invocation::Off];
        assert_eq!(all.map(|s| serde_json::to_value(s).unwrap()), names.map(serde_json::Value::from));
        let none = Contexts::new(&codex, &inv).of(&inv, "not-a-skill", Scope::Global);
        assert_eq!(none.cell(), "");
    }

    #[test]
    fn a_settings_file_that_does_not_parse_sets_no_override() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, inv) = setup(tmp.path());
        write(&tmp.path().join("a/settings.json"), "{");
        write(&tmp.path().join("b/settings.json"), r#"{"skillOverrides": ["named"]}"#);
        assert_eq!(Contexts::new(&cfg, &inv).of(&inv, "named", Scope::Global).cell(), "C●X●P●");
        let only_a = Config { homes: cfg.homes.into_iter().filter(|h| h.name != "b").collect(), ..cfg };
        let local = Contexts::new(&only_a, &inv).of(&inv, "local", Scope::Global);
        assert_eq!(local.cell(), "C×X●P●", "settings.local.json still reads");
    }

    #[test]
    fn a_project_skill_is_in_every_agent_with_claude_over_every_claude_home() {
        let tmp = tempfile::tempdir().unwrap();
        let (cfg, inv) = setup(tmp.path());
        let root = tmp.path().join("project");
        write(&root.join("mazo.lock"), "");
        skill(&root.join(".mazo/skills/user"), "");
        skill(&root.join(".mazo/skills/manual"), "disable-model-invocation: true\n");
        let inv = Inventory { project: Project::at(&root).unwrap(), ..inv };
        let ctx = Contexts::new(&cfg, &inv);
        assert_eq!(ctx.of(&inv, "user", Scope::Project).cell(), "C○X●P●");
        assert_eq!(ctx.of(&inv, "manual", Scope::Project).cell(), "C○X●P○");
        let no_claude = Config { homes: vec![], ..cfg };
        assert_eq!(Contexts::new(&no_claude, &inv).of(&inv, "user", Scope::Project).cell(), "C●X●P●");
    }
}
