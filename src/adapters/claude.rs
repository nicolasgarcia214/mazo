//! Claude Code transcripts (SPEC §4.2).
//!
//! Every event comes from the harness's own structured record: a Skill result
//! (`toolUseResult.commandName`), an `isMeta` record carrying `Base directory
//! for this skill`, or an assistant `Read`/`Bash` tool call. Text inside
//! `tool_result` content, `<command-name>` records and skill attachments are
//! never read.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use super::{ActorInputs, Adapter, Key, Parsed, Raw, Session, SkillRoots};
use crate::config::Home;
use crate::ledger::Channel;
use crate::tail::Chunk;

pub struct Claude;

const BASE_DIR: &str = "Base directory for this skill: ";

impl Adapter for Claude {
    fn parse(&self, roots: &SkillRoots, path: &Path, _header: Option<&[u8]>, chunk: &Chunk) -> Parsed {
        let in_subagents = path.to_string_lossy().contains("/subagents/");
        let mut out = Parsed::default();
        // tool_use id → index in `out.raws` of the event that id already produced.
        let mut loads: HashMap<String, usize> = HashMap::new();
        for (line, bytes) in &chunk.lines {
            let Ok(rec) = serde_json::from_slice::<Value>(bytes) else { continue };
            let Some(r) = Record::new(&rec, *line, in_subagents) else { continue };
            match rec["type"].as_str() {
                Some("user") => user(&r, &mut loads, &mut out),
                Some("assistant") => assistant(roots, &r, &mut out),
                _ => {}
            }
        }
        out
    }

    fn prompt_at(&self, src: &str) -> Option<String> {
        super::prompt_back(src, authored)
    }

    fn condense(&self, src: &str, max_chars: usize) -> Option<String> {
        super::condense(self, src, max_chars, authored, render)
    }
}

/// What the judge keeps of a record (SPEC §6.4): the user's text, assistant
/// text, tool calls and tool results. Meta records, which carry skill bodies,
/// keep nothing.
fn render(v: &Value) -> Vec<String> {
    if let Some(text) = authored(v) {
        return vec![format!("user: {text}")];
    }
    let content = &v["message"]["content"];
    match v["type"].as_str() {
        Some("user") if v["isMeta"] != true => blocks(content)
            .filter(|b| b["type"] == "tool_result")
            .map(|b| super::tool_result(&super::message_text(&b["content"])))
            .collect(),
        Some("assistant") => blocks(content)
            .filter_map(|b| match b["type"].as_str()? {
                "text" => Some(format!("assistant: {}", b["text"].as_str()?)),
                "tool_use" => Some(super::tool_call(b["name"].as_str()?, &b["input"].to_string())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// One transcript record with the fields every event takes from it.
struct Record<'a> {
    v: &'a Value,
    line: u64,
    ts: String,
    session: Session,
}

impl<'a> Record<'a> {
    fn new(v: &'a Value, line: u64, in_subagents: bool) -> Option<Record<'a>> {
        let s = |k: &str| v[k].as_str().map(str::to_string);
        let session = Session {
            id: s("sessionId")?,
            parent: None,
            actor: ActorInputs {
                entrypoint: s("entrypoint"),
                subagent: in_subagents || v["isSidechain"] == true,
                codex_exec: false,
            },
            cwd: s("cwd"),
            version: s("version"),
        };
        Some(Record { v, line, ts: s("timestamp")?, session })
    }

    fn raw(&self, key: String, skill: String, channel: Channel, path: Option<String>, name_origin: bool) -> Raw {
        Raw {
            key: Key::Plain(key),
            ts: self.ts.clone(),
            skill,
            channel,
            path,
            cwd: self.session.cwd.clone(),
            line: self.line,
            session: self.session.clone(),
            name_origin,
        }
    }

    fn content(&self) -> &'a Value {
        &self.v["message"]["content"]
    }
}

/// Rows 1–3: the Skill result, and the meta record that carries the skill body.
fn user(r: &Record, loads: &mut HashMap<String, usize>, out: &mut Parsed) {
    let v = r.v;
    let result = &v["toolUseResult"];
    if result["success"] == true
        && let Some(name) = result["commandName"].as_str()
    {
        let Some(id) = blocks(r.content()).find(|b| b["type"] == "tool_result").and_then(|b| b["tool_use_id"].as_str())
        else {
            return;
        };
        loads.insert(id.to_string(), out.raws.len());
        out.raws.push(r.raw(format!("cc:{id}"), name.to_string(), Channel::Tool, None, true));
        return;
    }
    if v["isMeta"] != true {
        return;
    }
    let Some(dir) = base_dir(r.content()) else { return };
    let skill_md = format!("{}/SKILL.md", dir.trim_end_matches('/'));
    let Some(name) = Path::new(dir).file_name().and_then(|n| n.to_str()) else { return };
    match v["sourceToolUseID"].as_str() {
        // The meta record of a Skill call: it supplies the path of the result's
        // event, or stands alone when the result is elsewhere or has no
        // `toolUseResult` (subagents, E5).
        Some(id) => match loads.get(id) {
            Some(&i) => {
                out.raws[i].path.get_or_insert(skill_md);
            }
            None => {
                loads.insert(id.to_string(), out.raws.len());
                out.raws.push(r.raw(format!("cc:{id}"), name.to_string(), Channel::Tool, Some(skill_md), false));
            }
        },
        None => {
            let Some(uuid) = v["uuid"].as_str() else { return };
            out.raws.push(r.raw(format!("cc:{uuid}"), name.to_string(), Channel::Slash, Some(skill_md), false));
        }
    }
}

/// The `<dir>` of the first text block starting `Base directory for this skill:`.
/// A plain-string `content` counts as block 0.
fn base_dir(content: &Value) -> Option<&str> {
    let texts: Box<dyn Iterator<Item = &str>> = match content {
        Value::String(s) => Box::new(std::iter::once(s.as_str())),
        _ => Box::new(blocks(content).filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str())),
    };
    texts
        .filter_map(|t| t.strip_prefix(BASE_DIR))
        .map(|rest| rest.split('\n').next().unwrap_or_default().trim())
        .find(|d| !d.is_empty())
}

fn blocks(content: &Value) -> impl Iterator<Item = &Value> {
    content.as_array().into_iter().flatten()
}

/// SPEC §4.7: a `typed` history line's `display`, or a user record that is
/// neither a `tool_result` nor `isMeta`. A slash command record stands for
/// `/name args`, and a bare `/name` for nothing, so the walk goes on to the
/// message before it.
fn authored(v: &Value) -> Option<String> {
    if let Some(display) = v["display"].as_str() {
        return super::strip_injected(display);
    }
    let content = &v["message"]["content"];
    if v["type"] != "user" || v["isMeta"] == true || blocks(content).any(|b| b["type"] == "tool_result") {
        return None;
    }
    let text = super::message_text(content);
    match tag(&text, "command-name") {
        Some(name) => {
            super::strip_injected(tag(&text, "command-args").unwrap_or_default()).map(|args| format!("{name} {args}"))
        }
        None => super::strip_injected(&text),
    }
}

/// The text between `<name>` and `</name>`.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(&format!("<{name}>"))?;
    Some(rest.split_once(&format!("</{name}>"))?.0)
}

/// Rows 4–5: `Read` and `Bash` tool calls on skill files, one event per skill
/// file keyed by its index among the files read.
fn assistant(roots: &SkillRoots, r: &Record, out: &mut Parsed) {
    let Some(uuid) = r.v["uuid"].as_str() else { return };
    let cwd = r.session.cwd.as_deref();
    for (ci, b) in blocks(r.content()).enumerate() {
        if b["type"] != "tool_use" {
            continue;
        }
        let input = &b["input"];
        let found = match (b["name"].as_str(), input["file_path"].as_str(), input["command"].as_str()) {
            (Some("Read"), Some(file), _) => roots.classify(file, cwd).map(|sf| (0, sf)).into_iter().collect(),
            (Some("Bash"), _, Some(cmd)) => roots.shell_reads(cmd, cwd, &mut out.unparsed),
            _ => continue,
        };
        for (fi, sf) in found {
            out.raws.push(r.raw(format!("cc:{uuid}:{ci}:{fi}"), sf.skill, sf.channel, Some(sf.path), false));
        }
    }
}

/// One-time `typed` events from `<home>/history.jsonl` (SPEC §4.2 History backfill).
/// A line counts when its `display` is `/<known name>` followed by whitespace or
/// the end, and no `<sessionId>.jsonl` transcript exists under the home.
pub fn backfill(home: &Home, known: &BTreeSet<String>) -> Vec<Raw> {
    let Ok(bytes) = std::fs::read(home.path.join("history.jsonl")) else { return Vec::new() };
    let transcripts: HashSet<String> = super::jsonl_files(&home.transcripts())
        .iter()
        .filter_map(|p| p.file_stem()?.to_str().map(str::to_string))
        .collect();
    let mut out = Vec::new();
    for (i, line) in bytes.split(|&b| b == b'\n').enumerate() {
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        let (Some(display), Some(sid), Some(ms)) =
            (v["display"].as_str(), v["sessionId"].as_str(), v["timestamp"].as_i64())
        else {
            continue;
        };
        let Some(name) = display.strip_prefix('/').and_then(|d| d.split(char::is_whitespace).next()) else {
            continue;
        };
        if !known.contains(name) || transcripts.contains(sid) {
            continue;
        }
        let Ok(ts) = jiff::Timestamp::from_millisecond(ms) else { continue };
        let cwd = v["project"].as_str().map(str::to_string);
        out.push(Raw {
            key: Key::Plain(format!("cch:{sid}:{ms}")),
            ts: format!("{ts:.3}"),
            skill: name.to_string(),
            channel: Channel::Typed,
            path: None,
            cwd: cwd.clone(),
            line: i as u64 + 1,
            session: Session { id: sid.to_string(), cwd, ..Session::default() },
            name_origin: true,
        });
    }
    out
}
