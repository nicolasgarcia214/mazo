//! The PolicyLoader port (SPEC §5.4).
//!
//! `install.sh` loaded frontmatter and `agents/openai.yaml` with PyYAML's
//! `SafeLoader` minus its YAML 1.1 booleans, plus duplicate-key and merge-key
//! rejection. This module rebuilds that loader over `saphyr-parser` events:
//! compose a node graph, resolve plain scalars with PyYAML's regexes, then
//! construct values the way `SafeConstructor` does. `tests/strict_yaml_diff.rs`
//! runs both loaders over one corpus and is the arbiter of every rule here.

use regex::Regex;
use saphyr_parser::{Event, Parser, ScalarStyle, Span};
use std::collections::HashMap;
use std::fmt;
use std::sync::LazyLock;

/// A loaded YAML value, typed as PyYAML's SafeLoader with literal-only booleans types it.
#[derive(Debug, Clone, PartialEq)]
pub enum Yaml {
    Null,
    Bool(bool),
    /// Decimal digits with an optional leading `-`; unbounded, as Python's `int`.
    Int(String),
    Float(f64),
    Str(String),
    /// A `datetime.date`, as `YYYY-MM-DD`.
    Date(String),
    /// A `datetime.datetime`, as Python's `isoformat()` writes it.
    DateTime(String),
    Binary(Vec<u8>),
    Seq(Vec<Yaml>),
    /// `!!omap` and `!!pairs`: a list of `(key, value)` tuples. Keys may repeat.
    Pairs(Vec<(Yaml, Yaml)>),
    /// `!!set`: the keys of a mapping, in document order.
    Set(Vec<Yaml>),
    Map(Vec<(Yaml, Yaml)>),
}

impl Yaml {
    /// The value under a string key of a mapping.
    pub fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Map(pairs) => pairs.iter().find(|(k, _)| matches!(k, Yaml::Str(s) if s == key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// A calendar date as `YYYY-MM-DD`: an unquoted date, the date part of an
    /// unquoted timestamp as written, or a string holding a valid `YYYY-MM-DD`.
    pub fn as_date(&self) -> Option<String> {
        match self {
            Yaml::Date(d) => Some(d.clone()),
            Yaml::DateTime(dt) => Some(dt[..10].to_string()),
            Yaml::Str(s) => {
                let (y, m, d) = (s.get(0..4)?, s.get(5..7)?, s.get(8..10)?);
                let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
                let shape = s.len() == 10 && &s[4..5] == "-" && &s[7..8] == "-";
                if !(shape && digits(y) && digits(m) && digits(d)) {
                    return None;
                }
                valid_date(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?).then(|| s.clone())
            }
            _ => None,
        }
    }
}

/// Python's `str()` of the constructed value, for error messages.
impl fmt::Display for Yaml {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Yaml::Null => f.write_str("None"),
            Yaml::Bool(b) => f.write_str(if *b { "True" } else { "False" }),
            Yaml::Int(s) | Yaml::Str(s) | Yaml::Date(s) => f.write_str(s),
            Yaml::DateTime(s) => f.write_str(&s.replacen('T', " ", 1)),
            Yaml::Float(x) if x.is_nan() => f.write_str("nan"),
            Yaml::Float(x) if x.is_infinite() => f.write_str(if *x > 0.0 { "inf" } else { "-inf" }),
            Yaml::Float(x) if x.fract() == 0.0 && x.abs() < 1e16 => write!(f, "{x:.1}"),
            Yaml::Float(x) => write!(f, "{x}"),
            Yaml::Binary(b) => write!(f, "b{:?}", String::from_utf8_lossy(b)),
            _ => write!(f, "{self:?}"),
        }
    }
}

/// The frontmatter of a SKILL.md, grade or retro file, extracted as
/// `install.sh:141-145` does and loaded with [`load`].
pub fn frontmatter(text: &str) -> crate::Result<Yaml> {
    load(&split_frontmatter(text)?.0)
}

/// `(frontmatter YAML, body)` per the SPEC §5.4 extraction rules, both joined with `\n`.
pub fn split_frontmatter(text: &str) -> crate::Result<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines = splitlines(text);
    if lines.first() != Some(&"---") {
        return Err("SKILL.md must start with YAML frontmatter".into());
    }
    let end = lines[1..].iter().position(|l| *l == "---").ok_or("YAML frontmatter has no closing --- line")? + 1;
    Ok((lines[1..end].join("\n"), lines[end + 1..].join("\n")))
}

/// Python's `str.splitlines()`.
fn splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            lines.push(&s[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && chars.peek().map(|&(_, n)| n) == Some('\n') {
                chars.next();
                start += 1;
            }
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// Load one YAML document as `yaml.load(text, Loader=PolicyLoader)` does.
/// An empty stream is `Null`.
pub fn load(text: &str) -> crate::Result<Yaml> {
    let text = reader(text)?;
    let mut events = Vec::new();
    for item in Parser::new_from_str(&text) {
        events.push(item.map_err(|e| format!("invalid YAML: {e}"))?);
    }
    lexical_checks(&text, &events)?;
    let mut composer = Composer { nodes: Vec::new(), anchors: HashMap::new(), depth: 0 };
    let mut root = None;
    let mut events = events.into_iter().map(|(e, _)| e);
    while let Some(event) = events.next() {
        if let Event::DocumentStart(_) = event {
            if root.is_some() {
                return Err("expected a single YAML document in the stream".into());
            }
            let first = events.next().ok_or("truncated YAML event stream")?;
            root = Some(composer.node(first, &mut events)?);
        }
    }
    let Some(root) = root else { return Ok(Yaml::Null) };
    if composer.nodes[root].size > MAX_NODES {
        return Err("YAML aliases expand to too many nodes".into());
    }
    Constructor { nodes: &composer.nodes }.construct(root)
}

/// PyYAML's `Reader`: drop a leading BOM, reject non-printable characters. NEL is a line break in
/// YAML 1.1 and is fed to saphyr as one; LS and PS are rejected rather than
/// given YAML 1.1 line-break semantics saphyr does not have.
fn reader(text: &str) -> crate::Result<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{85}' => out.push('\n'),
            '\u{2028}' | '\u{2029}' => return Err(format!("unsupported line separator {:?} in YAML", c)),
            '\t' | '\n' | '\r' | ' '..='~' | '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'.. => {
                out.push(c)
            }
            _ => return Err(format!("special characters are not allowed in YAML: {:?}", c)),
        }
    }
    Ok(out)
}

/// What a character of the source belongs to, for the checks saphyr (YAML 1.2)
/// does not make and PyYAML's 1.1 scanner does.
#[derive(Clone, Copy, PartialEq)]
enum Region {
    Structure,
    Plain,
    /// Quoted and block scalars: tabs are content.
    Text,
    Alias,
    Comment,
}

/// PyYAML's scanner only skips spaces between tokens, so a tab outside a
/// quoted scalar, block scalar or comment is an error (`a: b\tc`). It allows
/// only `[0-9A-Za-z_-]` in anchor and alias names, rejects a repeated anchor,
/// a key-less `: value`, and a `%YAML` directive other than 1.x.
fn lexical_checks(text: &str, events: &[(Event, Span)]) -> crate::Result<()> {
    let chars: Vec<char> = text.chars().collect();
    let mut region = vec![Region::Structure; chars.len()];
    let mut mark = |span: &Span, r: Region| {
        let end = span.end.index().min(chars.len());
        for slot in &mut region[span.start.index().min(end)..end] {
            *slot = r;
        }
    };
    for (event, span) in events {
        match event {
            Event::Scalar(_, ScalarStyle::Plain, ..) => mark(span, Region::Plain),
            Event::Scalar(..) => mark(span, Region::Text),
            Event::Alias(_) => mark(span, Region::Alias),
            _ => {}
        }
    }
    let mut line_start = true;
    let mut in_comment = false;
    for i in 0..chars.len() {
        let c = chars[i];
        if in_comment && region[i] == Region::Structure {
            region[i] = Region::Comment;
        }
        if c == '\n' || c == '\r' {
            in_comment = false;
        } else if c == '#' && region[i] == Region::Structure && (line_start || matches!(chars[i - 1], ' ' | '\t')) {
            in_comment = true;
            region[i] = Region::Comment;
        }
        line_start = c == '\n' || c == '\r';
    }

    // A `:` with no key before it (`: a`, `{: a}`) is a parse error in
    // PyYAML; saphyr reads an empty key. Only an explicit `?` may leave a key empty.
    let mut open: Vec<(bool, usize)> = Vec::new();
    for (event, span) in events {
        let key_slot = matches!(open.last(), Some((true, n)) if n % 2 == 0);
        match event {
            Event::SequenceEnd | Event::MappingEnd => {
                open.pop();
            }
            _ => {
                if let Some((_, n)) = open.last_mut() {
                    *n += 1;
                }
            }
        }
        match event {
            Event::SequenceStart(..) => open.push((false, 0)),
            Event::MappingStart(..) => open.push((true, 0)),
            Event::Scalar(v, ScalarStyle::Plain, 0, None) if key_slot && v.is_empty() => {
                let before = chars[..span.start.index().min(chars.len())]
                    .iter()
                    .zip(&region)
                    .rev()
                    .find(|&(c, r)| !c.is_whitespace() && *r != Region::Comment);
                if !matches!(before, Some(('?', _))) {
                    return Err("found a mapping value with no key".into());
                }
            }
            _ => {}
        }
    }

    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let name_at = |from: usize| {
        let len = chars[from..].iter().take_while(|&&c| !c.is_whitespace() && !",[]{}".contains(c)).count();
        let name: String = chars[from..from + len].iter().collect();
        if name.is_empty() || !name.chars().all(is_name) || matches!(chars.get(from + len), Some('[' | '{')) {
            return Err(format!("invalid anchor or alias name {name:?}: use only [0-9A-Za-z_-]"));
        }
        Ok(name)
    };
    let mut anchors = Vec::new();
    let mut directive = false;
    for i in 0..chars.len() {
        let c = chars[i];
        let starts_line = i == 0 || matches!(chars[i - 1], '\n' | '\r');
        if starts_line {
            directive = c == '%';
            let line: String = chars[i..].iter().take_while(|&&c| c != '\n' && c != '\r').collect();
            if region[i] == Region::Structure
                && let Some(version) = line.strip_prefix("%YAML")
            {
                let major = version.split_whitespace().next().and_then(|v| v.split('.').next());
                if major.and_then(|m| m.parse::<u64>().ok()) != Some(1) {
                    return Err(format!("incompatible YAML document {line:?}: version 1.* is required"));
                }
            }
        }
        match region[i] {
            Region::Structure | Region::Plain if c == '\t' => {
                return Err("found a tab outside a quoted scalar, block scalar or comment".into());
            }
            Region::Alias if i == 0 || region[i - 1] != Region::Alias => {
                name_at(i + 1)?;
            }
            Region::Structure
                if c == '&' && !directive && (starts_line || matches!(chars[i - 1], ' ' | '[' | '{' | ',')) =>
            {
                let name = name_at(i + 1)?;
                if anchors.contains(&name) {
                    return Err(format!("found duplicate anchor {name:?}"));
                }
                anchors.push(name);
            }
            _ => {}
        }
    }
    Ok(())
}

const NULL: &str = "tag:yaml.org,2002:null";
const BOOL: &str = "tag:yaml.org,2002:bool";
const INT: &str = "tag:yaml.org,2002:int";
const FLOAT: &str = "tag:yaml.org,2002:float";
const TIMESTAMP: &str = "tag:yaml.org,2002:timestamp";
const STR: &str = "tag:yaml.org,2002:str";
const SEQ: &str = "tag:yaml.org,2002:seq";
const MAP: &str = "tag:yaml.org,2002:map";
const MERGE: &str = "tag:yaml.org,2002:merge";
const VALUE: &str = "tag:yaml.org,2002:value";

/// A pathological alias graph stops here instead of exhausting memory.
const MAX_NODES: usize = 1_000_000;
/// Nesting stops here instead of overflowing the stack.
const MAX_DEPTH: usize = 500;

enum Kind {
    Scalar(String),
    Seq(Vec<usize>),
    Map(Vec<(usize, usize)>),
}

struct Node {
    tag: String,
    kind: Kind,
    /// Nodes in the fully alias-expanded subtree.
    size: usize,
    /// Nesting depth of the alias-expanded subtree, which bounds recursion.
    height: usize,
}

/// PyYAML's `Composer`: events to a node graph, aliases as shared indices.
struct Composer {
    nodes: Vec<Node>,
    /// Anchor id to its node, `None` while the anchored node is still open.
    anchors: HashMap<usize, Option<usize>>,
    /// Collections open around the current event.
    depth: usize,
}

impl Composer {
    fn node<'a>(&mut self, event: Event<'a>, events: &mut impl Iterator<Item = Event<'a>>) -> crate::Result<usize> {
        let is_seq = matches!(event, Event::SequenceStart(..));
        let (anchor, node) = match event {
            Event::Alias(id) => {
                return match self.anchors.get(&id) {
                    Some(Some(index)) => Ok(*index),
                    Some(None) => Err("recursive YAML aliases are not supported".into()),
                    None => Err("found undefined YAML alias".into()),
                };
            }
            Event::Scalar(value, style, anchor, tag) => {
                let tag = tag.map(|t| format!("{}{}", t.handle, t.suffix));
                let tag = match tag.as_deref() {
                    None if style == ScalarStyle::Plain => resolve(&value).to_string(),
                    None => STR.to_string(),
                    Some("!") => resolve(&value).to_string(),
                    Some(t) => t.to_string(),
                };
                (anchor, Node { tag, kind: Kind::Scalar(value.into_owned()), size: 1, height: 1 })
            }
            Event::SequenceStart(anchor, tag) | Event::MappingStart(anchor, tag) => {
                self.open(anchor);
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err("YAML nesting is too deep".into());
                }
                let tag = match tag.map(|t| format!("{}{}", t.handle, t.suffix)) {
                    Some(t) if t != "!" => t,
                    _ => if is_seq { SEQ } else { MAP }.to_string(),
                };
                let mut children = Vec::new();
                loop {
                    match events.next().ok_or("truncated YAML event stream")? {
                        Event::SequenceEnd | Event::MappingEnd => break,
                        child => children.push(self.node(child, events)?),
                    }
                }
                self.depth -= 1;
                let size = children.iter().fold(1usize, |n, &c| n.saturating_add(self.nodes[c].size));
                let height = 1 + children.iter().map(|&c| self.nodes[c].height).max().unwrap_or(0);
                if height > MAX_DEPTH {
                    return Err("YAML nesting is too deep".into());
                }
                let kind = if is_seq {
                    Kind::Seq(children)
                } else {
                    Kind::Map(children.chunks(2).map(|kv| (kv[0], kv[1])).collect())
                };
                (anchor, Node { tag, kind, size, height })
            }
            other => return Err(format!("unexpected YAML event {other:?}")),
        };
        self.nodes.push(node);
        let index = self.nodes.len() - 1;
        if anchor != 0 {
            self.anchors.insert(anchor, Some(index));
        }
        Ok(index)
    }

    fn open(&mut self, anchor: usize) {
        if anchor != 0 {
            self.anchors.insert(anchor, None);
        }
    }
}

/// `Resolver.resolve` for a plain scalar, with `PolicyLoader`'s resolver table:
/// SafeLoader's, minus its YAML 1.1 booleans, plus `^(true|false)$`.
fn resolve(value: &str) -> &'static str {
    static FLOAT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"^(?:[-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?",
            r"|\.[0-9][0-9_]*(?:[eE][-+][0-9]+)?",
            r"|[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*",
            r"|[-+]?\.(?:inf|Inf|INF)",
            r"|\.(?:nan|NaN|NAN))$",
        ))
        .unwrap()
    });
    static INT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"^(?:[-+]?0b[0-1_]+",
            r"|[-+]?0[0-7_]+",
            r"|[-+]?(?:0|[1-9][0-9_]*)",
            r"|[-+]?0x[0-9a-fA-F_]+",
            r"|[-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+)$",
        ))
        .unwrap()
    });
    static TIMESTAMP_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"^(?:[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]",
            r"|[0-9][0-9][0-9][0-9]-[0-9][0-9]?-[0-9][0-9]?",
            r"(?:[Tt]|[ \t]+)[0-9][0-9]?",
            r":[0-9][0-9]:[0-9][0-9](?:\.[0-9]*)?",
            r"(?:[ \t]*(?:Z|[-+][0-9][0-9]?(?::[0-9][0-9])?))?)$",
        ))
        .unwrap()
    });
    static BOOL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:true|false)$").unwrap());
    static NULL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:~|null|Null|NULL|)$").unwrap());
    static MERGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:<<)$").unwrap());
    static VALUE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:=)$").unwrap());
    static YAML_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:!|&|\*)$").unwrap());

    let candidates: &[(&'static str, &Regex)] = match value.chars().next() {
        None | Some('~' | 'n' | 'N') => &[(NULL, &NULL_RE)],
        Some('t' | 'f') => &[(BOOL, &BOOL_RE)],
        Some('0'..='9') => &[(FLOAT, &FLOAT_RE), (INT, &INT_RE), (TIMESTAMP, &TIMESTAMP_RE)],
        Some('-' | '+') => &[(FLOAT, &FLOAT_RE), (INT, &INT_RE)],
        Some('.') => &[(FLOAT, &FLOAT_RE)],
        Some('<') => &[(MERGE, &MERGE_RE)],
        Some('=') => &[(VALUE, &VALUE_RE)],
        Some('!' | '&' | '*') => &[("tag:yaml.org,2002:yaml", &YAML_RE)],
        Some(_) => &[],
    };
    candidates.iter().find(|(_, re)| py_match(re, value)).map_or(STR, |(tag, _)| tag)
}

/// Python's `re.match` against a `^…$` pattern: `$` also matches before a final newline.
fn py_match(re: &Regex, value: &str) -> bool {
    re.is_match(value) || value.strip_suffix('\n').is_some_and(|v| re.is_match(v))
}

/// PyYAML's `SafeConstructor` with `PolicyLoader`'s `construct_mapping` and
/// `construct_boolean`.
struct Constructor<'n> {
    nodes: &'n [Node],
}

impl Constructor<'_> {
    fn construct(&self, index: usize) -> crate::Result<Yaml> {
        let node = &self.nodes[index];
        Ok(match node.tag.as_str() {
            NULL => {
                self.scalar(index)?;
                Yaml::Null
            }
            BOOL => match &node.kind {
                Kind::Scalar(v) if v == "true" || v == "false" => Yaml::Bool(v == "true"),
                _ => return Err("invocation booleans must be literal true or false".into()),
            },
            INT => Yaml::Int(construct_int(self.scalar(index)?)?.to_string()),
            FLOAT => Yaml::Float(construct_float(self.scalar(index)?)?.0),
            "tag:yaml.org,2002:binary" => Yaml::Binary(construct_binary(self.scalar(index)?)?),
            TIMESTAMP => match &node.kind {
                Kind::Scalar(v) => construct_timestamp(v)?,
                _ => return Err("expected a scalar node for a timestamp".into()),
            },
            STR => Yaml::Str(self.scalar(index)?.to_string()),
            SEQ => match &node.kind {
                Kind::Seq(items) => Yaml::Seq(items.iter().map(|&i| self.construct(i)).collect::<crate::Result<_>>()?),
                _ => return Err("expected a sequence node".into()),
            },
            MAP => Yaml::Map(self.mapping(index)?),
            "tag:yaml.org,2002:set" => Yaml::Set(self.mapping(index)?.into_iter().map(|(k, _)| k).collect()),
            "tag:yaml.org,2002:omap" | "tag:yaml.org,2002:pairs" => Yaml::Pairs(self.pairs(index)?),
            tag => return Err(format!("could not determine a constructor for the tag {tag:?}")),
        })
    }

    /// `SafeConstructor.construct_scalar`: a scalar's text, or for a mapping,
    /// the scalar under its first `=` key.
    fn scalar(&self, index: usize) -> crate::Result<&str> {
        match &self.nodes[index].kind {
            Kind::Scalar(v) => Ok(v),
            Kind::Map(pairs) => match pairs.iter().find(|(k, _)| self.nodes[*k].tag == VALUE) {
                Some(&(_, v)) => self.scalar(v),
                None => Err("expected a scalar node, but found a mapping".into()),
            },
            Kind::Seq(_) => Err("expected a scalar node, but found a sequence".into()),
        }
    }

    /// `PolicyLoader.construct_mapping`: no merge keys, no duplicate keys
    /// (compared as Python compares them), then every value.
    fn mapping(&self, index: usize) -> crate::Result<Vec<(Yaml, Yaml)>> {
        let Kind::Map(pairs) = &self.nodes[index].kind else {
            return Err("expected a mapping node".into());
        };
        let mut seen: Vec<Key> = Vec::new();
        let mut out = Vec::with_capacity(pairs.len());
        for &(k, v) in pairs {
            if self.nodes[k].tag == MERGE {
                return Err("YAML merge keys are not supported in invocation policy".into());
            }
            let key = self.construct(k)?;
            let id = self.key(k, &key)?;
            if seen.contains(&id) {
                return Err(format!("duplicate YAML key: {key}"));
            }
            seen.push(id);
            out.push((key, self.construct(v)?));
        }
        Ok(out)
    }

    /// `construct_yaml_omap` / `construct_yaml_pairs`: a sequence of
    /// one-pair mappings, whatever their tags, with no key checks.
    fn pairs(&self, index: usize) -> crate::Result<Vec<(Yaml, Yaml)>> {
        let Kind::Seq(items) = &self.nodes[index].kind else {
            return Err("expected a sequence for an ordered map".into());
        };
        items
            .iter()
            .map(|&i| match &self.nodes[i].kind {
                Kind::Map(kv) if kv.len() == 1 => Ok((self.construct(kv[0].0)?, self.construct(kv[0].1)?)),
                _ => Err("expected a mapping of length 1 in an ordered map".into()),
            })
            .collect()
    }

    /// Python's hash equality for a mapping key.
    fn key(&self, index: usize, value: &Yaml) -> crate::Result<Key> {
        Ok(match value {
            Yaml::Null => Key::Null,
            Yaml::Bool(b) => Key::Number(if *b { "1" } else { "0" }.into()),
            Yaml::Int(n) => Key::Number(n.clone()),
            Yaml::Float(x) if x.is_nan() => {
                // `construct_yaml_float` returns one shared NaN object for
                // `.nan`, and `in` checks identity first; any other NaN is a
                // new object, equal only to itself (the same node via aliases).
                let shared = matches!(self.scalar(index).map(construct_float), Ok(Ok((_, true))));
                Key::Nan(if shared { None } else { Some(index) })
            }
            Yaml::Float(x) if x.is_infinite() => Key::Number(if *x > 0.0 { "inf" } else { "-inf" }.into()),
            Yaml::Float(x) if x.fract() == 0.0 => Key::Number(format!("{:.0}", x.abs())).signed(*x < 0.0),
            Yaml::Float(x) => Key::Number(format!("f{:x}", x.to_bits())),
            Yaml::Str(s) => Key::Str(s.clone()),
            Yaml::Binary(b) => Key::Bytes(b.clone()),
            Yaml::Date(d) => Key::Date(d.clone()),
            Yaml::DateTime(dt) => datetime_key(dt),
            Yaml::Seq(_) | Yaml::Pairs(_) | Yaml::Set(_) | Yaml::Map(_) => {
                return Err("found unhashable key".into());
            }
        })
    }
}

#[derive(PartialEq)]
enum Key {
    Null,
    /// Bools, ints and floats with an exact integer value, as decimal; other
    /// floats by bits.
    Number(String),
    /// `None` is PyYAML's shared `nan_value`; `Some(node)` a NaN of its own.
    Nan(Option<usize>),
    Str(String),
    Bytes(Vec<u8>),
    Date(String),
    NaiveDateTime(String),
    /// Microseconds since the epoch, UTC: aware datetimes compare as instants.
    AwareDateTime(i128),
}

impl Key {
    fn signed(self, negative: bool) -> Key {
        match self {
            Key::Number(n) if negative && n != "0" => Key::Number(format!("-{n}")),
            other => other,
        }
    }
}

fn datetime_key(iso: &str) -> Key {
    // isoformat: YYYY-MM-DDTHH:MM:SS[.ffffff][±HH:MM]
    let num = |r: std::ops::Range<usize>| iso[r].parse::<i128>().unwrap_or(0);
    let (body, offset) = match iso.len() {
        19 | 26 => return Key::NaiveDateTime(iso.to_string()),
        n => (&iso[..n - 6], &iso[n - 6..]),
    };
    let micros = if body.len() == 26 { num(20..26) } else { 0 };
    let days = days_from_civil(num(0..4) as i64, num(5..7) as i64, num(8..10) as i64) as i128;
    let secs = days * 86400 + num(11..13) * 3600 + num(14..16) * 60 + num(17..19);
    let off_min = offset[1..3].parse::<i128>().unwrap_or(0) * 60 + offset[4..6].parse::<i128>().unwrap_or(0);
    let off = if offset.starts_with('-') { -off_min } else { off_min };
    Key::AwareDateTime((secs - off * 60) * 1_000_000 + micros)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

fn valid_date(year: u32, month: u32, day: u32) -> bool {
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=9999).contains(&year) && (1..=days).contains(&day)
}

/// `construct_yaml_timestamp`.
fn construct_timestamp(value: &str) -> crate::Result<Yaml> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"^(?P<year>[0-9][0-9][0-9][0-9])-(?P<month>[0-9][0-9]?)-(?P<day>[0-9][0-9]?)",
            r"(?:(?:[Tt]|[ \t]+)(?P<hour>[0-9][0-9]?):(?P<minute>[0-9][0-9]):(?P<second>[0-9][0-9])",
            r"(?:\.(?P<fraction>[0-9]*))?",
            r"(?:[ \t]*(?P<tz>Z|(?P<tz_sign>[-+])(?P<tz_hour>[0-9][0-9]?)(?::(?P<tz_minute>[0-9][0-9]))?))?)?$",
        ))
        .unwrap()
    });
    let caps = RE
        .captures(value)
        .or_else(|| value.strip_suffix('\n').and_then(|v| RE.captures(v)))
        .ok_or_else(|| format!("invalid timestamp {value:?}"))?;
    let num = |name: &str| caps.name(name).map_or(0, |m| m.as_str().parse::<u32>().unwrap());
    let (year, month, day) = (num("year"), num("month"), num("day"));
    if !valid_date(year, month, day) {
        return Err(format!("invalid date in timestamp {value:?}"));
    }
    let date = format!("{year:04}-{month:02}-{day:02}");
    if caps.name("hour").is_none() {
        return Ok(Yaml::Date(date));
    }
    let (hour, minute, second) = (num("hour"), num("minute"), num("second"));
    if hour > 23 || minute > 59 || second > 59 {
        return Err(format!("invalid time in timestamp {value:?}"));
    }
    let mut iso = format!("{date}T{hour:02}:{minute:02}:{second:02}");
    let fraction = caps.name("fraction").map_or("", |m| m.as_str());
    let micros: u32 = format!("{:0<6}", &fraction[..fraction.len().min(6)]).parse().unwrap();
    if micros != 0 {
        iso += &format!(".{micros:06}");
    }
    if caps.name("tz_sign").is_some() {
        let offset = num("tz_hour") * 60 + num("tz_minute");
        if offset >= 24 * 60 {
            return Err(format!("timezone offset out of range in {value:?}"));
        }
        let sign = if offset == 0 || &caps["tz_sign"] == "+" { '+' } else { '-' };
        iso += &format!("{sign}{:02}:{:02}", offset / 60, offset % 60);
    } else if caps.name("tz").is_some() {
        iso += "+00:00";
    }
    Ok(Yaml::DateTime(iso))
}

/// `construct_yaml_int`, with Python's `int(text, base)` underneath.
fn construct_int(raw: &str) -> crate::Result<BigInt> {
    let bad = || format!("invalid integer {raw:?}");
    let value = raw.replace('_', "");
    let first = value.chars().next().ok_or_else(bad)?;
    let negative = first == '-';
    let value = if first == '-' || first == '+' { &value[1..] } else { &value[..] };
    let n = if value == "0" {
        BigInt::default()
    } else if let Some(digits) = value.strip_prefix("0b") {
        py_int(digits, 2).ok_or_else(bad)?
    } else if let Some(digits) = value.strip_prefix("0x") {
        py_int(digits, 16).ok_or_else(bad)?
    } else if value.chars().next().ok_or_else(bad)? == '0' {
        py_int(value, 8).ok_or_else(bad)?
    } else if value.contains(':') {
        let mut total = BigInt::default();
        for part in value.split(':') {
            total.mul_small(60);
            total = total.add(&py_int(part, 10).ok_or_else(bad)?);
        }
        total
    } else {
        py_int(value, 10).ok_or_else(bad)?
    };
    Ok(if negative { n.neg() } else { n })
}

/// `construct_yaml_float`. The flag marks PyYAML's shared `nan_value`.
fn construct_float(raw: &str) -> crate::Result<(f64, bool)> {
    let bad = || format!("invalid float {raw:?}");
    let value = raw.replace('_', "").to_lowercase();
    let first = value.chars().next().ok_or_else(bad)?;
    let sign = if first == '-' { -1.0 } else { 1.0 };
    let value = if first == '-' || first == '+' { &value[1..] } else { &value[..] };
    if value == ".inf" {
        return Ok((sign * f64::INFINITY, false));
    }
    if value == ".nan" {
        return Ok((f64::NAN, true));
    }
    if value.contains(':') {
        let digits = value.split(':').map(py_float).collect::<Option<Vec<f64>>>().ok_or_else(bad)?;
        let (mut total, mut base) = (0.0, 1u128);
        for digit in digits.iter().rev() {
            total += digit * base as f64;
            base = base.checked_mul(60).ok_or_else(bad)?;
        }
        return Ok((sign * total, false));
    }
    Ok((sign * py_float(value).ok_or_else(bad)?, false))
}

/// Python's `str.strip()` whitespace.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Python's `float(text)` for ASCII input.
fn py_float(text: &str) -> Option<f64> {
    let t = text.trim_matches(py_space);
    let unsigned = t.strip_prefix(['+', '-']).unwrap_or(t);
    // Rust also parses "infinity"/"inf"/"nan" case-insensitively, as Python does.
    if unsigned.is_empty() || unsigned.starts_with(['+', '-']) {
        return None;
    }
    t.parse().ok()
}

/// Python's `int(text, base)` for ASCII digits.
fn py_int(text: &str, base: u32) -> Option<BigInt> {
    let t = text.trim_matches(py_space);
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let prefix = match base {
        2 => ["0b", "0B"],
        8 => ["0o", "0O"],
        16 => ["0x", "0X"],
        _ => ["", ""],
    };
    let digits = prefix.iter().filter(|p| !p.is_empty()).find_map(|p| t.strip_prefix(p)).unwrap_or(t);
    if digits.is_empty() {
        return None;
    }
    let mut n = BigInt::default();
    for c in digits.chars() {
        n.mul_small(base);
        n = n.add(&BigInt::from(c.to_digit(base)?));
    }
    Some(if negative { n.neg() } else { n })
}

/// `base64.decodebytes`: CPython 3.14's non-strict `a2b_base64`, which skips
/// characters outside the alphabet and pads followed by more data, and
/// requires the final quad to be complete or completed by trailing pads.
fn construct_binary(text: &str) -> crate::Result<Vec<u8>> {
    if !text.is_ascii() {
        return Err("failed to convert base64 data into ascii".into());
    }
    let (mut out, mut quad, mut left, mut pads) = (Vec::new(), 0, 0u32, 0);
    for b in text.bytes() {
        if b == b'=' {
            pads += 1;
            continue;
        }
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        pads = 0;
        match quad {
            0 => left = v,
            1 => {
                out.push(((left << 2) | (v >> 4)) as u8);
                left = v & 0x0f;
            }
            2 => {
                out.push(((left << 4) | (v >> 2)) as u8);
                left = v & 0x03;
            }
            _ => out.push(((left << 6) | v) as u8),
        }
        quad = (quad + 1) % 4;
    }
    if quad == 1 || (quad != 0 && quad + pads < 4) {
        return Err("failed to decode base64 data: incorrect padding".into());
    }
    Ok(out)
}

/// A signed integer of any size, for Python's unbounded `int`.
#[derive(Clone, Default, PartialEq)]
struct BigInt {
    negative: bool,
    /// Little-endian base-1e9 limbs, no trailing zero limbs; zero is empty.
    limbs: Vec<u32>,
}

const LIMB: u64 = 1_000_000_000;

impl From<u32> for BigInt {
    fn from(n: u32) -> Self {
        let mut b = BigInt { negative: false, limbs: vec![n % LIMB as u32, n / LIMB as u32] };
        b.trim();
        b
    }
}

impl BigInt {
    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
        if self.limbs.is_empty() {
            self.negative = false;
        }
    }

    fn neg(mut self) -> Self {
        self.negative = !self.negative;
        self.trim();
        self
    }

    fn mul_small(&mut self, m: u32) {
        let mut carry = 0u64;
        for limb in &mut self.limbs {
            let x = *limb as u64 * m as u64 + carry;
            *limb = (x % LIMB) as u32;
            carry = x / LIMB;
        }
        while carry > 0 {
            self.limbs.push((carry % LIMB) as u32);
            carry /= LIMB;
        }
        self.trim();
    }

    fn cmp_magnitude(&self, other: &Self) -> std::cmp::Ordering {
        self.limbs.len().cmp(&other.limbs.len()).then_with(|| self.limbs.iter().rev().cmp(other.limbs.iter().rev()))
    }

    fn add(&self, other: &Self) -> Self {
        let mut limbs = Vec::new();
        let negative;
        if self.negative == other.negative {
            negative = self.negative;
            let mut carry = 0u64;
            for i in 0..self.limbs.len().max(other.limbs.len()) {
                let x = *self.limbs.get(i).unwrap_or(&0) as u64 + *other.limbs.get(i).unwrap_or(&0) as u64 + carry;
                limbs.push((x % LIMB) as u32);
                carry = x / LIMB;
            }
            if carry > 0 {
                limbs.push(carry as u32);
            }
        } else {
            let (big, small) = if self.cmp_magnitude(other).is_ge() { (self, other) } else { (other, self) };
            negative = big.negative;
            let mut borrow = 0i64;
            for i in 0..big.limbs.len() {
                let mut x = big.limbs[i] as i64 - *small.limbs.get(i).unwrap_or(&0) as i64 - borrow;
                borrow = (x < 0) as i64;
                if x < 0 {
                    x += LIMB as i64;
                }
                limbs.push(x as u32);
            }
        }
        let mut n = BigInt { negative, limbs };
        n.trim();
        n
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some((top, rest)) = self.limbs.split_last() else { return f.write_str("0") };
        if self.negative {
            f.write_str("-")?;
        }
        write!(f, "{top}")?;
        rest.iter().rev().try_for_each(|limb| write!(f, "{limb:09}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Yaml {
        Yaml::Str(v.into())
    }

    #[test]
    fn frontmatter_drops_bom_and_splits_crlf() {
        let y = frontmatter("\u{feff}---\r\nname: a\r\ndescription: b\r\n---\r\nbody\r\n").unwrap();
        assert_eq!(y, Yaml::Map(vec![(s("name"), s("a")), (s("description"), s("b"))]));
    }

    #[test]
    fn frontmatter_splits_on_every_python_line_break() {
        for sep in ["\n", "\r", "\r\n", "\x0b", "\x0c", "\x1c", "\x1d", "\x1e", "\u{85}", "\u{2028}", "\u{2029}"] {
            let y = frontmatter(&format!("---{sep}a: 1{sep}---{sep}")).unwrap();
            assert_eq!(y.get("a"), Some(&Yaml::Int("1".into())), "separator {sep:?}");
        }
    }

    #[test]
    fn frontmatter_requires_opening_and_closing_lines() {
        assert_eq!(frontmatter("name: a\n").unwrap_err(), "SKILL.md must start with YAML frontmatter");
        assert_eq!(frontmatter("").unwrap_err(), "SKILL.md must start with YAML frontmatter");
        assert_eq!(frontmatter("--- \na: 1\n---\n").unwrap_err(), "SKILL.md must start with YAML frontmatter");
        assert!(frontmatter("---\na: 1\n").is_err());
        assert!(frontmatter("---\na: 1\n----\n").is_err());
        assert_eq!(frontmatter("---\n---\n").unwrap(), Yaml::Null);
    }

    #[test]
    fn frontmatter_ends_at_the_first_closing_line() {
        let y = frontmatter("---\na: 1\n---\nb: 2\n---\n").unwrap();
        assert_eq!(y, Yaml::Map(vec![(s("a"), Yaml::Int("1".into()))]));
    }

    #[test]
    fn pathological_inputs_are_errors_not_crashes() {
        let flow = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(load(&flow).is_err());
        let block: String = (0..2000).map(|i| format!("{}-\n", "  ".repeat(i))).collect();
        assert!(load(&block).is_err());
        let a = "a: &a ".to_string() + &"[".repeat(200) + &"]".repeat(200);
        let b = "\nb: &b ".to_string() + &"[".repeat(200) + "*a" + &"]".repeat(200);
        let c = "\nc: ".to_string() + &"[".repeat(200) + "*b" + &"]".repeat(200);
        assert_eq!(load(&(a + &b + &c)).unwrap_err(), "YAML nesting is too deep");
        let mut bomb = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..9 {
            let p = i - 1;
            bomb += &format!("a{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n");
        }
        assert_eq!(load(&bomb).unwrap_err(), "YAML aliases expand to too many nodes");
    }

    #[test]
    fn as_date_reads_dates_and_datetimes() {
        let y =
            load("a: x\nb: true\nc: 2026-01-02\nd: '2026-02-03'\ne: 2026-01-02 10:00:00Z\nf: '2026-02-30'\n").unwrap();
        assert_eq!(y.get("c").and_then(Yaml::as_date).as_deref(), Some("2026-01-02"));
        assert_eq!(y.get("d").and_then(Yaml::as_date).as_deref(), Some("2026-02-03"));
        assert_eq!(y.get("e").and_then(Yaml::as_date).as_deref(), Some("2026-01-02"));
        assert_eq!(y.get("f").and_then(Yaml::as_date), None);
    }
}
