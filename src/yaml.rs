//! YAML loading: `saphyr` with the YAML 1.2 core schema, duplicate
//! keys rejected at any depth. Scalars are kept as written, so callers apply
//! the strictness they need where a value is used.

use std::borrow::Cow;

pub use saphyr::YamlOwned as Yaml;
use saphyr::{LoadableYamlNode, ScalarOwned, ScalarStyle, YamlLoader};
use saphyr_parser::Parser;

/// One YAML document, its scalars left as `Yaml::Representation` (text, style, tag).
pub fn load(text: &str) -> crate::Result<Yaml> {
    // saphyr's own load resolves scalars, so its duplicate-key check compares
    // `a`, `'a'` and `!!str a` as the same key, in every mapping.
    if Yaml::load_from_str(text).map_err(|e| e.to_string())?.len() > 1 {
        return Err("YAML holds more than one document".into());
    }
    let mut loader = YamlLoader::<Yaml>::default();
    loader.early_parse(false);
    Parser::new_from_str(text).load(&mut loader, true).map_err(|e| e.to_string())?;
    if let Some(e) = loader.error() {
        return Err(e.to_string().into());
    }
    Ok(loader.into_documents().pop().unwrap_or(Yaml::Value(ScalarOwned::Null)))
}

/// The value under a string key of a mapping.
pub fn get<'a>(map: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    match map {
        Yaml::Mapping(m) => m.iter().find(|(k, _)| string(k) == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

/// A scalar's text as written, whatever its style or tag.
pub fn text(node: &Yaml) -> Option<&str> {
    match node {
        Yaml::Representation(s, _, _) => Some(s),
        _ => None,
    }
}

/// A scalar the core schema resolves to a string.
pub fn string(node: &Yaml) -> Option<&str> {
    let Yaml::Representation(s, style, tag) = node else { return None };
    let tag = tag.as_ref().map(Cow::Borrowed);
    let value = ScalarOwned::parse_from_cow_and_metadata(s.as_str().into(), *style, tag.as_ref());
    matches!(value, Some(ScalarOwned::String(_))).then_some(s)
}

/// A scalar the core schema resolves to `true`.
pub fn is_true(node: &Yaml) -> bool {
    let Yaml::Representation(s, style, tag) = node else { return false };
    let tag = tag.as_ref().map(Cow::Borrowed);
    let value = ScalarOwned::parse_from_cow_and_metadata(s.as_str().into(), *style, tag.as_ref());
    matches!(value, Some(ScalarOwned::Boolean(true)))
}

/// The plain, untagged scalar `false`. `"false"`, `False`, `no` and `!!bool false` are not.
pub fn is_plain_false(node: &Yaml) -> bool {
    matches!(node, Yaml::Representation(s, ScalarStyle::Plain, None) if s == "false")
}

/// The frontmatter of a SKILL.md, grade or retro file, loaded with [`load`].
pub fn frontmatter(text: &str) -> crate::Result<Yaml> {
    load(&split_frontmatter(text)?.0)
}

/// `(frontmatter YAML, body)` per the frontmatter extraction rules, both joined with `\n`.
pub fn split_frontmatter(text: &str) -> crate::Result<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    if lines.first() != Some(&"---") {
        return Err("SKILL.md must start with YAML frontmatter".into());
    }
    let end = lines[1..].iter().position(|l| *l == "---").ok_or("YAML frontmatter has no closing --- line")? + 1;
    Ok((lines[1..end].join("\n"), lines[end + 1..].join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(yaml: &str) -> crate::Result<bool> {
        let doc = load(yaml)?;
        Ok(get(&doc, "policy").and_then(|p| get(p, "allow_implicit_invocation")).is_some_and(is_plain_false))
    }

    #[test]
    fn duplicate_keys_are_rejected_at_any_depth() {
        for yaml in [
            "a: 1\na: 2\n",
            "a: 1\n'a': 2\n",
            "a: 1\n!!str a: 2\n",
            "x:\n  y:\n    z: 1\n    \"z\": 2\n",
            "- k: 1\n  k: 2\n",
            "x: {a: 1, a: 2}\n",
            "1: a\n0x1: b\n",
        ] {
            assert!(load(yaml).is_err(), "{yaml:?} loaded");
        }
        assert!(load("a: 1\n'1': 2\n1: 3\nx:\n  a: 1\n").is_ok(), "distinct keys and the same key in two mappings");
    }

    #[test]
    fn only_plain_false_disables_implicit_invocation() {
        assert!(policy("policy:\n  allow_implicit_invocation: false\n").unwrap());
        for value in ["\"false\"", "'false'", "no", "False", "FALSE", "!!bool false", "!foo false", "0", "true"] {
            let yaml = format!("policy:\n  allow_implicit_invocation: {value}\n");
            assert!(!policy(&yaml).unwrap(), "{value} accepted");
        }
    }

    #[test]
    fn strings_are_scalars_the_core_schema_resolves_to_str() {
        let doc =
            load("a: text\nb: 'true'\nc: !!str 1\nd: 1\ne: true\nf: ~\ng: [x]\nh: {x: y}\ni: 2026-10-12\n").unwrap();
        let s = |k| get(&doc, k).and_then(string);
        assert_eq!([s("a"), s("b"), s("c"), s("i")], [Some("text"), Some("true"), Some("1"), Some("2026-10-12")]);
        for k in ["d", "e", "f", "g", "h"] {
            assert_eq!(s(k), None, "{k}");
        }
    }

    #[test]
    fn frontmatter_splits_on_lf_and_crlf_after_a_bom() {
        let (fm, body) = split_frontmatter("\u{feff}---\r\nname: s\n---\r\nbody\n").unwrap();
        assert_eq!((fm.as_str(), body.as_str()), ("name: s", "body"));
        assert!(split_frontmatter("---\nname: s\n").is_err());
        assert!(split_frontmatter("name: s\n---\n").is_err());
    }
}
