//! What the store asks, and where: the [`Io`] it reports and asks through,
//! the y/N of [`Consent`], the judge-call count of [`confirm_calls`], and the
//! skill picker of [`skills_in`].

use std::io::{BufRead, IsTerminal};
use std::path::Path;

use super::Add;
use crate::audit::Pane;
use crate::inventory::description;
use crate::{Error, Result, outln};

/// Where an add or update reports and asks: the [`Terminal`], or the TUI's job.
pub trait Io {
    /// The pane that takes the judge's progress; stderr without one.
    fn pane(&self) -> Option<&Pane>;
    fn line(&self, line: &str);
    fn error(&self, line: &str);
    /// Whether the caller asked the job to stop.
    fn cancelled(&self) -> bool;
    /// Whether a y/N can be answered at all.
    fn can_ask(&self) -> bool;
    /// Whether any untrusted add, not only `--all`, has its judge calls
    /// agreed to first.
    fn confirms_calls(&self) -> bool;
    /// `lines`, already reported, then `prompt` ending in `[Y/n]` or `[y/N]`:
    /// Enter takes `default`.
    fn yes_no(&self, lines: Vec<String>, prompt: String, default: bool) -> Result<bool>;
    /// The skills picked from each one's path and description, or `None`
    /// when there is no picker to show.
    fn pick(&self, items: Vec<(String, String)>) -> Result<Option<Vec<String>>>;
}

/// The CLI's [`Io`]: stdout and stderr, never cancelled, y/N on stdin while
/// it is a terminal, and the picker while stdin and stdout are.
pub struct Terminal;

impl Io for Terminal {
    fn pane(&self) -> Option<&Pane> {
        None
    }

    fn line(&self, line: &str) {
        outln!("{line}");
    }

    fn error(&self, line: &str) {
        eprintln!("{line}");
    }

    fn cancelled(&self) -> bool {
        false
    }

    fn can_ask(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn confirms_calls(&self) -> bool {
        false
    }

    fn yes_no(&self, _: Vec<String>, prompt: String, default: bool) -> Result<bool> {
        eprint!("{prompt} ");
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).map_err(|e| format!("stdin: {e}"))?;
        Ok(match answer.trim() {
            "" => default,
            a => matches!(a, "y" | "Y" | "yes"),
        })
    }

    fn pick(&self, items: Vec<(String, String)>) -> Result<Option<Vec<String>>> {
        let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        Ok(tty.then(|| crate::pick::pick(items)).transpose().map_err(|e| format!("skill picker: {e}"))?)
    }
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
    pub(super) io: &'a dyn Io,
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
        if self.yes || self.io.can_ask() {
            return Ok(());
        }
        Err(format!(
            "{} refused: a vetted {} asks y/N; pass --yes when stdin is not a terminal",
            self.verb(),
            self.act()
        ))
    }

    /// `prompt` asked y/N after `lines`, Enter taking `default`.
    pub(super) fn ask(self, lines: Vec<String>, prompt: &str, default: bool) -> Result<bool> {
        self.io.yes_no(lines, format!("{prompt} {}", if default { "[Y/n]" } else { "[y/N]" }), default)
    }
}

/// `add --all`, or any add from the TUI, of an untrusted source: the `n`
/// judge calls, one per skill, shown and agreed to y/N before the first;
/// `--yes` agrees, and without a terminal it is required.
pub(super) fn confirm_calls(n: usize, consent: Consent) -> Result<()> {
    let calls = crate::render::count(n, "judge call");
    let count = format!("vetting {} makes {calls}", crate::render::count(n, "skill"));
    consent.io.line(&count);
    consent.answerable().map_err(Error::Usage)?;
    if !consent.yes && !consent.ask(vec![count], &format!("make {calls}?"), false)? {
        return Err("add declined; nothing was installed".into());
    }
    Ok(())
}

/// The skills under `root` to install, each by its path below it: `root`
/// itself (`None`), named `own`, when it holds a `SKILL.md`; else its one
/// skill ([`skill_dirs`]), the one `--skill` names by path or by a name no
/// other skill shares, every one with `--all`, or those picked in the
/// [`Io`]'s picker. Without one, several skills and neither flag is a usage
/// error listing them.
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
        None => {
            // One line each: a folded description keeps its words, not its breaks.
            let line = |d: String| d.split_whitespace().collect::<Vec<_>>().join(" ");
            let items = found.iter().map(|f| (f.clone(), description(&root.join(f)).map(line).unwrap_or_default()));
            args.io
                .pick(items.collect())?
                .ok_or_else(|| usage(format!("several skills here; pass --skill with one of: {list}, or --all")))?
        }
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
        let entries = std::fs::read_dir(dir).map_err(crate::at(dir))?;
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
