# skima

skima counts how your coding agents use your skills and keeps every agent home
linked to one skills directory. It reads the session logs that Claude Code,
Codex and Pi already write, stores each skill load in its own ledger, and
replaces `skills/install.sh` for linking. SPEC.md is the design;
`how-skima-works.html` explains it in plain English.

## Get started

skima runs on Linux and macOS and needs a Rust toolchain, a skills directory
(one subdirectory per skill, each holding a `SKILL.md`) and at least one of
Claude Code, Codex or Pi.

The repository is private: only users invited to it can install skima. Cargo
fetches it with your git credentials when `CARGO_NET_GIT_FETCH_WITH_CLI=true`.

```
CARGO_NET_GIT_FETCH_WITH_CLI=true \
  cargo install --locked --git https://github.com/nicolasgarcia214/skima
skima init --source <skills dir>   # write the config
skima sync                         # link every skill into every agent home
skima usage                        # see which skills your agents load
```

## Configuration

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

[judge]                           # optional: the agent skima audit calls
agent = "claude"                  # claude (default), codex or pi
model = "sonnet"                  # optional; unset means the agent's own default
```

The ledger lives in `${XDG_DATA_HOME:-~/.local/share}/skima/`. Back it up: once
Claude deletes a session log, the ledger holds the only record of its loads.

## Commands

| Command | What it does |
|---|---|
| `skima scan` | Reads new log lines into the ledger. `--report` adds per-channel counts, unparsed commands and the parser canary; `--reparse` re-derives every event from the logs on disk. |
| `skima usage` | One row per skill: sessions, loads per channel, agents, last use, a 30-day sparkline, the latest grade. |
| `skima unused` | Skills with no strong load in the window, with `held`, `cluster`, `new` and `weak-only` notes. |
| `skima show <skill>` | The skill card: daily histogram and recent events, each with the log line it came from, then description, git history, every grade and retro, cites and cited-by, usage by agent, channel and project, then examples: the invocation on each agent, the manifest's `default_prompt`, up to three real prompts read from transcripts at display time and never stored, and up to three curated examples from the latest audit; then that audit's date, session count, verdict, rates and top three improvements. `--json` emits the whole card. |
| `skima report` | Writes one self-contained HTML page: the `usage --all` table with each skill's latest audit verdict, the grades to run as `/grade <skill>`, and every skill's card. It goes to `report.html` in the data directory with mode `0600` and is overwritten on each run. `--out <path>` writes elsewhere and warns inside a git worktree, because the page shows cwds; `--open` opens it. |
| `skima status` | Drift and policy for every home. Writes nothing. |
| `skima sync` | Links every skill into every home. Checks everything first and writes nothing if any check fails. |
| `skima init --source <dir>` | Writes the config for a skills directory: one home per existing agent directory, every policy rule off. Refuses to overwrite. |
| `skima new <name> --description <text>` | Creates `<name>/SKILL.md`, plus `agents/openai.yaml` when `codex_manifest` is on, and prints the `skillOverrides` line to add to each Claude home when `claude_name_only` is on. Refuses an existing entry. Does not sync. |
| `skima retire <name>` | Moves a skill to `.archive/` (`git mv` when tracked) and prunes its links. Refuses a skill used, or held by a used skill, in the last 30 days unless `--force`, and writes nothing if the resulting sync would fail. Never commits. |
| `skima prune [<name>]` | Permanently deletes archived skills from `.archive/`. Asks first, or takes `--yes`; in a git repository it refuses a file whose content is in no `HEAD` path unless `--force`. |
| `skima audit <skill>` | Judges how a skill performs in real sessions: each session not yet audited (newest first, round-robin across projects, at most `--limit`, default 20, or `--all`) goes to one call of the `[judge]` agent (Claude or Pi with tools off, Codex in a read-only sandbox with a scratch `CODEX_HOME`), run in an empty directory under the data directory, and each answer that cites real lines and quotes nothing is appended to `audits.jsonl`. With the Claude judge, calls share 75% of `--max-budget-usd` (default 1, at least 0.04) and none starts with less than a cent left; the output names sessions left for the next run. Codex and Pi report no cost, so `--limit` is their only cap and `--max-budget-usd` is a usage error. Never writes inside the source. `--dry-run` lists the sessions with their agent, project and condensed size, the judge command, an input-token estimate and, once records exist, a dollar estimate, and starts no judge. When any session record was written, or with `--resynth`, one more call writes the skill report over every session record of the skill: a verdict, patterns, ranked improvements and curated examples, with the rates `followed`, `achieved` and `abandoned or redone` computed by skima as `n of N`. With the Claude judge it gets the rest of the budget, and under a cent left skips it and exits 1. A run that drops every session it judged, or drops the skill report, also exits 1. `--json` prints the new skill record, the session records written and dropped, the spend (null for Codex and Pi) and the sessions left. |

`usage`, `unused`, `show`, `report` and `audit` scan first, so their numbers are current. Every
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
  when the session runs inside the git repository that holds the skills
  directory or inside a home's skills directory, or when the same session
  loaded a skill named in `[policy] authoring_skills`. The sampled ledger predates the skills-directory part of the
  rule and tagged 13 of the 19; that part covers 5 of the other 6, all run
  from `~/.codex/skills`. Reads are never added to the headline `sess` column.

Reads with commands skima's grammar does not parse (`awk`, globs, heredocs) are
missed; `skima scan --report` counts them and prints samples.

## Adding an agent

Each agent is one module in `src/adapters/` implementing `Adapter`, plus one
`Agent` variant. Adapters only turn transcript records into events, prompts and
condensed sessions; identity, actor, root session and authoring are computed
once, in `classify.rs`.

```rust
pub trait Adapter {
    /// Transcript files of a home, sorted by basename (SPEC §6.2).
    fn files(&self, home: &Home) -> Vec<PathBuf>;
    /// Raw events from the lines of `chunk`; `header` is line 1 of the file.
    fn parse(&self, roots: &SkillRoots, path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed;
    /// The user-authored text behind the load at `src` (`<file>:<line>`) (SPEC §4.7).
    fn prompt_at(&self, src: &str) -> Option<String>;
    /// The judge's view of the session window around the strong load at `src` (SPEC §6.4).
    fn condense(&self, src: &str, max_chars: usize) -> Option<String>;
}
```

- `files` has a default: every `*.jsonl` under the home's transcripts directory.
- `parse` gets the new lines since the file's cursor that pass the scan's
  prefilter, not the whole file.
- `prompt_at` and `condense` read the transcript at display or audit time and
  return `None` when it is gone. Both share the walk in `adapters/mod.rs`; an
  adapter supplies which records are user-authored and what the judge keeps
  of each record.

## Tests

```
cargo test
```

The suite includes the 18 ported `install.sh` tests and ingest properties such
as "a fresh scan equals an incremental one". The tests read the synthetic
skills tree in `tests/fixtures/skills`; set `SKIMA_TEST_SKILLS` to run them
against a real skills directory instead.
