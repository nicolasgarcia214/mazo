//! The live end-to-end fixture of `tests/live.rs`, built directly and never by
//! `skima sync`: a temporary `HOME` with a Claude, a Codex and a Pi directory,
//! the real Claude and Codex credentials linked in, one probe skill linked into
//! each home's `skills/`, a `skima.toml` with every policy rule on, and Claude
//! settings that set the probe to `"name-only"`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::{Skima, stderr, stdout, write};

/// A Pi extension registering the faux provider `probe`, whose one model
/// answers `ok` without a network call.
pub const PROBE_EXTENSION: &str = r#"import { createFauxCore, fauxAssistantMessage } from "@earendil-works/pi-ai";
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
    /// The temporary `HOME`.
    pub home: PathBuf,
    /// `CLAUDE_CONFIG_DIR`, the home named `claude`.
    pub claude: PathBuf,
    /// `CODEX_HOME`, the home named `codex`.
    pub codex: PathBuf,
    /// `PI_CODING_AGENT_DIR`, the home named `pi`.
    pub pi: PathBuf,
    /// `skima-probe-<random>`.
    pub skill: String,
    /// The directory the agents run in.
    pub work: PathBuf,
    /// The faux-provider extension Pi loads with `-e`.
    pub extension: PathBuf,
    run: Skima,
}

impl Probe {
    pub fn new() -> Probe {
        let temp = tempfile::tempdir().unwrap();
        // Agents record physical paths, so build on a physical root.
        let root = fs::canonicalize(temp.path()).unwrap();
        let real_home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
        let skill = format!("skima-probe-{:016x}", RandomState::new().hash_one(&root));
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

        let config = root.join("skima.toml");
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

        let mut run = Skima::new(&work, &home, &config, &root.join("data"));
        run.set("PI_CODING_AGENT_DIR", Some(pi.as_os_str())).set("PI_CODING_AGENT_SESSION_DIR", None);
        Probe { _temp: temp, home, claude, codex, pi, skill, work, extension, run }
    }

    /// `program args…` in the work directory, with every agent directory
    /// pointed into the fixture.
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

    /// `skima args…`, asserted to succeed.
    pub fn skima(&self, args: &[&str]) -> Output {
        let out = self.run.output(args);
        assert!(out.status.success(), "skima {args:?}\nstdout:\n{}\nstderr:\n{}", stdout(&out), stderr(&out));
        out
    }

    /// `skima <args…> --json`, parsed.
    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let args: Vec<&str> = args.iter().copied().chain(["--json"]).collect();
        serde_json::from_str(&stdout(&self.skima(&args))).unwrap()
    }

    /// `skima scan --json`: each home's new events.
    pub fn scan(&self) -> BTreeMap<String, Vec<serde_json::Value>> {
        let report = self.json(&["scan"]);
        let homes = report.as_object().unwrap();
        homes.iter().map(|(name, h)| (name.clone(), h["new_events"].as_array().unwrap().clone())).collect()
    }

    /// Whether any line of a transcript under `dir` contains every one of `needles`.
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
