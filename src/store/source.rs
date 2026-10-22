//! Where a skill comes from: a [`Source`] parsed, a [`Remote`]'s host and
//! trust, and its shallow clone into a [`Scratch`] directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::{Error, Result};
use crate::{audit, config};

/// Where `mazo add` reads a skill from.
#[derive(Debug, PartialEq)]
pub(super) enum Source {
    Local(PathBuf),
    Git(Remote),
}

#[derive(Debug, PartialEq)]
pub(super) struct Remote {
    /// The clone URL, without the subpath.
    pub(super) url: String,
    /// The URL's path segments, `.git` dropped from the last: owner, repo, ….
    path: Vec<String>,
    /// The directory inside the repository; empty for its root.
    pub(super) subpath: PathBuf,
}

impl Source {
    /// A path that exists or starts with `.`, `/` or `~` is local. Otherwise
    /// `scheme://…` and `[user@]host:path` are git URLs, and `owner/repo[/sub]`
    /// is a GitHub repository. In a URL the subpath follows the first
    /// segment ending in `.git`. A git source holding `?`, `#` or `\` is
    /// refused: git could fetch from another host than the one trust checks.
    pub(super) fn parse(arg: &str) -> std::result::Result<Source, String> {
        if arg.starts_with('-') {
            return Err("a source must not start with -".into());
        }
        if arg.starts_with(['.', '/', '~']) || Path::new(arg).exists() {
            return Ok(Source::Local(arg.into()));
        }
        if arg.contains(['?', '#', '\\']) {
            return Err(format!("a git source must not contain ?, # or \\: {arg:?}"));
        }
        let bad = || format!("not a local path, a git URL or owner/repo[/path]: {arg:?}");
        let (base, segments): (String, Vec<&str>) = if let Some((scheme, rest)) = arg.split_once("://") {
            let (host, path) = rest.split_once('/').ok_or_else(bad)?;
            (format!("{scheme}://{host}/"), path.split('/').collect())
        } else if let Some((host, path)) = arg.split_once(':').filter(|(host, _)| !host.contains('/')) {
            (format!("{host}:"), path.split('/').collect())
        } else {
            let segments: Vec<&str> = arg.trim_end_matches('/').split('/').collect();
            let repo = segments.get(1).ok_or_else(bad)?;
            let repo = repo.strip_suffix(".git").unwrap_or(repo);
            let path = vec![segments[0].to_string(), repo.to_string()];
            let url = format!("https://github.com/{}/{repo}.git", segments[0]);
            return Remote::new(url, path, &segments[2..]).map(Source::Git).ok_or_else(bad);
        };
        let segments: Vec<&str> = segments.into_iter().filter(|s| !s.is_empty()).collect();
        let cut = segments.iter().position(|s| s.ends_with(".git")).map_or(segments.len(), |i| i + 1);
        let mut url = format!("{base}{}", segments[..cut].join("/"));
        // One origin per repository; a `file://` path names the directory as it is.
        if !url.ends_with(".git") && !url.starts_with("file://") {
            url.push_str(".git");
        }
        // Only the last segment of the repository's path can end in `.git`.
        let path = segments[..cut].iter().map(|s| s.strip_suffix(".git").unwrap_or(s).to_string()).collect();
        Remote::new(url, path, &segments[cut..]).map(Source::Git).ok_or_else(bad)
    }
}

impl Remote {
    /// `None` when the path names no repository or a segment is empty, `.` or `..`.
    fn new(url: String, path: Vec<String>, subpath: &[&str]) -> Option<Remote> {
        let ok = |s: &str| !s.is_empty() && s != "." && s != "..";
        let valid = path.len() >= 2 && path.iter().all(|s| ok(s)) && subpath.iter().all(|s| ok(s));
        valid.then(|| Remote { url, path, subpath: subpath.iter().collect() })
    }

    /// The URL's host, without user or port; none for a `file://` URL.
    fn host(&self) -> Option<&str> {
        let authority = match self.url.split_once("://") {
            Some((scheme, _)) if scheme.eq_ignore_ascii_case("file") => return None,
            Some((_, rest)) => rest.split('/').next()?,
            None => self.url.split(':').next()?,
        };
        let host = authority.rsplit('@').next()?;
        host.split(':').next()
    }

    /// An entry trusts the repository when it names the repository's host and
    /// then its owner (the first segment) or owner/repo (the first two),
    /// ignoring ASCII case. An entry whose first segment has no `.` names a
    /// GitHub owner or repository; otherwise that segment is the host.
    pub(super) fn trusted_by(&self, trusted: &[String]) -> bool {
        let Some(host) = self.host() else { return false };
        let repo = format!("{}/{}", self.path[0], self.path[1]);
        trusted.iter().any(|t| {
            let (on, name) = t.split_once('/').filter(|(h, _)| h.contains('.')).unwrap_or(("github.com", t));
            on.eq_ignore_ascii_case(host)
                && (name.eq_ignore_ascii_case(&self.path[0]) || name.eq_ignore_ascii_case(&repo))
        })
    }

    /// The name of the skill at the subpath: its last segment, else the repository's.
    pub(super) fn name(&self) -> String {
        self.subpath.file_name().map_or_else(|| self.path[self.path.len() - 1].clone(), |n| n.to_string_lossy().into())
    }

    /// Shallow-clones the repository into `into` and returns the commit.
    /// Neither git nor ssh may prompt, unless the user's `GIT_SSH_COMMAND` says so.
    /// The clone is killed once `cancel` turns true, or after [`CLONE_LIMIT`].
    pub(super) fn clone_into(&self, into: &Path, cancel: &dyn Fn() -> bool) -> Result<String> {
        let mut git = std::process::Command::new("git");
        git.args(["clone", "--depth", "1", "--quiet", "--"]).arg(&self.url).arg(into).env("GIT_TERMINAL_PROMPT", "0");
        if std::env::var_os("GIT_SSH_COMMAND").is_none() {
            git.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        git.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let spawned = git.spawn().and_then(|child| audit::wait(child, None, CLONE_LIMIT, cancel));
        let Some(out) = spawned.map_err(|e| format!("git: {e}"))? else {
            if cancel() {
                return Err(audit::CANCELLED.into());
            }
            let minutes = CLONE_LIMIT.as_secs() / 60;
            return Err(format!("git clone {}: not done within {minutes} minutes; it was killed", self.url).into());
        };
        if !out.status.success() {
            return Err(format!("git clone {}: {}", self.url, String::from_utf8_lossy(&out.stderr).trim()).into());
        }
        let head = crate::inventory::git(into, &["rev-parse", "HEAD"]);
        head.map(|h| h.trim().to_string()).ok_or_else(|| format!("{}: no HEAD commit", self.url).into())
    }
}

/// How long a clone may run before it is killed.
const CLONE_LIMIT: Duration = Duration::from_secs(10 * 60);

/// A directory removed when dropped.
pub(super) struct Scratch(pub(super) PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A local source with `~` and `~/…` expanded against `home`, as config
/// paths are; `~user` is refused, since nothing expands it.
pub(super) fn local(path: &Path, home: &str) -> Result<PathBuf> {
    let raw = path.to_string_lossy();
    let expanded = config::expand_tilde(&raw, home);
    if expanded.starts_with('~') {
        return Err(Error::Usage(format!("add {raw}: only ~ and ~/ expand to $HOME; ~user is not supported")));
    }
    Ok(expanded.into())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Remote, Source, local};

    fn remote(arg: &str) -> Remote {
        match Source::parse(arg) {
            Ok(Source::Git(r)) => r,
            other => panic!("{arg}: {other:?}"),
        }
    }

    #[test]
    fn shorthand_resolves_to_github_with_a_subpath() {
        let r = remote("owner/repo");
        assert_eq!(
            (r.url.as_str(), r.path.clone(), r.subpath.to_str()),
            ("https://github.com/owner/repo.git", vec!["owner".into(), "repo".into()], Some(""))
        );
        assert_eq!(r.name(), "repo");
        let r = remote("owner/repo.git/a/b/");
        assert_eq!((r.url.as_str(), r.subpath.to_str()), ("https://github.com/owner/repo.git", Some("a/b")));
        assert_eq!(r.name(), "b");
        for same in
            ["https://github.com/owner/repo", "https://github.com/owner/repo/", "https://github.com/owner/repo.git"]
        {
            assert_eq!(remote(same).url, remote("owner/repo").url, "{same}");
        }
    }

    #[test]
    fn git_urls_split_the_subpath_after_the_git_segment() {
        for (arg, url, path, sub) in [
            ("https://github.com/o/r", "https://github.com/o/r.git", "o/r", ""),
            ("https://github.com/o/r/", "https://github.com/o/r.git", "o/r", ""),
            ("ssh://h/o/r", "ssh://h/o/r.git", "o/r", ""),
            ("git@h:o/r/", "git@h:o/r.git", "o/r", ""),
            ("file:///srv/git/r", "file:///srv/git/r", "srv/git/r", ""),
            ("https://gitlab.com/o/r.git/skills/x", "https://gitlab.com/o/r.git", "o/r", "skills/x"),
            ("ssh://git@host:22/o/r.git", "ssh://git@host:22/o/r.git", "o/r", ""),
            ("git@host.org:o/r.git/a", "git@host.org:o/r.git", "o/r", "a"),
            ("file:///srv/git/r.git/a/b", "file:///srv/git/r.git", "srv/git/r", "a/b"),
        ] {
            let r = remote(arg);
            assert_eq!((r.url.as_str(), r.path.join("/"), r.subpath.to_str()), (url, path.into(), Some(sub)), "{arg}");
        }
    }

    #[test]
    fn a_tilde_source_resolves_under_home_and_tilde_user_is_refused() {
        assert_eq!(local(Path::new("~/x"), "/h").unwrap(), Path::new("/h/x"));
        assert_eq!(local(Path::new("~"), "/h").unwrap(), Path::new("/h"));
        assert_eq!(local(Path::new("./x"), "/h").unwrap(), Path::new("./x"));
        assert!(local(Path::new("~bob/x"), "/h").unwrap_err().to_string().contains("~user is not supported"));
    }

    #[test]
    fn local_paths_stay_local_and_bad_sources_are_refused() {
        for arg in ["./x", "/x/y", "~/x", ".", "src"] {
            assert_eq!(Source::parse(arg), Ok(Source::Local(arg.into())), "{arg}");
        }
        for arg in [
            "-uhttps://x/o/r.git",
            "--upload-pack=x",
            "owner",
            "owner//repo",
            "o/r/../x",
            "https://host",
            "git@host:r.git",
            // Each would show github.com as its host while git fetches from evil.example.
            "https://evil.example#@github.com/anthropics/x",
            "https://evil.example?@github.com/anthropics/x",
            "https://evil.example\\@github.com/anthropics/x",
            "git@evil.example#@github.com:anthropics/x.git",
        ] {
            assert!(Source::parse(arg).is_err(), "{arg}");
        }
    }

    #[test]
    fn trust_matches_the_owner_or_owner_and_repo_on_its_host() {
        let list = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for arg in [
            "owner/repo/a",
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "ssh://git@GitHub.com:22/owner/repo",
        ] {
            let r = remote(arg);
            assert!(r.trusted_by(&list(&["owner"])), "{arg}");
            assert!(r.trusted_by(&list(&["x", "owner/repo"])), "{arg}");
            assert!(
                r.trusted_by(&list(&["github.com/owner"])) && r.trusted_by(&list(&["GitHub.com/owner/repo"])),
                "{arg}"
            );
            let others = ["repo", "owner/other", "other/repo", "github.com", "gitlab.com/owner"];
            assert!(!r.trusted_by(&list(&others)), "{arg}");
            assert!(!r.trusted_by(&[]), "{arg}");
            assert!(r.trusted_by(&list(&["OWNER"])) && r.trusted_by(&list(&["Owner/Repo"])), "{arg}");
        }
        for arg in ["https://evil.example/owner/repo", "git@codeberg.org:owner/repo.git", "file:///x/owner/repo"] {
            assert!(!remote(arg).trusted_by(&list(&["owner", "owner/repo"])), "{arg}");
        }
        let gitlab = remote("https://gitlab.com/acme/x.git");
        assert!(gitlab.trusted_by(&list(&["gitlab.com/acme"])) && gitlab.trusted_by(&list(&["GitLab.com/ACME/X"])));
        assert!(!remote("acme/x").trusted_by(&list(&["gitlab.com/acme"])));
        let own = list(&["git.example.org/acme/skills"]);
        assert!(remote("ssh://git@git.example.org/acme/skills.git").trusted_by(&own));
        assert!(!remote("ssh://git@git.example.org/acme/other.git").trusted_by(&own));
        assert!(!remote("acme/skills").trusted_by(&own));
    }
}
