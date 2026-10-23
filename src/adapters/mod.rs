//! Adapters turn transcript records into raw events. Identity, actor,
//! root session and origin are computed once, in `classify`.

pub mod claude;
pub mod codex;
pub mod pi;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::config::{self, Home};
use crate::ledger::{Agent, Channel};
use crate::readcmd::{self, Reads};
use crate::tail::{Chunk, Cursor};

/// Event identity before classification.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    /// Already final: `cc:…`, `cch:…`, `pi:…`.
    Plain(String),
    /// `cx:<root session>:<hash>:<index>`; the root is known only after classify.
    Codex { hash: String, index: String },
}

/// What the actor rules need.
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

/// Paths that decide whether a file is a skill file.
#[derive(Debug, Clone, Default)]
pub struct SkillRoots {
    pub source: Option<PathBuf>,
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
    /// Resolve against `cwd`, then the path either ends in `/SKILL.md`
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
        for root in self.source.iter().chain(&self.home_skills) {
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
    /// but is neither a load nor a known non-load goes to `unparsed`.
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
    /// inside a skill directory): the test for an unparsed command.
    /// Running a skill's scripts reads no instructions, so it does not count.
    pub fn names_skill_doc(&self, text: &str, cwd: Option<&str>) -> bool {
        text.split(|c: char| c.is_whitespace() || "'\"`;|&<>(){}=,:[]".contains(c))
            .filter(|w| w.ends_with(".md"))
            .any(|w| self.classify(w, cwd).is_some())
    }
}

/// How one agent's transcripts spell a tool call that reads a file or runs a
/// shell command.
pub(crate) struct ToolNames {
    /// The `type` of a tool-call block.
    pub block: &'static str,
    /// The field holding the call's arguments.
    pub input: &'static str,
    pub read: &'static str,
    /// The read tool's path argument.
    pub path: &'static str,
    pub shell: &'static str,
}

/// The skill files the read and shell tool calls in `content` load, one
/// `(call index, file index, skill file)` each; the file index counts the
/// files the call reads.
pub(crate) fn tool_reads(
    roots: &SkillRoots,
    names: &ToolNames,
    content: &Value,
    cwd: Option<&str>,
    unparsed: &mut Vec<String>,
) -> Vec<(usize, usize, SkillFile)> {
    let mut out = Vec::new();
    for (ci, b) in content.as_array().into_iter().flatten().enumerate() {
        if b["type"] != names.block {
            continue;
        }
        let input = &b[names.input];
        let found = match (b["name"].as_str(), input[names.path].as_str(), input["command"].as_str()) {
            (Some(n), Some(file), _) if n == names.read => {
                roots.classify(file, cwd).map(|sf| (0, sf)).into_iter().collect()
            }
            (Some(n), _, Some(cmd)) if n == names.shell => roots.shell_reads(cmd, cwd, unparsed),
            _ => continue,
        };
        out.extend(found.into_iter().map(|(fi, sf)| (ci, fi, sf)));
    }
    out
}

/// The result of parsing one chunk of one file.
#[derive(Debug, Default)]
pub struct Parsed {
    pub raws: Vec<Raw>,
    /// Commands that name a skill file but match neither reader rule.
    pub unparsed: Vec<String>,
    /// The file belongs to an excluded session (Codex guardian review, non-session Pi file).
    pub skipped: bool,
}

pub trait Adapter {
    /// Transcript files of a home, sorted by basename.
    fn files(&self, home: &Home) -> Vec<PathBuf> {
        jsonl_files(&home.transcripts())
    }
    /// Parses the prefiltered lines of one file. `header` is line 1 of the file.
    fn parse(&self, roots: &SkillRoots, path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed;
    /// How [`Adapter::prompt_at`] and [`Adapter::condense`] read a record.
    fn records(&self) -> Records;
    /// The user-authored text behind the load at `src` (`<file>:<line>`), read
    /// at display time and never stored: the first text `authored` reads
    /// walking back from that record, itself included, since a `/skill:`
    /// message holds its own prompt. `None` when the file is gone or holds no
    /// user turn up to that line.
    fn prompt_at(&self, src: &str) -> Option<String> {
        let t = transcript(src)?;
        authored_back(&t.chunk.lines[..t.line], &self.records().authored).map(|(_, text)| text)
    }
    /// The judge's view of the session window around the strong load at `src`,
    /// at most `max_chars` characters plus a cut marker. `None` when the file
    /// is gone or `src` is not a strong load. The window runs from the prompt
    /// behind the load (the load itself when no prompt precedes it) up to, not
    /// including, the first strong load of a different skill after `src`, the
    /// 40th `authored` record after the start, or the end of the file; each
    /// record's `render` parts are prefixed `L<line>: `.
    fn condense(&self, src: &str, max_chars: usize) -> Option<String> {
        let Records { authored, render } = self.records();
        let t = transcript(src)?;
        let lines = &t.chunk.lines;
        let header = lines.first().map(|(_, l)| l.as_slice());
        // Strong loads come from the harness's own markers, so no skill roots are needed.
        let strong: Vec<Raw> = self
            .parse(&SkillRoots::default(), &t.path, header, &t.chunk)
            .raws
            .into_iter()
            .filter(|r| r.channel.strong())
            .collect();
        let load = t.line as u64;
        let skill = &strong.iter().find(|r| r.line == load)?.skill;
        let start = authored_back(&lines[..t.line], &authored).map_or(load, |(n, _)| n);
        let other = strong.iter().filter(|r| r.line > load && r.skill != *skill).map(|r| r.line).min();
        let value = |l: &[u8]| serde_json::from_slice::<Value>(l).ok();
        let limit = lines
            .iter()
            .filter(|(n, l)| *n > start && value(l).and_then(|v| authored(&v)).is_some())
            .nth(WINDOW_MESSAGES - 1)
            .map(|(n, _)| *n);
        let end = other.into_iter().chain(limit).min().unwrap_or(u64::MAX);
        let text = lines
            .iter()
            .filter(|(n, _)| (start..end).contains(n))
            .filter_map(|(n, l)| Some((n, render(&value(l)?))))
            .flat_map(|(n, parts)| parts.into_iter().map(move |p| format!("L{n}: {p}")))
            .collect::<Vec<_>>()
            .join("\n");
        Some(cap(text, max_chars))
    }
}

/// A transcript's record readers: the user-authored text of a record, if it
/// is one, and what the judge keeps of it.
pub struct Records {
    pub authored: fn(&Value) -> Option<String>,
    pub render: fn(&Value) -> Vec<String>,
}

pub fn for_agent(agent: Agent) -> &'static dyn Adapter {
    match agent {
        Agent::Claude => &claude::Claude,
        Agent::Codex => &codex::Codex,
        Agent::Pi => &pi::Pi,
    }
}

/// The transcript `src` (`<file>:<line>`) names, read whole, and that line.
struct Transcript {
    path: PathBuf,
    chunk: Chunk,
    line: usize,
}

fn transcript(src: &str) -> Option<Transcript> {
    let (file, line) = src.rsplit_once(':')?;
    let line: usize = line.parse().ok()?;
    let bytes = std::fs::read(file).ok()?;
    let body = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let lines: Vec<(u64, Vec<u8>)> = (1..).zip(body.split(|&b| b == b'\n').map(<[u8]>::to_vec)).collect();
    if line == 0 || lines.len() < line {
        return None;
    }
    Some(Transcript {
        path: PathBuf::from(file),
        chunk: Chunk { lines, oversized: 0, cursor: Cursor::default() },
        line,
    })
}

/// The line and text of the first record `authored` reads walking back from
/// the last of `lines`.
fn authored_back(lines: &[(u64, Vec<u8>)], authored: &impl Fn(&Value) -> Option<String>) -> Option<(u64, String)> {
    lines.iter().rev().find_map(|(n, l)| Some((*n, authored(&serde_json::from_slice(l).ok()?)?)))
}

/// The window ends before the 40th user-authored message after its start.
const WINDOW_MESSAGES: usize = 40;
const TOOL_INPUT_CHARS: usize = 200;
const TOOL_RESULT_CHARS: usize = 500;
const CUT: &str = "\n[… cut …]\n";

/// `text` cut to the first two thirds and the last third of `max_chars`
/// characters, with [`CUT`] between, when it is longer.
fn cap(text: String, max_chars: usize) -> String {
    let len = text.chars().count();
    if len <= max_chars {
        return text;
    }
    let head = max_chars * 2 / 3;
    let at = |n: usize| text.char_indices().nth(n).map_or(text.len(), |(i, _)| i);
    format!("{}{CUT}{}", &text[..at(head)], &text[at(len - (max_chars - head))..])
}

/// A tool call as the judge sees it: its name and the start of its input.
fn tool_call(name: &str, input: &str) -> String {
    format!("tool {name}: {}", input.chars().take(TOOL_INPUT_CHARS).collect::<String>())
}

/// A tool result as the judge sees it: the start of its text.
fn tool_result(output: &str) -> String {
    format!("result: {}", output.chars().take(TOOL_RESULT_CHARS).collect::<String>())
}

/// The text of a message `content`: a string, or the `text` of its blocks
/// (`text`, `input_text`, `output_text`) joined by newlines, so an image
/// beside the text does not hide it.
fn message_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        _ => content.as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
    }
}

/// Text the harness puts inside a user turn: reminders, task notifications,
/// local command output and pasted content.
static INJECTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?s)<system-reminder>.*?</system-reminder>|<task-notification>.*?</task-notification>",
        r"|<local-command-stdout>.*?</local-command-stdout>|<pasted_content[^>]*>.*?</pasted_content[^>]*>",
    ))
    .expect("valid regex")
});
static BLANK_LINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").expect("valid regex"));

/// `text` without injected blocks; `None` when nothing the user wrote is left.
fn strip_injected(text: &str) -> Option<String> {
    let text = INJECTED.replace_all(text, "");
    let text = BLANK_LINES.replace_all(text.trim(), "\n\n");
    (!text.is_empty()).then(|| text.into_owned())
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
        let roots = SkillRoots { source: Some(src.clone()), home_skills: vec![], known: BTreeSet::new() };
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
