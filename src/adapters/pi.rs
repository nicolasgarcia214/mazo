//! Pi sessions (SPEC §4.4): `/skill:` expansions, `read` tool calls and `bash`
//! reads, keyed by entry id and timestamp so fork copies collapse.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::{ActorInputs, Adapter, Key, Parent, Parsed, Raw, Session, SkillRoots};
use crate::ledger::Channel;
use crate::tail::Chunk;

pub struct Pi;

/// The `/skill:name` expansion Pi injects as a user message.
static SKILL_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^<skill name="([^"]+)" location="([^"]+)">"#).expect("valid regex"));

impl Adapter for Pi {
    fn parse(&self, roots: &SkillRoots, _path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed {
        let Some(session) = header.and_then(session_of_header) else {
            return Parsed { skipped: true, ..Parsed::default() };
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
            let entry = Entry { roots, session: &session, id, ts, line: *line };
            let msg = &v["message"];
            match msg["role"].as_str() {
                Some("user") => out.raws.extend(entry.slash(&msg["content"])),
                Some("assistant") => entry.tool_calls(&msg["content"], &mut out),
                _ => {}
            }
        }
        out
    }

    fn prompt_at(&self, src: &str) -> Option<String> {
        super::prompt_back(src, user_turn)
    }
}

/// SPEC §4.7: a user `message` entry. A `/skill:` expansion stands for
/// `/skill:name args`, and a bare one for nothing, so the walk goes on to the
/// message before it.
fn user_turn(v: &Value) -> Option<String> {
    let msg = &v["message"];
    if v["type"] != "message" || msg["role"] != "user" {
        return None;
    }
    let text = super::message_text(&msg["content"]);
    match SKILL_BLOCK.captures(&text) {
        Some(caps) => super::strip_injected(text.split_once("</skill>").map_or("", |(_, args)| args))
            .map(|args| format!("/skill:{} {args}", &caps[1])),
        None => super::strip_injected(&text),
    }
}

/// One message entry being parsed.
struct Entry<'a> {
    roots: &'a SkillRoots,
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

    /// `read` and `bash` tool calls on skill files, one event per skill file
    /// keyed by its index among the files read.
    fn tool_calls(&self, content: &Value, out: &mut Parsed) {
        let Some(items) = content.as_array() else {
            return;
        };
        let roots = self.roots;
        let cwd = self.session.cwd.as_deref();
        for (ci, item) in items.iter().enumerate() {
            if item["type"] != "toolCall" {
                continue;
            }
            let args = &item["arguments"];
            let found = match (item["name"].as_str(), args["path"].as_str(), args["command"].as_str()) {
                (Some("read"), Some(path), _) => roots.classify(path, cwd).map(|sf| (0, sf)).into_iter().collect(),
                (Some("bash"), _, Some(cmd)) => roots.shell_reads(cmd, cwd, &mut out.unparsed),
                _ => continue,
            };
            for (fi, sf) in found {
                let key = format!("pi:{}@{}:{ci}:{fi}", self.id, self.ts);
                out.raws.push(self.raw(key, sf.skill, sf.channel, sf.path));
            }
        }
    }
}

fn session_of_header(line: &[u8]) -> Option<Session> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let (id, parent) = id_parent(&v)?;
    Some(Session {
        id,
        actor: ActorInputs { subagent: parent.is_some(), ..ActorInputs::default() },
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
    let parent = v["parentSession"].as_str().filter(|p| !p.is_empty()).map(PathBuf::from);
    Some((id, parent))
}

/// `(id, parentSession)` from a Pi session header line.
pub fn header_id_parent(line: &[u8]) -> Option<(String, Option<PathBuf>)> {
    id_parent(&serde_json::from_slice(line).ok()?)
}
