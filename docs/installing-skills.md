# Installing skills

This guide covers installing, updating and removing store skills with
`mazo add`, `mazo update` and `mazo retire`, and how mazo vets an untrusted
skill. Back to the [README](../README.md).

The store, the data directory and the `mazo.lock` format are in the
[configuration reference](configuration.md#the-data-directory-and-the-store).
Installing for one project with `--project` is in
[project skills](project-skills.md).

## The bundled skills

`mazo init` installs the skills bundled with mazo, `grade` and `retro`, as it
writes the config. It runs `mazo add
https://github.com/nicolasgarcia214/mazo/tree/v<version>/skills --all --trust`
in-process, `<version>` being the running mazo's, so `init` needs the network
unless you pass `--no-skills`, which skips the install. The trust covers that
one repository at that ref: `init` makes no judge call and adds nothing to
`trusted`. Each skill's `mazo.lock` entry records the ref.

- **No tag.** When the repository has no tag `v<version>`, `init` warns and
  installs the commit its default branch is at, recording that 40-hex commit
  as the ref.
- **Already installed.** A bundled skill your source or the store already
  holds is skipped, with a line saying so; `init` installs the other one.
- **Failed.** `init` keeps the config it wrote, prints the `mazo add` command
  that retries the install with the ref filled in, and exits 1.

If you set mazo up before this, or skipped the install, the TUI's top bar
names the missing ones, such as `retro not installed, press i`, until both are
installed; `i` opens the add dialog.

`mazo update` re-pins a bundled entry, one whose origin is mazo's repository
and whose subpath starts with `skills/`, to the running version's tag once
the repository has it, without vetting. It does so only for an entry pinned to
a commit or to a `v<major>.<minor>.<patch>` tag older than the running
version. A branch, a newer tag or any other tag stays, and `update` never
moves a bundled entry to a branch head.

## `mazo add`

`mazo add <source>` copies a skill into the store, records it in `mazo.lock`
and links it into every home. It installs nothing if the resulting sync would
fail. `--project` installs into the project at the current directory's git
root instead (see [project skills](project-skills.md)).

### Sources

`<source>` is one of:

- a local path: one that exists, or starts with `.`, `/` or `~`. `~` and `~/`
  expand to `$HOME`; `~user` is a usage error.
- a git URL: `https://…`, `ssh://…`, `git@host:owner/repo.git` or `file://…`.
  A path after the segment ending in `.git` is the subpath
  (`https://host/owner/repo.git/skills/x`).
- GitHub shorthand `owner/repo[/path/to/skill]`, cloned from
  `https://github.com/owner/repo.git`.
- a GitHub browser URL to a directory, `https://github.com/owner/repo/tree/<ref>/<path>`,
  or to its `SKILL.md`, `https://github.com/owner/repo/blob/<ref>/<path>/SKILL.md`.
  The skill is `<path>` at `<ref>`, which `mazo.lock` records as `ref`. Since a
  branch or tag may hold `/`, mazo runs `git ls-remote` on the repository before
  cloning and takes the longest branch or tag that the URL's segments start
  with; failing that, a full 40-hex commit id is fetched as that commit. A
  query or fragment the browser added (`?plain=1`, `#L1`) is dropped.

Any other git URL, or GitHub shorthand, containing `?`, `#` or `\` is a usage
error.

The recorded origin drops a trailing `/` and ends in `.git` (except a `file://`
URL), so `https://github.com/owner/repo` and `owner/repo` record the same one.

### Cloning

mazo shallow-clones a repository with the system `git` into the data
directory, without prompting, and removes the clone afterwards. ssh runs with
`BatchMode=yes` unless `GIT_SSH_COMMAND` is set. A failed clone exits 1 with
git's error and installs nothing.

### Picking skills

The source, at its subpath, is the skill when it holds a `SKILL.md`. Otherwise
its skills are the directories below it, at any depth, that hold one. The
search skips `.git`, `node_modules` and every directory whose name starts with
a dot, and does not descend into a skill. Each skill is named for its
directory and known by its path relative to the source, such as
`skills/b/foo`, which `mazo.lock` records as its subpath. With several:

- `--skill <name>` installs one and `--all` installs every one. `--skill` also
  takes a skill's path; a name two skills share is a usage error listing their
  paths.
- Without either, at a terminal (stdin and stdout), a picker lists each skill's
  path and `description`. ↑↓ or `j`/`k` move, Space selects, `a` selects all or
  none, Enter installs the selected, and Esc or `q` cancels. Selecting nothing,
  or cancelling, installs nothing and exits 0.
- Without either and off a terminal, mazo lists them and exits 2.
- `--all` cannot be combined with `--skill` or `--as`, and `--as` with several
  picked skills is a usage error.
- Of two picked skills sharing a name, the one whose path sorts first installs
  and the other is refused as taken; install it on its own with `--skill
  <path> --as <name>`.

mazo installs and vets each selected skill on its own. A refused skill leaves
the others to install. The exit code is the highest among the refusals (2 for
a usage error such as a taken name, 1 for a refusal), and 0 when none was
refused.

### Names

`mazo add` refuses (exit 2) a skill whose `SKILL.md` frontmatter is not a
mapping, has no nonempty `description` or one over 1024 characters, or does
not declare `name:` as the name it installs under (`--as` rewrites it). That
name must be at most 64 characters of lowercase ASCII letters and digits
joined by single hyphens.

mazo refuses a name already in `source` or the store (exit 2, writing
nothing). `--as <name>` installs the skill under another name and rewrites the
`name:` in its `SKILL.md`.

### Symlinks

mazo refuses a skill that contains any symlink, and a skill path that
resolves, through a symlink, outside the clone.

### Trust

mazo sends a source not on the `trusted` list to the `[judge]` before it copies
anything (see [vetting a skill](#vetting-a-skill)). Local paths are never on
the list. The [configuration reference](configuration.md#trusted) gives the
matching rules. `--trust` installs without vetting. The vetting section covers
[the refusal with no `[judge]`](#no-judge) and
[the `--all` prompt](#judge-calls-with---all).

## `mazo update`

`mazo update [names...] [--yes] [--force] [--project]` re-clones each
git-sourced store skill, or every one when no name is given, from its recorded
origin. It clones shallow and without prompting as `add` does, at the
recorded `ref` (the default branch when there is none), and reads the same
subpath.

- **Changed.** When the copy's content hash differs, mazo stages the new copy
  beside `store/<name>/` and swaps it in, so a failure leaves the old copy. It
  records the new `commit` and `content_hash`, keeps `installed_at`, and links
  the way `add` does.
- **Unchanged.** mazo reports the skill up to date and leaves its `mazo.lock`
  entry as it was.
- **Untrusted and changed.** When the skill's source is not on the `trusted`
  list and is not a [bundled skill](#the-bundled-skills), mazo re-vets it before replacing it
  (see [re-vetting an update](#re-vetting-an-update)). The entry's `trusted`
  value plays no part, so a skill added with `--trust`, or a source dropped
  from the list, is re-vetted. mazo does not vet a trusted one. With no
  `[judge]`, mazo refuses every untrusted skill before cloning it, changed
  or not. It prints one line
  per such skill, then the judge tables and issue link of
  [the add refusal](#no-judge) once, and exits 2.
- **Not updatable.** A local-path skill. A name that is not a store skill is a
  usage error.
- **Bundled and behind.** A [bundled skill](#the-bundled-skills) at a commit
  or an older version tag moves to the running version's tag once the
  repository has it. mazo records the new `ref` even when the content is the
  same, and does not vet it.

A failed clone, a refused re-vet, or an update the resulting sync would refuse
leaves that skill and its entry as they were. The rest still run, and the
command exits 1, or 2 when mazo refused a skill for want of a `[judge]`.
`--project` updates the project's skills instead (see
[project skills](project-skills.md)).

## Removing a store skill

`mazo retire <name>` removes a store skill; see
[`mazo retire`](managing-skills.md#mazo-retire).

## Vetting a skill

`mazo add` of a source not on the `trusted` list, without `--trust`, first
sends the skill to one call of the `[judge]`. The call has the same billing,
login check and default $1 cap (for a judge that reports its cost) as
[`audit`](audit.md).

The judge sees:

- every file of the skill;
- the name and description of every skill in `source` and the store, and,
  with `--project`, the project's store;
- a summary of your last 30 days of work, capped at 8 KiB as a whole.

mazo builds the summary from the ledger alone. It makes no judge call to build
it. The summary holds:

- the ten projects (`cwd`) used most, each with its event count;
- the twenty skills used most, each with its event count.

It holds none of your prompts: the verdict lands in the lockfile, and a skill
under vet could ask the judge to copy them there. With no history the summary
says so and the vet still runs.

The judge answers with:

- a security finding, or none;
- a `quality`: `good`, `weak` or `poor`;
- a `value`: `worth trying`, `low value`, `irrelevant` or `redundant`, naming
  the skill it overlaps;
- its reasons, what the skill is for, and up to three uses to try.

mazo retries an answer that breaks the schema once, then prints the verdict and
decides:

- A security finding refuses the install (exit 1) unless `--force`. With
  `--force` the install still asks as below.
- A file mazo withheld is a security finding of its own. mazo withholds a file
  that is not text, is over 64 KiB, or would take the total sent past 256 KiB;
  only files sent count towards that total. The refusal names each withheld
  file and why, before mazo calls the judge.
- `--yes` accepts without asking. Without a terminal on stdin and without
  `--yes`, mazo refuses the install (exit 2) before it calls the judge.

Otherwise mazo asks `install <name>? [Y/n]` or `install <name>? [y/N]`, and
Enter takes the default:

| Verdict | Prompt |
|---|---|
| `worth trying` and not `poor` | `[Y/n]` |
| anything else | `[y/N]` |

The verdict is one model's reading of text the skill's author wrote, and that
text can try to talk the judge into a clean verdict. mazo frames that text and
checks the answer, but it cannot keep the judge from being persuaded. Without
`--yes` you see the verdict and decide; with `--yes` on an untrusted source, a
skill the judge passes installs unread. Read the files of a source you do not
know before trusting either.

### Judge calls with `--all`

`--all` on an untrusted source makes one judge call per skill. It first prints
their number and asks `make <n> judge calls? [y/N]` before the first, which
`--yes` answers. Declining installs nothing and exits 1. Without a terminal it
needs `--yes`, and exits 2 before any call otherwise. A trusted `--all` asks
nothing.

### No judge

With no `[judge]`, mazo refuses an untrusted add (exit 2) unless `--trust`. The
refusal names the two judges any setup can configure, `command` and `http`,
and shows a pasteable `[judge]` table for each (see
[the `[judge]` table](configuration.md#judge)). It also links a prefilled
GitHub issue asking for a judge for your harness.

### After the verdict

A refused or declined install, or a judge that gave no valid answer, leaves no
store directory and no `mazo.lock` entry. An accepted one records the verdict
in its `mazo.lock` entry.

### Re-vetting an update

`mazo update` re-vets a store skill when its content changed and its source is
neither on the `trusted` list nor a [bundled skill](#the-bundled-skills); the
entry's `trusted` value plays no part. An unchanged skill, or a trusted one,
makes no judge call.

The one call sends the new `SKILL.md` and the output of `git diff --no-index`
from the installed copy to the new one, with binary files diffed as text. It
also sends the other skills and the work summary, as for `add`.

The same rules as `add` decide the update, with `update <name>?` as the prompt
(`[Y/n]` or `[y/N]` by the verdict, as in the table above) and `--yes` and `--force` on `update`:

- a security finding refuses it unless `--force`;
- a diff mazo did not send in full (not text, or over the caps) is a security
  finding of its own;
- without a terminal and without `--yes`, mazo refuses it before it calls the
  judge.

A refused update leaves the installed copy and its `mazo.lock` entry as they
were. The other skills still update, and the command exits 1. An accepted one
replaces `vetted_at`, `for`, `try` and the verdict in the entry.
