//! The skills bundled under `skills/`: they pass the always-on SKILL.md
//! checks and keep their reports under `evals_dir`, never in dot directories
//! of a skills root.

#![allow(clippy::disallowed_methods)]

use std::fs;
use std::path::{Path, PathBuf};

const BUNDLED: [&str; 2] = ["grade", "retro"];

fn skills() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")
}

/// Every file under `dir`, recursively.
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

#[test]
fn no_bundled_file_names_the_old_report_directories() {
    for file in files(&skills()) {
        let text = fs::read_to_string(&file).unwrap();
        for old in [".grades", ".retros"] {
            assert!(!text.contains(old), "{} mentions {old}", file.display());
        }
    }
}
