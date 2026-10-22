# mazo

mazo counts how your coding agents use your skills and keeps every agent home
linked to one skills directory. It reads the session logs that Claude Code,
Codex and Pi already write, stores each skill load in its own ledger, and
replaces `skills/install.sh` for linking.

## Get started

mazo runs on Linux and macOS and needs a Rust toolchain, a skills directory
(one subdirectory per skill, each holding a `SKILL.md`) and at least one of
Claude Code, Codex or Pi.

The repository is private: only users invited to it can install mazo. Cargo
fetches it with your git credentials when `CARGO_NET_GIT_FETCH_WITH_CLI=true`.

```
CARGO_NET_GIT_FETCH_WITH_CLI=true \
  cargo install --locked --git https://github.com/nicolasgarcia214/mazo
mazo init                         # write the config; add --source <skills dir> for your own skills
mazo sync                         # link every skill into every agent home
mazo usage                        # see which skills your agents load
```

## Configuration

mazo reads `$MAZO_CONFIG`, else `$XDG_CONFIG_HOME/mazo/config.toml`
(`~/.config/mazo/config.toml` when `XDG_CONFIG_HOME` is unset). Without one, or
with one that does not parse, every command but `init` exits 2.
`mazo init` writes it: a home for each of `~/.claude`,
`~/.codex` and `~/.pi/agent` that exists, every policy rule off, and a
detected `[judge]`. It never overwrites an existing file. A config can be a symlink; `source` and a relative home `path`
are resolved against the directory of the file it points to.

`source` is optional: it names a directory of skills you write yourself, and
`mazo init --source <skills dir>` sets it. Without it your own skill set is
empty, and `usage`, `unused`, `status`, `sync`, `scan`, `add` and retiring a
store skill work on store skills alone. `new`, `prune`, retiring a skill of
`source`, `loose` and home `links` need it; the commands exit 2 without it,
and a config with `loose` or `links` but no `source` does not load.

`init` tries claude, codex and pi in that order and writes the first found
as `[judge] agent = "..."`: found means the binary is on `PATH` and, for
claude and codex, the subscription login check `mazo audit` makes passes
(`claude auth status` reports a claude.ai or OAuth-token login; Codex's
`auth.json` holds a ChatGPT login). Pi has no login check. Detection reads
local state only and makes no model call. When none is found and stdin is a
terminal, `init` asks for one of claude, codex, pi, `command` or `http`; the
last two are written as a commented-out `[judge]` table of placeholders to
fill in and uncomment, since the table is invalid until then. Without a
terminal it writes no `[judge]` and prints how to add one.

A config without a `[judge]` table has no judge, and `mazo audit` exits 2.
mazo once assumed Claude when the table was missing; a config that relied on
that must now add it:

```toml
[judge]
agent = "claude"
```

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

`agent = "command"` runs any CLI that reads a prompt on stdin and prints its
answer on stdout. `argv` names the program and its arguments, and only the
command judge takes one; `command` without a non-empty `argv` is a usage error:

```toml
[judge]
agent = "command"
argv  = ["llm", "-m", "gpt-5"]
```

`agent = "http"` posts to an OpenAI-compatible chat completions endpoint,
`<url>/chat/completions`, for a user with an API key or a local model server
and no agent CLI. `url` and `model` are required; `api_key_env` is optional
and names the variable holding the key, sent as `Authorization: Bearer <key>`
and nowhere else: not to disk, the records, `--dry-run` or an error message.
Without `api_key_env` no Authorization header is sent. A set `api_key_env`
naming an unset or empty variable is a usage error, as are `url` or
`api_key_env` for any other agent:

```toml
[judge]
agent       = "http"
url         = "https://api.openai.com/v1"   # or http://localhost:11434/v1
model       = "gpt-5"
api_key_env = "OPENAI_API_KEY"
```

`trusted` lists the owners (`owner`, every repository of that owner) and
repositories (`owner/repo`) whose skills `mazo add` installs without vetting.
A git source's owner is the first segment of its URL's path and its repository
the second, on any host, compared ignoring ASCII case:
`git@codeberg.org:Me/skills.git` matches `me` and `me/skills`. An entry with an empty segment or more than two segments is a
usage error.
Skills in `source` are always trusted. A config without `trusted` trusts no
other source.

The ledger lives in `${XDG_DATA_HOME:-~/.local/share}/mazo/`. Back it up: once
Claude deletes a session log, the ledger holds the only record of its loads.

Skills installed with `mazo add` live in the store, `store/<name>/` in the same
directory, and every command reads them beside the skills in `source`: `sync`
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

A vetted install (see [Vetting a skill](#vetting-a-skill)) has `trusted =
false` and also records the judge's answer:

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

## Commands

| Command | What it does |
|---|---|
| `mazo scan` | Reads new log lines into the ledger. `--report` adds per-channel counts, unparsed commands and the parser canary; `--reparse` re-derives every event from the logs on disk. |
| `mazo usage` | One row per skill: sessions, loads per channel, agents, last use, a 30-day sparkline, the latest grade. |
| `mazo unused` | Skills with no strong load in the window, with `held`, `cluster`, `named by`, `new` and `weak-only` notes. |
| `mazo show <skill>` | The skill card. It opens with a manual of at most 15 lines at 80 columns: For, what the skill is for, and Try, up to three uses to try, both from the latest skill audit when it is newer than the vet or there is no vet, else from the vet in the lockfile, else For is the frontmatter description; and between them Your use, the first line of up to three real prompts, cut to 100 characters. An empty part is left out. Then the daily histogram and recent events, each with the log line it came from, then description, git history, every grade and retro, cites and cited-by, usage by agent, channel and project, then examples: the invocation on each agent, the manifest's `default_prompt`, up to three real prompts read from transcripts at display time and never stored, and up to three curated examples from the latest audit; then that audit's date, session count, verdict, rates and top three improvements. `--json` emits the whole card. |
| `mazo report` | Writes one self-contained HTML page: the `usage --all` table with each skill's latest audit verdict, the grades to run as `/grade <skill>`, and every skill's card. It goes to `report.html` in the data directory with mode `0600` and is overwritten on each run. `--out <path>` writes elsewhere and warns inside a git worktree, because the page shows cwds; `--open` opens it. |
| `mazo status` | Drift and policy for every home. Writes nothing. |
| `mazo sync` | Links every skill into every home. Checks everything first and writes nothing if any check fails. |
| `mazo init [--source <dir>]` | Writes the config, naming the skills directory when given: one home per existing agent directory, every policy rule off, and the first of claude, codex and pi found on `PATH` and logged in as `[judge]`, asking on a terminal when none is. Refuses to overwrite. |
| `mazo new <name> --description <text>` | Creates `<name>/SKILL.md`, plus `agents/openai.yaml` when `codex_manifest` is on, and prints the `skillOverrides` line to add to each Claude home when `claude_name_only` is on. Refuses an existing entry or a store skill of that name. Does not sync. |
| `mazo add <source>` | Copies a skill into the store, records it in `mazo.lock` and links it into every home. `<source>` is a local path (one that exists, or starts with `.`, `/` or `~`); a git URL (`https://…`, `ssh://…`, `git@host:owner/repo.git`, `file://…`), where a path after the segment ending in `.git` is the subpath (`https://host/owner/repo.git/skills/x`); or GitHub shorthand `owner/repo[/path/to/skill]`, cloned from `https://github.com/owner/repo.git`. The recorded origin drops a trailing `/` and ends in `.git` (except a `file://` URL), so `https://github.com/owner/repo` and `owner/repo` record the same one. A repository is shallow-cloned with the system `git` into the data directory, without prompting (ssh runs with `BatchMode=yes` unless `GIT_SSH_COMMAND` is set), and the clone removed afterwards; a failed clone exits 1 with git's error and installs nothing. The source (at its subpath) is the skill when it holds a `SKILL.md`; otherwise its child directories holding one are its skills, and with several `--skill <name>` picks one (without it, exit 2 lists them). Refuses (exit 2, writing nothing) a name already in `source` or the store; `--as <name>` installs it under another name and rewrites its `SKILL.md` `name:`. A skill containing a symlink, or one a symlink leads outside the clone, is refused. A source not on the `trusted` list is vetted by the `[judge]` before anything is copied (see [Vetting a skill](#vetting-a-skill)); local paths are never on it. With no `[judge]` it is refused (exit 2) unless `--trust`, which installs without vetting. Installs nothing if the resulting sync would fail. |
| `mazo update [names...] [--yes] [--force]` | Re-clones each git-sourced store skill (every one when no name is given) from its recorded origin, shallow and without prompting as `add` does, and reads the same subpath. When the copy's content hash differs, it replaces `store/<name>/` (staged beside it and swapped in, so a failure leaves the old copy) and records the new `commit` and `content_hash`, keeping `installed_at`; then it links the way `add` does. An unchanged skill is reported up to date and its `mazo.lock` entry is left as it was. A changed skill with `trusted = false`, whose source is not on the `trusted` list either, is re-vetted before it is replaced (see [Re-vetting an update](#re-vetting-an-update)), and skipped when there is no `[judge]`; a trusted one is not vetted. A local-path skill is not updatable. A failed clone, a refused re-vet, or an update the resulting sync would refuse, leaves that skill and its entry as they were; the rest still run and the command exits 1. A name that is not a store skill is a usage error. |
| `mazo retire <name>` | Moves a skill of `source` to `.archive/` (`git mv` when tracked) and prunes its links. A store skill is removed instead: its links, its `mazo.lock` entry and its store directory are deleted. Refuses a skill used, or held or named by a used skill, in the last 30 days unless `--force`, and writes nothing if the resulting sync would fail. Never commits. |
| `mazo prune [<name>]` | Permanently deletes archived skills from `.archive/`. Asks first, or takes `--yes`; in a git repository it refuses a file whose content is in no `HEAD` path unless `--force`. |
| `mazo audit <skill>` | Has an agent judge how the skill performs in real sessions and writes a skill report; see [Auditing a skill](#auditing-a-skill). |

Bare `mazo` at a terminal opens a TUI over the same report model: the
usage table, the unused list and a skill's card. Arrows or `hjkl` move, `Enter`
opens a card and `Esc` goes back, `Tab` switches between usage and unused, `/`
filters by name, `w` cycles the window (30 days, 60 days, all), `a` and `c`
cycle the agent and channel filters and `A` toggles authoring reads (not on the
unused list), and `q` quits. `r` retires the selected skill, or the card's, as
`mazo retire` does; a skill `retire` refuses shows its blockers, and there is
no `--force`. `s` shows the actions `mazo sync --dry-run` would print, or says
the homes are in sync. `x` audits the selected skill, or the card's, as
`mazo audit <skill>` does with its default caps: the dialog shows the cap (20
sessions, and $1 for a judge that reports its cost) or the refusal `mazo audit`
would give. The audit runs in the background and a pane above the footer
shows its progress and result while the views keep working; `Esc` on a list
closes the pane once the audit ends. One audit runs at a time. Quitting while
one runs asks first, and on `y` its judge call is killed. Each action runs only
on `y`; any other key cancels. The data is re-read after each action. It scans
first and writes nothing but what an action writes. Off a terminal, bare `mazo`
prints the help.

Exit codes: 0 on success; 1 when the command failed or found a problem
(a `status` error, a refused `sync`, `add`, `retire` or `prune`, a failed `update` of any skill, a failed audit run);
2 for a usage error: a bad argument, a config that does not parse or names a
missing source, an invalid Pi agent directory, an `add` of a taken name or an untrusted source, an `update` of a name that is not a store skill, an `audit` with no `[judge]`, or a failed judge login check. A usage error writes nothing,
except that `audit` checks the login after its scan.

`usage`, `unused`, `show`, `report`, `audit` and `retire` scan first, so their numbers are current. Every
report prints how far back each home's logs go: "unused since 2026-09-10" is not
"never used".

On a terminal, `usage`, `unused` and `status` colour their status tokens:
`unused` and `ERROR:` in red; `held`, `new`, the `named by` note, `warning:`
and a stale grade's `+N` or `+wip` in yellow. Piped output, or any output with
`NO_COLOR` set to a non-empty value, carries no colour.

## Vetting a skill

`mazo add` of a source not on the `trusted` list, without `--trust`, first
sends the skill to one call of the `[judge]`, with the same billing, login
check and default $1 cap (for a judge that reports its cost) as `audit`. The
judge sees every file of the skill and the name and description of every skill in
`source` and the store, and answers with a security finding (or
none), a `quality` (`good`, `weak`, `poor`), a `value` (`worth trying`, `low
value`, `irrelevant`, `redundant`, naming the skill it overlaps), its
reasons, what the skill is for, and up to three uses to try. An answer that
breaks the schema is retried once. mazo prints the verdict, then:

- A security finding refuses the install (exit 1) unless `--force`; with
  `--force` the install still asks as below. A file mazo did not send in
  full (not text, over 64 KiB, or past 256 KiB in all) is a security finding
  of its own: the refusal names each one and why, before the judge is called.
- It asks `install <name>? [Y/n]` when the skill is worth trying and not
  `poor`, and `[y/N]` otherwise; Enter takes the default.
- `--yes` accepts without asking. Without a terminal on stdin and without
  `--yes`, the install is refused (exit 2) before the judge is called.

A refused or declined install, or a judge that gave no valid answer, leaves
no store directory and no `mazo.lock` entry. An accepted one records the
verdict in its `mazo.lock` entry.

### Re-vetting an update

`mazo update` re-vets a store skill whose `mazo.lock` entry has `trusted =
false` and whose source is not on the `trusted` list, when its content
changed; an unchanged skill, or a trusted one, makes no judge call. The one
call sends the new `SKILL.md` and the output of `git diff --no-index` from the
installed copy to the new one (binary files diffed as text), with the other
skills as for `add`. The update is applied or refused by the same rules as
`add`, with `update <name>? [Y/n]` as the prompt and `--yes` and `--force` on
`update`: a security finding refuses it unless `--force`, a diff mazo did not
send in full (not text, or over the caps) is a security finding of its own,
and without a terminal and without `--yes` it is refused before the judge is
called. A refused update leaves the installed copy and its `mazo.lock` entry
as they were, the other skills still update, and the command exits 1. An
accepted one replaces `vetted_at`, `for`, `try` and the verdict in the entry.

## Auditing a skill

`mazo audit <skill>` sends each session not yet audited to one call of the
`[judge]` agent: newest first, round-robin across projects, at most `--limit`
(default 20), or every one with `--all`. Claude and Pi run with tools off,
Codex in a read-only sandbox with a scratch `CODEX_HOME`. A command judge gets
the prompt, the answer schema and the input on stdin, and its stdout must be
the answer as JSON. An http judge gets the same text as one user message, and
its reply's `choices[0].message.content` must be the answer as JSON; a
connection failure or a non-2xx status stops the run, naming the status and
the endpoint. Every call runs in an empty directory under the data
directory, and nothing is written inside the source. Each answer that cites
real lines and quotes nothing is appended to `audits.jsonl`.

When any session record was written, or with `--resynth`, one more call writes
the skill report over every session record of the skill: a verdict, patterns,
ranked improvements, curated examples, what the skill is for, and up to
three uses to try that are not among the uses the session records show, with
the rates `followed`,
`achieved` and `abandoned or redone` computed by mazo as `n of N`.

**Budget.** With the Claude judge, the session calls share 75% of
`--max-budget-usd` (default 1, at least 0.04), the skill report gets the rest,
and no call starts with less than a cent left. Codex, Pi, a command judge and
an http judge report no cost, so `--limit` is their only cap, `--max-budget-usd` is a usage
error, and their records' `cost_usd` is null.

**Who pays.** `[judge] billing` is `"subscription"`, the default, or `"api"`;
any other value is a config error. Under `"subscription"` every judge call runs
without `ANTHROPIC_API_KEY`,
`ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
`CLAUDE_CODE_USE_FOUNDRY`, `OPENAI_API_KEY` and `CODEX_API_KEY`, and before its
first call the run checks the login once, with the same variables removed, and
exits 2 when the check fails:
`claude auth status` must report a claude.ai login or an OAuth token (an
`apiKeyHelper` fails, since no environment change removes it), and the Codex
home's `auth.json` (else `~/.codex/auth.json`) must hold a ChatGPT login
(`auth_mode` `chatgpt`). Pi is
not checked: it picks its provider from its own settings, and a provider that
logs in, such as OpenRouter, can still bill per token, so check the provider and
model `--dry-run` prints. A command judge is not checked either: mazo only
removes the variables above from its environment. An http judge runs no
process, so `billing` changes nothing for it. A dry run, or a run with nothing to judge, starts no
call and checks nothing. A Claude dollar figure is labelled `API-equivalent`:
Claude's own estimate, not a charge, which `--max-budget-usd` still caps. A run
stopped by a judge failure names `billing = "api"` as the way to use an API key
instead. Under `"api"` the environment passes unchanged and nothing is checked.
Each record in `audits.jsonl` names its billing; one written before mazo
recorded it shows `unknown`.

**Dry run.** `--dry-run` lists the sessions with their agent, project and
condensed size, the judge command (a command judge's `argv`, an http judge's
endpoint and key variable), the billing and, for Pi and http, the model, an
input-token estimate and, once records exist, a dollar estimate. It starts no
judge.

**Exit codes.** A run exits 1 when it drops every session it judged, drops or
skips the skill report, or meets a judge that fails without an answer (a
login, key or network failure, or a signal mazo did not send). That last one
stops the run at once, keeps the records already written, and names the
sessions left for the next run, as a used-up budget does.

**Output.** `--json` prints the new skill record, the session records written
and dropped, the spend (null for Codex and Pi) and the sessions left.

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
  `sdk-cli` or `codex exec`) or `probe` (any other Claude entrypoint). `usage`
  leaves out `probe` unless `--all`, and `report`'s usage table always
  includes it; `unused`, `show` and the report's cards always leave it out.

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
  it. It never makes the skill held, but `retire` refuses it without `--force`
  when the namer was used in the last 30 days.

A grade cell in `usage` is the score as written, then:

- **`+N`**: `N` commits changed the skill after the grade.
- **`+wip`**: no such commit, but the skill has uncommitted changes.

## How accurate the counts are

Measured on 2026-10-13 against this machine's logs:

- **Strong loads** (`slash`, `tool`, `typed`) matched two independent counting
  scripts, written from the spec without reading mazo's code, id for id: 191
  slash, 137 tool and 644 typed events.
- **Precision:** two reviewers each sampled 20 strong events, 20 reads and 20
  log lines that mention a skill but produced no event, and checked them
  against the source records. Strong loads: 40 of 40 correct. Reads: 40 of 40
  correct. Missed loads among the 40 near misses: none.
- **Reads are weak evidence.** In those samples, 19 of 40 reads were authoring:
  editing or grading the skill rather than following it. mazo tags authoring
  when the session runs inside the git repository that holds the skills
  directory or inside a home's skills directory, or when the same session
  loaded a skill named in `[policy] authoring_skills`. The sampled ledger predates the skills-directory part of the
  rule and tagged 13 of the 19; that part covers 5 of the other 6, all run
  from `~/.codex/skills`. Reads are never added to the headline `sess` column.

Reads with commands mazo's grammar does not parse (`awk`, globs, heredocs) are
missed; `mazo scan --report` counts them and prints samples.

A transcript line that mentions a skill but is not JSON yields no event; a line
that mentions none is never parsed, so it cannot hide a load. The scan that
first reads one warns with the count per home, which `scan --json` reports as
`malformed` and `scan --report` prints; the next scan does not repeat it, since
the line is behind the cursor, but `scan --reparse` reads it again. The
parser canary in `scan --report` covers the other failure, a file with a skill
marker that yields no event at all.

## Adding an agent

Each agent is one module in `src/adapters/` implementing `Adapter`, one module
in `src/audit/judges/` implementing `Backend` with its arm in `judges::backend`,
plus one `Agent` variant and one `JudgeAgent` variant. Adapters only turn transcript records into events,
prompts and condensed sessions; identity, actor, root session and authoring are
computed once, in `classify.rs`.

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
cleanup are shared, in `src/audit/judges/mod.rs`; a judge that bills an API
key through its own environment variable adds it to `BILLED` there.

## Tests

```
cargo test
```

The suite includes the 18 ported `install.sh` tests and ingest properties such
as "a fresh scan equals an incremental one". The tests read the synthetic
skills tree in `tests/fixtures/skills`; set `MAZO_TEST_SKILLS` to run them
against a real skills directory instead.
