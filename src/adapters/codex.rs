//! Codex rollouts (SPEC §4.3): `$name` expansions and shell reads of skill
//! files, keyed by a hash of the record payload so fork copies collapse.

use std::path::Path;

use serde_json::Value;

use super::{ActorInputs, Adapter, Key, Parent, Parsed, Raw, Session, SkillRoots};
use crate::config;
use crate::ledger::Channel;
use crate::tail::Chunk;

pub struct Codex;

impl Adapter for Codex {
    fn parse(&self, roots: &SkillRoots, _path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed {
        let mut out = Parsed::default();
        let Some(Header { session, guardian }) = header.and_then(parse_header) else {
            return out;
        };
        if guardian {
            out.skipped = true;
            return out;
        }
        for (line, bytes) in &chunk.lines {
            let Ok(rec) = serde_json::from_slice::<Value>(bytes) else {
                continue;
            };
            // Only the model-facing record counts: `event_msg` duplicates,
            // `compacted` history and the session_meta copies in forks do not.
            if rec["type"] != "response_item" {
                continue;
            }
            let Some(ts) = rec["timestamp"].as_str() else { continue };
            let payload = &rec["payload"];
            let found = match payload["type"].as_str() {
                Some("message") if payload["role"] == "user" => slash_blocks(payload),
                Some("function_call") => function_call(payload)
                    .map_or_else(Vec::new, |c| read_events(roots, &session, 0, &c, &mut out.unparsed)),
                Some("custom_tool_call") if payload["name"] == "exec" => {
                    let input = payload["input"].as_str().unwrap_or_default();
                    let mut found = Vec::new();
                    let mut opaque = false;
                    for (i, call) in exec_calls(input).iter().enumerate() {
                        match call {
                            Ok(c) => found.extend(read_events(roots, &session, i, c, &mut out.unparsed)),
                            Err(()) => opaque = true,
                        }
                    }
                    // One record counts once, however many calls in it are opaque.
                    if opaque && roots.names_skill_doc(input, session.cwd.as_deref()) {
                        out.unparsed.push(input.to_string());
                    }
                    found
                }
                _ => continue, // `*_output` records and developer listings land here
            };
            if found.is_empty() {
                continue;
            }
            let hash = sha1_smol::Sha1::from(serde_json::to_vec(payload).expect("a Value always serialises"))
                .digest()
                .to_string();
            for f in found {
                out.raws.push(Raw {
                    key: Key::Codex { hash: hash.clone(), index: f.index },
                    ts: ts.to_string(),
                    skill: f.skill,
                    channel: f.channel,
                    path: Some(f.path),
                    cwd: f.cwd,
                    line: *line,
                    session: session.clone(),
                    name_origin: false,
                });
            }
        }
        out
    }

    fn prompt_at(&self, src: &str) -> Option<String> {
        super::prompt_back(src, user_turn)
    }
}

/// SPEC §4.7: an `event_msg` `user_message`, or the `item_completed`
/// `UserMessage` that replaced it from 2026-08. A bare `$name` gives way to the
/// turn before it, and a `<skill>` injection is never the user's.
fn user_turn(v: &Value) -> Option<String> {
    if v["type"] != "event_msg" {
        return None;
    }
    let p = &v["payload"];
    let text = match p["type"].as_str()? {
        "user_message" => p["message"].as_str()?.to_string(),
        "item_completed" if p["item"]["type"] == "UserMessage" => super::message_text(&p["item"]["content"]),
        _ => return None,
    };
    let text = super::strip_injected(&text)?;
    let bare = text.strip_prefix('$').is_some_and(|name| !name.contains(char::is_whitespace));
    (!bare && !text.starts_with("<skill>")).then_some(text)
}

/// The parent rollout id named by a `session_meta` header line, if any.
pub fn parent_of_header(header: &[u8]) -> Option<String> {
    match parse_header(header)?.session.parent? {
        Parent::Codex(id) => Some(id),
        Parent::Pi(_) => None,
    }
}

struct Header {
    session: Session,
    /// A guardian review replays other sessions' transcripts as evidence.
    guardian: bool,
}

fn parse_header(line: &[u8]) -> Option<Header> {
    let rec: Value = serde_json::from_slice(line).ok()?;
    if rec["type"] != "session_meta" {
        return None;
    }
    let p = &rec["payload"];
    let source = &p["source"];
    let subagent = &source["subagent"];
    let parent = p["forked_from_id"].as_str().or(subagent["thread_spawn"]["parent_thread_id"].as_str());
    let text = |v: &Value| v.as_str().map(str::to_string);
    Some(Header {
        session: Session {
            id: text(&p["id"])?,
            parent: parent.map(|id| Parent::Codex(id.to_string())),
            actor: ActorInputs {
                entrypoint: None,
                subagent: source.get("subagent").is_some(),
                codex_exec: p["originator"] == "codex_exec",
            },
            cwd: text(&p["cwd"]),
            version: text(&p["cli_version"]),
        },
        guardian: p["thread_source"] == "guardian_review" || subagent["other"] == "guardian",
    })
}

/// One event found in a record, before it gets the record's hash.
struct Found {
    index: String,
    skill: String,
    channel: Channel,
    path: String,
    cwd: Option<String>,
}

/// `$name` expansions: `input_text` blocks starting
/// `<skill>\n<name>NAME</name>\n<path>PATH</path>`, indexed by content position.
fn slash_blocks(payload: &Value) -> Vec<Found> {
    let Some(content) = payload["content"].as_array() else {
        return Vec::new();
    };
    content
        .iter()
        .enumerate()
        .filter(|(_, b)| b["type"] == "input_text")
        .filter_map(|(i, b)| {
            let rest = b["text"].as_str()?.strip_prefix("<skill>\n<name>")?;
            let (name, rest) = rest.split_once("</name>\n<path>")?;
            let (path, _) = rest.split_once("</path>")?;
            if name.is_empty() || name.contains(['<', '\n']) || path.is_empty() || path.contains(['<', '\n']) {
                return None;
            }
            Some(Found {
                index: i.to_string(),
                skill: name.into(),
                channel: Channel::Slash,
                path: path.into(),
                cwd: None,
            })
        })
        .collect()
}

/// A shell command and the directory it ran in.
struct Call {
    cmd: String,
    workdir: Option<String>,
}

/// A `function_call` whose JSON `arguments` carry a string `cmd`.
fn function_call(payload: &Value) -> Option<Call> {
    let args: Value = serde_json::from_str(payload["arguments"].as_str()?).ok()?;
    Some(Call { cmd: args["cmd"].as_str()?.to_string(), workdir: args["workdir"].as_str().map(str::to_string) })
}

/// Skill files that one command loads, indexed `<call>.<file>` by position in
/// the command's read list.
fn read_events(roots: &SkillRoots, session: &Session, call: usize, c: &Call, unparsed: &mut Vec<String>) -> Vec<Found> {
    let workdir = match (&c.workdir, &session.cwd) {
        (Some(w), Some(cwd)) => Some(config::resolve(Path::new(cwd), w).to_string_lossy().into_owned()),
        (w, _) => w.clone(),
    };
    let base = workdir.as_deref().or(session.cwd.as_deref());
    roots
        .shell_reads(&c.cmd, base, unparsed)
        .into_iter()
        .map(|(i, sf)| Found {
            index: format!("{call}.{i}"),
            skill: sf.skill,
            channel: sf.channel,
            path: sf.path,
            cwd: workdir.clone(),
        })
        .collect()
}

// ---- The JavaScript of an `exec` input -------------------------------------

const CALLEE: &[u8] = b"tools.exec_command";

/// The `cmd` and `workdir` of each `tools.exec_command({...})` call, in source
/// order. `Err` marks a call whose `cmd` is not a string literal or a template
/// literal without `${…}`, so the command it runs is unknown.
fn exec_calls(src: &str) -> Vec<Result<Call, ()>> {
    let s = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(CALLEE) && (i == 0 || !is_ident(s[i - 1])) {
            let mut j = skip_ws(s, i + CALLEE.len());
            if s.get(j) != Some(&b'(') {
                i = j;
                continue;
            }
            j = skip_ws(s, j + 1);
            let (call, end) = if s.get(j) == Some(&b'{') { object_call(s, j) } else { (Err(()), skip_expr(s, j)) };
            out.push(call);
            i = end;
        } else {
            i = skip_token(s, i);
        }
    }
    out
}

/// Reads the object literal at `s[i] == '{'`; returns the call and the end.
fn object_call(s: &[u8], mut i: usize) -> (Result<Call, ()>, usize) {
    let mut cmd: Option<Result<String, ()>> = None;
    let mut workdir = None;
    i += 1;
    loop {
        let entry_start = i;
        i = skip_ws(s, i);
        match s.get(i) {
            // `)` or `]` here means the object was never closed: stop, do not spin.
            None | Some(b')') | Some(b']') => break,
            Some(b'}') => {
                i += 1;
                break;
            }
            Some(b',') => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let key = match s[i] {
            b'"' | b'\'' => {
                let (k, end) = string_lit(s, i);
                i = end;
                k
            }
            c if is_ident(c) => {
                let start = i;
                while i < s.len() && is_ident(s[i]) {
                    i += 1;
                }
                Some(String::from_utf8_lossy(&s[start..i]).into_owned())
            }
            _ => None, // a spread or computed key: skip the entry
        };
        i = skip_ws(s, i);
        let value = if s.get(i) == Some(&b':') {
            i = skip_ws(s, i + 1);
            let (v, end) = literal(s, i);
            i = end;
            v
        } else {
            i = skip_expr(s, i);
            None // shorthand `{cmd}` or a skipped entry
        };
        match key.as_deref() {
            Some("cmd") => cmd = Some(value.ok_or(())),
            Some("workdir") => workdir = value,
            _ => {}
        }
        if i == entry_start {
            break; // malformed input that no rule consumes
        }
    }
    let call = match cmd {
        Some(Ok(cmd)) => Ok(Call { cmd, workdir }),
        _ => Err(()),
    };
    (call, i)
}

/// A string or substitution-free template literal at `i`, decoded, else `None`;
/// either way returns where the value expression ends.
fn literal(s: &[u8], i: usize) -> (Option<String>, usize) {
    let end = skip_expr(s, i);
    match s.get(i) {
        Some(b'"' | b'\'' | b'`') => {
            let (v, lit_end) = string_lit(s, i);
            if skip_ws(s, lit_end) == end { (v, end) } else { (None, end) } // `"a" + b`
        }
        _ => (None, end),
    }
}

/// Decodes the string or template literal at `i`; `None` for a template with
/// `${…}`. Returns the index after the closing quote.
fn string_lit(s: &[u8], i: usize) -> (Option<String>, usize) {
    let q = s[i];
    let mut out = Vec::new();
    let mut j = i + 1;
    let mut literal = true;
    while j < s.len() {
        match s[j] {
            c if c == q => return (literal.then(|| String::from_utf8_lossy(&out).into_owned()), j + 1),
            b'\\' => j = unescape(s, j + 1, &mut out),
            b'$' if q == b'`' && s.get(j + 1) == Some(&b'{') => {
                literal = false;
                j = skip_braces(s, j + 1);
            }
            c => {
                out.push(c);
                j += 1;
            }
        }
    }
    (None, j)
}

/// Decodes the escape after a backslash at `s[j - 1]`; returns the next index.
fn unescape(s: &[u8], j: usize, out: &mut Vec<u8>) -> usize {
    let Some(&c) = s.get(j) else { return j };
    let simple = match c {
        b'n' => Some('\n'),
        b't' => Some('\t'),
        b'r' => Some('\r'),
        b'b' => Some('\u{8}'),
        b'f' => Some('\u{c}'),
        b'v' => Some('\u{b}'),
        b'0' if !s.get(j + 1).is_some_and(u8::is_ascii_digit) => Some('\0'),
        _ => None,
    };
    let mut push = |ch: char| out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
    if let Some(ch) = simple {
        push(ch);
        return j + 1;
    }
    let hex = |a: usize, b: usize| std::str::from_utf8(s.get(a..b)?).ok().and_then(|h| u32::from_str_radix(h, 16).ok());
    match c {
        b'\n' => j + 1,
        b'\r' => j + 1 + usize::from(s.get(j + 1) == Some(&b'\n')),
        b'x' => match hex(j + 1, j + 3).and_then(char::from_u32) {
            Some(ch) => {
                push(ch);
                j + 3
            }
            None => j + 1,
        },
        b'u' if s.get(j + 1) == Some(&b'{') => {
            let close = s[j..].iter().position(|&b| b == b'}').map_or(s.len(), |p| j + p);
            if let Some(ch) = hex(j + 2, close).and_then(char::from_u32) {
                push(ch);
            }
            close + 1
        }
        b'u' => {
            let Some(hi) = hex(j + 1, j + 5) else { return j + 1 };
            // A surrogate pair is two `\uXXXX` escapes.
            if (0xD800..0xDC00).contains(&hi)
                && s.get(j + 5..j + 7) == Some(b"\\u")
                && let Some(lo @ 0xDC00..0xE000) = hex(j + 7, j + 11)
            {
                push(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{FFFD}'));
                return j + 11;
            }
            push(char::from_u32(hi).unwrap_or('\u{FFFD}'));
            j + 5
        }
        _ => {
            // Any other escaped character stands for itself, whole UTF-8 sequence included.
            out.push(c);
            j + 1
        }
    }
}

/// Skips one token: a whole string, template or comment, else one byte.
fn skip_token(s: &[u8], i: usize) -> usize {
    match (s[i], s.get(i + 1)) {
        (b'"' | b'\'' | b'`', _) => string_lit(s, i).1,
        (b'/', Some(b'/')) => s[i..].iter().position(|&b| b == b'\n').map_or(s.len(), |p| i + p),
        (b'/', Some(b'*')) => s[i + 2..].windows(2).position(|w| w == b"*/").map_or(s.len(), |p| i + 2 + p + 2),
        _ => i + 1,
    }
}

/// Skips an expression up to the `,`, `}`, `)` or `]` that ends it at depth 0.
fn skip_expr(s: &[u8], mut i: usize) -> usize {
    let mut depth = 0usize;
    while i < s.len() {
        match s[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' | b',' if depth == 0 => return i,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        i = skip_token(s, i);
    }
    i
}

/// Skips a `{…}` group starting at `s[i] == '{'`; returns the index after `}`.
fn skip_braces(s: &[u8], mut i: usize) -> usize {
    let mut depth = 0usize;
    while i < s.len() {
        match s[i] {
            b'{' => depth += 1,
            b'}' if depth <= 1 => return i + 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i = skip_token(s, i);
    }
    i
}

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        if s[i].is_ascii_whitespace() {
            i += 1;
        } else if s[i] == b'/' && matches!(s.get(i + 1), Some(b'/' | b'*')) {
            i = skip_token(s, i);
        } else {
            break;
        }
    }
    i
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(src: &str) -> Vec<Option<(String, Option<String>)>> {
        exec_calls(src).into_iter().map(|c| c.ok().map(|c| (c.cmd, c.workdir))).collect()
    }

    #[test]
    fn unclosed_objects_end_instead_of_spinning() {
        // A missing `}` once made the scanner loop forever while holding scan.lock.
        let r = exec_calls("tools.exec_command({cmd: 'cat /s/a/SKILL.md')");
        assert_eq!(r.len(), 1);
        let r = exec_calls("tools.exec_command({a: [1, 2]], cmd: 'x'})");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn exec_inputs_yield_literal_cmds_and_workdirs() {
        let two = "const a = await tools.exec_command({\"cmd\":\"sed -n '1,9p' a/SKILL.md\",\"workdir\":\"/w\"});\n\
                   text(await tools.exec_command({ cmd: `cat \"b\"\\n`, max_output_tokens: 10 }));";
        assert_eq!(
            calls(two),
            vec![Some(("sed -n '1,9p' a/SKILL.md".into(), Some("/w".into()))), Some(("cat \"b\"\n".into(), None)),]
        );
        assert_eq!(calls("tools.exec_command({cmd: `cat ${f}`})"), vec![None]);
        assert_eq!(calls("tools.exec_command({cmd})"), vec![None]);
        assert_eq!(calls("tools.exec_command({cmd: 'a' + b, workdir: w})"), vec![None]);
        assert_eq!(calls("tools.exec_command(args)"), vec![None]);
        assert_eq!(
            calls("tools.exec_command({cmd: '\\u00e9\\u{1F600}\\uD83D\\uDE00\\x41'})"),
            vec![Some(("é😀😀A".into(), None))]
        );
        // A callee named inside a string or comment is not a call.
        assert_eq!(calls("// tools.exec_command({cmd:'x'})\nlog(\"tools.exec_command({cmd:'y'})\")"), vec![]);
        assert_eq!(
            calls("tools.exec_command({cmd: `x`, opts: {a: [1, ')']}, workdir: \"/d\"})"),
            vec![Some(("x".into(), Some("/d".into())))]
        );
    }
}
