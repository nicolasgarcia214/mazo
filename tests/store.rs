//! `mazo add` of a local directory into the store, and the store as a second
//! skill root for `status`, `sync`, `show`, `new` and `retire`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::sync::*;

/// A skill directory outside the source, passing every policy rule of the fixture.
fn incoming(fx: &Fixture, name: &str) -> PathBuf {
    let dir = fx.root.join("incoming").join(name);
    common::write(
        &dir.join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: From elsewhere.\n---\n\n# {name}\n"),
    );
    common::write(&dir.join("agents/openai.yaml"), "policy:\n  allow_implicit_invocation: false\n");
    common::write(&dir.join("references/notes.md"), "notes\n");
    dir
}

/// A fixture already synced, with `theirs` waiting in `incoming/` and allowed
/// in the Claude settings.
fn fixture() -> (Fixture, PathBuf) {
    let mut fx = Fixture::new();
    fx.allow("theirs");
    fx.mazo(&["sync"]).assert_ok();
    let dir = incoming(&fx, "theirs");
    (fx, dir)
}

fn store(fx: &Fixture) -> PathBuf {
    fx.data.join("mazo/store")
}

/// Every skills directory the fixture's homes link into.
fn skills_dirs(fx: &Fixture) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        [".claude", ".claude-work", ".codex"].iter().map(|d| fx.home.join(d).join("skills")).collect();
    dirs.push(fx.target.clone());
    dirs
}

fn lock(fx: &Fixture) -> Vec<toml::Value> {
    let text = fs::read_to_string(fx.data.join("mazo/mazo.lock")).unwrap();
    let lock: toml::Table = toml::from_str(&text).unwrap();
    lock["skill"].as_array().unwrap().clone()
}

fn add(fx: &Fixture, args: &[&str]) -> Run {
    fx.mazo(&[&["add"], args].concat())
}

#[test]
fn add_copies_the_skill_records_it_and_links_it_into_every_home() {
    let (fx, dir) = fixture();
    let r = add(&fx, &[&s(&dir), "--trust"]);
    r.assert_ok();
    let to = store(&fx).join("theirs");
    assert!(r.stdout().contains(&format!("installed {}", to.display())), "{}", r.stdout());
    for rel in ["SKILL.md", "agents/openai.yaml", "references/notes.md"] {
        assert_eq!(fs::read(to.join(rel)).unwrap(), fs::read(dir.join(rel)).unwrap(), "{rel}");
    }
    for skills in skills_dirs(&fx) {
        assert_eq!(fs::read_link(skills.join("theirs")).unwrap(), to, "{}", skills.display());
    }

    let entries = lock(&fx);
    assert_eq!(entries.len(), 1);
    let e = entries[0].as_table().unwrap();
    let mut keys: Vec<&str> = e.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["content_hash", "installed_at", "name", "origin", "subpath", "trusted"]);
    assert_eq!(e["name"].as_str(), Some("theirs"));
    assert_eq!(e["origin"].as_str(), Some(s(&dir).as_str()));
    assert_eq!(e["subpath"].as_str(), Some("."));
    assert_eq!(e["trusted"].as_bool(), Some(true));
    assert!(e["installed_at"].as_str().unwrap().parse::<jiff::Timestamp>().is_ok(), "{}", e["installed_at"]);
    let mut sha = sha1_smol::Sha1::new();
    for rel in ["SKILL.md", "agents/openai.yaml", "references/notes.md"] {
        let bytes = fs::read(dir.join(rel)).unwrap();
        sha.update(format!("{rel}\0{}\0", bytes.len()).as_bytes());
        sha.update(&bytes);
    }
    assert_eq!(e["content_hash"].as_str(), Some(sha.digest().to_string().as_str()));
}

#[test]
fn status_and_sync_keep_store_skills_beside_source_skills() {
    let (fx, dir) = fixture();
    add(&fx, &[&s(&dir), "--trust"]).assert_ok();
    let count = fx.skills.len() + 1;
    let r = fx.mazo(&["status", "--agent", "codex"]);
    assert_eq!(r.stdout(), format!("codex: in sync ({count} skills)\n"), "{}", r.stderr());

    let link = fx.home.join(".codex/skills/theirs");
    fs::remove_file(&link).unwrap();
    let r = fx.mazo(&["status", "--agent", "codex"]);
    assert_eq!(r.code(), 1);
    assert_eq!(r.stdout(), "codex: theirs is not linked (missing)\n");
    let r = fx.mazo(&["sync"]);
    r.assert_ok();
    assert_eq!(r.stdout(), format!("linked    {}\n", link.display()));
    assert_eq!(fs::read_link(&link).unwrap(), store(&fx).join("theirs"));

    // The per-skill rules read the store's copy.
    let manifest = store(&fx).join("theirs/agents/openai.yaml");
    fs::write(&manifest, "policy:\n  allow_implicit_invocation: true\n").unwrap();
    let r = fx.mazo(&["sync"]);
    assert_eq!(r.code(), 1);
    let needle = format!("ERROR: {}: agents/openai.yaml must set", store(&fx).join("theirs").display());
    assert!(r.stderr().contains(&needle), "{}", r.stderr());
}

#[test]
fn a_taken_name_is_refused_before_any_write() {
    let (fx, dir) = fixture();
    add(&fx, &[&s(&dir), "--trust"]).assert_ok();
    let before = fx.snapshot();
    let r = add(&fx, &[&s(&dir), "--trust"]);
    assert_eq!(r.code(), 2, "{}", r.stderr());
    let taken = store(&fx).join("theirs");
    assert!(r.stderr().contains(&format!("{} already exists; pass --as", taken.display())), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before, "the first install is intact");
    assert_eq!(lock(&fx).len(), 1);

    let mine = &fx.skills[0];
    let r = add(&fx, &[&s(&incoming(&fx, mine)), "--trust"]);
    assert_eq!(r.code(), 2, "{}", r.stderr());
    assert!(r.stderr().contains(&format!("{} already exists", fx.source.join(mine).display())), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn as_installs_under_another_name_and_rewrites_the_name() {
    let (mut fx, dir) = fixture();
    add(&fx, &[&s(&dir), "--trust"]).assert_ok();
    fx.allow("other");
    add(&fx, &[&s(&dir), "--as", "other", "--trust"]).assert_ok();
    let text = fs::read_to_string(store(&fx).join("other/SKILL.md")).unwrap();
    assert_eq!(text, "---\nname: other\ndescription: From elsewhere.\n---\n\n# theirs\n");
    assert_eq!(fs::read_link(fx.home.join(".claude/skills/other")).unwrap(), store(&fx).join("other"));
    let names: Vec<String> = lock(&fx).iter().map(|e| e["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["theirs", "other"]);

    let before = fx.snapshot();
    let r = add(&fx, &[&s(&dir), "--as", "Bad_Name", "--trust"]);
    assert_eq!(r.code(), 2, "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);
}

#[test]
fn an_untrusted_local_path_is_refused_until_trust_is_given() {
    let (fx, dir) = fixture();
    let before = fx.snapshot();
    let r = add(&fx, &[&s(&dir)]);
    assert_eq!(r.code(), 2);
    assert!(r.stderr().contains("vetting is not available yet"), "{}", r.stderr());
    assert!(r.stderr().contains("pass --trust to install it without vetting"), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);
    assert!(!exists(&store(&fx)));
}

#[test]
fn an_add_the_sync_would_refuse_installs_nothing() {
    let (fx, _) = fixture();
    // Not in the Claude settings' skillOverrides.
    let dir = incoming(&fx, "unlisted");
    let before = fx.snapshot();
    let r = add(&fx, &[&s(&dir), "--trust"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("\"unlisted\": \"name-only\""), "{}", r.stderr());
    assert!(!exists(&store(&fx).join("unlisted")));
    assert!(!exists(&fx.data.join("mazo/mazo.lock")));
    assert!(!exists(&fx.home.join(".codex/skills/unlisted")));
    let after = fx.snapshot();
    let changed: Vec<&Path> =
        after.iter().filter(|(p, v)| before.get(*p) != Some(*v)).map(|(p, _)| p.as_path()).collect();
    assert!(changed.iter().all(|p| p.starts_with(&fx.data)), "{changed:?}");
}

#[test]
fn new_and_retire_leave_store_skills_alone_and_show_reads_them() {
    let (fx, dir) = fixture();
    add(&fx, &[&s(&dir), "--trust"]).assert_ok();
    let r = fx.mazo(&["new", "theirs", "--description", "Mine."]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains(&format!("{} already exists", store(&fx).join("theirs").display())), "{}", r.stderr());
    assert!(!exists(&fx.source.join("theirs")));

    let r = fx.mazo(&["retire", "theirs", "--force"]);
    assert_eq!(r.code(), 2, "{}", r.stderr());
    assert!(exists(&store(&fx).join("theirs/SKILL.md")));

    let r = fx.mazo(&["show", "theirs", "--json"]);
    r.assert_ok();
    let card: serde_json::Value = serde_json::from_str(&r.stdout()).unwrap();
    assert_eq!(card["description"], "From elsewhere.");
}
