# Configuration reference

This reference covers the config file, every key, `mazo init`, the data
directory and store, the `mazo.lock` format and the exit codes. Back to the
[README](../README.md).

## Where the config lives

mazo reads `$MAZO_CONFIG`, else `$XDG_CONFIG_HOME/mazo/config.toml`, which is
`~/.config/mazo/config.toml` when `XDG_CONFIG_HOME` is unset. Without a config,
or with one that does not parse, every command but `init` exits 2.

A config can be a symlink. mazo resolves `source` and a relative home `path`
against the directory of the file it points to.

## Example

```toml
source = "."                      # your skills directory, relative to this file; optional
loose  = ["SKILLS_USAGE.md", "GOAL_SKILLS_USAGE.md"]
trusted = ["owner", "owner/repo"] # sources whose skills install without vetting
evals_dir = "~/evals"             # where grades and retros live; <data dir>/evals by default
context_window = 200000           # the tokens the agents' listing budgets are shares of; 200000 by default

[[home]]
name  = "claude"
agent = "claude"
path  = "~/.claude"

[[home]]
name  = "pi"
agent = "pi"
path  = "~/.pi/agent"
links = { "extensions/pi-explicit-skills.ts" = "pi-explicit-skills.ts" }

[judge]                           # the agent that vets installs and runs audits; none without this table
agent = "claude"                  # claude (default), codex, pi, command or http
model = "sonnet"                  # optional; unset means the agent's own default
billing = "subscription"          # subscription (default) or api
```

## `mazo init`

`mazo init [--source <dir>] [--no-skills]` writes the config, and refuses to
overwrite an existing one. It then installs the bundled `grade` and `retro`
skills from mazo's repository, which needs the network; `--no-skills` skips
that (see [the bundled skills](installing-skills.md#the-bundled-skills)). It
writes:

- a home for each of `~/.claude`, `~/.codex` and `~/.pi/agent` that exists;
- every policy rule off;
- `source`, when `--source` names your skills directory;
- a detected `[judge]`.

To detect the judge, `init` tries claude, codex and pi in that order and writes
the first one found as `[judge] agent = "..."`. Found means the binary is on
`PATH` and, for claude and codex, that it passes the subscription login check
`mazo audit` makes. For claude, `claude auth status` must report a claude.ai or
OAuth-token login. For codex, Codex's `auth.json` must hold a ChatGPT login. Pi
has no login check. Detection reads local state only and makes no model call.

When it finds none and stdin is a terminal, `init` asks for one of claude,
codex, pi, `command` or `http`. It writes the last two as a commented-out
`[judge]` table of placeholders for you to fill in and uncomment, since the
table is invalid until then. Without a terminal it writes no `[judge]` and
prints how to add one.

## `source`

Optional. It names a directory of skills you write yourself, and
`mazo init --source <skills dir>` sets it. A relative `source` resolves against
the config's directory.

Without it your own skill set is empty, and `usage`, `unused`, `status`,
`sync`, `scan`, `add` and retiring a store skill work on store skills alone.
`new`, `prune`, retiring a skill of `source`, `loose` and home `links` need
it. Those commands exit 2 without it, and a config with `loose` or `links` but
no `source` does not load.

Skills in `source` are always trusted.

## `loose` and home `links`

`loose` lists files in `source` that `sync` links into each home's skills
directory beside the skills. A home's `links` maps extra files to link: each
key is relative to the home, each value relative to `source`.

```toml
loose = ["SKILLS_USAGE.md", "GOAL_SKILLS_USAGE.md"]

[[home]]
name  = "pi"
agent = "pi"
path  = "~/.pi/agent"
links = { "extensions/pi-explicit-skills.ts" = "pi-explicit-skills.ts" }
```

## `[[home]]`

One table per agent home, with a `name`, an `agent` (`claude`, `codex` or
`pi`) and a `path`.

## `trusted`

Lists the owners (`owner`, meaning every repository of that owner) and
repositories (`owner/repo`) whose skills `mazo add` installs without
[vetting](installing-skills.md#vetting-a-skill). A config without `trusted`
trusts no source other than `source`.

- An entry whose first segment has no `.` is on GitHub: `me` trusts
  `https://github.com/Me/skills.git` and no other host.
- Otherwise that segment names the host: `codeberg.org/me/skills` trusts
  `git@codeberg.org:me/skills.git`.
- Names compare ignoring ASCII case.
- An entry never trusts a `file://` or local source.
- An entry with an empty segment, or with more than owner/repo after an
  optional host, is a usage error.

## `evals_dir`

Optional. It names the directory that holds grades and retros, and defaults
to `evals` under the [data directory](#the-data-directory-and-the-store). A
relative `evals_dir` resolves against the config's directory, and a leading `~`
is your home directory. Point it at a versioned repository to keep grades in
git.

mazo reads a skill's grades from `<evals_dir>/grades/<skill>/*.md` and its
retros from `<evals_dir>/retros/<skill>/*.md`, for the skills of `source` and
the store alike. A project skill has no grade. `mazo evals dir <skill>` prints
both directories, one per line, as `grades` or `retros`, a tab, and the
absolute path; it exits 2 for a skill mazo does not know.

A grade whose frontmatter `content_hash` is not the skill's current one, the
`content_hash` that `mazo show <skill> --json` prints, is stale. A grade
without one is stale too.

## `context_window`

Optional. The context window, in tokens, that the TUI's top bar measures each
agent's skill listing against, 200000 by default. The top bar estimates the
listing at a quarter token per character: a skill in context with its
description counts its name and description, a name-only one its name. Claude
lists skills within 1% of the window and Codex within 2%, and past that
budget an agent trims the descriptions without saying so. An agent's total
turns the warning colour at 80% of its budget. Pi has no budget. One window
covers every agent. A `context_window` of 0 is a usage error.

## `[policy]`

The three boolean rules are off by default, and `sync` and `status` check
them. `authoring_skills` is empty by default.

| Key | Effect |
|---|---|
| `no_disable_model_invocation` | When on, a `SKILL.md` must omit `disable-model-invocation`. |
| `codex_manifest` | When on, each skill needs `agents/openai.yaml` with `policy.allow_implicit_invocation: false`, and `mazo new` writes one. |
| `claude_name_only` | When on, each Claude home's `settings.json` must set every skill to `name-only` in `skillOverrides`, and `mazo new` prints the line to add. |
| `authoring_skills` | A list of skills. mazo tags a read in a session that loaded one as authoring (see [how accurate the counts are](usage-tracking.md#how-accurate-the-counts-are)). |

## `[judge]`

The agent that `mazo audit` and vetting call. A config without a `[judge]`
table has no judge: `mazo audit` exits 2, and mazo refuses an untrusted
`mazo add` unless `--trust`. When the table exists, `agent` defaults to `claude`.

Earlier versions assumed claude when the table was missing; a config that
relied on that must add it:

```toml
[judge]
agent = "claude"
```

| Key | Meaning |
|---|---|
| `agent` | `claude` (the default within the table), `codex`, `pi`, `command` or `http`. |
| `model` | Optional. Unset means the agent's own default. Required for `http`. |
| `billing` | `"subscription"` (the default) or `"api"`; any other value is a config error. See [who pays](audit.md#who-pays). |
| `argv` | The `command` judge's program and arguments. |
| `url` | The `http` judge's base URL. |
| `api_key_env` | The variable holding the `http` judge's key. |

### claude, codex and pi

mazo runs the agent's own CLI. [Auditing a skill](audit.md#how-an-audit-runs)
describes how each one runs.

### command

Runs any CLI that reads a prompt on stdin and prints its answer on stdout.
`argv` names the program and its arguments, and only the command judge takes
one. `command` without a non-empty `argv` is a usage error.

```toml
[judge]
agent = "command"
argv  = ["llm", "-m", "gpt-5"]
```

### http

Posts to an OpenAI-compatible chat completions endpoint,
`<url>/chat/completions`, for a user with an API key or a local model server
and no agent CLI. `url` and `model` are required.

`api_key_env` is optional and names the variable holding the key. mazo sends
the key as `Authorization: Bearer <key>` and nowhere else. It never puts the
key on disk, in the records, in `--dry-run` output or in an error message. Without
`api_key_env`, mazo sends no Authorization header.

These are usage errors:

- a set `api_key_env` naming an unset or empty variable;
- `url` or `api_key_env` for any other agent.

```toml
[judge]
agent       = "http"
url         = "https://api.openai.com/v1"   # or http://localhost:11434/v1
model       = "gpt-5"
api_key_env = "OPENAI_API_KEY"
```

## The data directory and the store

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
ref = "main"                      # the branch, tag or commit cloned; none for the default branch
commit = "9fceb02…"               # the commit cloned; local paths have none
content_hash = "3f2a…"            # SHA-1 of its sorted relative paths and file contents
trusted = true
installed_at = "2026-10-21T12:00:00Z"
```

A [vetted install](installing-skills.md#vetting-a-skill) has `trusted = false`
and also records the judge's answer:

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

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. |
| 1 | The command failed or found a problem: a `status` error, a refused `sync`, `add`, `retire` or `prune`, a failed `update` of any skill, a failed audit run. |
| 2 | A usage error: a bad argument, a config that does not parse or names a missing source, an invalid Pi agent directory, an `add` of a taken name or of an untrusted source with no `[judge]`, an `add`, `update` or `retire --project` outside a git repository, an `update` of a name that is not a store skill, an `audit` with no `[judge]`, or a failed judge login check. |

A usage error writes nothing, except that `audit` checks the login after its
scan. The [add](installing-skills.md#picking-skills) and
[audit](audit.md#audit-exit-codes) guides give their own exit codes.
