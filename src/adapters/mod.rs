pub mod claude;
pub mod codex;
pub mod pi;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use clap::ValueEnum;
use regex::Regex;
use serde_json::Value;

use crate::config::{self, Home};
use crate::ledger::{Agent, Channel};
use crate::readcmd::{self, Reads};
use crate::tail::{self, Chunk};

#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Plain(String),
    Codex { hash: String, index: String },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorInputs {
    pub entrypoint: Option<String>,
    pub subagent: bool,
    pub codex_exec: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parent {
    Codex(String),
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
    pub cwd: Option<String>,
    pub line: u64,
    pub session: Session,
    pub name_origin: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SkillRoots {
    pub source: Option<PathBuf>,
    pub home_skills: Vec<PathBuf>,
    pub known: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkillFile {
    pub skill: String,
    pub channel: Channel,
    pub path: String,
}

impl SkillRoots {
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
            parts.next()?;
            let real_skill = !name.starts_with('.') && root.join(name).join("SKILL.md").is_file();
            let markdown = abs.extension().is_some_and(|e| e == "md");
            if markdown && (self.known.contains(name) || real_skill) {
                return Some(SkillFile { skill: name.to_string(), channel: Channel::Ref, path: abs_str });
            }
            return None;
        }
        None
    }

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

    pub fn names_skill_doc(&self, text: &str, cwd: Option<&str>) -> bool {
        text.split(|c: char| c.is_whitespace() || "'\"`;|&<>(){}=,:[]".contains(c))
            .filter(|w| w.ends_with(".md"))
            .any(|w| self.classify(w, cwd).is_some())
    }
}

pub(crate) struct ToolNames {
    pub block: &'static str,
    pub input: &'static str,
    pub read: &'static str,
    pub path: &'static str,
    pub shell: &'static str,
}

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

#[derive(Debug, Default)]
pub struct Parsed {
    pub raws: Vec<Raw>,
    pub unparsed: Vec<String>,
    pub skipped: bool,
    pub malformed: usize,
}

fn values<'a>(chunk: &'a Chunk, malformed: &'a mut usize) -> impl Iterator<Item = (u64, Value)> + 'a {
    chunk.lines.iter().filter_map(|(n, l)| match serde_json::from_slice(l) {
        Ok(v) => Some((*n, v)),
        Err(_) => {
            *malformed += usize::from(serde_json::from_slice::<serde::de::IgnoredAny>(l).is_err());
            None
        }
    })
}

pub trait Adapter {
    fn files(&self, home: &Home) -> Vec<PathBuf> {
        jsonl_files(&home.transcripts())
    }
    fn parse(&self, roots: &SkillRoots, path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed;
    fn records(&self) -> Records;
    fn prompt_at(&self, memo: &mut Transcripts, src: &str) -> Option<String> {
        let (_, chunk, line) = memo.at(src)?;
        authored_back(upto(&chunk.lines, line), &self.records().authored).map(|(_, text)| text)
    }
    fn condense(&self, memo: &mut Transcripts, src: &str, max_chars: usize) -> Option<String> {
        let Records { authored, render } = self.records();
        let (path, chunk, load) = memo.at(src)?;
        let lines = &chunk.lines;
        let strong = strong_loads(self, path, chunk);
        let skill = &strong.iter().find(|r| r.line == load)?.skill;
        let start = authored_back(upto(lines, load), &authored).map_or(load, |(n, _)| n);
        let other = strong.iter().filter(|r| r.line > load && r.skill != *skill).map(|r| r.line).min();
        let value = |l: &[u8]| serde_json::from_slice::<Value>(l).ok();
        let limit = lines
            .iter()
            .filter(|(n, l)| *n > start && value(l).and_then(|v| authored(&v)).is_some())
            .nth(WINDOW_MESSAGES - 1)
            .map(|(n, _)| *n);
        let end = other.into_iter().chain(limit).min().unwrap_or(u64::MAX);
        Some(cap(numbered(&rendered(lines, start..end, render)), max_chars))
    }
    fn trace(&self, file: &Path, skill: &str, reviewer: &str) -> Option<Trace> {
        let Records { authored, render } = self.records();
        let chunk = tail::whole(file).ok()?;
        let strong = strong_loads(self, file, &chunk);
        let lines = &chunk.lines;
        let last = |name: &str, before: u64| {
            strong.iter().filter(|r| r.skill == name && r.line < before).map(|r| r.line).max()
        };
        let eof = chunk.cursor.line + 1;
        let reviews = || strong.iter().filter(|r| r.skill == reviewer).map(|r| r.line);
        let load = last(skill, reviews().filter(|l| last(skill, *l).is_some()).max().unwrap_or(eof))?;
        let prompt = |line: u64| authored_back(upto(lines, line), &authored);
        let start = prompt(load).map_or(load, |(n, _)| n);
        let end = reviews().filter(|l| *l > load).min().map_or(eof, |r| {
            let asked = prompt(r).filter(|(p, text)| *p > load && text.contains(reviewer));
            asked.map_or(r, |(p, _)| p)
        });
        let kept = rendered(lines, start..end, render);
        Some(Trace {
            text: numbered(&kept),
            lines: (start, end - 1),
            records: (end - start) as usize,
            kept: kept.len(),
        })
    }
}

#[derive(Debug, PartialEq)]
pub struct Trace {
    pub text: String,
    pub lines: (u64, u64),
    pub records: usize,
    pub kept: usize,
}

fn strong_loads<A: Adapter + ?Sized>(adapter: &A, path: &Path, chunk: &Chunk) -> Vec<Raw> {
    let header = chunk.lines.first().map(|(_, l)| l.as_slice());
    let parsed = adapter.parse(&SkillRoots::default(), path, header, chunk);
    parsed.raws.into_iter().filter(|r| r.channel.strong()).collect()
}

fn rendered(
    lines: &[(u64, Vec<u8>)],
    range: std::ops::Range<u64>,
    render: fn(&Value) -> Vec<String>,
) -> Vec<(u64, Vec<String>)> {
    lines
        .iter()
        .filter(|(n, _)| range.contains(n))
        .filter_map(|(n, l)| Some((*n, render(&serde_json::from_slice(l).ok()?))))
        .filter(|(_, parts)| !parts.is_empty())
        .collect()
}

fn numbered(records: &[(u64, Vec<String>)]) -> String {
    records.iter().flat_map(|(n, parts)| parts.iter().map(move |p| format!("L{n}: {p}"))).collect::<Vec<_>>().join("\n")
}

pub struct Records {
    pub authored: fn(&Value) -> Option<String>,
    pub render: fn(&Value) -> Vec<String>,
}

pub fn markers() -> impl Iterator<Item = &'static str> {
    Agent::value_variants().iter().map(|agent| match agent {
        Agent::Claude => claude::MARKER,
        Agent::Codex => codex::MARKER,
        Agent::Pi => pi::MARKER,
    })
}

pub fn for_agent(agent: Agent) -> &'static dyn Adapter {
    match agent {
        Agent::Claude => &claude::Claude,
        Agent::Codex => &codex::Codex,
        Agent::Pi => &pi::Pi,
    }
}

#[derive(Default)]
pub struct Transcripts(HashMap<PathBuf, Option<Chunk>>);

impl Transcripts {
    fn at<'m, 's>(&'m mut self, src: &'s str) -> Option<(&'s Path, &'m Chunk, u64)> {
        let (file, line) = src.rsplit_once(':')?;
        let (file, line) = (Path::new(file), line.parse().ok()?);
        let chunk = self.0.entry(file.to_path_buf()).or_insert_with(|| tail::whole(file).ok()).as_ref()?;
        (1..=chunk.cursor.line).contains(&line).then_some((file, chunk, line))
    }
}

fn upto(lines: &[(u64, Vec<u8>)], line: u64) -> &[(u64, Vec<u8>)] {
    &lines[..lines.partition_point(|(n, _)| *n <= line)]
}

fn authored_back(lines: &[(u64, Vec<u8>)], authored: &impl Fn(&Value) -> Option<String>) -> Option<(u64, String)> {
    lines.iter().rev().find_map(|(n, l)| Some((*n, authored(&serde_json::from_slice(l).ok()?)?)))
}

const WINDOW_MESSAGES: usize = 40;
const TOOL_INPUT_CHARS: usize = 200;
const TOOL_RESULT_CHARS: usize = 500;
const CUT: &str = "\n[… cut …]\n";

fn cap(text: String, max_chars: usize) -> String {
    let len = text.chars().count();
    if len <= max_chars {
        return text;
    }
    let head = max_chars * 2 / 3;
    let at = |n: usize| text.char_indices().nth(n).map_or(text.len(), |(i, _)| i);
    format!("{}{CUT}{}", &text[..at(head)], &text[at(len - (max_chars - head))..])
}

fn tool_call(name: &str, input: &str) -> String {
    format!("tool {name}: {}", input.chars().take(TOOL_INPUT_CHARS).collect::<String>())
}

fn tool_result(output: &str) -> String {
    format!("result: {}", output.chars().take(TOOL_RESULT_CHARS).collect::<String>())
}

fn message_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        _ => content.as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
    }
}

static INJECTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?s)<system-reminder>.*?</system-reminder>|<task-notification>.*?</task-notification>",
        r"|<local-command-stdout>.*?</local-command-stdout>|<pasted_content[^>]*>.*?</pasted_content[^>]*>",
    ))
    .expect("valid regex")
});
static BLANK_LINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").expect("valid regex"));

fn strip_injected(text: &str) -> Option<String> {
    let text = INJECTED.replace_all(text, "");
    let text = BLANK_LINES.replace_all(text.trim(), "\n\n");
    (!text.is_empty()).then(|| text.into_owned())
}

pub fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
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
    fn values_count_only_lines_that_are_not_json() {
        let lines = [&br#"{"a":1}"#[..], br#"{"a":"\ud800"}"#, br#"{"a":1e400}"#, b"{not json", b""];
        let lines = lines.iter().zip(1..).map(|(l, n)| (n, l.to_vec())).collect();
        let chunk = Chunk { lines, oversized: 0, cursor: Default::default(), header: None };
        let mut malformed = 0;
        let kept: Vec<u64> = values(&chunk, &mut malformed).map(|(n, _)| n).collect();
        assert_eq!((kept, malformed), (vec![1], 2));
    }

    #[test]
    fn skill_files_resolve_relative_paths_and_skip_non_skills() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("skills");
        fs::create_dir_all(src.join("grade/references")).unwrap();
        fs::write(src.join("grade/SKILL.md"), "x").unwrap();
        fs::create_dir_all(src.join(".grades/grade")).unwrap();
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
