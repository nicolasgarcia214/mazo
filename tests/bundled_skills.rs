#![allow(clippy::disallowed_methods)]

use std::fs;
use std::path::{Path, PathBuf};

const BUNDLED: [&str; 2] = ["grade", "retro"];

fn skills() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")
}

fn files(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .flat_map(|p| if p.is_dir() { files(&p) } else { vec![p] })
        .collect()
}

#[test]
fn bundled_skills_pass_the_always_on_checks() {
    for name in BUNDLED {
        let text = fs::read_to_string(skills().join(name).join("SKILL.md")).unwrap();
        let meta = mazo::yaml::frontmatter(&text).unwrap();
        mazo::policy::always_on(&meta, name).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

fn fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let mark = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let run = trimmed.chars().take_while(|&c| c == mark).count();
    (run >= 3).then_some((mark, run))
}

fn without_code_spans(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        out.push_str(&rest[..start]);
        let run = rest[start..].chars().take_while(|&c| c == '`').count();
        let after = &rest[start + run..];
        let close = "`".repeat(run);
        match after.find(&close) {
            Some(end) => rest = &after[end + run..],
            None => {
                out.push_str(&rest[start..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn link_targets(text: &str) -> Vec<String> {
    let link = regex::Regex::new(r"\]\(\s*<?([^)\s>]+)").unwrap();
    let mut open: Option<(char, usize)> = None;
    let mut targets = Vec::new();
    for line in text.lines() {
        match (open, fence(line)) {
            (None, Some(f)) => open = Some(f),
            (Some((mark, run)), Some((m, r))) if m == mark && r >= run => open = None,
            (None, None) => targets.extend(link.captures_iter(&without_code_spans(line)).map(|c| c[1].to_string())),
            _ => {}
        }
    }
    targets
}

fn has_scheme(target: &str) -> bool {
    target.split_once(':').is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

#[test]
fn every_relative_link_resolves_inside_skills() {
    let root = skills().canonicalize().unwrap();
    let mut broken = Vec::new();
    for file in files(&root) {
        if file.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for target in link_targets(&text) {
            let path = target.split('#').next().unwrap();
            if path.is_empty() || has_scheme(&target) {
                continue;
            }
            let resolved = file.parent().unwrap().join(path).canonicalize();
            if !resolved.is_ok_and(|r| r.starts_with(&root)) {
                let name = file.strip_prefix(&root).unwrap().display();
                broken.push(format!("{name}: {target}"));
            }
        }
    }
    assert!(broken.is_empty(), "unresolved links:\n{}", broken.join("\n"));
}
