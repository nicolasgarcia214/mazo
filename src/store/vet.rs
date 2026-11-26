use std::fs;
use std::path::{Path, PathBuf};

use super::ask::Consent;
use super::work;
use crate::Result;
use crate::audit::{self, Caller, Vet};
use crate::inventory::Inventory;
use crate::ledger::Ledger;

const VET_FILE_BYTES: u64 = 64 * 1024;
const VET_TOTAL_BYTES: u64 = 256 * 1024;

const JUDGE_EXAMPLES: [&str; 2] = [
    "# command: any CLI that reads the prompt on stdin\n[judge]\nagent = \"command\"\nargv = [\"llm\", \"-m\", \"gpt-5\"]\n",
    "# http: an OpenAI-compatible endpoint\n[judge]\nagent = \"http\"\nurl = \"https://api.openai.com/v1\"\n\
     model = \"gpt-5\"\napi_key_env = \"OPENAI_API_KEY\"\n",
];

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

pub(super) fn contents(dir: &Path, files: &[PathBuf]) -> Result<Vec<(String, Vec<u8>)>> {
    let files = files.iter().filter(|rel| !dir.join(rel).is_dir());
    files
        .map(|rel| {
            let path = dir.join(rel);
            Ok((rel.to_string_lossy().into_owned(), fs::read(&path).map_err(crate::at(&path))?))
        })
        .collect()
}

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
                withheld.push(format!("{} ({size} bytes, {why})", crate::render::inert(label)));
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
    text.iter().for_each(|l| consent.io.line(l));
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
