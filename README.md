# mazo

Count how your coding agents use your skills, and link every agent home to one
skills directory.

mazo reads the session logs that Claude Code, Codex and Pi already write and
stores each skill load in its own ledger. It links your skills, and skills you
install from git, into every agent home, replacing `skills/install.sh`. Before
it installs a skill from a source you have not trusted, it has an agent of your
choice vet it.

## What it does

- Counts every skill load per session, channel and agent, with a 30-day
  sparkline and the last use.
- Lists skills with no strong load in a window, and retires them.
- Links your skills directory and the skills you install into each configured
  agent home, such as `~/.claude`, `~/.codex` and `~/.pi/agent`, and reports
  drift.
- Installs skills from a git URL, a GitHub `owner/repo` or a local path into a
  store pinned by `mazo.lock`, for you or for one project.
- Sends an untrusted skill to a judge agent before installing it, and again
  when an update changes it.
- Audits how a skill performs in your real sessions and writes a report with
  ranked improvements.
- Shows all of this in a terminal UI and in one self-contained HTML page.

## Install

mazo runs on Linux and macOS (x86_64 and aarch64) and needs at least one of
Claude Code, Codex or Pi. A release ships a prebuilt binary through npm,
Homebrew and a shell installer:

```sh
npx mazo                          # or: npm install -g mazo
brew install nicolasgarcia214/tap/mazo
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/nicolasgarcia214/mazo/releases/latest/download/mazo-installer.sh | sh
```

These download from the repository's GitHub releases, so they work only once a
release is published and readable.

### Build from source

Until then, or to build from source with a Rust toolchain, let Cargo fetch the
repository with your git credentials by setting
`CARGO_NET_GIT_FETCH_WITH_CLI=true`:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true \
  cargo install --locked --git https://github.com/nicolasgarcia214/mazo
```

## Quick start

For your own skills you need a skills directory: one subdirectory per skill,
each holding a `SKILL.md`. If you only install other people's skills with
`mazo add`, you need none.

```sh
mazo init                         # write the config; add --source <skills dir> for your own skills
mazo sync                         # link every skill into every agent home
mazo usage                        # see which skills your agents load
mazo unused                       # list skills with no strong load in the window
mazo add owner/repo               # install a skill from GitHub, vetted unless trusted
mazo                              # open the terminal UI
```

## Commands

| Command | What it does |
|---|---|
| [`mazo init`](#mazo-init) | Write the config. |
| [`mazo scan`](#mazo-scan) | Read new log lines into the ledger. |
| [`mazo usage`](#mazo-usage) | One row per skill with its loads, last use and quality. |
| [`mazo unused`](#mazo-unused) | Skills with no strong load in the window. |
| [`mazo show <skill>`](#mazo-show) | The skill card. |
| [`mazo report`](#mazo-report) | One self-contained HTML page of usage and cards. |
| [`mazo status`](#mazo-status) | Drift and policy for every home. Writes nothing. |
| [`mazo sync`](#mazo-sync) | Link every skill into every home. |
| [`mazo new <name>`](#mazo-new) | Create a skill in `source`. |
| [`mazo add <source>`](#mazo-add) | Install a skill into the store. |
| [`mazo update [names...]`](#mazo-update) | Re-fetch store skills from their origins. |
| [`mazo retire <name>`](#mazo-retire) | Archive a skill of `source`, or remove a store skill. |
| [`mazo prune [<name>]`](#mazo-prune) | Delete archived skills from `.archive/`. |
| [`mazo audit <skill>`](#mazo-audit) | Have an agent judge how the skill performs in real sessions. |
| [`mazo`](#the-tui) | Open the TUI at a terminal; print the help off one. |

`usage`, `unused`, `show`, `report`, `audit` and `retire` scan first, so their
numbers are current. Every report prints how far back each home's logs go:
"unused since 2026-09-10" is not "never used".

On a terminal, `usage`, `unused` and `status` colour their status tokens:
`unused` and `ERROR:` in red; `held`, `new`, the `named by` note, `warning:`
and a stale grade's `+N` or `+wip` in yellow. Piped output, or any output with
`NO_COLOR` set to a non-empty value, has no colour.

### `mazo init`

`mazo init [--source <dir>]` writes the config, and refuses to overwrite an
existing one. It writes:

- a home for each of `~/.claude`, `~/.codex` and `~/.pi/agent` that exists,
- every policy rule off,
- `source`, when `--source` names your skills directory,
- a detected `[judge]`.

To detect the judge, `init` tries claude, codex and pi in that order and writes
the first one found as `[judge] agent = "..."`. Found means the binary is on
`PATH` and, for claude and codex, that it passes the subscription login check
`mazo audit` makes. For claude, `claude auth status` must report a claude.ai or
OAuth-token login. For codex, Codex's `auth.json` must hold a ChatGPT login. Pi
has no login check.
Detection reads local state only and makes no model call.

When it finds none and stdin is a terminal, `init` asks for one of claude,
codex, pi, `command` or `http`. It writes the last two as a commented-out
`[judge]` table of placeholders for you to fill in and uncomment, since the
table is invalid until then. Without a terminal it writes no `[judge]` and
prints how to add one.

### `mazo scan`

Reads new log lines into the ledger. `--report` adds per-channel counts,
unparsed commands and the parser canary. `--reparse` re-derives every event
from the logs on disk. See [how accurate the counts are](#how-accurate-the-counts-are)
for what a scan can miss.

### `mazo usage`

One row per skill: sessions, loads per channel, agents, last use, a 30-day
sparkline, the latest grade, and quality. Quality is the first of these that
exists, cut to 40 characters with `…`:

1. the latest audit's verdict, labelled `audit:`,
2. the verdict `mazo add` recorded when it vetted the skill, labelled `vet:` and
   led by any security finding,
3. `not judged`.

`--json` carries the full `verdict` and its `verdict_source` (`audit`, `vet`,
or null when not judged). `usage` leaves out `probe` sessions unless `--all`
(see [actors](#actors)).

### `mazo unused`

Lists the skills with no strong load in the window, each with a status and the
`held`, `cluster`, `named by`, `new` and `weak-only` notes. The
[glossary](#statuses-and-notes) defines each one.

### `mazo show`

`mazo show <skill>` prints the skill card. `--json` emits the whole card.

The card opens with a manual of at most 15 lines at 80 columns, in three parts.
An empty part is left out.

- **For** is what the skill is for.
- **Your use** is the first line of up to three real prompts, cut to 100
  characters.
- **Try** is up to three uses to try.

For and Try come from the latest skill audit when it is newer than the vet or
there is no vet, else from the vet in the lockfile. Without either, For is the
frontmatter description.

After the manual come:

- the daily histogram and recent events, each with the log line it came from,
- the description, git history, every grade and retro, cites and cited-by,
- usage by agent, channel and project,
- examples: the invocation on each agent, the manifest's `default_prompt`, up
  to three real prompts read from transcripts at display time and never
  stored, and up to three curated examples from the latest audit,
- that audit's date, session count, verdict, rates and top three improvements.

### `mazo report`

Writes one self-contained HTML page: the `usage --all` table with each skill's
quality verdict in full, the grades to run as `/grade <skill>`, and every
skill's card. The page goes to `report.html` in the data directory with mode
`0600`, and each run overwrites it. `--out <path>` writes it elsewhere and
warns inside a git worktree, because the page shows cwds. `--open` opens it.

### `mazo status`

Reports drift and policy for every home and, inside a project, its skills'
links (see [project skills](#project-skills)). Writes nothing.

### `mazo sync`

Links every skill, from `source` and the store, into every home. It checks
everything first and writes nothing if any check fails. `--dry-run` prints the
actions without taking them.

### `mazo new`

`mazo new <name> --description <text>` creates `<name>/SKILL.md` in `source`,
plus `agents/openai.yaml` when `codex_manifest` is on. When `claude_name_only`
is on, it prints the `skillOverrides` line to add to each Claude home. It
refuses a name that already has an entry or a store skill, and it does not
sync.

### `mazo add`

`mazo add <source>` copies a skill into the store, records it in `mazo.lock`
and links it into every home. It installs nothing if the resulting sync would
fail. `--project` installs into the project at the current directory's git
root instead (see [project skills](#project-skills)).

**Sources.** `<source>` is one of:

- a local path: one that exists, or starts with `.`, `/` or `~`;
- a git URL: `https://…`, `ssh://…`, `git@host:owner/repo.git` or `file://…`.
  A path after the segment ending in `.git` is the subpath
  (`https://host/owner/repo.git/skills/x`). A URL containing `?`, `#` or `\`
  is a usage error;
- GitHub shorthand `owner/repo[/path/to/skill]`, cloned from
  `https://github.com/owner/repo.git`.

The recorded origin drops a trailing `/` and ends in `.git` (except a `file://`
URL), so `https://github.com/owner/repo` and `owner/repo` record the same one.

**Cloning.** mazo shallow-clones a repository with the system `git` into the
data directory, without prompting, and removes the clone afterwards. ssh runs
with `BatchMode=yes` unless `GIT_SSH_COMMAND` is set. A failed clone exits 1
with git's error and installs nothing.

**Picking skills.** The source, at its subpath, is the skill when it holds a
`SKILL.md`. Otherwise its child directories that hold one are its skills. With
several:

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

**Names.** A name already in `source` or the store is refused (exit 2, writing
nothing). `--as <name>` installs the skill under another name and rewrites the
`name:` in its `SKILL.md`.

**Refusals.** mazo refuses a skill that contains a symlink, or one that a
symlink leads outside the clone.

**Vetting.** A source not on the `trusted` list goes to the `[judge]` before
mazo copies anything (see [vetting a skill](#vetting-a-skill)). Local paths are
never on the list. With no `[judge]` the add is refused (exit 2) unless
`--trust`, which installs without vetting. The refusal shows a pasteable
`[judge]` table for a `command` and an `http` judge, and a link to a
prefilled GitHub issue asking for a judge for your harness.

`--all` on an untrusted source first prints the number of judge calls, one per
skill, and asks `make <n> judge calls? [y/N]` before the first. Declining
installs nothing and exits 1. Without a terminal it needs `--yes`, and exits 2
before any call otherwise. A trusted `--all` asks nothing.

### `mazo update`

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
- **Untrusted and changed.** A skill with `trusted = false` whose source is not
  on the `trusted` list either is re-vetted before it is replaced (see
  [re-vetting an update](#re-vetting-an-update)). A trusted one is not vetted.
  With no `[judge]`, mazo refuses it: one line per such skill, then once the
  judge tables and issue link that `add` shows, and exit 2.
- **Not updatable.** A local-path skill. A name that is not a store skill is a
  usage error.

A failed clone, a refused re-vet, or an update the resulting sync would refuse
leaves that skill and its entry as they were. The rest still run, and the
command exits 1, or 2 when a skill was refused for want of a `[judge]`.
`--project` updates the project's skills instead (see
[project skills](#project-skills)).

### `mazo retire`

`mazo retire <name>` moves a skill of `source` to `.archive/` (`git mv` when
tracked) and prunes its links. A store skill is removed instead: mazo deletes
its links, its `mazo.lock` entry and its store directory.

It refuses a skill used in the last 30 days, or one held or named by a skill
used in that time, unless `--force`. It writes nothing if the resulting sync
would fail, and it never commits. `--project` retires a project skill instead
(see [project skills](#project-skills)).

### `mazo prune`

`mazo prune [<name>]` permanently deletes archived skills from `.archive/`. It
asks first, or takes `--yes`. In a git repository it refuses a file whose
content is in no `HEAD` path unless `--force`.

### `mazo audit`

`mazo audit <skill>` has an agent judge how the skill performs in real sessions
and writes a skill report. See [auditing a skill](#auditing-a-skill).

## Configuration

mazo reads `$MAZO_CONFIG`, else `$XDG_CONFIG_HOME/mazo/config.toml`, which is
`~/.config/mazo/config.toml` when `XDG_CONFIG_HOME` is unset. Without a config,
or with one that does not parse, every command but `init` exits 2.

A config can be a symlink. mazo resolves `source` and a relative home `path`
against the directory of the file it points to.

### Example

```toml
source = "."                      # your skills directory, relative to this file; optional
loose  = ["SKILLS_USAGE.md", "GOAL_SKILLS_USAGE.md"]
trusted = ["owner", "owner/repo"] # sources whose skills install without vetting

[[home]]
name  = "claude"
agent = "claude"
path  = "~/.claude"

[[home]]
name  = "pi"
agent = "pi"
path  = "~/.pi/agent"
links = { "extensions/pi-explicit-skills.ts" = "pi-explicit-skills.ts" }

[judge]                           # the agent mazo audit calls; none without this table
agent = "claude"                  # claude (default), codex, pi, command or http
model = "sonnet"                  # optional; unset means the agent's own default
billing = "subscription"          # subscription (default) or api
```

### `source`

Optional. It names a directory of skills you write yourself, and
`mazo init --source <skills dir>` sets it.

Without it your own skill set is empty, and `usage`, `unused`, `status`,
`sync`, `scan`, `add` and retiring a store skill work on store skills alone.
`new`, `prune`, retiring a skill of `source`, `loose` and home `links` need
it. Those commands exit 2 without it, and a config with `loose` or `links` but
no `source` does not load.

Skills in `source` are always trusted.

### `loose` and home `links`

`loose` lists files in `source` that `sync` links into each home's skills
directory beside the skills. A home's `links` maps extra files to link: each
key is relative to the home, each value relative to `source`.

### `[[home]]`

One table per agent home, with a `name`, an `agent` (`claude`, `codex` or
`pi`) and a `path`.

### `trusted`

Lists the owners (`owner`, meaning every repository of that owner) and
repositories (`owner/repo`) whose skills `mazo add` installs without vetting.
A config without `trusted` trusts no source other than `source`.

- An entry whose first segment has no `.` is on GitHub: `me` trusts
  `https://github.com/Me/skills.git` and no other host.
- Otherwise that segment names the host: `codeberg.org/me/skills` trusts
  `git@codeberg.org:me/skills.git`.
- Names compare ignoring ASCII case.
- An entry never trusts a `file://` or local source.
- An entry with an empty segment, or with more than owner/repo after an
  optional host, is a usage error.

### `[policy]`

Every rule is off by default. `sync` and `status` check the rules.

| Key | Effect when on |
|---|---|
| `no_disable_model_invocation` | A `SKILL.md` must omit `disable-model-invocation`. |
| `codex_manifest` | Each skill needs `agents/openai.yaml` with `policy.allow_implicit_invocation: false`; `mazo new` writes one. |
| `claude_name_only` | Each Claude home's `settings.json` must set every skill to `name-only` in `skillOverrides`; `mazo new` prints the line to add. |
| `authoring_skills` | A list of skills; a session that loaded one is tagged authoring (see [how accurate the counts are](#how-accurate-the-counts-are)). |

### `[judge]`

The agent that `mazo audit` and vetting call. A config without a `[judge]`
table has no judge, and `mazo audit` exits 2. mazo once assumed Claude when
the table was missing, so a config that relied on that must now add it:

```toml
[judge]
agent = "claude"
```

| Key | Meaning |
|---|---|
| `agent` | `claude` (the default), `codex`, `pi`, `command` or `http`. |
| `model` | Optional. Unset means the agent's own default. Required for `http`. |
| `billing` | `"subscription"` (the default) or `"api"`; any other value is a config error. See [who pays](#who-pays). |
| `argv` | The `command` judge's program and arguments. |
| `url` | The `http` judge's base URL. |
| `api_key_env` | The variable holding the `http` judge's key. |

**claude, codex, pi.** mazo runs the agent's own CLI. See
[auditing a skill](#auditing-a-skill) for how each runs.

**command.** Runs any CLI that reads a prompt on stdin and prints its answer on
stdout. `argv` names the program and its arguments, and only the command judge
takes one. `command` without a non-empty `argv` is a usage error.

```toml
[judge]
agent = "command"
argv  = ["llm", "-m", "gpt-5"]
```

**http.** Posts to an OpenAI-compatible chat completions endpoint,
`<url>/chat/completions`, for a user with an API key or a local model server
and no agent CLI. `url` and `model` are required. `api_key_env` is optional and
names the variable holding the key. mazo sends the key as
`Authorization: Bearer <key>` and nowhere else: not to disk, the records,
`--dry-run` or an error message. Without `api_key_env` no Authorization header
is sent. These are usage errors:

- a set `api_key_env` naming an unset or empty variable,
- `url` or `api_key_env` for any other agent.

```toml
[judge]
agent       = "http"
url         = "https://api.openai.com/v1"   # or http://localhost:11434/v1
model       = "gpt-5"
api_key_env = "OPENAI_API_KEY"
```

## How it works

### The data directory and the store

The ledger lives in `${XDG_DATA_HOME:-~/.local/share}/mazo/`. Back it up: once
Claude deletes a session log, the ledger holds the only record of its loads.

Skills installed with `mazo add` live in the store, `store/<name>/` in the same
directory. Every command reads them beside the skills in `source`: `sync`
links both, and a name in both is a conflict. `mazo.lock` there records one
`[[skill]]` per installed skill:

```toml
[[skill]]
name = "theirs"
origin = "https://github.com/owner/repo.git" # a git URL, or a local path's physical directory
subpath = "skills/theirs"         # its directory inside origin; "." for origin itself
commit = "9fceb02…"               # the commit cloned; local paths have none
content_hash = "3f2a…"            # SHA-1 of its sorted relative paths and file contents
trusted = true
installed_at = "2026-10-21T12:00:00Z"
```

A vetted install (see [vetting a skill](#vetting-a-skill)) has
`trusted = false` and also records the judge's answer:

```toml
vetted_at = "2026-10-21T12:00:00Z"
for = "Tidies release notes before a tag."  # what the skill is for
try = ["Tidy the changelog for 1.2."]       # up to three uses to try
[skill.verdict]
security = "…"                     # only with a security finding
quality = "good"                   # good, weak or poor
value = "worth trying"             # worth trying, low value, irrelevant or redundant
redundant_with = "mine"            # only when it overlaps an existing skill
reasons = ["Clear steps."]
```

### Vetting a skill

`mazo add` of a source not on the `trusted` list, without `--trust`, first
sends the skill to one call of the `[judge]`. The call has the same billing,
login check and default $1 cap (for a judge that reports its cost) as `audit`.

The judge sees:

- every file of the skill;
- the name and description of every skill in `source` and the store, and,
  with `--project`, the project's store;
- a summary of your last 30 days of work, capped at 8 KiB as a whole.

mazo builds the summary from the ledger and the transcripts it points into,
without a judge call. It holds the ten projects (`cwd`) and twenty skills used
most, each with its event count, and up to ten of your latest prompts, one per
transcript and from at most twenty transcripts. With no history the summary
says so and the vet still runs. The prompts reach the configured judge as you
wrote them, with whitespace collapsed and cut to 200 characters. They are not
redacted.

The judge answers with a security finding or none, a `quality` (`good`,
`weak`, `poor`), a `value` (`worth trying`, `low value`, `irrelevant`,
`redundant`, naming the skill it overlaps), its reasons, what the skill is for,
and up to three uses to try. mazo retries an answer that breaks the schema
once, then prints the verdict and decides:

- A security finding refuses the install (exit 1) unless `--force`. With
  `--force` the install still asks as below.
- A file mazo did not send in full is a security finding of its own: one that
  is not text, is over 64 KiB, or comes past 256 KiB in all. The refusal names
  each such file and why, before the judge is called.
- `--yes` accepts without asking. Without a terminal on stdin and without
  `--yes`, the install is refused (exit 2) before the judge is called.
- `--all` makes one judge call per skill. It prints their number and asks
  `make <n> judge calls? [y/N]` before the first, which `--yes` answers.

Otherwise mazo asks `install <name>? [Y/n]` or `install <name>? [y/N]`, and
Enter takes the default:

| Verdict | Prompt |
|---|---|
| `worth trying` and not `poor` | `[Y/n]` |
| anything else | `[y/N]` |

With no `[judge]` the add is refused (exit 2). The refusal names the two judges
any setup can configure, `command` and `http` (see
[the `[judge]` table](#judge)), and links a prefilled GitHub issue asking for a
judge for your harness.

A refused or declined install, or a judge that gave no valid answer, leaves no
store directory and no `mazo.lock` entry. An accepted one records the verdict
in its `mazo.lock` entry.

#### Re-vetting an update

`mazo update` re-vets a store skill when its content changed, its `mazo.lock`
entry has `trusted = false`, and its source is not on the `trusted` list. An
unchanged skill, or a trusted one, makes no judge call.

The one call sends the new `SKILL.md` and the output of `git diff --no-index`
from the installed copy to the new one (binary files diffed as text), with the
other skills and the work summary as for `add`. The same rules as `add` apply
or refuse the update, with `update <name>? [Y/n]` as the prompt and `--yes`
and `--force` on `update`:

- a security finding refuses it unless `--force`;
- a diff mazo did not send in full (not text, or over the caps) is a security
  finding of its own;
- without a terminal and without `--yes`, it is refused before the judge is
  called.

A refused update leaves the installed copy and its `mazo.lock` entry as they
were, the other skills still update, and the command exits 1. An accepted one
replaces `vetted_at`, `for`, `try` and the verdict in the entry.

### Project skills

`mazo add --project` installs for one project, the git repository holding the
current directory, instead of for you. It writes three things, and you commit
all three with the project:

- the skill, to `.mazo/skills/<name>/` at the repository's root;
- its entry, to `mazo.lock` there, in the same `[[skill]]` shape;
- relative links, `../../.mazo/skills/<name>`, in `.claude/skills` (Claude
  Code) and `.agents/skills` (Codex and Pi). Never into the homes.

The links resolve on any checkout. mazo writes no `.gitignore` and fetches
nothing back.

Vetting, `--skill`, `--all`, `--as`, `--trust`, `--yes` and `--force` work as
for a global add. A name already in the project's store, the global store or
`source`, or at either project link, is refused (exit 2). A refused or failed
install leaves no store directory, no entry and no link.

`mazo update --project [names...]` updates the project's skills from their
origins as `update` does the store's, re-vetting an untrusted one on its diff,
and rewrites the project's `mazo.lock`. The relative links need no change.

`mazo retire --project <name>` removes the project skill's directory, its
`mazo.lock` entry and both of its links, with the same blockers and `--force`
as a global retire. Blockers match usage by skill name, so recent use of a
global skill of the same name also blocks retiring the project copy, which
`--force` overrides.

Without `--project`, `update` and `retire` act on global skills only; given a
project skill's name, they point to `--project`. Outside a git repository,
`--project` exits 2 and writes nothing.

Run inside a git repository with a `mazo.lock` at its root, `mazo status` and
`mazo usage` list that project's skills beside the global ones, with
` (project)` after the name.

- `status` checks each one's two project links. It prints
  `project: <name> (project) in sync`, or else one drift line per bad link,
  such as `.agents/skills: <name> (project) is not linked (missing)`, which
  fails it like home drift. `--json` lists every project link under `project`.
- `usage` counts a project skill's loads through either project link or its
  `.mazo/skills/<name>` directory. Its quality is the vet in the project's
  `mazo.lock`, and it has no grade. A name both global and in the project gets
  one row per scope, counted apart.
- The `--json` rows of both commands carry `scope`, `global` or `project`.
  Outside a project that field is the only change.

### Usage tracking

`mazo scan` reads each home's transcripts from a per-file cursor and appends
the skill loads it finds to the ledger. Every event has a channel, the way the
agent loaded the skill:

| Channel | Strength | Meaning |
|---|---|---|
| `slash` | strong | The user invoked the skill, as `/name` in Claude, `$name` in Codex or a `<skill>` block in Pi. |
| `tool` | strong | Claude called its Skill tool on the skill. |
| `typed` | strong | A `/name` line in Claude's `history.jsonl` whose session transcript is gone; a one-time backfill. |
| `read` | weak | The agent read the skill's `SKILL.md` with a file or shell tool. |
| `ref` | weak | The agent read another Markdown file inside the skill's directory. |

A strong load means the skill was used. Only strong loads count towards
`sess`, `last` and the sparkline, and only they make a skill used for `unused`
and `retire`. A weak load means the agent looked at the skill, which may be
authoring rather than following it.

#### Actors

The actor is who ran the session: `main` (you), `subagent`, `headless` (Claude
`sdk-cli` or `codex exec`) or `probe` (any other Claude entrypoint). `usage`
leaves out `probe` unless `--all`, and `report`'s usage table always includes
it. `unused`, `show` and the report's cards always leave it out.

#### Statuses and notes

`unused` gives each skill without a strong load in the window one status:

| Status | Meaning |
|---|---|
| `unused` | Nothing holds it. |
| `held` | A skill that cites it was used in the window, so it is still in service; the note names the holders. |
| `new` | Its first commit is inside the window, or it is not committed yet. Outside a git repository no skill is new. |

and these notes:

| Note | Meaning |
|---|---|
| `cluster` | Skills that cite it, none of them used in the window either, so they may go together. |
| `weak-only` | It has weak loads in the window and no strong one. |
| `named by` | A skill used in the window names it in prose without citing it. It never makes the skill held, but `retire` refuses it without `--force` when the namer was used in the last 30 days. |

A grade cell in `usage` is the score as written, then:

| Suffix | Meaning |
|---|---|
| `+N` | `N` commits changed the skill after the grade. |
| `+wip` | No such commit, but the skill has uncommitted changes. |

### Auditing a skill

`mazo audit <skill>` sends each session not yet audited to one call of the
`[judge]` agent, newest first and round-robin across projects. It sends at
most `--limit` sessions (default 20), or every one with `--all`.

- Claude and Pi run with tools off.
- Codex runs in a read-only sandbox with a scratch `CODEX_HOME`.
- A command judge gets the prompt, the answer schema and the input on stdin,
  and its stdout must be the answer as JSON.
- An http judge gets the same text as one user message, and its reply's
  `choices[0].message.content` must be the answer as JSON. A connection
  failure or a non-2xx status stops the run, naming the status and the
  endpoint.

Every call runs in an empty directory under the data directory, and nothing is
written inside the source. mazo appends to `audits.jsonl` each answer that
cites real lines and quotes nothing.

When the run wrote any session record, or with `--resynth`, one more call
writes the skill report over every session record of the skill. The report
holds a verdict, patterns, ranked improvements, curated examples, what the
skill is for, and up to three uses to try that are not among the uses the
session records show. mazo computes the rates `followed`, `achieved` and
`abandoned or redone` itself, as `n of N`.

#### Budget

With the Claude judge, the session calls share 75% of `--max-budget-usd`
(default 1, at least 0.04) and the skill report gets the rest. No call starts
with less than a cent left. Codex, Pi, a command judge and an http judge report
no cost, so `--limit` is their only cap, `--max-budget-usd` is a usage error,
and their records' `cost_usd` is null.

#### Who pays

`[judge] billing` is `"subscription"`, the default, or `"api"`. Any other value
is a config error.

Under `"subscription"`, every judge call runs without `ANTHROPIC_API_KEY`,
`ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
`CLAUDE_CODE_USE_FOUNDRY`, `OPENAI_API_KEY` and `CODEX_API_KEY`. Before its
first call the run checks the login once, with the same variables removed, and
exits 2 when the check fails:

- `claude auth status` must report a claude.ai login or an OAuth token. An
  `apiKeyHelper` fails, since no environment change removes it.
- The Codex home's `auth.json`, else `~/.codex/auth.json`, must hold a ChatGPT
  login (`auth_mode` `chatgpt`).
- Pi is not checked. It picks its provider from its own settings, and a
  provider that logs in, such as OpenRouter, can still bill per token, so check
  the provider and model that `--dry-run` prints.
- A command judge is not checked either. mazo only removes the variables above
  from its environment.
- An http judge runs no process, so `billing` changes nothing for it.

A dry run, or a run with nothing to judge, starts no call and checks nothing. A
Claude dollar figure is labelled `API-equivalent`: Claude's own estimate, not a
charge, which `--max-budget-usd` still caps. A run stopped by a judge failure
names `billing = "api"` as the way to use an API key instead.

Under `"api"` the environment passes unchanged and nothing is checked.

Each record in `audits.jsonl` names its billing. One written before mazo
recorded it shows `unknown`.

#### Dry run

`--dry-run` lists the sessions with their agent, project and condensed size. It
also prints the judge command (a command judge's `argv`, an http judge's
endpoint and key variable), the billing and, for Pi and http, the model, an
input-token estimate and, once records exist, a dollar estimate. It starts no
judge.

#### Audit exit codes

A run exits 1 when it drops every session it judged, drops or skips the skill
report, or meets a judge that fails without an answer: a login, key or network
failure, or a signal mazo did not send. That last one stops the run at once,
keeps the records already written, and names the sessions left for the next
run, as a used-up budget does.

#### JSON output

`--json` prints the new skill record, the session records written and dropped,
the spend (null for Codex and Pi) and the sessions left.

### The TUI

Bare `mazo` at a terminal opens a TUI over the same report model: the usage
table, the unused list and a skill's card. Off a terminal, bare `mazo` prints
the help. The TUI scans first and writes nothing but what an action writes, and
it re-reads the data after each action.

| Key | Action |
|---|---|
| arrows, `hjkl` | Move. |
| `Enter` | Open a card. On the unused list, on a store skill whose status is `unused` (its `new` window since install has ended), ask to retire it instead. |
| `Esc` | Go back. On a list, close the audit pane once the audit ends. |
| `Tab` | Switch between usage and unused. |
| `/` | Filter by name. |
| `w` | Cycle the window: 30 days, 60 days, all. |
| `a`, `c` | Cycle the agent and channel filters. |
| `A` | Toggle authoring reads (not on the unused list). |
| `r` | Retire the selected skill, or the card's. |
| `s` | Show the actions `mazo sync --dry-run` would print, or say the homes are in sync. |
| `u` | Update the selected store skill, or the card's. |
| `U` | Update every store skill. |
| `i` | Add a source. |
| `x` | Audit the selected skill, or the card's. |
| `q` | Quit. |

**Retire (`r`)** works as `mazo retire` does. A skill `retire` refuses shows
its blockers, and there is no `--force`.

**Update (`u`, `U`)** works as `mazo update` does. A source skill, or one
installed from a local path, shows that it is not updatable. The dialog names
the judge and its cap and asks "update and ask about each judge verdict?". The
update runs in the background pane. Each re-vet of an untrusted skill that
changed opens the y/N dialog an add uses, where Enter takes the gate's default
and Esc or `q` answers No. There is no `--force`, so a security finding still
refuses that skill's update. An up-to-date skill is reported without a judge
call.

**Add (`i`)** asks for a source (Enter adds, Esc cancels) and adds it as
`mazo add <source>` does, in the background pane.

- An untrusted source with no `[judge]` shows the refusal `add` gives and
  starts nothing.
- A source of several skills opens the picker in a dialog.
- For an untrusted source, the dialog shows the number of judge calls, one per
  picked skill, and asks `make <n> judge calls? [y/N]` before the first. A
  trusted source asks nothing and calls no judge.
- Each verdict then opens a dialog with the `install <name>?` prompt and the
  default `add` gives it. Enter takes the default, `y` and `n` answer, and Esc
  or `q` is No (`q` then asks to quit).
- There is no `--force`, so a security finding refuses that skill.
- A skill reaches the store only once you accept its verdict.

**Audit (`x`)** works as `mazo audit <skill>` does with its default caps. The
dialog shows the cap (20 sessions, and $1 for a judge that reports its cost)
or the refusal `mazo audit` would give. The audit runs in the background, and a
pane above the footer shows its progress and result while the views keep
working.

One job, an add, an update or an audit, runs at a time. Quitting while one runs
asks first, and on `y` mazo kills its judge call. Every other action runs only
on `y`, and any other key cancels.

### How accurate the counts are

Measured on 2026-10-13 against this machine's logs:

- **Strong loads** (`slash`, `tool`, `typed`) matched two independent counting
  scripts, written from the spec without reading mazo's code, id for id: 191
  slash, 137 tool and 644 typed events.
- **Precision.** Two reviewers each sampled 20 strong events, 20 reads and 20
  log lines that mention a skill but produced no event, and checked them
  against the source records. Strong loads: 40 of 40 correct. Reads: 40 of 40
  correct. Missed loads among the 40 near misses: none.
- **Reads are weak evidence.** In those samples, 19 of 40 reads were authoring:
  editing or grading the skill rather than following it. mazo tags authoring
  when the session runs inside the git repository that holds the skills
  directory or inside a home's skills directory, or when the same session
  loaded a skill named in `[policy] authoring_skills`. The sampled ledger
  predates the skills-directory part of the rule and tagged 13 of the 19; that
  part covers 5 of the other 6, all run from `~/.codex/skills`. Reads are never
  added to the headline `sess` column.

mazo misses reads whose commands its grammar does not parse (`awk`, globs,
heredocs). `mazo scan --report` counts them and prints samples.

A transcript line that mentions a skill but is not JSON yields no event. A line
that mentions none is never parsed, so it cannot hide a load. The scan that
first reads a malformed line warns with the count per home, which `scan --json`
reports as `malformed` and `scan --report` prints. The next scan does not
repeat the warning, since the line is behind the cursor, but `scan --reparse`
reads it again. The parser canary in `scan --report` covers the other failure:
a file with a skill marker that yields no event at all.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. |
| 1 | The command failed or found a problem: a `status` error, a refused `sync`, `add`, `retire` or `prune`, a failed `update` of any skill, a failed audit run. |
| 2 | A usage error: a bad argument, a config that does not parse or names a missing source, an invalid Pi agent directory, an `add` of a taken name or an untrusted source, an `add`, `update` or `retire --project` outside a git repository, an `update` of a name that is not a store skill, an `audit` with no `[judge]`, or a failed judge login check. |

A usage error writes nothing, except that `audit` checks the login after its
scan.

## Privacy

mazo collects no telemetry. It sends data only to the `[judge]` you configure
(see [vetting a skill](#vetting-a-skill)). Its only other network use is the
git clone that `mazo add` and `mazo update` make of the source you name.

## Development

CI runs these on Linux and macOS:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
TZ=Pacific/Kiritimati cargo test --locked
TZ=Pacific/Pago_Pago cargo test --locked
```

The two `TZ` values are UTC+14 and UTC-11. The suite
includes the 18 ported `install.sh` tests and ingest properties such as "a
fresh scan equals an incremental one". The tests read the synthetic skills
tree in `tests/fixtures/skills`; set `MAZO_TEST_SKILLS` to run them against a
real skills directory instead.

### Adding an agent

Each agent is one module in `src/adapters/` implementing `Adapter`, one module
in `src/audit/judges/` implementing `Backend` with its arm in
`judges::backend`, plus one `Agent` variant and one `JudgeAgent` variant.
Adapters only turn transcript records into events, prompts and condensed
sessions. `classify.rs` computes identity, actor, root session and authoring
once, for every agent.

```rust
pub trait Adapter {
    /// Raw events from the lines of `chunk`; `header` is line 1 of the file.
    fn parse(&self, roots: &SkillRoots, path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed;
    /// Which records are user-authored, and what the judge keeps of each.
    fn records(&self) -> Records;
    // Provided: files, prompt_at, condense.
}
```

- `parse` gets the new lines since the file's cursor that pass the scan's
  prefilter, not the whole file.
- `records` returns two functions: `authored`, the user's text in a record if
  it has any, and `render`, the parts of a record the judge sees.
- The provided methods need no override. `files` lists every `*.jsonl` under
  the home's transcripts directory. `prompt_at` (the prompt behind a load, for
  the card) and `condense` (the judge's view of a session) walk the transcript
  with `records` at display or audit time and return `None` once it is gone.

A judge backend holds what differs between agents when mazo calls one:

```rust
pub trait Backend {
    /// The argv of a call, with stand-ins for the files the call writes.
    fn argv(&self, stage: &Stage) -> Vec<String>;
    /// What the call reads on stdin; none closes it.
    fn stdin(&self, stage: &Stage, input: &str) -> Option<String>;
    /// Under `subscription`, the login check made once before the first call.
    fn login(&self, data: &Path) -> Result<()>;
    /// A variable naming a scratch home, made afresh before and after each call.
    fn scratch(&self, data: &Path) -> Result<Option<(&'static str, PathBuf)>>;
    /// What a finished call returned: its cost and its answer, or why there is none.
    fn reply(&self, out: &Output, written: Option<Vec<u8>>, cap: Option<u64>) -> Result<Reply>;
    /// The flag that caps a call's dollar cost, for an agent that reports one.
    fn budget_flag(&self) -> Option<&'static str>;
    /// The judge line and model `--dry-run` prints.
    fn shown(&self, data: &Path, argv: &str) -> String;
    fn model(&self) -> Option<String>;
}
```

`login`, `scratch`, `budget_flag` and `model` have defaults: no check, no
scratch home, no dollar cap (so `--limit` is the only one), and the model
named in the argv. Running the call, its timeout, the billing scrub and the
cleanup are shared, in `src/audit/judges/mod.rs`. A judge that bills an API key
through its own environment variable adds it to `BILLED` there.

## License

[MIT](LICENSE)
