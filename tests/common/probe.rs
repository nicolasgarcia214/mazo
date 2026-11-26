use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::{Mazo, stderr, stdout, write};

const PROBE_EXTENSION: &str = r#"import { createFauxCore, fauxAssistantMessage } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  const core = createFauxCore({ api: "probe", provider: "probe", models: [{ id: "probe" }] });
  core.setResponses([fauxAssistantMessage("ok")]);
  pi.registerProvider("probe", {
    api: "probe", apiKey: "unused", streamSimple: core.streamSimple,
    models: core.models.map((model) => ({ ...model, name: model.id })),
  });
}
"#;

pub struct Probe {
    _temp: tempfile::TempDir,
    home: PathBuf,
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub pi: PathBuf,
    pub skill: String,
    work: PathBuf,
    pub extension: PathBuf,
    run: Mazo,
}

impl Probe {
    pub fn new() -> Probe {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let real_home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
        let skill = format!("mazo-probe-{:016x}", RandomState::new().hash_one(&root));
        let [home, claude, codex, pi, source, work] =
            ["home", "claude", "codex", "pi", "skills", "work"].map(|d| root.join(d));
        for dir in [&home, &claude, &codex, &pi, &work] {
            fs::create_dir_all(dir).unwrap();
        }
        symlink(real_home.join(".claude/.credentials.json"), claude.join(".credentials.json")).unwrap();
        symlink(real_home.join(".codex/auth.json"), codex.join("auth.json")).unwrap();

        let dir = source.join(&skill);
        write(
            &dir.join("SKILL.md"),
            &format!(
                "---\nname: {skill}\ndescription: Live test probe. Use only when asked for it by name.\n---\n\n\
                 Reply with exactly `PROBE-OK` and nothing else.\n"
            ),
        );
        write(&dir.join("agents/openai.yaml"), "policy:\n  allow_implicit_invocation: false\n");
        for h in [&claude, &codex, &pi] {
            fs::create_dir_all(h.join("skills")).unwrap();
            symlink(&dir, h.join("skills").join(&skill)).unwrap();
        }
        write(&claude.join("settings.json"), &serde_json::json!({"skillOverrides": {&skill: "name-only"}}).to_string());

        let config = root.join("mazo.toml");
        let homes: String = [("claude", &claude), ("codex", &codex), ("pi", &pi)]
            .iter()
            .map(|(name, path)| format!("\n[[home]]\nname  = \"{name}\"\nagent = \"{name}\"\npath  = {path:?}\n"))
            .collect();
        write(
            &config,
            &format!(
                "source = \"skills\"\n\n[policy]\nno_disable_model_invocation = true\ncodex_manifest = true\n\
                 claude_name_only = true\n{homes}"
            ),
        );
        let extension = root.join("probe.ts");
        write(&extension, PROBE_EXTENSION);

        let mut run = Mazo::new(&work, &home, &config, &root.join("data"));
        run.set("PI_CODING_AGENT_DIR", Some(pi.as_os_str())).set("PI_CODING_AGENT_SESSION_DIR", None);
        Probe { _temp: temp, home, claude, codex, pi, skill, work, extension, run }
    }

    pub fn agent(&self, program: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("CODEX_HOME", &self.codex)
            .env("PI_CODING_AGENT_DIR", &self.pi)
            .env_remove("PI_CODING_AGENT_SESSION_DIR");
        cmd
    }

    fn mazo(&self, args: &[&str]) -> Output {
        let out = self.run.output(args);
        assert!(out.status.success(), "mazo {args:?}\nstdout:\n{}\nstderr:\n{}", stdout(&out), stderr(&out));
        out
    }

    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let args: Vec<&str> = args.iter().copied().chain(["--json"]).collect();
        serde_json::from_str(&stdout(&self.mazo(&args))).unwrap()
    }

    pub fn scan(&self) -> BTreeMap<String, Vec<serde_json::Value>> {
        let report = self.json(&["scan"]);
        let homes = report.as_object().unwrap();
        homes.iter().map(|(name, h)| (name.clone(), h["new_events"].as_array().unwrap().clone())).collect()
    }

    pub fn transcript_has(dir: &Path, needles: &[&str]) -> bool {
        let Ok(entries) = fs::read_dir(dir) else { return false };
        entries.flatten().any(|e| {
            let path = e.path();
            if path.is_dir() {
                return Probe::transcript_has(&path, needles);
            }
            path.extension() == Some(OsStr::new("jsonl"))
                && fs::read_to_string(&path).is_ok_and(|t| t.lines().any(|l| needles.iter().all(|n| l.contains(n))))
        })
    }
}
