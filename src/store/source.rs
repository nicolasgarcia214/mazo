//! Where a skill comes from: a [`Source`] parsed, a [`Remote`]'s host,
//! trust and ref, and its shallow clone into a [`Scratch`] directory.

use std::ffi::OsStr;
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
    /// The branch, tag or full commit id to clone; the default branch when none.
    pub(super) r#ref: Option<String>,
    /// Whether `ref` and `subpath` are a GitHub browser URL's first guess at
    /// where its ref ends, which [`Remote::resolve`] settles.
    browsed: bool,
}

impl Source {
    /// A path that exists or starts with `.`, `/` or `~` is local. Otherwise
    /// `scheme://…` and `[user@]host:path` are git URLs, and `owner/repo[/sub]`
    /// is a GitHub repository. In a URL the subpath follows the first
    /// segment ending in `.git`, except in a GitHub browser URL
    /// ([`github_browser_url`]). Any other git source holding `?`, `#` or `\`
    /// is refused: git could fetch from another host than the one trust checks.
    pub(super) fn parse(arg: &str) -> std::result::Result<Source, String> {
        if arg.starts_with('-') {
            return Err("a source must not start with -".into());
        }
        if arg.starts_with(['.', '/', '~']) || Path::new(arg).exists() {
            return Ok(Source::Local(arg.into()));
        }
        if let Some(browsed) = github_browser_url(arg) {
            return browsed;
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

/// `url`, a git remote URL naming a repository on github.com over https,
/// ssh or the `git@github.com:` form, as `https://github.com/<owner>/<repo>.git`;
/// `None` for a local path, `owner/repo` shorthand or another host.
pub fn github_repo(url: &str) -> Option<String> {
    // Shorthand has no `:`, and parses as GitHub whatever wrote it.
    let (true, Ok(Source::Git(remote))) = (url.contains(':'), Source::parse(url)) else { return None };
    let [owner, repo] = remote.path.as_slice() else { return None };
    let bare = remote.subpath.as_os_str().is_empty() && remote.r#ref.is_none();
    (bare && remote.host()?.eq_ignore_ascii_case("github.com"))
        .then(|| format!("https://github.com/{owner}/{repo}.git"))
}

/// A GitHub browser URL, `https://github.com/owner/repo/tree/<ref>/<path>`
/// or `…/blob/<ref>/<path>/SKILL.md`, naming `<path>` at `<ref>`, its query
/// or fragment (`?plain=1`, `#L1`) dropped; `None` for any other source. A
/// ref may hold `/`, so the split of `<ref>/<path>` is a guess until
/// [`Remote::resolve`].
fn github_browser_url(arg: &str) -> Option<std::result::Result<Source, String>> {
    const GITHUB: &str = "https://github.com/";
    let url = arg.split(['?', '#']).next()?;
    let rest = url.get(GITHUB.len()..).filter(|_| url[..GITHUB.len()].eq_ignore_ascii_case(GITHUB))?;
    if rest.contains('\\') {
        return None;
    }
    let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    let [owner, repo, kind @ ("tree" | "blob"), tail @ ..] = segments.as_slice() else { return None };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let tail = match (*kind, tail) {
        ("tree", _) => tail,
        (_, [dir @ .., "SKILL.md"]) => dir,
        _ => return Some(Err(format!("a GitHub blob URL must name a SKILL.md: {arg:?}"))),
    };
    let url = format!("https://github.com/{owner}/{repo}.git");
    let remote = Remote::browsed(url, vec![owner.to_string(), repo.to_string()], tail);
    Some(remote.map(Source::Git).ok_or_else(|| format!("not a GitHub tree or blob URL with a ref: {arg:?}")))
}

impl Remote {
    /// `None` when the path names no repository or a segment is empty, `.` or `..`.
    fn new(url: String, path: Vec<String>, subpath: &[&str]) -> Option<Remote> {
        let ok = |s: &str| !s.is_empty() && s != "." && s != "..";
        let valid = path.len() >= 2 && path.iter().all(|s| ok(s)) && subpath.iter().all(|s| ok(s));
        valid.then(|| Remote { url, path, subpath: subpath.iter().collect(), r#ref: None, browsed: false })
    }

    /// A browser URL's repository at `tail`, `<ref>/<path>`, its first
    /// segment taken for the ref until [`Remote::resolve`]; `None` as
    /// [`Remote::new`], or when `tail` is empty.
    fn browsed(url: String, path: Vec<String>, tail: &[&str]) -> Option<Remote> {
        let (r#ref, subpath) = tail.split_first()?;
        let remote = Remote::new(url, path, tail)?;
        Some(Remote { subpath: subpath.iter().collect(), r#ref: Some(r#ref.to_string()), browsed: true, ..remote })
    }

    /// Settles a browser URL's ref: the longest branch or tag that
    /// `git ls-remote` lists and that whole segments of `<ref>/<path>` start
    /// with, else a full commit id in its first segment. Parsing reads no
    /// network, so this runs before the clone.
    pub(super) fn resolve(&mut self, cancel: &dyn Fn() -> bool) -> Result<()> {
        let (true, Some(guess)) = (self.browsed, &self.r#ref) else { return Ok(()) };
        let tail = Path::new(guess).join(&self.subpath);
        let listed = self.git(None, &[&"ls-remote", &"--refs", &"--heads", &"--tags", &"--", &self.url], cancel)?;
        let listed = String::from_utf8_lossy(&listed);
        let names = listed.lines().filter_map(|line| {
            let name = line.split_once('\t')?.1;
            name.strip_prefix("refs/heads/").or_else(|| name.strip_prefix("refs/tags/"))
        });
        let found = names.filter(|name| tail.starts_with(name)).max_by_key(|name| Path::new(name).components().count());
        let r#ref = match found {
            Some(name) => name.to_string(),
            None if is_commit(guess) => guess.clone(),
            None => return Err(format!("{}: no branch or tag starts {}", self.url, tail.display()).into()),
        };
        self.subpath = tail.strip_prefix(&r#ref).expect("the ref starts the tail").to_path_buf();
        (self.r#ref, self.browsed) = (Some(r#ref), false);
        Ok(())
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

    /// Shallow-clones the repository at its ref into `into` and returns the commit.
    pub(super) fn clone_into(&self, into: &Path, cancel: &dyn Fn() -> bool) -> Result<String> {
        if let Some(sha) = self.r#ref.as_ref().filter(|r| is_commit(r)) {
            // `clone --branch` takes no commit, and `--revision` needs git
            // 2.49, so a commit is fetched alone into an empty repository.
            self.git(None, &[&"init", &"--quiet", &"--", &into], cancel)?;
            self.git(Some(into), &[&"fetch", &"--depth", &"1", &"--quiet", &"--", &self.url, sha], cancel)?;
            self.git(Some(into), &[&"checkout", &"--quiet", &"--detach", &"FETCH_HEAD"], cancel)?;
        } else {
            let mut clone: Vec<&dyn AsRef<OsStr>> = vec![&"clone", &"--depth", &"1", &"--quiet"];
            if let Some(branch) = &self.r#ref {
                clone.extend([&"--branch" as &dyn AsRef<OsStr>, branch]);
            }
            clone.extend([&"--" as &dyn AsRef<OsStr>, &self.url, &into]);
            self.git(None, &clone, cancel)?;
        }
        let head = crate::inventory::git(into, &["rev-parse", "HEAD"]);
        head.map(|h| h.trim().to_string()).ok_or_else(|| format!("{}: no HEAD commit", self.url).into())
    }

    /// The stdout of `git <args…>`, run in `dir` when there is one. Neither
    /// git nor ssh may prompt, unless the user's `GIT_SSH_COMMAND` says so.
    /// It is killed once `cancel` turns true, or after [`CLONE_LIMIT`].
    fn git(&self, dir: Option<&Path>, args: &[&dyn AsRef<OsStr>], cancel: &dyn Fn() -> bool) -> Result<Vec<u8>> {
        let verb = args[0].as_ref().to_string_lossy();
        let mut git = std::process::Command::new("git");
        git.args(args).env("GIT_TERMINAL_PROMPT", "0");
        if let Some(dir) = dir {
            git.current_dir(dir);
        }
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
            return Err(format!("git {verb} {}: not done within {minutes} minutes; it was killed", self.url).into());
        };
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("git {verb} {}: {}", self.url, stderr.trim()).into());
        }
        Ok(out.stdout)
    }
}

/// Whether `r#ref` is a full commit id: 40 hex digits.
fn is_commit(r#ref: &str) -> bool {
    r#ref.len() == 40 && r#ref.bytes().all(|b| b.is_ascii_hexdigit())
}

/// How long a clone or ls-remote may run before it is killed.
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

    use super::{Remote, Source, github_repo, local};

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
    fn github_remotes_normalise_and_others_do_not() {
        let repo = Some("https://github.com/o/r.git".to_string());
        for url in [
            "https://github.com/o/r.git",
            "https://github.com/o/r",
            "git@github.com:o/r.git",
            "ssh://git@GitHub.com/o/r",
        ] {
            assert_eq!(github_repo(url), repo, "{url}");
        }
        for url in ["/x", "o/r", "git@gitlab.com:o/r.git", "https://github.com/o/r.git/sub", "file:///o/r.git"] {
            assert_eq!(github_repo(url), None, "{url}");
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
    fn github_tree_and_blob_urls_name_a_directory_at_a_ref() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        for (arg, r#ref, sub) in [
            (
                "https://github.com/mattpocock/skills/tree/main/skills/engineering/research",
                "main",
                "skills/engineering/research",
            ),
            (
                "https://GitHub.com/mattpocock/skills/blob/main/skills/engineering/research/SKILL.md",
                "main",
                "skills/engineering/research",
            ),
            ("https://github.com/mattpocock/skills/tree/main", "main", ""),
            ("https://github.com/mattpocock/skills/blob/main/SKILL.md", "main", ""),
            // Until resolved, a ref holding `/` is cut at its first segment.
            ("https://github.com/mattpocock/skills/tree/release/v2/skills/x/", "release", "v2/skills/x"),
            (&format!("https://github.com/mattpocock/skills/tree/{sha}/skills/x"), sha, "skills/x"),
            // A browser's query or fragment, and a `.git` on the repository, are dropped.
            ("https://github.com/mattpocock/skills/blob/main/skills/x/SKILL.md#L1", "main", "skills/x"),
            ("https://github.com/mattpocock/skills/blob/main/skills/x/SKILL.md?plain=1#L2", "main", "skills/x"),
            ("https://github.com/mattpocock/skills/tree/main/skills/x?tab=readme", "main", "skills/x"),
            ("https://github.com/mattpocock/skills.git/tree/main/skills/x", "main", "skills/x"),
        ] {
            let r = remote(arg);
            assert_eq!(
                (r.url.as_str(), r.path.join("/"), r.subpath.to_str(), r.r#ref.as_deref(), r.browsed),
                ("https://github.com/mattpocock/skills.git", "mattpocock/skills".into(), Some(sub), Some(r#ref), true),
                "{arg}"
            );
        }
        assert_eq!(remote("https://github.com/o/r/tree/main/skills/x").name(), "x");
        // Elsewhere, and without a ref, `tree` is a directory like any other.
        let r = remote("https://gitlab.com/o/r.git/tree/main/x");
        assert_eq!((r.subpath.to_str(), r.r#ref, r.browsed), (Some("tree/main/x"), None, false));
        for arg in [
            "https://github.com/o/r/blob/main/skills/x/README.md",
            "https://github.com/o/r/blob/main",
            "https://github.com/o/r/tree",
            "https://github.com/o/r/tree/../x",
            "https://github.com/o/r/tree/main/a/../b",
            "https://github.com/o/r/tree/main/x\\y",
            // A query or fragment is dropped only from a tree or blob URL on github.com.
            "https://github.com/o/r#x",
            "https://github.com.evil.example/o/r/tree/main#x",
            "https://evil.example#@github.com/o/r/tree/main/x",
        ] {
            assert!(Source::parse(arg).is_err(), "{arg}");
        }
    }

    #[test]
    fn resolve_takes_the_longest_branch_or_tag_else_a_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(tmp.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "a"]);
        git(&["branch", "release/v2"]);
        git(&["tag", "release"]);
        let sha = git(&["rev-parse", "HEAD"]);
        let url = format!("file://{}", tmp.path().display());
        let resolved = |tail: &str| {
            let tail: Vec<&str> = tail.split('/').collect();
            let mut r = Remote::browsed(url.clone(), vec!["o".into(), "r".into()], &tail).unwrap();
            r.resolve(&|| false).map(|()| (r.r#ref.unwrap(), r.subpath.to_string_lossy().into_owned(), r.browsed))
        };
        for (tail, r#ref, sub) in [
            ("release/v2/skills/x", "release/v2", "skills/x"),
            ("release/v2", "release/v2", ""),
            ("release/v3/x", "release", "v3/x"),
            ("main/skills/x", "main", "skills/x"),
            (&format!("{sha}/skills/x"), &sha, "skills/x"),
        ] {
            assert_eq!(resolved(tail).unwrap(), (r#ref.into(), sub.into(), false), "{tail}");
        }
        for tail in ["mainline/x", "rel/x"] {
            let e = resolved(tail).unwrap_err().to_string();
            assert!(e.contains("no branch or tag starts"), "{tail}: {e}");
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
