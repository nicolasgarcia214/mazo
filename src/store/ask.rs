use std::io::{BufRead, IsTerminal};
use std::path::Path;

use super::Add;
use crate::audit::Pane;
use crate::inventory::description;
use crate::{Error, Result, outln};

pub trait Io {
    fn pane(&self) -> Option<&Pane>;
    fn line(&self, line: &str);
    fn error(&self, line: &str);
    fn cancelled(&self) -> bool;
    fn can_ask(&self) -> bool;
    fn confirms_calls(&self) -> bool;
    fn yes_no(&self, lines: Vec<String>, prompt: String, default: bool) -> Result<bool>;
    fn pick(&self, items: Vec<(String, String)>) -> Result<Option<Vec<String>>>;
}

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

#[derive(Clone, Copy)]
pub(super) struct Consent<'a> {
    pub(super) update: bool,
    pub(super) yes: bool,
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

    pub(super) fn ask(self, lines: Vec<String>, prompt: &str, default: bool) -> Result<bool> {
        self.io.yes_no(lines, format!("{prompt} {}", if default { "[Y/n]" } else { "[y/N]" }), default)
    }
}

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

pub(super) fn skills_in(root: &Path, own: &str, args: &Add) -> Result<Vec<Option<String>>> {
    let usage = |e: String| Error::Usage(crate::render::inert_line(&format!("add {}: {e}", args.source)));
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

pub(super) fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

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
