//! `mazo add` of a local directory or a git repository into the store, the
//! store as a second skill root for `status`, `sync`, `show` and `new`, and
//! `retire` of a store skill.

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
fn new_leaves_store_skills_alone_and_show_reads_them() {
    let (fx, dir) = fixture();
    add(&fx, &[&s(&dir), "--trust"]).assert_ok();
    let r = fx.mazo(&["new", "theirs", "--description", "Mine."]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains(&format!("{} already exists", store(&fx).join("theirs").display())), "{}", r.stderr());
    assert!(!exists(&fx.source.join("theirs")));

    let r = fx.mazo(&["show", "theirs", "--json"]);
    r.assert_ok();
    let card: serde_json::Value = serde_json::from_str(&r.stdout()).unwrap();
    assert_eq!(card["description"], "From elsewhere.");
}

/// A ledger holding one strong use of `skill` a day ago.
fn used(fx: &Fixture, skill: &str) {
    let ts = jiff::Timestamp::now().checked_sub(jiff::SignedDuration::from_hours(24)).unwrap().to_string();
    let event = mazo::ledger::Event {
        cwd: None,
        src: "/nonexistent/t.jsonl:1".into(),
        ..common::event(&format!("{skill}-1"), skill, mazo::ledger::Channel::Slash, &format!("s-{skill}"), &ts)
    };
    common::write(&fx.data.join("mazo/events.jsonl"), &(serde_json::to_string(&event).unwrap() + "\n"));
}

#[test]
fn retire_removes_a_store_skill_its_lock_entry_and_its_links() {
    let (mut fx, dir) = fixture();
    add(&fx, &[&s(&dir), "--trust"]).assert_ok();
    fx.allow("other");
    add(&fx, &[&s(&dir), "--as", "other", "--trust"]).assert_ok();
    let source = fx.snapshot().into_iter().filter(|(p, _)| p.starts_with(&fx.source)).collect::<Vec<_>>();

    let r = fx.mazo(&["retire", "theirs"]);
    r.assert_ok();
    let to = store(&fx).join("theirs");
    assert!(r.stdout().contains(&format!("removed   {}", to.display())), "{}", r.stdout());
    assert!(!exists(&to));
    for skills in skills_dirs(&fx) {
        assert!(!exists(&skills.join("theirs")), "{}", skills.display());
        assert_eq!(fs::read_link(skills.join("other")).unwrap(), store(&fx).join("other"));
    }
    let names: Vec<String> = lock(&fx).iter().map(|e| e["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["other"]);
    let after = fx.snapshot().into_iter().filter(|(p, _)| p.starts_with(&fx.source)).collect::<Vec<_>>();
    assert_eq!(after, source, "the source, archive included, is untouched");
    assert!(!exists(&fx.source.join(".archive/theirs")));
    let r = fx.mazo(&["status", "--agent", "codex"]);
    assert_eq!(r.stdout(), format!("codex: in sync ({} skills)\n", fx.skills.len() + 1), "{}", r.stderr());
}

#[test]
fn retire_refuses_a_store_skill_cited_by_a_used_skill_and_force_overrides_it() {
    let (fx, dir) = fixture();
    add(&fx, &[&s(&dir), "--trust"]).assert_ok();
    let mine = &fx.skills[0];
    let skill_md = fx.source.join(mine).join("SKILL.md");
    let text = fs::read_to_string(&skill_md).unwrap();
    fs::write(&skill_md, format!("{text}\nRun the theirs skill first.\n")).unwrap();
    used(&fx, mine);

    let before = fx.snapshot();
    let r = fx.mazo(&["retire", "theirs"]);
    assert_eq!(r.code(), 1, "{}", r.stdout());
    assert!(r.stderr().contains(&format!("theirs is held by {mine}")), "{}", r.stderr());
    assert!(r.stderr().contains("ERROR: retire refused; pass --force"), "{}", r.stderr());
    let after = fx.snapshot();
    let changed: Vec<&Path> =
        after.iter().filter(|(p, v)| before.get(*p) != Some(*v)).map(|(p, _)| p.as_path()).collect();
    assert!(changed.iter().all(|p| p.starts_with(fx.data.join("mazo")) && !p.starts_with(store(&fx))), "{changed:?}");
    assert!(exists(&store(&fx).join("theirs/SKILL.md")));
    assert_eq!(lock(&fx).len(), 1);

    fx.mazo(&["retire", "theirs", "--force"]).assert_ok();
    assert!(!exists(&store(&fx).join("theirs")));
    assert!(lock(&fx).is_empty());
    assert!(skills_dirs(&fx).iter().all(|d| !exists(&d.join("theirs"))));
}

#[test]
fn a_used_store_skill_holds_the_source_skill_it_cites() {
    let (fx, dir) = fixture();
    let mine = &fx.skills[0];
    let text = format!("---\nname: theirs\ndescription: From elsewhere.\n---\n\nRun the {mine} skill first.\n");
    fs::write(dir.join("SKILL.md"), text).unwrap();
    add(&fx, &[&s(&dir), "--trust"]).assert_ok();
    used(&fx, "theirs");
    let r = fx.mazo(&["retire", mine]);
    assert_eq!(r.code(), 1, "{}", r.stdout());
    assert!(r.stderr().contains(&format!("{mine} is held by theirs")), "{}", r.stderr());
    assert!(exists(&fx.source.join(mine).join("SKILL.md")));
}

/// A bare repository `origin/<repo>.git` holding a skill per `(subpath, name)`,
/// `.` being its root, and its `file://` URL.
fn bare_repo(fx: &Fixture, repo: &str, skills: &[(&str, &str)]) -> String {
    let work = fx.root.join("work").join(repo);
    for (sub, name) in skills {
        let dir = work.join(sub);
        common::write(
            &dir.join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: From a repo.\n---\n\n# {name}\n"),
        );
        common::write(&dir.join("agents/openai.yaml"), "policy:\n  allow_implicit_invocation: false\n");
    }
    common::git(&work, &["init", "-q"], common::NOW);
    common::git(&work, &["add", "."], common::NOW);
    common::git(&work, &["commit", "-q", "-m", "skills"], common::NOW);
    let bare = fx.root.join("origin").join(format!("{repo}.git"));
    common::git(&work, &["clone", "-q", "--bare", ".", &s(&bare)], common::NOW);
    format!("file://{}", bare.display())
}

fn head(url: &str) -> String {
    let dir = url.strip_prefix("file://").unwrap();
    let out = std::process::Command::new("git").args(["-C", dir, "rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// The fixture with its data directory, where `add` clones, already made.
fn git_fixture() -> Fixture {
    let (fx, _) = fixture();
    fs::create_dir_all(fx.data.join("mazo")).unwrap();
    fx
}

/// No clone is left in the data directory.
fn assert_no_clone(fx: &Fixture) {
    let left: Vec<_> = fs::read_dir(fx.data.join("mazo"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".clone"))
        .map(|e| e.path())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn add_clones_a_one_skill_repo_and_records_its_origin_and_commit() {
    let fx = git_fixture();
    let url = bare_repo(&fx, "theirs", &[(".", "theirs")]);
    let r = add(&fx, &[&url, "--trust"]);
    r.assert_ok();
    let to = store(&fx).join("theirs");
    assert!(r.stdout().contains(&format!("installed {}", to.display())), "{}", r.stdout());
    assert!(exists(&to.join("SKILL.md")));
    assert!(!exists(&to.join(".git")));
    assert_eq!(fs::read_link(fx.home.join(".codex/skills/theirs")).unwrap(), to);
    let entries = lock(&fx);
    let e = entries[0].as_table().unwrap();
    let mut keys: Vec<&str> = e.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["commit", "content_hash", "installed_at", "name", "origin", "subpath", "trusted"]);
    assert_eq!(e["origin"].as_str(), Some(url.as_str()));
    assert_eq!(e["subpath"].as_str(), Some("."));
    assert_eq!(e["commit"].as_str(), Some(head(&url).as_str()));
    assert_eq!(head(&url).len(), 40);
    assert_no_clone(&fx);
}

#[test]
fn a_repo_with_several_skills_needs_skill_or_a_subpath() {
    let mut fx = git_fixture();
    fx.allow("one");
    fx.allow("two");
    let url = bare_repo(&fx, "pack", &[("skills/one", "one"), ("skills/two", "two")]);
    let before = fx.snapshot();
    let r = add(&fx, &[&format!("{url}/skills"), "--trust"]);
    assert_eq!(r.code(), 2, "{}", r.stderr());
    assert!(r.stderr().contains("pass --skill with one of: one, two"), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);

    let r = add(&fx, &[&format!("{url}/skills"), "--skill", "nope", "--trust"]);
    assert_eq!(r.code(), 2, "{}", r.stderr());
    assert!(r.stderr().contains("no skill named nope; the skills here are: one, two"), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);

    add(&fx, &[&format!("{url}/skills"), "--skill", "two", "--trust"]).assert_ok();
    add(&fx, &[&format!("{url}/skills/one"), "--trust"]).assert_ok();
    let entries = lock(&fx);
    let got: Vec<(&str, &str, &str)> = entries
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e["origin"].as_str().unwrap(), e["subpath"].as_str().unwrap()))
        .collect();
    assert_eq!(got, [("two", url.as_str(), "skills/two"), ("one", url.as_str(), "skills/one")]);
    assert!(exists(&store(&fx).join("one/SKILL.md")) && exists(&store(&fx).join("two/SKILL.md")));
    assert_no_clone(&fx);
}

#[test]
fn a_repo_on_the_trust_list_installs_without_trust() {
    let fx = git_fixture();
    let url = bare_repo(&fx, "theirs", &[(".", "theirs")]);
    let before = fx.snapshot();
    let r = add(&fx, &[&url]);
    assert_eq!(r.code(), 2, "{}", r.stderr());
    assert!(r.stderr().contains("is not on the trust list"), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);

    // The owner and repository of a file URL are its first two path segments.
    let segments: Vec<&str> = url.strip_prefix("file:///").unwrap().split('/').take(2).collect();
    let config = fx.source.join("mazo.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("trusted = [{:?}]\n{text}", segments.join("/"))).unwrap();
    add(&fx, &[&url]).assert_ok();
    assert_eq!(lock(&fx)[0]["trusted"].as_bool(), Some(true));
}

#[test]
fn a_failed_clone_exits_1_with_gits_error_and_writes_nothing() {
    let fx = git_fixture();
    let url = format!("file://{}", fx.root.join("origin/missing.git").display());
    let before = fx.snapshot();
    let r = add(&fx, &[&url, "--trust"]);
    assert_eq!(r.code(), 1, "{}", r.stderr());
    assert!(r.stderr().contains(&format!("git clone {url}: fatal:")), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);
    assert!(!exists(&store(&fx)));
    assert!(!exists(&fx.data.join("mazo/mazo.lock")));
}

#[test]
fn a_subpath_symlinked_out_of_the_clone_is_refused() {
    let fx = git_fixture();
    let outside = incoming(&fx, "outside");
    let work = fx.root.join("work/linky");
    fs::create_dir_all(&work).unwrap();
    std::os::unix::fs::symlink(&outside, work.join("outside")).unwrap();
    common::git(&work, &["init", "-q"], common::NOW);
    common::git(&work, &["add", "."], common::NOW);
    common::git(&work, &["commit", "-q", "-m", "link"], common::NOW);
    let bare = fx.root.join("origin/linky.git");
    common::git(&work, &["clone", "-q", "--bare", ".", &s(&bare)], common::NOW);
    let before = fx.snapshot();
    let r = add(&fx, &[&format!("file://{}/outside", bare.display()), "--trust"]);
    assert_eq!(r.code(), 2, "{}", r.stderr());
    assert!(r.stderr().contains(&format!("{} leaves the repository", outside.display())), "{}", r.stderr());
    assert_eq!(fx.snapshot(), before);
}
