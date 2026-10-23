//! What the store asks: the TUI's [`Question`]s, the y/N of [`Consent`],
//! the judge-call count of [`confirm_calls`], and the skill picker of
//! [`skills_in`].

use std::io::{BufRead, IsTerminal};
use std::path::Path;
use std::sync::mpsc;

use super::{Add, Out, err};
use crate::inventory::description;
use crate::{Error, Result};

/// A question the TUI's add job puts to the main loop, which answers it in a
/// dialog. A dialog closed without a reply is No, or nothing picked.
pub enum Question {
    /// The skills picked from each one's path and description.
    Pick(Vec<(String, String)>, mpsc::Sender<Vec<String>>),
    /// `lines`, then `prompt` ending in `[Y/n]` or `[y/N]`: Enter takes `default`.
    YesNo { lines: Vec<String>, prompt: String, default: bool, reply: mpsc::Sender<bool> },
}

/// What the vet gate guards, an add or an update, which words its prompt and
/// refusals, the flags that answer for the user, and where it asks.
#[derive(Clone, Copy)]
pub(super) struct Consent<'a> {
    pub(super) update: bool,
    /// `--yes`.
    pub(super) yes: bool,
    /// `--force`.
    pub(super) force: bool,
    /// The TUI's dialogs; the terminal when `None`.
    pub(super) asks: Option<&'a mpsc::Sender<Question>>,
}

impl Consent<'_> {
    pub(super) fn verb(self) -> &'static str {
        if self.update { "update" } else { "add" }
    }

    pub(super) fn act(self) -> &'static str {
        if self.update { "update" } else { "install" }
    }

    pub(super) fn kept(self) -> &'static str {
        if self.update { "the skill was left as it was" } else { "nothing was installed" }
    }

    /// Refused before the judge is paid for an answer nobody can accept.
    pub(super) fn answerable(self) -> std::result::Result<(), String> {
        if self.yes || self.asks.is_some() || std::io::stdin().is_terminal() {
            return Ok(());
        }
        Err(format!(
            "{} refused: a vetted {} asks y/N; pass --yes when stdin is not a terminal",
            self.verb(),
            self.act()
        ))
    }

    /// `prompt` asked y/N, Enter taking `default`: on stdin, after `lines`
    /// were printed, or in a TUI dialog showing `lines` first.
    pub(super) fn ask(self, lines: Vec<String>, prompt: &str, default: bool) -> Result<bool> {
        let prompt = format!("{prompt} {}", if default { "[Y/n]" } else { "[y/N]" });
        if let Some(asks) = self.asks {
            let (reply, answer) = mpsc::channel();
            _ = asks.send(Question::YesNo { lines, prompt, default, reply });
            return Ok(answer.recv().unwrap_or(false));
        }
        eprint!("{prompt} ");
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("stdin: {e}"))?;
        Ok(match answer.trim() {
            "" => default,
            a => matches!(a, "y" | "Y" | "yes"),
        })
    }
}

/// `add --all`, or any add from the TUI, of an untrusted source: the `n`
/// judge calls, one per skill, shown and agreed to y/N before the first;
/// `--yes` agrees, and without a terminal it is required.
pub(super) fn confirm_calls(n: usize, consent: Consent, out: Out) -> Result<()> {
    let s = if n == 1 { "" } else { "s" };
    let count = format!("vetting {n} skill{s} makes {n} judge call{s}");
    out.line(&count);
    consent.answerable().map_err(Error::Usage)?;
    if !consent.yes && !consent.ask(vec![count], &format!("make {n} judge call{s}?"), false)? {
        return Err("add declined; nothing was installed".into());
    }
    Ok(())
}

/// The skills under `root` to install, each by its path below it: `root`
/// itself (`None`), named `own`, when it holds a `SKILL.md`; else its one
/// skill ([`skill_dirs`]), the one `--skill` names by path or by a name no
/// other skill shares, every one with `--all`, or those picked in the TUI's
/// picker or when stdin and stdout are a terminal. Off both, several skills
/// and neither flag is a usage error listing them.
pub(super) fn skills_in(root: &Path, own: &str, args: &Add) -> Result<Vec<Option<String>>> {
    // Inert: the skill paths it lists are directories the source's author named.
    let usage = |e: String| Error::Usage(crate::render::inert(&format!("add {}: {e}", args.source)));
    if root.join("SKILL.md").is_file() {
        return match args.pick {
            Some(p) if p != own => Err(usage(format!("no skill named {p}; the only skill here is {own}"))),
            _ => Ok(vec![None]),
        };
    }
    let found = skill_dirs(root).map_err(usage)?;
    let list = found.join(", ");
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let paths: Vec<String> = match args.pick {
        Some(p) if found.iter().any(|f| f == p) => vec![p.to_string()],
        Some(p) => match found.iter().filter(|f| name(f) == p).collect::<Vec<_>>()[..] {
            [] => return Err(usage(format!("no skill named {p}; the skills here are: {list}"))),
            [one] => vec![one.clone()],
            ref several => {
                let at = several.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", ");
                return Err(usage(format!("several skills named {p}, at: {at}; pass --skill with one of those paths")));
            }
        },
        None if found.is_empty() => return Err(usage("no SKILL.md here or in any directory below".into())),
        None if args.all || found.len() == 1 => found,
        None if tty || args.asks.is_some() => {
            // One line each: a folded description keeps its words, not its breaks.
            let line = |d: String| d.split_whitespace().collect::<Vec<_>>().join(" ");
            let items = found.iter().map(|f| (f.clone(), description(&root.join(f)).map(line).unwrap_or_default()));
            match args.asks {
                Some(asks) => {
                    let (reply, picked) = mpsc::channel();
                    _ = asks.send(Question::Pick(items.collect(), reply));
                    picked.recv().unwrap_or_default()
                }
                None => crate::pick::pick(items.collect()).map_err(|e| format!("skill picker: {e}"))?,
            }
        }
        None => return Err(usage(format!("several skills here; pass --skill with one of: {list}, or --all"))),
    };
    if args.rename.is_some() && paths.len() > 1 {
        return Err(usage("--as names one skill; pick one with --skill".into()));
    }
    Ok(paths.into_iter().map(Some).collect())
}

/// The name of the skill at `path` below a source: its last segment.
pub(super) fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The directories below `root` holding a `SKILL.md`, at any depth, as
/// sorted `/`-joined paths relative to it. The search skips `.git`,
/// `node_modules` and every dot directory, follows no symlinked directory,
/// and does not descend into a skill.
fn skill_dirs(root: &Path) -> std::result::Result<Vec<String>, String> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) -> std::result::Result<(), String> {
        let entries = std::fs::read_dir(dir).map_err(|e| err(dir, e))?;
        for e in entries.flatten() {
            let child = e.file_name().to_string_lossy().into_owned();
            if child.starts_with('.') || child == "node_modules" {
                continue;
            }
            let (path, rel) = (e.path(), if rel.is_empty() { child } else { format!("{rel}/{child}") });
            if path.join("SKILL.md").is_file() {
                out.push(rel);
            } else if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, &rel, out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, "", &mut out)?;
    out.sort();
    Ok(out)
}
