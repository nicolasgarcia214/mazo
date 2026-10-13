//! Pi sessions (SPEC §4.4): `/skill:` expansions, `read` tool calls and `bash`
//! reads, keyed by entry id and timestamp so fork copies collapse.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{ActorInputs, Adapter, Key, Parent, ParseCtx, Parsed, Raw, Session, SkillRoots};
use crate::config::Home;
use crate::ledger::Channel;
use crate::readcmd::{self, Reads};
use crate::tail::Chunk;

pub struct Pi;

/// The `/skill:name` expansion Pi injects as a user message.
static SKILL_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^<skill name="([^"]+)" location="([^"]+)">"#).expect("valid regex")
});

impl Adapter for Pi {
    fn files(&self, home: &Home) -> Vec<PathBuf> {
        super::jsonl_files(&home.transcripts())
    }

    fn parse(&self, ctx: &ParseCtx, _path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed {
        let Some(session) = header.and_then(session_of_header) else {
            return Parsed {
                skipped: true,
                ..Parsed::default()
            };
        };
        let mut out = Parsed::default();
        for (line, bytes) in &chunk.lines {
            let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
                continue;
            };
            if v["type"] != "message" {
                continue;
            }
            let (Some(id), Some(ts)) = (v["id"].as_str(), v["timestamp"].as_str()) else {
                continue;
            };
            let entry = Entry {
                ctx,
                session: &session,
                id,
                ts,
                line: *line,
            };
            let msg = &v["message"];
            match msg["role"].as_str() {
                Some("user") => out.raws.extend(entry.slash(&msg["content"])),
                Some("assistant") => entry.tool_calls(&msg["content"], &mut out),
                _ => {}
            }
        }
        out
    }
}

/// One message entry being parsed.
struct Entry<'a> {
    ctx: &'a ParseCtx<'a>,
    session: &'a Session,
    id: &'a str,
    ts: &'a str,
    line: u64,
}

impl Entry<'_> {
    fn raw(&self, key: String, skill: String, channel: Channel, path: String) -> Raw {
        Raw {
            key: Key::Plain(key),
            ts: self.ts.to_string(),
            skill,
            channel,
            path: Some(path),
            cwd: None,
            line: self.line,
            session: self.session.clone(),
            name_origin: false,
        }
    }

    /// A user message whose first text item opens with a `<skill …>` block naming
    /// an absolute `…/SKILL.md` location (placeholders fail the location check).
    fn slash(&self, content: &Value) -> Option<Raw> {
        let text = match content {
            Value::String(s) => s.as_str(),
            Value::Array(items) => items.iter().find(|i| i["type"] == "text")?["text"].as_str()?,
            _ => return None,
        };
        let caps = SKILL_BLOCK.captures(text)?;
        let (name, location) = (&caps[1], &caps[2]);
        if !location.starts_with('/') || !location.ends_with("/SKILL.md") {
            return None;
        }
        let key = format!("pi:{}@{}", self.id, self.ts);
        Some(self.raw(key, name.to_string(), Channel::Slash, location.to_string()))
    }

    /// `read` and `bash` tool calls on skill files.
    fn tool_calls(&self, content: &Value, out: &mut Parsed) {
        let Some(items) = content.as_array() else {
            return;
        };
        let cwd = self.session.cwd.as_deref();
        for (ci, item) in items.iter().enumerate() {
            if item["type"] != "toolCall" {
                continue;
            }
            let args = &item["arguments"];
            match (
                item["name"].as_str(),
                args["path"].as_str(),
                args["command"].as_str(),
            ) {
                (Some("read"), Some(path), _) => self.emit(ci, [path], out),
                (Some("bash"), _, Some(cmd)) => match readcmd::reads(cmd) {
                    Reads::Load(files) => self.emit(ci, files.iter().map(String::as_str), out),
                    Reads::NotLoad => {}
                    Reads::Unknown if names_skill_file(self.ctx.roots, cmd, cwd) => {
                        out.unparsed.push(cmd.to_string())
                    }
                    Reads::Unknown => {}
                },
                _ => {}
            }
        }
    }

    /// One event per skill file among `files`, keyed by its index in `files`.
    fn emit<'f>(&self, ci: usize, files: impl IntoIterator<Item = &'f str>, out: &mut Parsed) {
        let cwd = self.session.cwd.as_deref();
        for (fi, f) in files.into_iter().enumerate() {
            if let Some(sf) = self.ctx.roots.classify(f, cwd) {
                let key = format!("pi:{}@{}:{ci}:{fi}", self.id, self.ts);
                out.raws.push(self.raw(key, sf.skill, sf.channel, sf.path));
            }
        }
    }
}

/// Whether any path-like word of `cmd` is a skill file (SPEC §4.5 Unparsed).
fn names_skill_file(roots: &SkillRoots, cmd: &str, cwd: Option<&str>) -> bool {
    cmd.split(|c: char| c.is_whitespace() || "'\"`;|&<>(){}=,:".contains(c))
        .filter(|w| w.contains('/') || w.ends_with("SKILL.md"))
        .any(|w| roots.classify(w, cwd).is_some())
}

fn session_of_header(line: &[u8]) -> Option<Session> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let (id, parent) = id_parent(&v)?;
    Some(Session {
        id,
        actor: ActorInputs {
            subagent: parent.is_some(),
            ..ActorInputs::default()
        },
        parent: parent.map(Parent::Pi),
        cwd: v["cwd"].as_str().map(str::to_string),
        version: None,
    })
}

fn id_parent(v: &Value) -> Option<(String, Option<PathBuf>)> {
    if v["type"] != "session" {
        return None;
    }
    let id = v["id"].as_str()?.to_string();
    let parent = v["parentSession"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    Some((id, parent))
}

/// `(id, parentSession)` from a Pi session header line.
pub fn header_id_parent(line: &[u8]) -> Option<(String, Option<PathBuf>)> {
    id_parent(&serde_json::from_slice(line).ok()?)
}
