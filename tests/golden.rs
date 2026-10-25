//! Golden files: every report rendered from the report fixture at NOW in
//! UTC, and the judge's answer schemas, compared byte for byte with `tests/golden/`. `UPDATE_GOLDEN=1`
//! rewrites the files instead, so a renderer change shows up as their diff.

mod common;

use std::fs;
use std::path::Path;

use common::golden::Golden;
use mazo::report::{self, Since, UsageArgs};
use mazo::{audit, html};

/// `out` with the fixture's path, and its `~` form, replaced by `placeholder`.
fn scrub(out: String, source: &Path, placeholder: &str) -> String {
    let path = source.display().to_string();
    let out = out.replace(&path, placeholder).replace(&report::tilde(&path), placeholder);
    // The tempdir's name holds no character the HTML escapes, so nothing
    // escaped is left behind; a leftover would make the files machine-specific.
    let name = source.file_name().unwrap().to_str().unwrap();
    assert!(!out.contains(name), "the fixture path is left in: {out}");
    out
}

#[test]
fn reports_match_their_golden_files() {
    let golden = Golden::new();
    let repo = &golden.repo;
    let input = golden.world.input(&golden.events);
    let usage = UsageArgs { since: Since::Days(30), agent: None, channel: None, all: true, authoring: false };
    let mut outputs = vec![];
    for (ext, json) in [("json", true), ("txt", false)] {
        outputs.push((format!("usage-all.{ext}"), report::usage(&input, &usage, json, false)));
        outputs.push((format!("unused.{ext}"), report::unused(&input, Since::Days(30), json, false)));
        for skill in ["goal-smart", "unslop", "writing-for-agents"] {
            let card = report::show(&input, skill, report::SINCE, 20, json, false).unwrap();
            outputs.push((format!("show-{skill}.{ext}"), card));
        }
    }
    outputs.push(("report.html".into(), html::render(&report::report(&input, Since::Days(30)))));
    // The judge's answer schemas, so a change to them is deliberate.
    outputs.push(("session-schema.json".into(), audit::SESSION_SCHEMA.clone()));
    outputs.push(("skill-schema.json".into(), audit::SKILL_SCHEMA.clone()));

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let update = std::env::var_os("UPDATE_GOLDEN").is_some_and(|v| v == "1");
    let mut stale = vec![];
    for (file, out) in outputs {
        // The page shows the placeholder escaped, so it renders as written.
        let placeholder = if file.ends_with(".html") { "&lt;skills&gt;" } else { "<skills>" };
        let out = scrub(out, repo.path(), placeholder);
        // Pretty JSON, so a change is a readable diff.
        let out = if file.ends_with(".json") {
            serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(&out).unwrap()).unwrap() + "\n"
        } else {
            out
        };
        let path = dir.join(&file);
        if update {
            fs::create_dir_all(&dir).unwrap();
            fs::write(&path, &out).unwrap();
        } else if fs::read_to_string(&path).ok().as_deref() != Some(out.as_str()) {
            stale.push(file);
        }
    }
    assert!(stale.is_empty(), "differ from tests/golden/ (UPDATE_GOLDEN=1 rewrites them): {stale:?}");
}
