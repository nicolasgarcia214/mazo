//! The vet gate: one judge call on a skill, within byte caps that withhold
//! what cannot be sent, and its verdict obeyed; [`no_judge`] when there is
//! no judge to call.

use std::fs;
use std::path::{Path, PathBuf};

use super::ask::Consent;
use super::{Out, err};
use crate::Result;
use crate::audit::{self, Caller, Vet};
use crate::inventory::Inventory;
use crate::ledger::Ledger;
use crate::work;

/// The most bytes of one file, and of all files, the vet judge is sent; a
/// file past either, or one that is not text, is withheld.
const VET_FILE_BYTES: u64 = 64 * 1024;
const VET_TOTAL_BYTES: u64 = 256 * 1024;

/// The judges any setup can configure, each as a `[judge]` table to paste
/// into the config, after a TOML comment saying what it calls.
const JUDGE_EXAMPLES: [&str; 2] = [
    "# command: any CLI that reads the prompt on stdin\n[judge]\nagent = \"command\"\nargv = [\"llm\", \"-m\", \"gpt-5\"]\n",
    "# http: an OpenAI-compatible endpoint\n[judge]\nagent = \"http\"\nurl = \"https://api.openai.com/v1\"\n\
     model = \"gpt-5\"\napi_key_env = \"OPENAI_API_KEY\"\n",
];

/// What to do with no `[judge]`, after an untrusted add or update is
/// refused for want of one: [`JUDGE_EXAMPLES`], and a prefilled issue asking
/// for a judge for the user's harness.
pub(super) fn no_judge() -> String {
    let body = "mazo refuses an untrusted add or update without a [judge], and none of its judges fits the harness I use.\n\n\
                Harness: <name and version>\n\
                How it runs one prompt without a terminal: <command or API>\n";
    let issue = url::Url::parse_with_params(
        "https://github.com/nicolasgarcia214/mazo/issues/new",
        [("title", "Support <harness> as a judge"), ("body", body)],
    )
    .expect("a valid URL");
    format!(
        "add one of these judges to the config:\n{}or ask for a judge for your harness: {issue}",
        JUDGE_EXAMPLES.join("")
    )
}

/// Every file of `files` under `src` as `(relative path, bytes)`, with
/// `skill_md` as its `SKILL.md`.
pub(super) fn contents(src: &Path, files: &[PathBuf], skill_md: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let files = files.iter().filter(|rel| !src.join(rel).is_dir());
    files
        .map(|rel| {
            let bytes = if rel == Path::new("SKILL.md") {
                skill_md.as_bytes().to_vec()
            } else {
                let path = src.join(rel);
                fs::read(&path).map_err(|e| err(&path, e))?
            };
            Ok((rel.to_string_lossy().into_owned(), bytes))
        })
        .collect()
}

/// The vet gate, before anything is installed: one judge call on `files` of
/// the skill `name` and, for an update, the `diff` from the installed copy,
/// with the names and descriptions of every other source and store skill
/// and the [`work::summary`] of the user's recent work; its verdict shown,
/// then obeyed. A file or diff withheld from the judge (not text, or past
/// [`VET_FILE_BYTES`] or [`VET_TOTAL_BYTES`]) was never reviewed, so it is a
/// security finding of mazo's own. A security finding
/// refuses (exit 1) unless `--force`, a withheld one before the judge is
/// called; then the user is asked y/N, defaulting to the verdict's
/// [`VetVerdict::default_yes`](audit::VetVerdict::default_yes), unless
/// `--yes`. A judge that gives no valid answer, or a No, refuses.
pub(super) fn gate(
    c: &Caller,
    inv: &Inventory,
    consent: Consent,
    name: &str,
    files: Vec<(String, Vec<u8>)>,
    diff: Option<Vec<u8>>,
) -> Result<Vet> {
    let (verb, act, kept) = (consent.verb(), consent.act(), consent.kept());
    let (mut withheld, mut total) = (Vec::new(), 0);
    let mut sent = |label: &str, bytes: Vec<u8>| {
        let size = bytes.len() as u64;
        let why = if size > VET_FILE_BYTES {
            Err("over the per-file cap")
        } else if total + size > VET_TOTAL_BYTES {
            Err("over the total cap")
        } else {
            String::from_utf8(bytes).map_err(|_| "not text")
        };
        match why {
            Ok(text) => {
                total += size;
                text
            }
            Err(why) => {
                withheld.push(format!("{label} ({size} bytes, {why})"));
                format!("(withheld: {size} bytes, {why})")
            }
        }
    };
    let files: Vec<(String, String)> = files.into_iter().map(|(rel, bytes)| (rel.clone(), sent(&rel, bytes))).collect();
    let diff = diff.map(|d| sent("the diff", d));
    let withheld = (!withheld.is_empty()).then(|| format!("not sent to the judge in full: {}", withheld.join(", ")));
    if let Some(withheld) = &withheld
        && !consent.force
    {
        return Err(format!("{verb} refused: {withheld}; pass --force to {act} it anyway").into());
    }
    let others = inv.skills.iter().filter(|s| *s != name);
    let existing: Vec<(String, String)> =
        others.clone().map(|s| (s.clone(), inv.description(s).unwrap_or_default())).collect();
    let work = work::summary(Ledger::load(c.data)?.events(), jiff::Timestamp::now());
    let input = audit::vet_input(name, &files, diff.as_deref(), &existing, &work);
    let names: Vec<&str> = others.map(String::as_str).collect();
    let mut vet = audit::vet(c, name, &input, &names)?
        .map_err(|why| format!("{verb} refused: vetting {name} failed: {why}; {kept}"))?;
    if let Some(withheld) = withheld {
        let found = vet.verdict.security.take();
        vet.verdict.security = Some(found.map_or_else(|| withheld.clone(), |f| format!("{f}; {withheld}")));
    }
    let text: Vec<String> = vet.text(name).lines().map(String::from).collect();
    text.iter().for_each(|l| Out(c.pane).line(l));
    if vet.verdict.security.is_some() && !consent.force {
        return Err(format!("{verb} refused: the judge found a security issue; pass --force to {act} it anyway").into());
    }
    if consent.yes {
        return Ok(vet);
    }
    if !consent.ask(text, &format!("{act} {name}?"), vet.verdict.default_yes())? {
        return Err(format!("{verb} declined; {kept}").into());
    }
    Ok(vet)
}

#[cfg(test)]
mod tests {
    use super::{JUDGE_EXAMPLES, no_judge};

    #[test]
    fn each_judge_example_is_printed_and_loads_as_a_valid_config_judge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for example in JUDGE_EXAMPLES {
            assert!(no_judge().contains(example), "{example}");
            std::fs::write(&path, format!("source = \".\"\n{example}")).unwrap();
            let cfg = crate::config::load(&path).unwrap_or_else(|e| panic!("{example}: {e}")).unwrap();
            assert!(cfg.judge.is_some(), "{example}");
        }
    }
}
