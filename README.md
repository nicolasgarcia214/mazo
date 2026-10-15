# skima

skima counts how your coding agents use your skills and keeps every agent home
linked to one skills directory. It reads the session logs that Claude Code,
Codex and Pi already write, stores each skill load in its own ledger, and
replaces `skills/install.sh` for linking. SPEC.md is the design;
`how-skima-works.html` explains it in plain English.

## Install

```
cargo install --locked --path ~/projects/skima
```

skima reads `$SKIMA_CONFIG`, else `$XDG_CONFIG_HOME/skima/config.toml`
(`~/.config/skima/config.toml` when `XDG_CONFIG_HOME` is unset). Without one,
every command but `init` exits 2. `skima init --source <skills dir>` writes it:
a home for each of `~/.claude`, `~/.codex` and `~/.pi/agent` that exists, and
every policy rule off. It never overwrites an existing file. A config can be a
symlink; `source` is resolved against the directory of the file it points to.

```toml
source = "."                      # the skills directory, relative to this file
loose  = ["SKILLS_USAGE.md", "GOAL_SKILLS_USAGE.md"]

[[home]]
name  = "claude"
agent = "claude"
path  = "~/.claude"

[[home]]
name  = "pi"
agent = "pi"
path  = "~/.pi/agent"
links = { "extensions/pi-explicit-skills.ts" = "pi-explicit-skills.ts" }
```

The ledger lives in `${XDG_DATA_HOME:-~/.local/share}/skima/`. Back it up: once
Claude deletes a session log, the ledger holds the only record of its loads.

## Commands

| Command | What it does |
|---|---|
| `skima scan` | Reads new log lines into the ledger. `--report` adds per-channel counts, unparsed commands and the parser canary; `--reparse` re-derives every event from the logs on disk. |
| `skima usage` | One row per skill: sessions, loads per channel, agents, last use, a 30-day sparkline, the latest grade. |
| `skima unused` | Skills with no strong load in the window, with `held`, `cluster`, `new` and `weak-only` notes. |
| `skima show <skill>` | A skill's daily histogram and recent events, each with the log line it came from. |
| `skima status` | Drift and policy for every home. Writes nothing. |
| `skima sync` | Links every skill into every home. Checks everything first and writes nothing if any check fails. |
| `skima init --source <dir>` | Writes the config for a skills directory: one home per existing agent directory, every policy rule off. Refuses to overwrite. |
| `skima new <name> --description <text>` | Creates `<name>/SKILL.md`, plus `agents/openai.yaml` when `codex_manifest` is on, and prints the `skillOverrides` line to add to each Claude home when `claude_name_only` is on. Refuses an existing entry. Does not sync. |
| `skima retire <name>` | Moves a skill to `.archive/` (`git mv` when tracked) and prunes its links. Refuses a skill used, or held by a used skill, in the last 30 days unless `--force`, and writes nothing if the resulting sync would fail. Never commits. |
| `skima prune [<name>]` | Permanently deletes archived skills from `.archive/`. Asks first, or takes `--yes`; in a git repository it refuses a file whose content is in no `HEAD` path unless `--force`. |

`usage`, `unused` and `show` scan first, so their numbers are current. Every
report prints how far back each home's logs go: "unused since 2026-09-10" is not
"never used".

## How accurate the counts are

Measured on 2026-10-13 against this machine's logs (SPEC §10.2, §10.3):

- **Strong loads** (`slash`, `tool`, `typed`) matched two independent counting
  scripts, written from the spec without reading skima's code, id for id: 191
  slash, 137 tool and 644 typed events.
- **Precision:** two reviewers each sampled 20 strong events, 20 reads and 20
  log lines that mention a skill but produced no event, and checked them
  against the source records. Strong loads: 40 of 40 correct. Reads: 40 of 40
  correct. Missed loads among the 40 near misses: none.
- **Reads are weak evidence.** In those samples, 19 of 40 reads were authoring:
  editing or grading the skill rather than following it. skima tags authoring
  when the session runs inside the dotfiles repo or a home's skills directory,
  or when the same session loaded `grade`, `retro`, `writing-for-agents` or
  `skill-creator`. The sampled ledger predates the skills-directory part of the
  rule and tagged 13 of the 19; that part covers 5 of the other 6, all run
  from `~/.codex/skills`. Reads are never added to the headline `sess` column.

Reads with commands skima's grammar does not parse (`awk`, globs, heredocs) are
missed; `skima scan --report` counts them and prints samples.

## Tests

```
cargo test
```

The suite includes the 18 ported `install.sh` tests and ingest properties such
as "a fresh scan equals an incremental one". The tests read the synthetic
skills tree in `tests/fixtures/skills`; set `SKIMA_TEST_SKILLS` to run them
against a real skills directory instead.
