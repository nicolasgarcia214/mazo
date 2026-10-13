//! Adapters turn transcript records into raw events (SPEC §4). Identity, actor,
//! root session and origin are computed once, in `classify`.

pub mod claude;
pub mod codex;
pub mod pi;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::{self, Home};
use crate::ledger::{Agent, Channel};
use crate::readcmd::{self, Reads};
use crate::tail::Chunk;

/// Event identity before classification.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    /// Already final: `cc:…`, `cch:…`, `pi:…`.
    Plain(String),
    /// `cx:<root session>:<hash>:<index>`; the root is known only after classify.
    Codex { hash: String, index: String },
}

/// What the actor rules need (SPEC §4.1 Actor).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorInputs {
    /// Claude `entrypoint`.
    pub entrypoint: Option<String>,
    /// Claude sidechain or `/subagents/` file, Codex `source.subagent`, Pi `parentSession`.
    pub subagent: bool,
    /// Codex `originator == codex_exec`.
    pub codex_exec: bool,
}

/// How to find a session's parent, for the root-session walk.
#[derive(Debug, Clone, PartialEq)]
pub enum Parent {
    /// A Codex rollout id (`forked_from_id` or `thread_spawn.parent_thread_id`).
    Codex(String),
    /// A Pi session file (`parentSession`).
    Pi(PathBuf),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Session {
    pub id: String,
    pub parent: Option<Parent>,
    pub actor: ActorInputs,
    pub cwd: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Raw {
    pub key: Key,
    pub ts: String,
    pub skill: String,
    pub channel: Channel,
    pub path: Option<String>,
    /// The cwd the event happened in (record cwd, else session cwd).
    pub cwd: Option<String>,
    /// 1-based line in the transcript, for `src`.
    pub line: u64,
    pub session: Session,
    /// Origin comes from the skill name alone (Claude Skill results, typed history).
    pub name_origin: bool,
}

/// Paths that decide whether a file is a skill file (SPEC §4.5).
#[derive(Debug, Clone, Default)]
pub struct SkillRoots {
    pub source: PathBuf,
    pub home_skills: Vec<PathBuf>,
    pub known: BTreeSet<String>,
}

/// A path classified as a skill file.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillFile {
    pub skill: String,
    pub channel: Channel,
    pub path: String,
}

impl SkillRoots {
    /// SPEC §4.5: resolve against `cwd`, then the path either ends in `/SKILL.md`
    /// or lies in a `<name>/` child of a home skills dir or the source root, where
    /// `<name>` is a known name or a non-hidden directory containing `SKILL.md`.
    pub fn classify(&self, path: &str, cwd: Option<&str>) -> Option<SkillFile> {
        let abs = if path.starts_with('/') {
            config::resolve(Path::new("/"), path)
        } else {
            config::resolve(Path::new(cwd?), path)
        };
        let abs_str = abs.to_str()?.to_string();
        if abs.file_name()? == "SKILL.md" {
            let skill = abs.parent()?.file_name()?.to_str()?.to_string();
            return Some(SkillFile { skill, channel: Channel::Read, path: abs_str });
        }
        for root in std::iter::once(&self.source).chain(&self.home_skills) {
            let Ok(rest) = abs.strip_prefix(root) else { continue };
            let mut parts = rest.components();
            let name = parts.next()?.as_os_str().to_str()?;
            parts.next()?; // the directory itself, or a loose file, is not a skill file
            let real_skill = !name.starts_with('.') && root.join(name).join("SKILL.md").is_file();
            // `ref` is instruction text only: scripts, tests and manifests are not loads.
            let markdown = abs.extension().is_some_and(|e| e == "md");
            if markdown && (self.known.contains(name) || real_skill) {
                return Some(SkillFile { skill: name.to_string(), channel: Channel::Ref, path: abs_str });
            }
            return None;
        }
        None
    }

    /// The skill files a shell command run in `cwd` loads, each with its index
    /// in the command's read list. A command that names a skill's Markdown file
    /// but is neither a load nor a known non-load goes to `unparsed` (SPEC §4.5).
    pub fn shell_reads(&self, cmd: &str, cwd: Option<&str>, unparsed: &mut Vec<String>) -> Vec<(usize, SkillFile)> {
        let files = match readcmd::reads(cmd) {
            Reads::Load(files) => files,
            Reads::NotLoad => return Vec::new(),
            Reads::Unknown => {
                if self.names_skill_doc(cmd, cwd) {
                    unparsed.push(cmd.to_string());
                }
                return Vec::new();
            }
        };
        files.iter().enumerate().filter_map(|(i, f)| Some((i, self.classify(f, cwd)?))).collect()
    }

    /// Whether `text` names a skill's Markdown file (`SKILL.md` or another `.md`
    /// inside a skill directory): the test for an unparsed command (SPEC §4.5).
    /// Running a skill's scripts reads no instructions, so it does not count.
    pub fn names_skill_doc(&self, text: &str, cwd: Option<&str>) -> bool {
        text.split(|c: char| c.is_whitespace() || "'\"`;|&<>(){}=,:[]".contains(c))
            .filter(|w| w.ends_with(".md"))
            .any(|w| self.classify(w, cwd).is_some())
    }
}

/// Context shared by every parse in one scan.
pub struct ParseCtx<'a> {
    pub home: &'a Home,
    pub roots: &'a SkillRoots,
}

/// The result of parsing one chunk of one file.
#[derive(Debug, Default)]
pub struct Parsed {
    pub raws: Vec<Raw>,
    /// Commands that name a skill file but match neither reader rule (SPEC §4.5).
    pub unparsed: Vec<String>,
    /// The file belongs to an excluded session (Codex guardian review, non-session Pi file).
    pub skipped: bool,
}

pub trait Adapter {
    /// Transcript files of a home, sorted by basename (SPEC §6.2 Scan order).
    fn files(&self, home: &Home) -> Vec<PathBuf> {
        jsonl_files(&home.transcripts())
    }
    /// Parses the prefiltered lines of one file. `header` is line 1 of the file.
    fn parse(&self, ctx: &ParseCtx, path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed;
}

pub fn for_agent(agent: Agent) -> &'static dyn Adapter {
    match agent {
        Agent::Claude => &claude::Claude,
        Agent::Codex => &codex::Codex,
        Agent::Pi => &pi::Pi,
    }
}

/// Every `*.jsonl` under `dir` (recursively), sorted by basename then path.
pub fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| a.file_name().cmp(&b.file_name()).then_with(|| a.cmp(b)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_files_resolve_relative_paths_and_skip_non_skills() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("skills");
        std::fs::create_dir_all(src.join("grade/references")).unwrap();
        std::fs::write(src.join("grade/SKILL.md"), "x").unwrap();
        std::fs::create_dir_all(src.join(".grades/grade")).unwrap();
        let roots = SkillRoots { source: src.clone(), home_skills: vec![], known: BTreeSet::new() };
        let s = src.to_str().unwrap();
        let r = roots.classify("SKILL.md", Some(&format!("{s}/grade"))).unwrap();
        assert_eq!((r.skill.as_str(), r.channel), ("grade", Channel::Read));
        let r = roots.classify("grade/references/x.md", Some(s)).unwrap();
        assert_eq!((r.skill.as_str(), r.channel), ("grade", Channel::Ref));
        assert!(roots.classify(&format!("{s}/.grades/grade/g.md"), None).is_none());
        assert!(roots.classify("references/x.md", None).is_none());
        assert!(roots.classify(&format!("{s}/grade/scripts/run.py"), None).is_none(), "scripts are not instructions");
        assert_eq!(roots.classify("/tmp/p/probe/SKILL.md", None).unwrap().skill, "probe");
    }
}
