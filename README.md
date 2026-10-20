# skima

skima counts how your coding agents use your skills and keeps every agent home
linked to one skills directory. It reads the session logs that Claude Code,
Codex and Pi already write, stores each skill load in its own ledger, and
replaces `skills/install.sh` for linking.

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
symlink; `source` and a relative home `path` are resolved against the
directory of the file it points to.

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
billing = "subscription"          # subscription (default) or api
```

### Who pays for `skima audit`

`[judge] billing` decides who pays for the judge's calls. Any value other than
`"subscription"` or `"api"` is a config error.

With `"subscription"`, the default, an API key left in the shell cannot bill
the run. Every judge call runs without `ANTHROPIC_API_KEY`,
`ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
`CLAUDE_CODE_USE_FOUNDRY`, `OPENAI_API_KEY` and `CODEX_API_KEY`, whatever the
judge agent. Before its first judge call, a run checks the login once, with
the same variables removed, and exits 2 when the check fails:

- Claude: `claude auth status` must report a claude.ai login or an OAuth
  token. Anything else fails, including `api_key_helper` from an
  `apiKeyHelper` setting, which no environment change removes.
- Codex: the configured Codex home's `auth.json` (else `~/.codex/auth.json`)
  must hold a ChatGPT login (`auth_mode` `chatgpt`).
- Pi: not checked. Pi picks its provider from its own settings, and a provider
  that logs in, such as OpenRouter, can still bill per token. `--dry-run`
  prints the provider and model Pi will use, so check them there.

`--dry-run` and a run with nothing to judge start no call and check nothing.
Under `"subscription"`, every dollar figure for a Claude judge is labelled
`API-equivalent`: it is Claude's client-side estimate, not a charge, and
`--max-budget-usd` still caps it. A run stopped by a judge failure names
`billing = "api"` as the way to use an API key instead.

With `"api"`, the judge's environment passes unchanged and nothing is checked.

Records in `audits.jsonl` carry the billing under `judge`; records written
before it was recorded show it as unknown.

The ledger lives in `${XDG_DATA_HOME:-~/.local/share}/skima/`. Back it up: once
Claude deletes a session log, the ledger holds the only record of its loads.

## Commands

| Command | What it does |
|---|---|
| `skima scan` | Reads new log lines into the ledger. `--report` adds per-channel counts, unparsed commands and the parser canary; `--reparse` re-derives every event from the logs on disk. |
| `skima usage` | One row per skill: sessions, loads per channel, agents, last use, a 30-day sparkline, the latest grade. |
| `skima unused` | Skills with no strong load in the window, with `held`, `cluster`, `named by`, `new` and `weak-only` notes. |
| `skima show <skill>` | The skill card: daily histogram and recent events, each with the log line it came from, then description, git history, every grade and retro, cites and cited-by, usage by agent, channel and project, then examples: the invocation on each agent, the manifest's `default_prompt`, up to three real prompts read from transcripts at display time and never stored, and up to three curated examples from the latest audit; then that audit's date, session count, verdict, rates and top three improvements. `--json` emits the whole card. |
| `skima report` | Writes one self-contained HTML page: the `usage --all` table with each skill's latest audit verdict, the grades to run as `/grade <skill>`, and every skill's card. It goes to `report.html` in the data directory with mode `0600` and is overwritten on each run. `--out <path>` writes elsewhere and warns inside a git worktree, because the page shows cwds; `--open` opens it. |
| `skima status` | Drift and policy for every home. Writes nothing. |
| `skima sync` | Links every skill into every home. Checks everything first and writes nothing if any check fails. |
| `skima init --source <dir>` | Writes the config for a skills directory: one home per existing agent directory, every policy rule off. Refuses to overwrite. |
| `skima new <name> --description <text>` | Creates `<name>/SKILL.md`, plus `agents/openai.yaml` when `codex_manifest` is on, and prints the `skillOverrides` line to add to each Claude home when `claude_name_only` is on. Refuses an existing entry. Does not sync. |
| `skima retire <name>` | Moves a skill to `.archive/` (`git mv` when tracked) and prunes its links. Refuses a skill used, or held or named by a used skill, in the last 30 days unless `--force`, and writes nothing if the resulting sync would fail. Never commits. |
| `skima prune [<name>]` | Permanently deletes archived skills from `.archive/`. Asks first, or takes `--yes`; in a git repository it refuses a file whose content is in no `HEAD` path unless `--force`. |
| `skima audit <skill>` | Judges how a skill performs in real sessions: each session not yet audited (newest first, round-robin across projects, at most `--limit`, default 20, or `--all`) goes to one call of the `[judge]` agent (Claude or Pi with tools off, Codex in a read-only sandbox with a scratch `CODEX_HOME`), run in an empty directory under the data directory, and each answer that cites real lines and quotes nothing is appended to `audits.jsonl`. With the Claude judge, calls share 75% of `--max-budget-usd` (default 1, at least 0.04) and none starts with less than a cent left; the output names sessions left for the next run. Codex and Pi report no cost, so `--limit` is their only cap and `--max-budget-usd` is a usage error. Never writes inside the source. `--dry-run` lists the sessions with their agent, project and condensed size, the judge command, the billing and, for Pi, the model, an input-token estimate and, once records exist, a dollar estimate, and starts no judge. When any session record was written, or with `--resynth`, one more call writes the skill report over every session record of the skill: a verdict, patterns, ranked improvements and curated examples, with the rates `followed`, `achieved` and `abandoned or redone` computed by skima as `n of N`. With the Claude judge it gets the rest of the budget, and under a cent left skips it and exits 1. A run that drops every session it judged, or drops the skill report, also exits 1. A judge that fails without an answer (a login, key or network failure, or a signal skima did not send) stops the run at once with exit 1, keeps the records already written, and names the sessions not judged as left for the next run. `--json` prints the new skill record, the session records written and dropped, the spend (null for Codex and Pi) and the sessions left. |

`usage`, `unused`, `show`, `report`, `audit` and `retire` scan first, so their numbers are current. Every
report prints how far back each home's logs go: "unused since 2026-09-10" is not
"never used".

On a terminal, `usage`, `unused` and `status` colour their status tokens:
`unused` and `ERROR:` in red; `held`, `new`, the `named by` note, `warning:`
and a stale grade's `+N` or `+wip` in yellow. Piped output, or any output with
`NO_COLOR` set to a non-empty value, carries no colour.

## Glossary

Every event has a channel, the way the agent loaded the skill:

- **`slash`**: the user invoked the skill, as `/name` in Claude, `$name` in Codex
  or a `<skill>` block in Pi.
- **`tool`**: Claude called its Skill tool on the skill.
- **`typed`**: a `/name` line in Claude's `history.jsonl` whose session
  transcript is gone; a one-time backfill.
- **`read`**: the agent read the skill's `SKILL.md` with a file or shell tool.
- **`ref`**: the agent read another Markdown file inside the skill's directory.

- **Strong** loads are `slash`, `tool` and `typed`: the skill was used. Only
  they count towards `sess`, `last` and the sparkline, and only they make a
  skill used for `unused` and `retire`.
- **Weak** loads are `read` and `ref`: the agent looked at the skill, which
  may be authoring rather than following it.
- **Actor**: who ran the session: `main` (you), `subagent`, `headless` (Claude
  `sdk-cli` or `codex exec`) or `probe` (any other Claude entrypoint). Reports
  leave out `probe` unless `--all`.

`unused` gives each skill without a strong load in the window one status:

- **`unused`**: nothing holds it.
- **`held`**: a skill that cites it was used in the window, so it is still in
  service; the note names the holders.
- **`new`**: its first commit is inside the window, or it is not committed
  yet. Outside a git repository no skill is new.

and these notes:

- **`cluster`**: skills that cite it, none of them used in the window either,
  so they may go together.
- **`weak-only`**: it has weak loads in the window and no strong one.
- **`named by`**: a skill used in the window names it in prose without citing
  it. It never makes the skill held, but `retire` refuses it without `--force`.

A grade cell in `usage` is the score as written, then:

- **`+N`**: `N` commits changed the skill after the grade.
- **`+wip`**: no such commit, but the skill has uncommitted changes.

## How accurate the counts are

Measured on 2026-10-13 against this machine's logs:

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
    /// Transcript files of a home, sorted by basename.
    fn files(&self, home: &Home) -> Vec<PathBuf>;
    /// Raw events from the lines of `chunk`; `header` is line 1 of the file.
    fn parse(&self, roots: &SkillRoots, path: &Path, header: Option<&[u8]>, chunk: &Chunk) -> Parsed;
    /// The user-authored text behind the load at `src` (`<file>:<line>`).
    fn prompt_at(&self, src: &str) -> Option<String>;
    /// The judge's view of the session window around the strong load at `src`.
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
