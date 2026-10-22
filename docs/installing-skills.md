# Installing skills

This guide covers installing, updating and removing store skills with
`mazo add`, `mazo update` and `mazo retire`, and how mazo vets an untrusted
skill. Back to the [README](../README.md).

The store, the data directory and the `mazo.lock` format are in the
[configuration reference](configuration.md#the-data-directory-and-the-store).
Installing for one project with `--project` is in
[project skills](project-skills.md).

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

A git source, URL or GitHub shorthand, containing `?`, `#` or `\` is a usage
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
its child directories that hold one are its skills. With several:

- `--skill <name>` installs one and `--all` installs every one.
- Without either, at a terminal (stdin and stdout), a picker lists each skill's
  name and `description`. ↑↓ or `j`/`k` move, Space selects, `a` selects all or
  none, Enter installs the selected, and Esc or `q` cancels. Selecting nothing,
  or cancelling, installs nothing and exits 0.
- Without either and off a terminal, mazo lists them and exits 2.
- `--all` cannot be combined with `--skill` or `--as`, and `--as` with several
  picked skills is a usage error.

mazo installs and vets each selected skill on its own. A refused skill leaves
the others to install. The exit code is the highest among the refusals (2 for
a usage error such as a taken name, 1 for a refusal), and 0 when none was
refused.

### Names

mazo refuses a name already in `source` or the store (exit 2, writing
nothing). `--as <name>` installs the skill under another name and rewrites the
`name:` in its `SKILL.md`.

### Symlinks

mazo refuses a skill that contains a symlink, or one that a symlink leads
outside the clone.

### Trust

mazo sends a source not on the `trusted` list to the `[judge]` before it copies
anything (see [vetting a skill](#vetting-a-skill)). Local paths are never on
the list. The [configuration reference](configuration.md#trusted) gives the
matching rules.

With no `[judge]`, mazo refuses the add (exit 2) unless `--trust`, which
installs without vetting. The refusal shows a pasteable `[judge]` table for a
`command` and an `http` judge, and a link to a prefilled GitHub issue asking
for a judge for your harness.

`--all` on an untrusted source first prints the number of judge calls, one per
skill, and asks `make <n> judge calls? [y/N]` before the first. Declining
installs nothing and exits 1. Without a terminal it needs `--yes`, and exits 2
before any call otherwise. A trusted `--all` asks nothing.

## `mazo update`

`mazo update [names...] [--yes] [--force] [--project]` re-clones each
git-sourced store skill, or every one when no name is given, from its recorded
origin. It clones shallow and without prompting as `add` does, and reads the
same subpath.

- **Changed.** When the copy's content hash differs, mazo stages the new copy
  beside `store/<name>/` and swaps it in, so a failure leaves the old copy. It
  records the new `commit` and `content_hash`, keeps `installed_at`, and links
  the way `add` does.
- **Unchanged.** mazo reports the skill up to date and leaves its `mazo.lock`
  entry as it was.
- **Untrusted and changed.** When the skill has `trusted = false` and its source
  is not on the `trusted` list either, mazo re-vets it before replacing it (see
  [re-vetting an update](#re-vetting-an-update)). mazo does not vet a trusted
  one. With no `[judge]`, mazo refuses the untrusted skill. It prints one line
  per such skill, then once the judge tables and issue link that `add` shows,
  and exits 2.
- **Not updatable.** A local-path skill. A name that is not a store skill is a
  usage error.

A failed clone, a refused re-vet, or an update the resulting sync would refuse
leaves that skill and its entry as they were. The rest still run, and the
command exits 1, or 2 when mazo refused a skill for want of a `[judge]`.
`--project` updates the project's skills instead (see
[project skills](project-skills.md)).

## `mazo retire` on a store skill

`mazo retire <name>` on a store skill removes it: mazo deletes its links, its
`mazo.lock` entry and its store directory. The blockers, `--force` and the data
lock are the same as for a skill of `source`; see
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

mazo builds the summary from the ledger and the transcripts it points into. It
makes no judge call to build it. The summary holds:

- the ten projects (`cwd`) used most, each with its event count;
- the twenty skills used most, each with its event count;
- up to ten of your latest prompts, one per transcript, from at most twenty
  transcripts.

With no history the summary says so and the vet still runs. The prompts reach
the configured judge as you wrote them, with whitespace collapsed and cut to
200 characters. mazo does not redact them.

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
- A file mazo did not send in full is a security finding of its own. That is a
  file that is not text, is over 64 KiB, or arrives after the first 256 KiB of
  the skill's files. The refusal names each such file and why, before mazo
  calls the judge.
- `--yes` accepts without asking. Without a terminal on stdin and without
  `--yes`, mazo refuses the install (exit 2) before it calls the judge.
- `--all` makes one judge call per skill. It prints their number and asks
  `make <n> judge calls? [y/N]` before the first, which `--yes` answers.

Otherwise mazo asks `install <name>? [Y/n]` or `install <name>? [y/N]`, and
Enter takes the default:

| Verdict | Prompt |
|---|---|
| `worth trying` and not `poor` | `[Y/n]` |
| anything else | `[y/N]` |

With no `[judge]`, mazo refuses the add (exit 2). The refusal names the two
judges any setup can configure, `command` and `http` (see
[the `[judge]` table](configuration.md#judge)), and links a prefilled GitHub
issue asking for a judge for your harness.

A refused or declined install, or a judge that gave no valid answer, leaves no
store directory and no `mazo.lock` entry. An accepted one records the verdict
in its `mazo.lock` entry.

### Re-vetting an update

`mazo update` re-vets a store skill when its content changed, its `mazo.lock`
entry has `trusted = false`, and its source is not on the `trusted` list. An
unchanged skill, or a trusted one, makes no judge call.

The one call sends the new `SKILL.md` and the output of `git diff --no-index`
from the installed copy to the new one, with binary files diffed as text. It
also sends the other skills and the work summary, as for `add`.

The same rules as `add` decide the update, with `update <name>? [Y/n]` as the
prompt and `--yes` and `--force` on `update`:

- a security finding refuses it unless `--force`;
- a diff mazo did not send in full (not text, or over the caps) is a security
  finding of its own;
- without a terminal and without `--yes`, mazo refuses it before it calls the
  judge.

A refused update leaves the installed copy and its `mazo.lock` entry as they
were. The other skills still update, and the command exits 1. An accepted one
replaces `vetted_at`, `for`, `try` and the verdict in the entry.
