# skima: specification

skima is a skill manager for one person's coding agents. Anyone on Linux or
macOS with a skills directory and Claude Code, Codex or Pi can use it. It
answers five questions from files already on disk:

1. Which of my skills actually load, on which agent, through which channel, and when?
2. Which skills are dead weight?
3. Does every agent home see exactly the canonical skill set, with the invocation policy intact?
4. What is each skill for, how do I use it in practice, and how has it changed?
5. How well does each skill work in real sessions, and how should it improve?

It is inspired by [MoltenBase](https://www.moltenbase.com/), which offers usage
tracking, cross-agent sync, team policy and local-first storage. skima keeps
usage tracking, sync and local-first storage, and drops teams. It adds what one
person with a git-tracked skills repo needs: a durable usage ledger, provenance
for every number, a join with `.grades/` and `.retros/`, a per-skill card, an
HTML report, lifecycle commands, and an on-demand audit of real use.

Version 1 (§3 F1–F6) merged six independent plans written on 2026-10-12, each
based on read-only inspection of the author's machine, and shipped on
2026-10-13. Version 2 (§3 F11–F19) was settled in a design interview on
2026-10-14, after four independent audits scored version 1 at 5.5 of 10: sound
ingest, but no progress feedback, thin per-skill insight and personal rules
hardcoded as errors. §12 lists every fact the design rests on, numbered
E1–E18, and how each was observed. Other sections cite those numbers instead
of repeating them.

---

## 1. Decisions

| # | Decision | Why |
|---|---|---|
| D1 | **Transcripts are the only telemetry source. skima installs no hooks.** | Claude Code, Codex and Pi each write a structured record when a skill loads (§4). Transcripts cover every load channel, subagents included, and give months of history on the first run. A Claude `PreToolUse:Skill` hook would see one channel on one agent, starting the day it was installed, and would be a second event stream to deduplicate. |
| D2 | **skima keeps its own append-only ledger.** | Claude Code deletes transcripts after `cleanupPeriodDays`, which defaults to 30 and is unset here (E2). skima does not ask for longer retention. A transcript only has to survive until the next scan, and the ledger is the record that survives any retention setting and any future change to the harness's cleanup. |
| D3 | **Every command that reads usage scans first (`usage`, `unused`, `show`, `report`, `audit`, `retire`). No timer, no daemon.** | A usage report is never stale. `status` and `sync` read links and settings, not transcripts, so they do not scan. Data is lost only if no scan runs for a whole retention window. `status` and every usage report warn before that happens (§5.4). |
| D4 | **skima absorbed `skills/install.sh`, which was deleted in the same change.** | Two tools managing the same links would drift. The hard-coded target list in `install.sh` already caused a real bug (E1). Done on 2026-10-13 (§5.7). |
| D5 | **Symlinks only. No copy mode, no frontmatter transforms.** | All three hosts read the same `SKILL.md` and follow symlinks. Per-host differences already live in configuration: `skillOverrides` for Claude, `agents/openai.yaml` for Codex, `pi-explicit-skills.ts` for Pi. `sync` validates those and generates nothing; only `skima new` writes skill files, once, at creation. |
| D6 | **Rust, one binary, with clap. A CLI plus one static HTML report. JSONL storage, no SQLite.** | §8 gives the reasons. The report is a file, not a server, so D3's "no daemon" holds. |
| D7 | **Every count traces to a `file:line` in a transcript.** | This follows the user's rule that every claim carries its evidence. `skima show` prints the locators, and `retro` can open them. |
| D8 | **"Unused" is reported with its coverage window.** | "Unused since 2026-09-10 on Claude and since 2026-03-29 on Codex" is a different claim from "never used". Reports say which one they mean. |
| D9 | **Personal conventions are configuration, off by default.** | Version 1 hardcoded the author's rules (`openai.yaml` policy, Claude `name-only`, the authoring skills), so `sync` refused a plain Agent Skills directory: three of four auditors reproduced the failure. The rules move to a `[policy]` table (§5.4); the author's config turns them all on and behaves exactly as before. |
| D10 | **Judging runs only when the user types `skima audit`.** | It spends model quota. `scan`, `usage`, `show` and `report` never call a model. Every audit has a budget cap and a `--dry-run` (F18). |
| D11 | **No prompt text is stored, with one named exception.** | Real examples are read from transcripts at display time and vanish when the transcript does (F15). Audit records hold the judge's paraphrase and verdicts, never quoted prompts (§6.4). The exception is `report.html` (F16): a derived view, overwritten on every run, mode `0600`, under the data directory by default. The ledger and audits may describe client work under NDA. |
| D12 | **One data model, three renderers.** | Each report builds one struct; text, `--json` and HTML render it. The HTML report shows nothing the JSON lacks, so agents (`grade`, `retro`) and people see the same facts. |
| D13 | **skima's own audit runs never count as usage.** | An audit is an agent session that reads the skill. Audits run ephemerally where the agent allows it (E16) and in a working directory the scanner skips (F18). |

## 2. Scope

**Kept from MoltenBase:**
- invocation counts, last-used timestamps and the never-called report
- a 30-day trend
- a retire action
- a single source of truth with per-agent targets
- drift detection
- plain local files

**Dropped:**
- Team policy, accounts and cloud sync. Multi-machine sync is git's job. The
  tool is shareable; each user runs it on their own skills.
- A web server, live dashboard or TUI. The HTML report (F16) is one static file.
- Copy-based sync and per-agent transforms (D5).
- Adapters for agents beyond Claude Code, Codex and Pi. An adapter is written
  only against real transcripts it can be tested on (E13); §9 documents the
  adapter interface for contributors.

**Added:**
- the durable ledger (D2), plus a one-time backfill of typed `/name` commands
  from `~/.claude/history.jsonl`, which goes back to 2026-02-24 (E3)
- channel and actor on every event (§4.1)
- provenance per event (D7)
- coverage windows (D8)
- reference awareness, so a hub skill that live skills depend on is never
  offered for retirement (§5.3)
- a join with `.grades/` and `.retros/` frontmatter, every grade and not only
  the latest (§6.3)
- a per-skill card with description, history, examples and audit (F14, F15)
- an on-demand audit of how a skill performs in real sessions (F18)
- `init`, `new` and `prune` (F11, F17)

**Non-goals:**
- Writing harness settings files. skima prints the edits they need.
- Grading the text of a skill. `grade` owns that; skima shows every grade,
  says when one is stale, and feeds `grade` usage evidence (F19). skima's
  judging is about real use (F18).
- Storing prompt text, command arguments or skill bodies (D11).
- Managing project-scope skills (`<repo>/.claude/skills`).
- Windows. Network access other than the judge agent an audit calls.

## 3. Features

Every acceptance criterion is proven by a run (§10), not by reading the code.

### MVP

**F1. `skima scan`: incremental ingest.**
- A second consecutive run appends 0 lines and finishes in under 2 s.
- Deleting `cursors.json` and rescanning appends 0 lines. The cursor is an
  optimisation. The append rule in §6.2 is the guarantee.
- A scan into an empty data directory yields the same events, after sorting by
  id, as the incremental ledger. The comparison covers every transcript still on
  disk and excludes `typed` events, which only a home's first scan creates
  (§4.2).
- The first full scan of this machine finishes in under 90 s. The corpus is
  about 1.0 GB of Claude, 5.2 GB of Codex and 585 MB of Pi transcripts. A
  planner's prefiltered prototype read 7.27 GB in 58 s.
- Deleting a transcript never removes ledger events.
- For each agent and channel, the ledger's counts equal those of two
  independent counter scripts (§10.2).

**F2. `skima usage`: the usage table.**
- One row per canonical skill, including skills with zero use. The columns, in
  order:
  - `sess`: distinct root sessions with a strong event, the headline number
  - `slash`, `tool`, `typed`, `read`, `ref`: collapsed counts per channel (§4.1)
  - `agents`: the agents with any event, as letters (`C` Claude, `X` Codex,
    `P` Pi)
  - `last`: the time of the last strong event in the window, `-` when there is
    none (the all-time last use is in `show`)
  - `30d`: a sparkline of strong events per day; a day with none prints `·`
  - `grade`: the latest grade score, followed by `+N` when N commits changed
    the skill after it, or `+wip` when only uncommitted changes did (§6.3)
- Strong and weak channels (§4.1) get separate columns and are never summed.
- A footer prints the coverage window for each home and a one-line legend for
  the agent letters, `·`, `+N` and `+wip` (F12).
- `--json` emits the same rows.

**F3. `skima unused`: the never-called report.**
- It lists canonical skills with zero strong events in the window (default 60
  days), and prints that window for each home.
- A skill whose first commit in the skills repo is newer than the window start,
  or that has no commit yet (`break-invariants` is untracked today), is marked
  `new` instead.
- A skill is `held` when a skill that cites it (§5.3) has a strong event in the
  window. Held skills are listed with their live holders and are not retirement
  candidates.
- A skill cited only by unused skills is listed as unused. It carries a
  `cluster: <holders>` note so the group can be judged together.
- A skill with only weak events is listed with the flag `weak-only`.

**F4. `skima show <skill>`: one skill's evidence.**
- A daily histogram for the window, then the last N events. Each event shows
  its timestamp, agent, channel, actor, cwd and `file:line`.
- Each locator opens at the record that produced the event. If the transcript
  has been cleaned up, the locator is marked `expired`.
- The latest grade, with its score, its date and whether the skill changed
  after it (§6.3).
- The retro count, the latest retro's date, and the number of strong events
  since that retro.

**F5. `skima status`: drift and policy, read-only.**
- Every pair of a configured home and an entry, canonical or not, gets one
  state from §5.2.
- Runs every policy rule in §5.4.
- Exits 0 when clean and 1 on any error-level finding.
- Writes nothing. A test proves this by comparing tree snapshots before and after
  a run.
- On 2026-10-13, before the first `sync`, it reported exactly one error:
  `claude-work: missing break-invariants` (E1).

**F6. `skima sync`: replaces `install.sh`.**
- Builds the whole plan, policy checks included, before the first write. Any
  conflict or policy failure aborts with zero writes in every home.
- A second run prints one line, `in sync: <homes> homes, <skills> skills`
  (F12).
- Prunes only owned dangling links: absolute links equal to
  `<source>/<basename>` whose target is gone.
- Links the loose files and each home's `links` entries.
- `--agent A` limits linking and the per-home policy rules to homes of agent
  `A`. Per-skill rules always run. This matches `install.sh --pi`, which still
  checks every skill file and skips only the Claude settings check (E8).
- The ported `test_install.py` passes. The one-shot differential checks against
  the old `install.sh` match in both the default mode and `--agent pi` (§10.5).
- After the first real `sync`, `status` exits 0 and `~/.claude-work/skills`
  contains `break-invariants`.

### Version 2

Settled in the design interview of 2026-10-14. Each item's acceptance criteria are
proven by a run, like the MVP's.

**F11. `skima init` and the config location.**
- skima reads `$SKIMA_CONFIG`, else `$XDG_CONFIG_HOME/skima/config.toml`, with
  `XDG_CONFIG_HOME` defaulting to `~/.config` on Linux and macOS alike. There is
  no other default (§6.1).
- With no config, every command except `init` exits 2 with
  `ERROR: no config at <path>; run skima init --source <skills dir>`.
- `skima init --source <dir>` writes that file. It lists one home for each of
  `~/.claude`, `~/.codex` and `~/.pi/agent` that exists, `source` as an
  absolute path, every `[policy]` boolean `false`, `authoring_skills = []`,
  and an empty `loose`. It refuses to overwrite an existing file.
- A fresh `HOME` holding one plain skill (a `SKILL.md` with `name` and
  `description`, nothing else) and an empty `~/.claude/settings.json`: `init`,
  then `sync`, exits 0 and links the skill. This is the test three auditors
  failed on version 1.

**F12. Progress and plain output.**
- **Progress.** When stderr is a TTY and a scan has run for more than 1 s,
  skima rewrites one stderr line per home: `scanning codex 1,204/6,111 files`.
  The line is cleared when the scan ends. Nothing is printed when stderr is not
  a TTY. No progress crate.
- **Sync and status.** `sync` with nothing to do prints
  `in sync: <homes> homes, <skills> skills`; `sync --dry-run` with nothing to
  do prints the same line. `status` prints one line per home: `<home>: in sync
  (<n> skills)` when it has no error-level finding, otherwise one plain line
  per error, such as `claude-work: break-invariants is not linked (missing)`.
  The other error states read `<home>: <skill> link target is gone
  (dangling)`, `<home>: <skill> links somewhere else (wrong-link)` and
  `<home>: <skill> is a real file, not a link (obstructed)`.
  Info-level findings follow on their own lines in either case, such as
  `claude: synced is not from the source; left alone (foreign)`. A rule that
  belongs to a skill rather than a home prints `ERROR: <skill dir>: <reason>`
  on stderr (§5.5); homes with no error of their own still print `in sync`.
  Exit codes do not change.
- **Legend.** `usage` and `unused` end with a legend line (F2).
- **Paths.** `show` prints `$HOME` as `~` and a transcript as
  `<agent>:<file name>:<line>` with the full path in `--json`.
- **Reparse.** `scan --reparse` prints `<home> <n> events (was <m>)` in
  place of the `scan:` summary line, since `+N` is meaningless after a
  rewrite.

**F13. Portability.**
- Policy rules are configuration, off by default (D9, §5.4).
- `strict_yaml.rs`, its PyYAML differential test and `tests/policy_loader.py`
  are deleted (§5.4). Every
  real skill's files pass the replacement, proven by a run of `status` against
  the author's config, before the deletion lands.
- The authoring skill names are configuration (§4.1, §6.1).
- The repository is private on GitHub at first, MIT-licensed, with CI running
  `cargo test` and `cargo clippy` on Linux and macOS. It installs with
  `cargo install --git`. The first push is confirmed with the owner.
- The README opens with a newcomer path: install, `init`, `sync`, `usage`.
  While the repository is private it says so: only invited users can install
  from it.

**F14. The skill card: `show <skill>`.** F4's histogram and events stay; the
card adds, in this order:
- the `description` from the frontmatter and the skill's path;
- **history:** the first commit, the last commit touching the skill with its
  date and subject, and whether the working tree has uncommitted changes;
- **grades:** every grade, newest first, with date and score, and how many
  commits changed the skill after the latest one (§6.3);
- **retros:** every retro with date, score and verdict, and the strong events
  since the latest;
- **links:** the skills it cites and the skills that cite it (§5.3), with
  `held` status;
- **usage:** strong events by agent and channel, all-time last use, and the
  top three projects by distinct sessions;
- **examples** (F15);
- **audit:** the latest skill-level audit's verdict, rates and top three
  improvements, with its date and how many sessions it covered (F18).
- `--json` emits the whole card. It is the contract `grade` reads (F19).

**F15. Examples.**
- **Written:** the invocation for each configured agent (`/name` on Claude,
  `$name` on Codex, `/skill:name` on Pi) and `interface.default_prompt` from
  `agents/openai.yaml` when it exists (E17).
- **Real:** up to three prompts that loaded the skill, read from transcripts at
  display time and never stored (D11). Only strong events with actor `main`
  qualify. skima takes the most recent event per project (cwd), then the three
  most recent projects. The text is the user prompt that caused the load
  (§4.7), cut to 300 characters. A candidate whose text, after stripping, is
  only the invocation or shorter than 20 characters ("yes", "go on") is
  skipped for the next most recent event in the same project. A `typed` event uses its `history.jsonl`
  display text, which outlives the transcript.
- **Curated:** up to three from the latest audit (F18).
- When a transcript has expired, its example is skipped, not marked.

**F16. `skima report`: the HTML report.**
- Scans first, then writes one self-contained HTML file: inline CSS, inline
  SVG charts, no script loaded from anywhere and no network access. Light and
  dark themes follow `prefers-color-scheme`.
- Content: the coverage footer, the `usage --all` table (canonical,
  non-inventory and `other` rows) with each skill's latest audit verdict, a list of stale or missing grades with the command to run
  (`/grade <skill>`), and one section per skill holding its card (F14).
- The default output is `<data dir>/report.html`, outside any git repository,
  written with mode `0600` and overwritten on every run, because the page
  shows cwds and prompt excerpts (D11). `--out <path>` writes elsewhere and
  warns when the target is inside a git worktree; `--open` opens it with
  `xdg-open` or `open`.
- It renders the same structs as `--json` (D12). A test asserts that every
  skill in `usage --all --json` appears in the HTML, and that no `src` or `href`
  attribute points outside the file.

**F17. Lifecycle: `new`, `retire`, `prune`.**
- **`skima new <name> --description <text>`** validates the name and the
  description (§5.4),
  refuses an existing entry, and writes `<source>/<name>/SKILL.md` with
  frontmatter and a heading. With `codex_manifest` on, it also writes
  `agents/openai.yaml` with `allow_implicit_invocation: false` and a
  `default_prompt` naming `$<name>`. With `claude_name_only` on, it prints the
  `"<name>": "name-only"` line for each Claude home. It never runs `sync`.
- **`skima retire <name> [--force]`** scans first (D3). It refuses when the
  skill is `held` (§5.3) or has a strong event in the last 30 days; `--force`
  overrides both. It then builds the `sync` plan as if the skill were already
  archived, and aborts with zero writes if that plan has any conflict or
  policy error. Only then does it move `<source>/<name>` to
  `<source>/.archive/<name>` (`git mv` when the skill is tracked, so the move
  is staged) and apply the plan, which prunes the now-dangling links. It prints
  the settings lines that can go. It never commits.
- **`skima prune [<name>] [--yes] [--force]`** permanently deletes
  `<source>/.archive/<name>`, or every entry in `.archive/` without a name. It
  lists what it will delete and asks for confirmation on a TTY; with no TTY it
  requires `--yes`, else exits 2. In a git repository it refuses an archived
  skill holding any file whose content is in no commit, because that work
  exists nowhere else, unless `--force`. A file is safe when its blob
  (`git hash-object`) equals the blob at `HEAD:./<name>/<file>` or
  `HEAD:./.archive/<name>/<file>`, resolved with `git -C <source>` so a
  source below the repository root works, so a skill moved by `retire` and not yet
  committed is safe, and a `.gitignore` cannot hide an edit. Outside a git
  repository it deletes after confirmation. Ledger events, audits, grades and
  retros of a pruned skill are kept; it is no longer canonical, so `usage`
  shows it only under `--all`, as a non-inventory row.

**F18. `skima audit <skill>`: judged performance in real use.** §6.4 has the
design. The audit reports and suggests; it never edits `SKILL.md` or any other
file in the source. The Claude and Pi judges run with tools disabled; the
Codex judge, which has no such flag, runs in a read-only sandbox in an empty
directory. Acceptance:
- `--dry-run` lists the sessions it would judge, their projects and condensed
  sizes, and the judge command, and calls nothing.
- A run judges at most `--limit` sessions (default 20) not yet audited, then
  writes the skill-level report. A second run with no new sessions calls only
  the skill-level stage, and only with `--resynth`.
- Every rate in the report is computed by skima from session records and
  printed as `n of N` with session sources.
- The audit's own sessions add no event to the ledger (D13): a scan after an
  audit appends 0 lines.
- With the Claude judge, stage-1 calls share 75% of `--max-budget-usd`
  (default 1) and stage 2 gets the rest (§6.4); no call starts with a cap
  under $0.01, and a budget under $0.04 is a usage error (exit 2). Stage 2 is
  skipped only when the last stage-1 call overshot so far that less than
  $0.01 remains; the run
  then says so and exits 1. Worst-case spend is the budget plus one call's
  overshoot.
- `--json` prints the new skill record, the number of session records written
  and dropped, and the spend.

**F19. `grade` integration.**
- skima's side is F2's `+N` and F16's stale-grade list.
- The `grade` skill gains one step, made in the skills repo through the normal
  skill-editing flow, not by skima: when `skima` is on `PATH`, run
  `skima show <skill> --json` and weigh the usage, examples and latest audit as
  evidence.

### Later

Each of these is built only when a real need shows up.

**F9. Policy-leak report.** Classifies each `tool` or `read` load by its cause.
`requested` means the user's prompt for that turn names the skill. `chain` means
a skill loaded earlier in the session names it. `rule` means `AGENTS.md` names
it. `unprompted` means none of these, which is a leak in the name-only policy.
This would replace the hand-run probes behind the README line "loaded 10 of 14
unprompted runs". It is deferred because the classification is lexical: a
paraphrased request lands in `unprompted`. It ships only after its precision is
measured on real sessions.

## 4. Invocation detection

One rule holds for every agent: **count only the harness's own structured
record, never a mention in text.** A raw grep for
`<command-name>/validate-findings-backlog` matched 73 transcript lines on
2026-10-12, but only 3 of them were slash loads. The rest are quotes inside tool output and prompts. Before emitting
an event, every parser checks the record type, the role and an anchored prefix.

### 4.1 Event vocabulary

**Channel**: how the skill body entered a context.

| Channel | Strength | Meaning |
|---|---|---|
| `slash` | strong | The user typed `/name` (Claude), `$name` (Codex) or `/skill:name` (Pi), and the harness injected the body. |
| `tool` | strong | The model called Claude's Skill tool, and the load succeeded. |
| `typed` | strong | A `/name` line in `<home>/history.jsonl` for a session that had no transcript at the home's first scan (§4.2). |
| `read` | weak | The model read `<skill>/SKILL.md` with a file or shell tool. |
| `ref` | weak | The model read another Markdown file inside the skill directory, such as `references/*.md`. Reading a skill's scripts, tests or `agents/openai.yaml` is not instruction loading and yields no event. |

`read` is weak because it cannot tell reading to follow a skill from reading to
edit or grade it. On Codex and Pi it is still the main channel. Both hosts hide
skills from the model (E11), so the `AGENTS.md` rule makes the model read
`SKILL.md` by path. One planner counted 1,472 Codex reads against 167 `$name`
loads. Leaving reads out would undercount those hosts about tenfold, and mixing
them into the headline would count authoring as use. They therefore get their
own column.

Refused Skill calls produce no event. They are not use, and nothing reads them.

**Actor**, where the first matching rule wins:
1. `probe`: a Claude `entrypoint` other than `cli` or `sdk-cli` (E10). These are
   benchmark and policy-probe harnesses, excluded from reports by default.
2. `subagent`: a Claude `isSidechain` record or a `/subagents/` path, a Codex
   rollout with `source.subagent`, or a Pi session whose header has
   `parentSession`.
3. `headless`: Claude `entrypoint = sdk-cli`, or Codex `originator = codex_exec`.
4. `main`: everything else.

**Root session.** The event's `session` is the root of its session chain:
- Claude: the subagent transcript shares `sessionId` with its parent.
- Codex: follow `forked_from_id` or `source.subagent.thread_spawn.parent_thread_id`
  until a rollout has neither.
- Pi: follow `parentSession`.

A skill loaded anywhere in a chain therefore counts once in `sess`, on every
agent.

**Origin** has two values, which is the only split a report uses:
- `canonical` when any of these holds:
  - `realpath` of the event's path lands under the source root;
  - the path is `<some home's skills dir>/<name>/…` and `<name>` is a known
    name;
  - the event has no path, and its name is a known name.

  A known name is a skill that is canonical today, or the name of the
  directory holding any `SKILL.md` path in the skills repo's git history
  (the basename of its parent, from `git log --name-only`). Taking the parent's
  basename matters: the repo once nested skills as `skills/claude/goal-audit`
  and `skills/shared/grade`, and the top-level component would make `claude`,
  `codex` and `shared` known names (E15). The second rule keeps events for
  skills whose link was later pruned or renamed, including most Codex `$name`
  events (E6).
- `other` for everything else: plugin skills (`anthropic-skills:pdf`),
  `~/.agents/skills/find-skills`, Codex `.system` skills, and the `/tmp` probe
  skills from policy experiments.

Reports show only `canonical` unless `--all` is given. Each event keeps its raw
`path`, so a finer split can be computed later without a re-ingest.

**Authoring tag.** A `read` or `ref` event gets `authoring = true` when:
- the event's cwd is inside the git worktree that contains the source root
  (the source root itself when it is not in a repository), or inside any
  configured home's skills directory (grading or editing skills from
  `~/.codex/skills` is authoring too), or
- its root session holds a strong event for a skill named in
  `policy.authoring_skills` (§6.1), anywhere in the ledger. The list is empty
  by default; the author's config names `grade`, `retro`, `writing-for-agents`
  and `skill-creator`.

The first condition is stored at ingest, because the cwd travels with the
event. The second is evaluated at report time over the whole ledger, so it does
not depend on the order in which files were scanned, and a fresh scan and an
incremental scan tag the same reads. Authoring events are excluded by default
and shown with `--authoring`. M1 measures the second condition (§10.3). If it
changes fewer than 5% of read events, it is deleted.

**Collapse.** Reports count distinct `(agent, root session, skill, channel)`.
Codex reads long files in chunks (`sed -n '1,220p'`, then `sed -n '220,440p'`),
and compaction can replay an injection. Neither should count twice. The raw
events stay in the ledger.

### 4.2 Claude Code

**Homes:** every configured home with `agent = "claude"`; the author's are
`~/.claude` and `~/.claude-work`, which the `cw` alias selects with
`CLAUDE_CONFIG_DIR`. Transcripts live at
`<home>/projects/<cwd-slug>/<sessionId>.jsonl` and at
`<home>/projects/<cwd-slug>/<sessionId>/subagents/agent-*.jsonl`. The fields
read are `uuid`, `timestamp`, `sessionId`, `cwd`, `version`, `isSidechain`,
`entrypoint` and `promptId`.

| Record | Event |
|---|---|
| `type:"user"`, `isMeta:true`, any text block (a plain-string `content` counts as block 0) starting `Base directory for this skill: <dir>`, with `sourceToolUseID` | `tool`, `path = <dir>/SKILL.md` |
| The same record without `sourceToolUseID` | `slash`, `path = <dir>/SKILL.md` |
| A user record whose `toolUseResult` has `success:true` and a `commandName` (the Skill result; its `tool_use_id` is the event id, so no earlier record is needed) | `tool`, `path = null` |
| An assistant `tool_use` named `Read` whose `input.file_path` is a file inside a skill directory | `read` or `ref` |
| A `Bash` `tool_use` whose `input.command` matches the reader grammar (§4.5) | `read` or `ref` |

Rows 1 and 3 describe the same load, and both use the tool_use id as the event
id. When a scan ends between the result and the meta record, the next scan
upgrades the event with its path (§6.2). The event set is the union of the two
rows. A success result can appear without a meta record (E5). A meta record can
appear without a success result: inside subagent files the Skill result has no
`toolUseResult` at all (12 meta records on 2026-10-12, all in `agent-*.jsonl`).

Row 2 needs no earlier record. Built-in commands such as `/clear` write a
`<command-name>` record but no meta record, so the absence of
`sourceToolUseID` alone identifies a slash load. The record classifies on its
own, so a scan that ends between the command record and the meta record loses
nothing. The `Base directory` text is usually block 0. In subagents started
with a `/name` prompt it is block 1, after a `<command-message>` block (7
records on 2026-10-12, all `/tmp` probes), so the rule matches any block.

**Ignored:**
- `<command-name>` records. A skill's slash load is recorded from its meta
  record; built-ins such as `/clear` and `/model` have none.
- The `attachment.type` values `skill_mention`, `skill_listing`,
  `invoked_skills` (re-injection after compaction) and `dynamic_skill`.
- Any match inside `tool_result` content.

**Event ids:**
- `cc:<tool_use id>` for rows 1 and 3.
- `cc:<meta uuid>` for row 2.
- `cc:<assistant uuid>:<content index>:<file index>` for reads, the file
  index counted among all files the command reads (§4.3).

**History backfill.** Each `<home>/history.jsonl` line has `display`,
`timestamp` (in milliseconds), `project` and `sessionId`. Backfill runs once per
home, during the scan that creates the home's `coverage.json` entry:
- For every line whose `display` matches `^/<name>(\s|$)`, where `<name>` is
  a known name and the line's `sessionId` has no transcript on disk at that
  moment, it emits a `typed` event with id `cch:<sessionId>:<timestamp>`. The
  anchor keeps plugin commands such as `/argus:triage` out (E15).
- A `typed` event's origin is `canonical` only when `<name>` is canonical at the
  moment of backfill, and `other` otherwise. History has no path to check, and
  a name such as `argus` was a skill until 2026-04-01 and a plugin command
  after it; both kinds of line read `/argus`.
- After that, `history.jsonl` is never ingested again. Every later session is
  recorded from its own transcript, well inside retention. F15 reads a
  `typed` event's line at display time, which creates no event.

The two sources are therefore disjoint by construction. A `typed` event can
include an aborted prompt, and reports label the channel. `scan --reparse`
copies `typed` events verbatim and never re-derives them.

### 4.3 Codex

**Files:** `~/.codex/sessions/YYYY/MM/DD/rollout-<start time>-<id>.jsonl` (E6).
Line 1 is `type:"session_meta"`, with `payload.id`, `session_id`, `cwd`,
`originator`, `source`, `thread_source`, `forked_from_id` and `cli_version`.
Only line 1 is the header. 499 forked rollouts carry a second or third
`session_meta` line, which is the parent's header copied in, and the parser
ignores it. To walk the chain to the root, the adapter finds a parent by
globbing `rollout-*-<parent id>.jsonl` and reads that file's line 1; the
results are cached per scan.

| Record | Event |
|---|---|
| `type:"response_item"`, `payload.type:"message"`, `role:"user"`, with an `input_text` block starting `<skill>\n<name>NAME</name>\n<path>PATH</path>` | `slash` (the `$name` expansion) |
| `payload.type` `function_call` (JSON `arguments` with `cmd`) or `custom_tool_call` named `exec` (JavaScript calling `tools.exec_command({cmd: …})`), where a `cmd` matches the reader grammar | `read` or `ref`, one per file read |

From an `exec` input, skima takes the `cmd` value of each object passed to
`tools.exec_command`, whether the key is bare or quoted (real inputs use
`{"cmd":"sed …","workdir":…}`), and the `workdir` beside it. A value counts
when it is a JavaScript string or template literal without `${…}`
substitutions. Any other `cmd` value that
names a skill file increments the unparsed counter (§4.5).

**Excluded:**
- sessions with `thread_source == "guardian_review"`, which replay other
  sessions' transcripts as evidence
- `role:"developer"` `<skills_instructions>` listings, which make up about 98% of
  raw `SKILL.md` hits
- `*_output` records, duplicates in `event_msg.item_completed`, and
  `compacted.replacement_history`

**Event id:** `cx:<root session>:<sha1(payload)>:<index>`, where `payload` is
the record's `payload` parsed into a `serde_json::Value` (whose objects are
`BTreeMap`s, so keys are sorted by byte) and written back with
`serde_json::to_vec`: compact separators, non-ASCII unescaped, UTF-8. The
crate's `preserve_order` feature stays off, because it would keep the file's
key order instead. Ids are permanent, so this serialisation never changes. The record
timestamp is not part of the id.
- A forked or subagent rollout copies parent records with a new timestamp and
  a byte-identical payload (E14), and it shares the parent's root, so the copies
  collapse to one id. A hash that included the timestamp would count every copy.
- `payload.id` alone is not used, because it is null on most `$name` blocks
  (E6). A `function_call` or `exec` payload carries a unique `call_id`, so two
  real reads never share an id.
- Two genuine `$name` blocks with identical text in one root session do share
  an id. Reports collapse them per root session anyway (§4.1), so only `show`
  lists one line fewer.
- The index separates several events in one record. For reads it is
  `<call index>.<file index>`, where the file index is the file's position
  among all files the command reads, skill files or not, so an id does not
  move when a file's classification changes: an `exec` wrapper can call
  `tools.exec_command` more than once, and one `cmd` can read two files. For a
  `$name` block it is the content index of its `input_text` block.

`~/.codex/history.jsonl` is not read. Codex does not clean up rollouts, so it
adds nothing.

### 4.4 Pi

**Files:**
`${PI_CODING_AGENT_SESSION_DIR:-<home>/sessions}/--<cwd-slug>--/<ts>_<id>.jsonl`.
A session file's line 1 is `{type:"session", version:3, id, timestamp, cwd,
parentSession?}`. `parentSession` holds the parent session's file path (E12).
Entries are `{type:"message", id, parentId, timestamp, message:{role,
content}}`. The header `cwd`, not the directory slug, decides the workspace.
`files()` globs `*.jsonl` only, because the directories also hold run-state
files (`*.jsonl.herdr-complete.json`, `{"version":1,"runId":…}`) and Markdown.
A `.jsonl` file whose line 1 is not `type:"session"` is skipped.

| Record | Event |
|---|---|
| A `role:"user"` message whose text starts `<skill name="NAME" location="PATH">`, where `PATH` is absolute and ends in `/SKILL.md` | `slash` (the `/skill:name` expansion) |
| An assistant content item `{type:"toolCall", name:"read", arguments:{path}}` on a file inside a skill directory | `read` or `ref` |
| A `toolCall` named `bash` whose command matches the reader grammar | `read` or `ref` |

The `location` check is required. An unchecked regex also matches `...`,
`{name}` and `${skill.name}` placeholders in prose and code (E7).

**Event ids:** `pi:<entry id>@<entry timestamp>`, followed by `:<content
index>:<file index>` for reads. Forked sessions copy ancestor entries with the
same id and timestamp, so the copies collapse (E7). Pi's `/fork` also writes
`parentSession`, so a session the user forked counts as `subagent`. This is
accepted: it changes the actor column only, never `sess`.

### 4.5 Shared reader grammar

`readcmd.rs` decides whether a shell command string loads a skill file. Codex,
Pi `bash` and Claude `Bash` all use it.

**A skill file** is a path that, after a relative path is resolved against the
call's working directory (Codex `workdir`, else the session cwd), either:
- ends in `/SKILL.md`, or
- lies under a configured home's skills directory or the source root, in a
  `<name>/` child directory, where `<name>` is a known name (§4.1) or a
  non-hidden directory that contains `SKILL.md`. Reads of `.grades/`,
  `.retros/` or `tests/` are therefore not skill reads.

Relative reads are common: 467 of 2,267 Codex `SKILL.md` commands, such as
`sed -n '1,260p' SKILL.md` run from inside the skill. The skill name is the
parent directory of `SKILL.md`, or the `<name>` component. Origin (§4.1) then
separates `/tmp` probes and `.system` skills from canonical ones. The `Read`
and Pi `read` rows use the same definition.

- **A load** is a command made only of reader verbs: `cat`, `sed -n 'a,bp'`,
  `head`, `tail`, `nl -ba`, `bat` or `less`, each naming files inside skill
  directories. Several reads may be chained with `&&` or `;`, whether they are
  chunks of one file or different files. Each distinct file read emits one
  event. A reader may also name files outside skills; only the skill files
  emit events. `sed -n` may print several ranges (`'1,80p;120,300p'`).
- `cd <dir>` segments are allowed and rebase later relative paths, and a
  leading `~/` is expanded, because both are common in real reads
  (`cd ~/dotfiles/skills && cat grade/SKILL.md`).
- **A pipeline** is a load when its first stage is a reader and every later
  stage only passes lines through (`nl`, `cat`, `head`, `tail`, `sed -n`), as
  in `sed -n '1,200p' f | nl`. Only the last stage's output reaches the model,
  so `sed -n '1,200p' f | wc -l` or `| rg foo` is not a load.
- **Not a load:** `rg`, `grep`, `wc`, `ls`, `find`, `git` or `apply_patch` on
  skill files, and any pipeline whose first stage is not a reader.
- **Unparsed:** a command that names a skill's Markdown file (`SKILL.md` or
  another `.md` inside a skill directory) but matches neither rule increments
  `unparsed_skill_cmd`. Running a skill's scripts reads no instructions, so it
  is neither a load nor unparsed. `scan --report` shows that count with 5
  samples, so misses are visible rather than silent.

### 4.6 Accuracy caveats

These are printed by `skima usage --help` and in the README.

1. Claude history from before the first scan is lost, except typed `/name`
   commands recovered from `history.jsonl`. The coverage footer says so.
2. A load is not an execution.
3. `read` relies on a heuristic.
   - Unusual readers (`awk`, `python -c`) are missed, and show up in the
     unparsed counter.
   - Reads for review outside the authoring rules are over-counted.
   - Collapsing per root session limits the error to one per session.
4. Transcript formats are undocumented, apart from Pi's
   `docs/session-format.md`.
   - Each event records `agent_version`: Claude's record `version`, Codex's
     `cli_version`, and null for Pi, whose header `version` is the file format.
   - `scan --report` warns about any file that contains a skill marker
     (`Base directory for this skill`, `<skill>`, `<skill name=`) and yields no
     event. That warning is the canary for a broken parser.

### 4.7 The prompt behind a load

F15's real examples and F18's session selection need the user-authored text
that caused a strong event. Each adapter has `prompt_at(src) -> Option<String>`,
evaluated at display time against the transcript, never stored (D11):
- **`slash`:** the arguments the user typed with the command, prefixed by the
  invocation (`/name`, `$name`, `/skill:name`). A bare invocation with no
  arguments takes the `tool` rule instead, because the request is then in the
  message before it.
- **`tool`** (a channel only Claude has, §4.1, and the fallback for a bare
  invocation on any agent): the most recent user-authored message before the
  load in the same session file.
- **`typed`:** the `display` text of the `history.jsonl` line.
- Injected skill bodies, `<system-reminder>` blocks and pasted-content blocks
  are stripped before the 300-character cut, and so are Claude's
  `<task-notification>` and `<local-command-stdout>` blocks, which the harness
  writes as user records (509 task notifications on 2026-10-14). A message
  with nothing left after stripping is not user-authored.

A user-authored message is, per agent: on Claude, a `user` record that is
neither a `tool_result` nor `isMeta`, whose content is a string or holds
`text` blocks (an attached image does not disqualify it), with a
`<command-name>` record read as the command and its `<command-args>`; on
Codex, an `event_msg` of type `user_message`, or from 2026-08 an
`item_completed` event whose item is a `UserMessage` (rollouts from 2026-10
have no `user_message`); on Pi, a `message` entry with role `user`, a
`/skill:` expansion read as the invocation and the text after `</skill>`.
Skill injections, which Codex and Pi also record as user turns, are excluded
by the same markers §4.3 and §4.4 detect. The walk back starts at the load's
own record, since a Pi expansion carries its own arguments.

Each agent's rule is pinned by a fixture taken from a real session (§10.1).
`None` (an expired transcript, or no user turn found) skips the example.

**Audit runs.** Every session whose cwd is inside `<data dir>/audit-runs/` is
skipped at ingest: its records produce no event (D13).

## 5. Sync, drift and policy

### 5.1 Source of truth

The source is the `source` directory of the config (§6.1); the author's is
`~/dotfiles/skills/`. A skill is any non-hidden child directory that contains
`SKILL.md`. Hidden entries (`.grades`, `.retros`, `.dev`, `.archive`) are never
installed. The `loose` files, the author's `SKILLS_USAGE.md` and
`GOAL_SKILLS_USAGE.md`, are linked next to the skills.

### 5.2 Drift states

Each pair of a home and an entry, canonical or not, gets exactly one state:

| State | Meaning | Severity | `sync` action |
|---|---|---|---|
| `ok` | An absolute link to `<source>/<name>` | n/a | none |
| `missing` | A canonical skill with no entry | error | link |
| `dangling` | An owned link whose target is gone | error | remove |
| `wrong-link` | A link with a canonical name whose `readlink` text is not exactly `<source>/<name>`; a relative link that resolves to the source still counts | error | abort |
| `obstructed` | A real file or directory with a canonical name | error | abort |
| `foreign` | A non-canonical entry (`synced/`, `.system/`, the `.codex` marker) | info | none |

Three inputs are not entries and get no state:
- For each directory skima links into (`<home>/skills`, and the parent of each
  `links` key, such as `<home>/extensions`), the directory itself or, if it
  does not exist, its first existing ancestor must be a directory. A regular
  file or a dangling symlink there is a conflict, and `sync` aborts
  (`install.sh:178-184, 239, 244`, `test_install.py:232`).
- A `links` source that does not exist is an error before any write, for every
  `--agent` that selects that home (`install.sh:234`,
  `test_install.py:228`).
- A `loose` file missing from the source is skipped, as `install.sh:225-231`
  skips it.

The same states apply to the files a home lists in `links`, such as Pi's
`extensions/pi-explicit-skills.ts`. `sync` prunes owned dangling links there
too, as `skills/install.sh:255` does. A home directory or `links` directory that
does not exist is created by `sync`, as `install.sh:247,253` does; `status`
reports each of its entries as `missing`.

Three further checks were considered and left out, because nothing on this
machine needs them:
- **Copy hashing.** There are no copies, so an obstruction is reported by its
  path alone.
- **A `shadowed` state.** The only entry in a second discovery root is
  `~/.agents/skills/find-skills`, which is not a canonical name.
- **An `orphan-override` check.** Both settings files hold about 30
  `skillOverrides` keys for built-in and bundled Claude skills that exist nowhere
  on disk (E9). skima cannot tell those apart from stale keys.

### 5.3 Fan-in graph

`inventory.rs` builds a static citation graph from the body of each `SKILL.md`
and every `.md` file under its `references/` directory. A skill never cites
itself: self-edges are dropped. The frontmatter is left out. Descriptions name
neighbouring skills to mark boundaries, such as "(that is blast-radius)" at
`interrogate/SKILL.md:12`, and those mentions are not dependencies.

Before matching, the matcher removes backticks and collapses every run of
whitespace, newlines included, into one space. This is needed because a
citation can break across lines: `review-audit-fixes/SKILL.md:80-81` reads
"owned by the" / "`blast-radius` skill". After that, A → B is an edge when A's
body contains `the B skill`, `owned by B`, or `B/` as a path component.

F3 and F17's `retire` use the graph only through "held", which is non-static: a citation
protects B only while A is itself used (F3). On 2026-10-12 the rules produce
these edges:
- Into `blast-radius`, seven of them (E4).
- Into `goal-smart`, eight of them, from the goal suite and
  `writing-for-agents`.
- Into `writing-for-agents`, twenty of them, through the
  `../writing-for-agents/references/pi-runtime.md` pointer.

16 of the 29 skills have at least one incoming edge (E4). The tests pin each
rule on the synthetic `tests/fixtures/skills` tree, so editing a real skill
cannot break them (§10.1).

### 5.4 Policy

**YAML.** Frontmatter and `agents/openai.yaml` are parsed with `saphyr` (YAML
1.2 core schema), plus one check over `saphyr-parser` events: a duplicate key
in any mapping is an error. Version 1's `strict_yaml.rs` reproduced the PyYAML
loader of the deleted `install.sh` in 984 lines plus a 664-line differential
test; with `install.sh` gone that fidelity serves nothing, and it is deleted
(F13). The strictness that matters is kept where a value is used:
- `allow_implicit_invocation` must be the plain scalar `false`. A quoted
  `"false"`, `no`, `False` or a tag is an error.
- `description` must be a string scalar.
- `date` in grades and retros is read as a string and must match
  `YYYY-MM-DD` at its start (§6.3).

**Always-on rules**, from the Agent Skills format, run on every `sync` and
`status`, whatever `--agent` says:
- `name` equals the directory name, matches `[a-z0-9]+(-[a-z0-9]+)*`, and is at
  most 64 characters.
- `description` is non-empty after trimming and at most 1024 characters,
  counted in Unicode code points.
- The frontmatter is the text between a first line that is exactly `---` and
  the next line that is exactly `---`, after dropping a leading BOM; lines
  split on `\n` and `\r\n`. A missing closing line is an error.
- **Any home with `links`:** every entry resolves to its source file. This
  rule applies to the homes `--agent` selects.

**Configurable rules**, each a boolean in `[policy]` (§6.1), off by default:

| Key | Scope | Rule |
|---|---|---|
| `no_disable_model_invocation` | every skill | the frontmatter has no `disable-model-invocation` key |
| `codex_manifest` | every skill | `agents/openai.yaml` exists and sets `policy.allow_implicit_invocation: false` |
| `claude_name_only` | Claude homes selected by `--agent` | `<home>/settings.json` exists and maps every canonical skill to `"name-only"` in `skillOverrides`; a missing file is an error |

Every-skill rules run whatever `--agent` says, as the always-on rules do. With
`claude_name_only` off, a Claude home needs no `settings.json` at all. When a
setting is missing, skima prints the exact JSON line to add and leaves the
file alone.

**Scan freshness**, a warning rather than a rule, printed by `status` and by
every usage report. For each Claude home, skima warns that transcripts will
expire unscanned when:
- the home has no `coverage.json` entry (it has never been scanned), or
- `now - last_scan` is more than the home's retention minus 7 days, compared
  in seconds.

A home's first scan always creates its `coverage.json` entry, with `first`
null when it has no transcripts yet.

Retention is `cleanupPeriodDays` from `<home>/settings.json`, or 30 when it is
unset; other Claude settings layers are not read. With a retention of 7 days
or less the warning is always on, which is correct: no scan schedule is
assumed. A usage report evaluates the rule after its own scan. A scan
advances a home's `last_scan` only when it read every transcript of that home
without an I/O error, so after a clean scan the warning stays off unless
retention is 7 days or less. Codex and Pi do not clean up
rollouts, so they get no warning.

### 5.5 Conflicts

`sync` never overwrites, never backs up and has no `--force`. It prints every
conflicting path with the `mv` or `rm` that would resolve it, then exits 1
without writing anything.

Error lines go to stderr and start with `ERROR: `. Where `install.sh` had a
message, skima keeps its wording, because the ported tests match on it:

| Case | Message |
|---|---|
| A path conflicts with a canonical entry | `ERROR: <path> conflicts with a canonical entry; preserved (resolve manually)`, then the `mv`/`rm` hint |
| A home or `links` directory, or its first existing ancestor, is not a directory | `ERROR: <path> is not a directory` |
| A Claude home has no `settings.json`, with `claude_name_only` on | `ERROR: <home path>: no Claude settings profile` |
| A settings file fails a check | `ERROR: <settings path>: <reason>`, with the missing `"name": "name-only"` lines when that is the reason |
| A skill fails a per-skill rule | `ERROR: <skill dir>: <reason>`, naming the file (`SKILL.md` or `agents/openai.yaml`) |
| A `links` source is missing | `ERROR: <source path> is missing` |
| `PI_CODING_AGENT_DIR` is invalid | `ERROR: invalid PI_CODING_AGENT_DIR: <reason>` |
| No canonical skill in the source | `ERROR: no canonical skills found in <source>` |

`links::plan()` is a pure function that returns either a list of actions or a
list of conflicts, and `links::apply()` is the only writer. That makes "all checks
before any write" a property of the code's structure, not something a reviewer
has to remember.

### 5.6 Pi directory normalisation

`install.sh` calls Node to normalise `PI_CODING_AGENT_DIR` (`install.sh:40-57`).
`config.rs` reproduces those steps exactly, in this order:
1. An unset or empty value means `~/.pi/agent`, as Node's `||` does.
2. `~` alone is `$HOME`; `~/x` is the string `$HOME` + `/` + `x`, concatenated
   before any path handling. `path.resolve` in step 5 then removes the empty
   segment and the `..`, so `~//../a` is `$HOME/../a`, resolved lexically. No other `~` form and no `$VAR` is
   expanded.
3. A value starting `file://` is parsed with `url::Url`, which follows the
   WHATWG URL rules that Node's `fileURLToPath` uses: surrounding whitespace is
   trimmed, tabs and newlines are removed, `\` becomes `/`, dot segments are
   resolved by the URL parser (so `file:///a/empty//../agent` is
   `/a/empty/agent`), and the query and fragment are dropped. Then, as Node
   does after parsing:
   - a host other than empty or `localhost` (in any case) is an error;
   - an encoded slash, `%2F` or `%2f`, in the path is an error;
   - the path is percent-decoded strictly: a malformed `%` escape or bytes that
     are not valid UTF-8 are an error.
4. A NUL anywhere in the result is an error.
5. The skills directory is `path.resolve(value, "skills")`: a lexical join
   against the cwd with `.` and `..` removed, no symlinks followed.

Every error aborts before any write, with the message `ERROR: invalid
PI_CODING_AGENT_DIR: <reason>` (§5.5). This matches how Pi itself resolves the
variable, so it stays after `install.sh`; the ported cases in
`tests/sync_port.rs` are its acceptance tests.

### 5.7 Migration from `install.sh` (done)

Done on 2026-10-13. `skima sync` linked the one missing entry
(`claude-work/skills/break-invariants`, E1), and `status` exited 0. Before the
switch, a differential run of the old `install.sh` and `skima sync` in
identical temporary homes produced identical trees in all 6 scenarios. The 18
cases of `test_install.py` live on as `tests/sync_port.rs`, and
`test_pi_integration.py` calls `skima sync --agent pi`. `install.sh` and
`test_install.py` were deleted in dotfiles, and every reference was updated.
The dotfiles bootstrap installs skima with `cargo install` and runs
`skima sync`; once the repository is on GitHub (F13), that step installs from
it.

## 6. Data model and storage

### 6.1 Configuration

skima reads `$SKIMA_CONFIG`, else `$XDG_CONFIG_HOME/skima/config.toml`
(`~/.config/skima/config.toml` when the variable is unset, on Linux and macOS
alike), with the `toml` crate. `skima init` writes it (F11). The config path is
resolved physically (realpath) before `source` is resolved against its
directory, so a config symlinked from a skills repo finds that repo. The
author's `~/.config/skima/config.toml` is a symlink to
`~/dotfiles/skills/skima.toml`, tracked with the skills because the homes are
part of the collection's definition. The author's file:

```toml
source = "."                      # relative to the physical config file, resolved physically
loose  = ["SKILLS_USAGE.md", "GOAL_SKILLS_USAGE.md"]

[policy]                          # every key is optional and off by default (§5.4)
no_disable_model_invocation = true
codex_manifest              = true
claude_name_only            = true
authoring_skills            = ["grade", "retro", "writing-for-agents", "skill-creator"]

[judge]                           # optional (§6.4)
agent = "claude"                  # claude (default) | codex | pi
model = "sonnet"                  # optional; unset means the agent's own default

[[home]]
name  = "claude"
agent = "claude"
path  = "~/.claude"

[[home]]
name  = "claude-work"
agent = "claude"
path  = "~/.claude-work"

[[home]]
name  = "codex"
agent = "codex"
path  = "~/.codex"

[[home]]
name  = "pi"
agent = "pi"
path  = "~/.pi/agent"
links = { "extensions/pi-explicit-skills.ts" = "pi-explicit-skills.ts" }
```

- `name` labels the home in the coverage footer, in `coverage.json`, and in
  `status` output.
- Every path goes through one expansion: `~`, and the Pi normalisation
  (§5.6). For the Pi home, a set `PI_CODING_AGENT_DIR` overrides `path`, as it
  does for Pi itself.
- `links` lists extra files to link into the home. The keys are relative to the
  home, the values relative to `source`.
- An unknown key anywhere is an error, so a typo in a policy name never
  silently turns a rule off.

Each adapter derives everything else from `path`:
- **Claude:** `skills/`, `settings.json`, `projects/` and `history.jsonl`.
- **Codex:** `skills/` and `sessions/`.
- **Pi:** `skills/`, and `sessions/`, unless `PI_CODING_AGENT_SESSION_DIR` is
  set.

Adding a home therefore turns on linking, policy checks and ingest together. E1
happened because these lists were kept separately; this layout makes that
impossible.

### 6.2 State

State lives in `${XDG_DATA_HOME:-~/.local/share}/skima/` on Linux and macOS
alike. It is data, not cache: once Claude cleans up a transcript, the ledger
and the audits hold the only copy of what it showed. It is kept out of git,
because event `cwd` values and audit text can describe client work under NDA,
and a skills repo is often pushed to GitHub. Because the
ledger cannot be rebuilt, the directory belongs in the user's existing
home-directory backup.

```
events.jsonl     the ledger
cursors.json     {"<abs path>": {"dev": …, "ino": …, "offset": …, "line": …}}
coverage.json    {"<home name>": {"first": "<ts>", "last_scan": "<ts>"}}
scan.lock        flock target, also held by audit while it appends
audits.jsonl     audit records (§6.4)
report.html      the default output of skima report (F16)
audit-runs/      one fresh empty subdirectory per judge call; sessions here are never ingested (§4.7)
codex-judge-home/  the Codex judge's CODEX_HOME: only a symlink to the user's auth.json (§6.4)
```

Every path above follows `XDG_DATA_HOME`, so a test or a comparison run
points a binary at its own copy by setting that variable.

**Event schema v1.** One JSON object per line, with keys in this order:

```json
{"v":1,"id":"cc:toolu_01EKQ5…","ts":"2026-09-15T06:15:31.455Z",
 "agent":"claude","home":"claude","agent_version":"2.1.272",
 "session":"2e112b1e-…","actor":"main","skill":"unslop","origin":"canonical",
 "channel":"tool","path":"/home/nicolas/.claude/skills/unslop/SKILL.md",
 "cwd":"/home/nicolas/projects/argus","authoring":false,
 "src":"/home/nicolas/.claude/projects/-home-nicolas-projects-argus/2e112b1e-….jsonl:412"}
```

- `actor`, `origin`, `session` (the root) and the cwd part of `authoring` are
  derived at ingest. Their inputs, such as the chain headers, the entrypoint and
  the cwd, sit in transcripts that may later be deleted. The other authoring
  condition is computed at report time from the ledger (§4.1).
- No prompt text, command arguments or skill bodies are ever stored.

**Coverage.** `first` is the earliest record timestamp in any transcript that a
home's first scan read. It never moves. `last_scan` is when the latest scan
finished.

**Locking.** Every scan takes `scan.lock` with a blocking `flock`, and loads the
id set only after it holds the lock. Two concurrent invocations therefore run
one after the other.

**Append rule.** A scan appends an event in exactly two cases:
- its id is not in the ledger;
- its id is in the ledger, the stored event has `path: null`, and the new event
  has a path (§4.2).

Every other event with a known id is dropped. Readers keep the last line for
each id. A rescan with no cursors therefore appends nothing. The Skill result
comes before its meta record in the file, so the Claude adapter merges the
pair within one parse and emits one event with the path. The merged event
takes `ts` and `src` from the Skill result, and its `skill` and `origin`
from the result's `commandName`; the meta record only supplies `path`. An
upgrade line therefore changes nothing a fresh merge would have set
differently. When a scan stops between the two,
the next scan's upgrade line copies the stored event and sets only `path`, so
both ways end with the same event. A meta record with no Skill result
(subagents, E5) takes its own `ts` and `src`. Only a scan that
stops between the two costs one extra line, once.

**Cursors:**
- A scan resumes from `offset` when `dev` and `ino` match and the file's
  current size is at least `offset`.
  Otherwise it rescans the file from 0, and the append rule absorbs the overlap.
- The offset only advances past a `\n`, so a session still being written is
  never half read. `line` is the number of lines before `offset`, so `src` can
  name a line number without recounting the prefix.
- **Scan order.** Within one scan, files are processed in order of their
  basename. Codex (`rollout-<start time>-…`) and Pi (`<start time>_<id>`)
  basenames begin with the session's start time, and a parent always starts
  before its forks, so the parent's copy of a shared record is stored first. A
  later incremental scan also meets the parent first, because a fork copies
  only records that already existed. The stored event, including its actor and
  `src`, is therefore the same in a fresh and an incremental scan. Claude forks
  are the known exception: they copy records under a new `sessionId`, keep the
  `uuid`, record no parent, and have random basenames. On 2026-10-12 no copied
  Claude record carried a skill event; the §10.2 reconciliation exposes it if
  that changes.
- A resumed parse always re-reads line 1 first. The Codex `session_meta` and
  the Pi session header carry the root session, actor inputs, cwd and the
  guardian filter.
- Before JSON parsing, each line passes a byte prefilter for `Base directory for
  this skill`, `"Skill"`, `"commandName"`, `<skill>`, `<skill name=`, `SKILL.md`
  or `/skills/`, plus the source root's path and each configured home's skills
  path, which catch a relative `references/` read run from inside the source
  root when the line itself carries the cwd (Claude records, Codex calls with
  `workdir`). Pi tool calls and Codex calls without `workdir` carry no cwd on
  the line, so such relative `ref` reads are missed; §10.2 measures how many. A Skill result line carries only `"commandName"` among these.
  The prefilter skips more than 99% of lines.

**Reparse.** `scan --reparse` is the only command that rewrites the ledger. It
applies a changed parser rule, whether the rule removes events or adds them:
1. It takes `scan.lock` and writes `events.jsonl.new`.
2. It copies verbatim every `typed` event and every event whose `src` is not
   among the files step 3 parses: the transcript is gone, or it is no longer
   under a configured home.
3. It parses every transcript on disk from offset 0, in scan order, and
   appends the result under the append rule, as a fresh scan would. A rule
   that removes false positives therefore removes them, and a rule that finds
   missed loads adds them, including in transcripts that had no events.
4. It `fsync`s the new file, renames it over `events.jsonl`, and rewrites
   `cursors.json` to the offset each parse reached, which stops before a
   half-written last line as in any scan.

**Why JSONL and not SQLite.** The volume is about 300 to 2,000 events a month.
Loading the whole file takes milliseconds, the file stays greppable with `jq`
and `rg`, and there are no schema migrations. Revisit above 10⁶ events.

### 6.3 Evaluation artifacts

skima reads these at report time and never copies or writes them. It reads only
`<source>/.grades/<skill>/*.md` and `<source>/.retros/<skill>/*.md`. It skips
top-level files such as `.grades/GRADES.md`, and any file without YAML
frontmatter.

- **Order.** Frontmatter is read with the §5.4 parser. A file it rejects is
  skipped with a warning. `date` is read as a string, quoted or not, and its
  first 10 characters must be `YYYY-MM-DD`. Grades and retros are listed
  newest first by that string, with ties broken by filename. Every one is
  shown (F14), not only the latest.
- **Grades.** skima reads the frontmatter `target`, `date` and `score`. Let `G`
  be the last commit that touched the grade file
  (`git log -1 --format=%H -- <grade file>`). The grade is stale when
  `git rev-list G..HEAD -- <skill>` is non-empty, that is, when a later commit
  changed the skill. Its length is the `+N` of F2. An edit committed together with the grade does not count.
  An uncommitted grade is not stale. Uncommitted changes to the skill
  (`git status --porcelain -- <skill>` non-empty) make any grade stale, because
  the text being used is not the text that was graded; with no later commit
  this shows as `+wip`.
- **Cost.** Version 1 ran two or three `git` processes per skill per report.
  Version 2 runs one `git log --name-only --format=%H%x00%cI%x00%s` over the
  source and one `git status --porcelain` per report, and derives first commit,
  last commit, `+N` and `+wip` for every skill from them.
- **Not a repository.** When the source is not in a git worktree, history,
  `+N` and `new` (F3) are omitted, and grades are never marked stale.
- **`baseline_commit`** is deliberately unused. Only some grades carry it, and
  the current `grade` template no longer writes it (E9).
- **Retros.** skima reads the frontmatter `skill`, `date`, `score` and
  `verdict`.

### 6.4 Audit

`skima audit <skill>` judges how a skill performs in real sessions, and turns
that, with everything else skima knows about the skill, into one report. It
runs in two stages so that cost grows with new use, not with history, and so
that a session's verdict survives its transcript.

**Stage 1: one record per session.**
1. **Selection.** The candidates are root sessions with a strong event for the
   skill, actor `main` or `headless`, whose transcript is still on disk, and
   with no `session` record for this skill in `audits.jsonl`. Weak-only
   sessions are skipped; they are mostly authoring (§4.1). `headless` is
   included because worker agents started with `claude -p` or `codex exec`
   run skills for real (an `issue-swarm` worker, for example); `probe` is not.
   Candidates are
   ordered newest first and taken round-robin across projects (cwd), up to
   `--limit` (default 20). `--all` removes the limit.
2. **Condensing.** The window starts at the prompt behind the session's first
   strong load of the skill (§4.7), in the transcript holding that load. It
   ends at the earlier of the first strong load of a different skill, the 40th
   user-authored message (§4.7) after the start, or the end of the file. It keeps user and assistant
   text, each tool call's name with its first 200 characters of input, and the
   first 500 characters of each tool result. Injected skill bodies are
   dropped, because the judge gets the skill separately. Every kept record is
   prefixed with its line number (`L412:`) so the judge can cite it. Beyond
   60,000 characters (`max_chars`), skima keeps the first two thirds and the
   last third (40,000 and 20,000) and marks the cut.
3. **The skill as it was.** The judge gets `SKILL.md` from the last commit
   before the session started (`git show <commit>:<path>`), or the current
   file when there is no repository or no earlier commit.
4. **Judging.** One judge call per session, with the prompt shipped in the
   binary (`prompts/session.md`). The judge returns JSON with:
   - `asked`: what the user wanted, paraphrased, at most 240 characters;
   - `followed`: `yes`, `partly` or `no`;
   - `outcome`: `achieved`, `partial`, `failed`, `abandoned` or `unclear`;
   - `redone`: whether the user had to redo or correct the work;
   - `strengths` and `frictions`: short findings, each citing a line number;
   - `notes`: at most 400 characters.
5. **Validation.** skima checks the JSON against the schema, that every cited
   line is in the condensed text, and that no text field quotes the session
   (D11): no 40 consecutive characters shared with a user message, an
   assistant message or a tool result, and no whole user message of 12 to 39
   characters. Both sides are compared after lowercasing and collapsing
   whitespace. A record
   that fails is retried once, then dropped and counted as an error. It is
   never stored half-valid.

**Stage 2: one record per skill.** Runs after stage 1 when any new session
record was written, or with `--resynth`. Its input is:
- the current `SKILL.md`, and the skill's last 30 commits (date and subject);
- every grade's frontmatter, and the latest grade's body cut to 6,000
  characters;
- every retro's frontmatter, and the bodies of the latest three, cut to 4,000
  characters each;
- the card's usage section (F14);
- every `session` record for the skill, from all audits.

The judge (`prompts/skill.md`) returns:
- `verdict`, at most 400 characters;
- `patterns`: how the skill is actually used;
- `works` and `failures`, each citing session ids;
- `improvements`, ranked, each a concrete change with its reason and the
  sessions behind it;
- `versus_grades`: where real use agrees or disagrees with the grades and
  retros;
- `examples`: at most three, each a situation, what was asked (paraphrased),
  what happened and a tip, citing one session.

skima, not the judge, computes the rates from the session records:
`followed: yes`, `outcome: achieved`, and `abandoned or redone`, each as
`n of N`. Cited session ids must exist, or the record is retried and then
dropped.

**The judge.** `[judge] agent` picks the command. Each call runs in a fresh,
empty subdirectory of `<data dir>/audit-runs/`, which is not a git repository,
removed after the call. skima writes the
shipped prompt and the input to files in a fresh temporary directory, and
passes them as the table says. Every flag below exists today (E16):

| Agent | Command | Structured output | Spend |
|---|---|---|---|
| `claude` | `claude -p --safe-mode --tools "" --system-prompt "$(cat prompt)" --output-format json --json-schema <schema> --no-session-persistence --max-budget-usd <cap>`, input on stdin | enforced by `--json-schema` | `total_cost_usd` per call |
| `codex` | `CODEX_HOME=<data dir>/codex-judge-home codex exec --ephemeral --skip-git-repo-check --sandbox read-only --output-schema <file> -o <result file> --json -`, with prompt and input concatenated on stdin | enforced by `--output-schema`; read from `-o` | not reported; token totals from `--json` |
| `pi` | `pi -p --no-session --no-tools --no-extensions --no-skills --no-context-files --system-prompt "$(cat prompt)" --mode json @<input file>` | the final message, parsed and validated | not reported; token totals printed |

The Claude judge uses `--safe-mode`, which turns off `CLAUDE.md`, skills,
plugins, hooks, MCP servers and custom agents while authentication works
normally, so a subscription login keeps working. `--bare` is not used: it
reads only `ANTHROPIC_API_KEY`, never the OAuth login (E16). Safe mode keeps
built-in tools, so `--tools ""` turns them off. Together they keep each call
cheap and stop the judge from touching files. Codex has
no flag to disable tools; its read-only sandbox and the empty working
directory stand in. Codex also has no flag to skip the user's `AGENTS.md` or
skill list, so the Codex judge runs with `CODEX_HOME` set to a scratch home
holding only a symlink to the user's `auth.json`. M9 proves the isolation by
one real call per agent, with a sentinel file telling the model to put the
word `PELICAN` in every text field, placed where that agent would otherwise
read user instructions:
- **Claude:** a `CLAUDE.md` in the call's working directory; `--safe-mode` must
  suppress it.
- **Pi:** an `AGENTS.md` and a `CLAUDE.md` in the working directory;
  `--no-context-files` must suppress them.
- **Codex:** no flag suppresses an `AGENTS.md` in the working directory, so the
  guarantee there is construction: every call's directory is created empty, and
  the test asserts it still is when the call starts. The sentinel goes in
  `<decoy HOME>/.codex/AGENTS.md`, with `HOME` pointed at the decoy, which
  proves the scratch `CODEX_HOME` is what Codex reads.

The result must not contain `PELICAN`. Nothing is written into the user's real
agent homes. The Claude call must also return
structured output with `--tools ""` set. An agent that fails is not offered as
a judge until it passes; `[judge] agent` naming it is a config error. `[judge] model` is unset by
default, which means the agent's own default; when set, it is passed as
`--model` (`-m` for Codex). The skipped working directory (§4.7) covers any
agent that writes a session anyway.

**Spend.** Calls run one at a time, with a progress line (F12). Let `B` be
`--max-budget-usd` (default 1) and `spent` the sum of reported
`total_cost_usd`, both kept as integer micro-dollars. A reported cost is read
from its decimal text with `serde_json::value::RawValue` (the `raw_value`
feature, not `arbitrary_precision`, which §10.5 forbids) and truncated to
micro-dollars, never parsed through a float. Each stage-1 call gets
`--max-budget-usd 0.75·B − spent`, and stage 1 starts no call once that is
under $0.01. Stage 2 then runs on every
session record that exists, old and new, with `--max-budget-usd B − spent`.
Caps are passed rounded down to cents. When `B − spent` is under $0.01, stage
2 is skipped, the output names the skip, and the run exits 1. Claude checks a
cap between model turns, and a judge call with no tools is one turn, so the
cap that binds is skima refusing to start a call; a call can exceed its own
cap by that one turn. Records already written are kept, and the output names
any session left for the next run. `--dry-run` estimates input tokens as
condensed characters divided by 4, and, once earlier audits exist, a dollar
figure from their recorded cost per input character. Codex and Pi report no dollar figure, so with
those judges `--limit` is the cap, and passing `--max-budget-usd` is a usage
error (exit 2).

**Records.** `audits.jsonl` is append-only, written under `scan.lock`:

```json
{"v":1,"kind":"session","skill":"unslop","session":"2e112b1e-…","agent":"claude",
 "src":"…/2e112b1e-….jsonl:412","session_ts":"2026-09-15T06:15:31Z",
 "audited_at":"2026-10-15T10:02:11Z","skill_commit":"a1b2c3d",
 "judge":{"agent":"claude","model":"sonnet","cost_usd":0.031},
 "prompt":"session-v1","result":{…}}
{"v":1,"kind":"skill","skill":"unslop","audited_at":"…","sessions":["2e112b1e-…",…],
 "judge":{…},"prompt":"skill-v1","rates":{"followed":[17,20],"achieved":[15,20],
 "abandoned_or_redone":[3,20]},"result":{…}}
```

The latest `skill` record is the one F14 and F16 show. Older ones stay, so a
skill's trend is visible across audits. `prompt` names the shipped prompt
version, so a changed prompt can be told apart from a changed skill.

## 7. Interface

```
skima scan    [--reparse] [--report] [--json]
skima usage   [--since 30d|all] [--agent A] [--channel C] [--all] [--authoring] [--json]
skima unused  [--since 60d] [--json]
skima show    <skill> [--since 30d] [--limit 20] [--json]
skima status  [--agent A] [--json]
skima sync    [--agent A] [--dry-run]
skima init    --source <dir>                                          # F11
skima report  [--since 30d] [--out <path>] [--open]                   # F16
skima new     <name> --description <text>                             # F17
skima retire  <name> [--force]                                        # F17
skima prune   [<name>] [--yes] [--force]                              # F17
skima audit   <skill> [--limit 20 | --all] [--max-budget-usd 1] [--resynth] [--dry-run] [--json]   # F18
```

- `usage`, `unused`, `show`, `report`, `audit` and `retire` scan first. Tests call the `report`
  functions of the library crate directly on a fixture ledger, so no flag exists to skip the scan.
- `--all` adds `other` origins, `probe` actors, and rows for skills no longer in the inventory (retired or pruned). `--authoring` adds authoring
  reads.
- `scan` prints one summary line, the number of new events for each home.
  `--report` adds the per-channel counts, the unparsed samples and the parser
  canary.
- Exit codes: 0 for success or clean, 1 for drift, a conflict, a policy
  failure, a refused `retire` or `prune`, an audit that had candidates and
  dropped every record (no candidates is exit 0), or an audit whose stage 2
  was skipped for budget; 2
  for usage errors, including a missing config (F11).
- Output is aligned text, bold only when stdout is a TTY. The sparkline uses
  `▁▂▃▄▅▆▇█`, one cell per day, and `·` for a day with no event. Progress goes
  to stderr (F12).

Example `usage` layout (the numbers are illustrative, not measured):

```
skill                   sess slash tool typed read ref agents last    30d                             grade
validate-resolve-issue    70    67    0     0   25   3 X      2d ago  ··▃▇▅▂···▃▅▇▅▂·····▃▅▇▅▂····▃▅  8.9
unslop                   120     1   75     0  140   0 CXP    today   ▅▇▇▅▇▅▇▇▅▇▅▇▇▅▇▅▇▇▅▇▅▇▇▅▇▅▇▇▅▇  9.1 +7
goal-migrate               0     0    0     0    0   0        -       ······························  8.4 +wip
coverage: claude since 2026-09-10 (typed since 2026-02-24) · claude-work since … · codex since 2026-03-29 · pi since 2026-08-29
legend: agents C Claude, X Codex, P Pi · · no events that day · +N commits since the grade · +wip uncommitted changes since the grade
```

## 8. Tech stack

**Rust, edition 2024, built with cargo 1.98.1.** One binary, installed with
`cargo install --locked --git <repository>` (F13), or `--path` from a clone.

| Crate | Use |
|---|---|
| `clap` 4 (derive) | the CLI (§7) |
| `serde`, `serde_json` (`float_roundtrip`, `raw_value`) | transcript records, the ledger, cursors, `--json` output, exact judge costs (§6.4) |
| `toml` | the config (§6.1) |
| `saphyr`, `saphyr-parser` | YAML 1.2 loading, and the event stream for the duplicate-key check (§5.4) |
| `regex` | the reader grammar, fan-in patterns and record prefixes |
| `sha1_smol` | Codex event ids (§4.3) |
| `jiff` | timestamps, day buckets and windows |
| `url` | WHATWG parsing of `file://` values (§5.6) |

- `std::fs::File::lock` (stable since Rust 1.89) gives the blocking `flock`
  on `scan.lock`. There is no locking crate.
- `git` is run as a subprocess for known names, first commits and grade
  staleness. There is no `git2`.
- Tests are `cargo test`. Every module lives in the library crate (`lib.rs`)
  and `main.rs` only parses arguments, so integration tests in `tests/` can
  call `report`, `ledger` and the adapters directly, and can also run the
  built binary against temporary homes. A test that needs a changed parser
  rule (§10.4 reparse) calls the reparse function with a test adapter. The only
  dev-dependency is `tempfile`.
- The first scan stays bounded at 90 s (F1). Rust is not chosen for speed: a
  Python prototype already met that bound. It is chosen for a single
  installed binary with no interpreter or package to manage (it links only
  the system `libc` and `libgcc_s`), and for the type checking of the event model.
- The HTML report is built with `format!` and one escaping function, and the
  progress line is a `\r`-rewritten stderr line. Neither needs a crate. The
  audit calls agents as subprocesses; there is no HTTP client.

## 9. Architecture

```
skima/
  Cargo.toml  Cargo.lock  LICENSE  .github/workflows/ci.yml   (Linux and macOS)
  prompts/
    session.md      stage 1 judge prompt (§6.4), compiled in with include_str!
    skill.md        stage 2 judge prompt
  src/
    lib.rs          the library crate: every module below except main.rs
    main.rs         clap dispatch, exit codes
    config.rs       config discovery, [policy], [judge]; path expansion and Pi normalisation; init
    yaml.rs         saphyr loading, the duplicate-key check, frontmatter extraction (§5.4)
    inventory.rs    canonical skills, frontmatter, known names, fan-in graph, one-pass git history
    policy.rs       always-on and configurable rules (§5.4), scan-freshness warning
    links.rs        drift classification; plan() (pure) and apply() (the only link writer)
    lifecycle.rs    new, retire, prune (F17)
    scan.rs         the scan driver: scan order, cursors, coverage, typed backfill, reparse, progress
    tail.rs         incremental JSONL reader: cursors, partial-line safety, byte prefilter
    ledger.rs       Event, append rule, lock, coverage, reparse
    adapters/
      mod.rs        the Adapter trait, below
      claude.rs  codex.rs  pi.rs
    readcmd.rs      shared reader grammar (§4.5)
    classify.rs     origin, actor, root session, authoring tag, audit-run skip
    evals.rs        .grades/.retros frontmatter, every grade, +N and +wip
    report.rs       usage, unused, the card (show), the report model; collapse, held, cluster
    examples.rs     written and real examples (F15)
    audit.rs        selection, judge calls, validation, rates, audits.jsonl (§6.4)
    render.rs       aligned tables, sparklines, legend
    html.rs         the self-contained HTML report (F16)
  tests/
    fixtures/{claude,codex,pi}/   one trimmed real record per positive and negative case
    fixtures/skills/   a synthetic skills tree, the default skills directory under test
    adapters_{claude,codex,pi}.rs  ingest.rs  reports.rs  sync_port.rs  sync.rs
    portability.rs  lifecycle.rs  html.rs  audit.rs
```

The code splits into four paths:
- **Write:** `tail → adapters → classify → ledger`.
- **Read:** `ledger + inventory + evals + audits → report → render | html | json`.
- **Sync:** `config + inventory + policy → links`, and `lifecycle` on top.
- **Judge:** `ledger + adapters (condense) → audit → audits.jsonl`.

The adapter interface, documented in the README for contributors:

```rust
pub trait Adapter {
    /// Transcript files under one home, in scan order (§6.2).
    fn files(&self, home: &Home) -> Vec<PathBuf>;
    /// Events from the lines of `chunk`; `header` is line 1 of the file.
    fn parse(&self, roots: &SkillRoots, path: &Path, header: &str, chunk: Chunk) -> Parsed;
    /// The user-authored text behind the load at `src` (§4.7).
    fn prompt_at(&self, src: &Locator) -> Option<String>;
    /// The judge's view of the session window starting at `src` (§6.4).
    fn condense(&self, src: &Locator, max_chars: usize) -> Option<Condensed>;
}
```

Adapters only turn records into events, prompts and condensed sessions.
Identity, actor, root session and authoring are computed once, in
`classify.rs`. Adding an agent means one adapter module and one `Agent`
variant; the README documents the trait for contributors.

## 10. Verification

A milestone is done when its runs pass and their output is pasted into the
milestone's commit or PR. That is rung 4 of Evidence Ladder v1. Reading the code
does not count.

### 10.1 Fixture tests

Each case below is one fixture: a real record from this machine, with its text
scrubbed. Each test asserts the exact event list, or that no event is produced.

**Claude:**
- meta record with `sourceToolUseID`
- slash pair
- successful Skill result with no meta record
- subagent meta record whose Skill result has no `toolUseResult`: one `tool` event
- a scan boundary between a Skill result and its meta record: one event, which
  gains its path on the second scan; the reduced ledger equals a fresh scan's
- `disable-model-invocation` refusal and `PreToolUse:Skill` hook refusal: no event
- `/clear`: no event
- subagent started with a `/name` prompt, `Base directory` in block 1: `slash`
- scan resumed between a `<command-name>` record and its meta record: one `slash` event
- `<command-name>` inside a `tool_result`: no event
- `skill_mention` and `invoked_skills` attachments: no event
- subagent file: actor `subagent`, root session of the parent
- `entrypoint: probe` and `entrypoint: bench`: actor `probe`
- authoring `Read` in a dotfiles session
- `Bash` reading two skill files in one command: two events with distinct ids
- first scan with two `history.jsonl` lines, one for a session with a
  transcript on disk (no event) and one without (`typed`). Then a second scan
  after another history line is appended: no event.
- `history.jsonl` lines `/argus:triage …` (no event) and `/argus` after the
  skill's removal (`typed`, origin `other`)

**Codex:**
- `$name` block with `payload.id: null` from a current-CLI rollout
- `exec` wrapper making two `exec_command` reads: two events, distinct ids
- subagent rollout: loads root to the parent session
- forked rollout with the parent's `session_meta` copied in as line 2: the line-1 header wins
- fresh scan and incremental scan of a fork pair: identical stored events, actor and `src` from the parent
- real fork pair, whose child re-stamps the copied records: the copies collapse to one id
- split `sed -n` read: one collapsed load
- relative read (`sed -n '1,260p' SKILL.md` with `workdir` inside a skill): one `read` event
- reader-first pipeline (`sed -n '1,200p' f | nl`): one event; `| wc -l`: no event
- `rg` over skills: no event
- guardian-review session: no event
- path echoed in a `*_output` record: no event

**Pi:**
- `/skill:` block
- `<skill name="...">` placeholder inside a `toolResult`: no event
- `read` tool call
- noisy `bash` loop
- entry duplicated across fork files: one event
- child session: actor `subagent`, root session of the parent
- run-state JSON file in a sessions directory: skipped

**Fan-in:** the exact edge set of the synthetic `tests/fixtures/skills` tree,
which holds one skill per §5.3 rule: a `the B skill` citation, the same
citation broken across lines, an `owned by B` citation, a `B/` path component,
a citation in a `references/` file, a description mention that is not an edge,
and a self-mention that is not an edge. With `SKIMA_TEST_SKILLS` set, a second
test checks a real repository for invariants only: no self-edges, and every
edge target is a skill.

**Held:** in a fixture ledger, the goal suite is unused and `writing-for-agents`
is held by one used skill. `unused` lists the goal skills with a `cluster` note
and leaves `writing-for-agents` out.

**Evals:**
- a grade without frontmatter, and `.grades/GRADES.md`: both skipped
- two grades for one skill: the later `date` wins
- a grade committed together with a skill edit: not stale
- a later skill commit: stale
- a quoted and an unquoted `date`: compared correctly
- three grades for one skill: all three listed, newest first

**Prompt behind a load** (§4.7), one real fixture each: a Claude `slash` with
arguments, a Claude `tool` load after a user message and a `tool_result`, a
Codex `$name` block, a Pi `/skill:` block, and a `typed` history line. Each
asserts the exact text returned, with injected bodies stripped.

**Audit runs:** a Claude, a Codex and a Pi session whose cwd is inside
`<data dir>/audit-runs/`: no event.

### 10.2 Reconciliation on the real corpus

Two counter scripts are written by two different agents, each working from §4
alone without reading skima's parsers. Both run read-only on the full corpus.
`skima scan` then runs into a temporary `XDG_DATA_HOME`. For each agent and
channel, skima's counts must equal both scripts' counts after the documented
dedupe rules. Any mismatch prints the differing `src` lines and has to be
explained before M0 closes. Baselines are recomputed at run time, because
Claude's cleanup changes them daily (E5).

### 10.3 Precision

Two validator agents work separately. Each samples 20 strong events, 20 `read`
events, and 20 lines that pass the prefilter but produced no event, and judges
each against its source record. The two then reconcile.

- Strong-channel precision must be at least 95%.
- Read-channel precision is printed in the README, whatever it is.
- The same pass counts how many read events the `authoring_skills` rule
  changes. That count decides whether the rule stays (§4.1).

### 10.4 Ingest properties

- Scan twice: +0 lines.
- Delete `cursors.json` and rescan: +0 lines.
- Append one record: +1 event, read from the stored offset.
- Write a half line, scan, then finish the line: exactly one event.
- Replace the file with a new inode: +0 duplicate lines.
- Delete the transcript, then run `scan` and `scan --reparse`: no events lost.
- Change a rule so one fixture event becomes a false positive, then run
  `scan --reparse`: that event is gone, and events from deleted transcripts are
  byte-identical.
- Change a rule so a fixture transcript with no events yields one, then run
  `scan --reparse`: the event appears.
- Two concurrent `scan` processes: zero duplicate lines written.

### 10.5 Sync parity

**Permanent tests**, kept in skima's suite:
- The 18 `test_install.py` cases, ported to `tests/sync_port.rs`, driving
  `skima sync` against a temporary `HOME` (§5.7).
- **YAML rules** (§5.4): a duplicate key at any depth, `allow_implicit_invocation`
  spelled `"false"`, `no`, `False` or `!!bool false`, and a non-string
  `description` are each rejected; a plain `false` and quoted and unquoted
  `date` values load. Every skill's files load, read from
  `SKIMA_TEST_SKILLS`, or from `tests/fixtures/skills` when it is unset.
- **Golden Codex id.** The pinned value is computed once by an independent
  Python script (`json.dumps(sort_keys=True, separators=(",", ":"),
  ensure_ascii=False)`), not by skima. One fixed payload with nested, unsorted and non-ASCII
  keys, a float and an integer above 2^53 must hash to a pinned `cx:` id.
  `serde_json` is used with `float_roundtrip` on and no `preserve_order` or
  `arbitrary_precision`. Without `float_roundtrip`, serde's fast float parser
  can be one ULP off (a real Codex payload carries `create_time:
  1791311493.9632053`), so the re-serialised payload and its id would depend
  on that approximation instead of the value. The pin exists so a change in serialisation (for
  example, a dependency turning on `serde_json/preserve_order`) fails a test
  instead of silently duplicating every Codex event.
- A conflict in the last home leaves zero writes in every home, checked with
  tree snapshots.
- `~/.claude-work`, a home the old installer lacked, gets linked and
  policy-checked.
- `sync --agent pi` runs the per-skill rules and skips the Claude settings.
- `last_scan` 24 days old with `cleanupPeriodDays` unset: `status` warns and exits 0; 22 days old: no warning.
- The fixture config turns every `[policy]` rule on, so the ported cases keep
  testing the author's rules; `tests/portability.rs` covers them off (§10.8).

`test_pi_integration.py`, pointed at `skima sync`, stays in dotfiles' own suite
(§5.7).

The one-shot checks against `install.sh` ran once in M3 and were deleted with
it (§5.7).

### 10.6 Live end-to-end runs

These are opt-in with `SKIMA_LIVE=1`. The Claude and Codex runs cost one cheap
model turn each.

**Setup.** A test helper builds everything directly, without `skima sync`, so
the runs can gate M1 before sync exists. It creates:
- a temporary `HOME`, with `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and
  `PI_CODING_AGENT_DIR` pointing at temporary directories;
- the credentials, as symlinks to exactly two files:
  `~/.claude/.credentials.json` and `~/.codex/auth.json`;
- a temporary source root holding one skill, `skima-probe-<random>`. Its body
  tells the model to reply `PROBE-OK`, and its `agents/openai.yaml` sets
  `allow_implicit_invocation: false`;
- a temporary `skima.toml` listing the three temporary homes, with the probe
  linked into each home's `skills/`;
- a temporary Claude `settings.json` that maps the probe to `"name-only"`.

**Gating runs.** These are deterministic: the harness performs the expansion
whatever the model does next.

| Run | Expected event |
|---|---|
| `claude -p "/skima-probe-… ok" --model haiku` | `slash`, actor `headless` |
| `codex exec '$skima-probe-… ok'` | `slash`, actor `headless` |
| Pi in `--mode rpc`, with session persistence on, sending the prompt `/skill:skima-probe-…`. The faux provider is registered by an extension, as `PROBE_EXTENSION` does in `test_pi_integration.py:36-55` | `slash`, actor `main` |

`skima scan --report --json` must report exactly these 3 events, with the
expected agent, channel, actor and `src`. A second scan must report 0.

**Model-dependent runs.** These are recorded but do not gate. Each runs up to 3
times. A run where the model never called the tool, or never ran the read, is
reported as `skipped`, not `failed`. A run where the model did act but skima
emitted no event, or the wrong one, fails.

| Run | Expected event |
|---|---|
| `claude -p "Call the Skill tool with skill skima-probe-…, then stop." --model haiku` | `tool`, actor `headless` |
| `codex exec 'Run: cat <path>/SKILL.md'` | `read`, actor `headless` |

### 10.7 Real machine

Version 1, passed on 2026-10-13:
1. `skima status` reported `claude-work: missing break-invariants` and no
   other error.
2. `skima sync`, then `skima status`, exited 0.
3. `skima sync --dry-run` printed nothing.

Version 2, with the author's config (every `[policy]` rule on), run at the
end of every milestone from M5 on:
1. `skima status` exits 0 with the same set of findings as version 1: none at
   error level and the three `foreign` entries. From M6 on, they print in the
   F12 wording: four `in sync` lines and the three `foreign` lines.
2. The rules are on: a scratch config with the author's `[policy]`, a copy of
   the skills and scratch homes runs `sync`, then `status`, which exits 0.
   Then each violation alone (`allow_implicit_invocation: true` in one
   manifest, a `disable-model-invocation` key in one `SKILL.md`, one skill
   missing from a scratch `skillOverrides`) makes `status` exit 1 with that
   rule's error line.
3. `skima sync --dry-run` writes nothing; from M6 on it prints only the
   `in sync` line.
4. Counts do not change. At the start of M5, the version 1 binary is built
   from the `v1` tag (placed on the last version-1 commit when M5 starts) and kept with a version-1-shaped copy of the config (no
   `[policy]`). At each milestone end, the data directory is copied twice;
   version 1 and version 2 run `usage --all --json` back to back, each on its
   own copy, and every `sess` and channel count must be equal on every row
   both emit (version 2 adds rows for retired and pruned skills). When their
   scans appended different numbers of events, a transcript changed between
   the runs, and the pair is rerun.

### 10.8 Version 2 tests

**Portability** (`tests/portability.rs`):
- A fresh `HOME` with one plain skill and an empty `~/.claude/settings.json`:
  `init --source`, then `sync`, exits 0 and links the skill (F11). Without a
  config, `usage` exits 2 and names `skima init`.
- Each `[policy]` rule, turned on alone, rejects its own violation and nothing
  else; all off, the same tree passes.
- An unknown key under `[policy]` is an error.
- A config symlinked from another directory resolves `source` against the
  physical file.
- CI runs the whole suite on Linux and macOS.

**Lifecycle** (`tests/lifecycle.rs`):
- `new` with every rule on writes files that pass the per-skill rules; after
  the test adds the printed `name-only` line to each Claude home and runs
  `sync`, `status` exits 0. With every rule off, `new` writes only `SKILL.md`,
  and `sync` then `status` exits 0. `new` without `--description` exits 2.
- `retire` refuses a held skill and a skill used in the last 30 days, and
  `--force` overrides both; after it, `status` exits 0 and the links are gone.
- `prune` without a TTY and without `--yes` exits 2 and deletes nothing.
  `prune <name> --yes` deletes one archived skill. In a git fixture it refuses
  an archived skill with uncommitted changes, and `--force` overrides that.
  The ledger is byte-identical before and after.

**Output** (`tests/reports.rs`, `tests/html.rs`):
- `+N` and `+wip` on fixture repositories with known commit histories.
- `last` is `-` for a skill whose only event is older than the window.
- The progress decision is a pure function of "stderr is a TTY" and elapsed
  time, unit-tested at both edges; an integration test checks that a scan with
  stderr redirected to a file writes no progress. M6 records one run under
  `script(1)` showing the line on a cold scan.
- The legend line ends `usage` and `unused`; `show` prints `~` for `$HOME` and
  `<agent>:<file>:<line>` locators, with full paths in `--json`;
  `scan --reparse` prints `(was <m>)` for each home.
- `status` prints `in sync` for a home whose only findings are `foreign`, with
  those lines after it.
- The HTML report holds every skill in the JSON, has no `src` or `href`
  attribute pointing outside the file, and is written under the data
  directory by default with mode `0600`.
- Real examples: actor `subagent` and `probe` events never appear; the three
  examples come from three different projects; an expired transcript yields no
  example; text over 300 characters is cut; a bare `/name` with a one-word
  previous message is skipped for an older event in the same project.

**Audit** (`tests/audit.rs`). A stub `claude` on `PATH` returns canned JSON
and records its arguments, so no test spends money:
- Selection takes at most `--limit` sessions, round-robin across projects, and
  skips sessions already audited and weak-only sessions.
- The stub receives `--safe-mode`, `--tools ""`, never `--bare`,
  `--no-session-persistence`, `--json-schema`, and `--max-budget-usd` of
  `0.75·B − spent` in stage 1. With 12 candidate sessions, a stub that reports
  $0.10 per call and a $1 budget, stage 1 passes exactly the caps 0.75, 0.65,
  0.55, 0.45, 0.35, 0.25, 0.15 and 0.05, judges 8 sessions, and stage 2 runs
  with a cap of 0.20. With 30 candidate sessions, `--all` and a stub that
  reports $0.031, stage 1 makes exactly 24 calls, the last cap is 0.03, no cap
  is 0.00, and stage 2 runs with 0.25. With a stub that reports $0.50, stage 1
  passes 0.75 and 0.25, spends $1.00, stage 2 is skipped, and the run exits 1. With a stub that reports $0.30
  per call, stage 1 judges 3 sessions, spends $0.90, and stage 2 runs with
  $0.10.
- An invalid record (a cited line outside the condensed text, an unknown
  session id, 40 characters copied from a user message or a tool result) is
  retried once and then dropped.
- Rates are computed from session records, not taken from the judge.
- D13: the stub writes a Claude transcript with a Skill load into a project
  directory whose cwd is under `audit-runs/`; a scan after the audit appends
  0 events.
- `--dry-run` never starts the stub. The judge never writes inside the source:
  a tree snapshot of the fixture source is equal before and after an audit.

**Audit precision**, a live check run once on the real corpus in M9, opt-in
and capped at $3: `skima audit --limit 10 --max-budget-usd 1.5` runs on
`unslop` and `grade`, each picked because it has more than 10 candidate
sessions (checked with `--dry-run` first; another skill with enough
candidates replaces one that has fewer). Two
validator agents work separately, each judging the same 10 session records,
5 from each skill, against their transcripts, then reconcile. `followed` and
`outcome` must match the validators' verdict in at least 8 of 10 records, and
at most 2 of the 20 candidate sessions may have been dropped by validation.
The result goes in the README, as §10.3's precision does.

## 11. Milestones

Version 1 (M0–M3) shipped on 2026-10-13, with the §10.6 live runs still
pending. Version 2 starts with portability, because the `[policy]` table and
the config shape are what `new`, `init` and the tests build on. The card comes
before the audit, because the audit's report is shown inside the card.

| M | Scope | Done when |
|---|---|---|
| M0–M3 | Version 1: ingest, reports, status, sync, the `install.sh` migration | Done 2026-10-13 |
| M5 | F11 `init` and config discovery, F13 `[policy]`, `authoring_skills`, `yaml.rs` replacing `strict_yaml.rs`, README newcomer path, CI, private repository; on the author's machine, add `[policy]` to `skills/skima.toml` and symlink it to `~/.config/skima/config.toml` | §10.8 portability passes; §10.7 version 2 steps 1 to 4 pass with the author's config (step 1 without the F12 wording); the first push is confirmed with the owner |
| M6 | F12 progress, legend, `+N`/`+wip`, `last` within the window, plain `status` and `sync` lines, short paths, the reparse summary | §10.8 output checks (except HTML and examples) pass; §10.7 version 2 steps 1 to 4 pass, wording included |
| M7 | F14 card, F15 examples with `prompt_at`, F16 HTML report, the one-pass git history | §10.1 `prompt_at` fixtures and §10.8 HTML and examples checks pass; the report renders the author's 29 skills |
| M8 | F17 `new`, `retire`, `prune` | §10.8 lifecycle passes |
| M9 | F18 audit, then F19's change to the `grade` skill | §10.8 audit and audit precision pass |
| Later | F9 leak report | Its acceptance criteria are met |

## 12. Evidence and open questions

### 12.1 Facts the design rests on

All were observed read-only on this machine on 2026-10-12. Counts that
Claude's cleanup changes daily are given as the range seen across several
independent runs that day.

| # | Fact | How observed |
|---|---|---|
| E1 | `~/.claude-work/skills` lacks `break-invariants`; `install.sh:33` never targets that directory | `diff <(ls ~/.claude/skills) <(ls ~/.claude-work/skills)` prints `< break-invariants` |
| E2 | Claude cleanup is active; transcripts reach back only to 2026-09-10 and moved from 2026-09-06 during planning; `cleanupPeriodDays` is unset | `~/.claude/.last-cleanup` = `2026-10-12T05:43:31.135Z`; the oldest `~/.claude/projects/**/*.jsonl` mtime |
| E3 | `~/.claude/history.jsonl` has 6,443 lines starting 2026-02-24 | `grep -c .`; the first line's `timestamp` is `1771891828339` |
| E4 | Seven skills cite `blast-radius` as "owned by the `blast-radius` skill" or "owned by `blast-radius`", never by path: `interrogate:52`, `diagnose-bug:41`, `break-invariants:40`, `review-audit-fixes:80-81`, `validate-resolve-issue:114`, `triage-finding:22`, `validate-findings-backlog:72` | `rg -n 'blast-radius' --glob '*/SKILL.md'`; a run of a reimplementation of the §5.3 matcher over the repo gives the edge counts in §5.3 |
| E5 | The successful Claude Skill results (102–114) slightly outnumber the meta records with `sourceToolUseID` (99–108); 12 meta records sit in subagent files whose Skill result has no `toolUseResult` | `rg` over `~/.claude/projects`, several runs |
| E6 | 6,111 Codex rollouts from 2026-03-29, with no cleanup. Of 167 `$name` blocks, 131 name a `~/.codex/skills` symlink path, 26 the resolved dotfiles path, and 10 `/tmp` probe or `.system` skills. 141 of the 167 have `payload.id: null` | `ls`, `rg` over `~/.codex/sessions` |
| E7 | Pi forks copy entries across files (entry `dfea0f8f` appears in 6 files). The raw `<skill name=` regex is mostly noise: 132 hits against 4 real | `rg` over `~/.pi/agent/sessions` |
| E8 | `install.sh` is 256 lines; `--pi` still runs the per-skill checks (lines 150–158) and skips only `CHECK_CLAUDE`; `skills/tests/` holds `test_install.py` and `test_pi_integration.py` | `wc -l`, reading `install.sh:15-30,150-158` |
| E9 | Both settings files hold 59 `skillOverrides` keys, 30 of them for skills not on disk. `baseline_commit` appears in 28 grade files and not in the `grade` template | `jq`, `rg -l baseline_commit .grades`, `grade/SKILL.md` |
| E10 | Claude `entrypoint` values include `bench` (192 records), `probe` (150) and `probe-cli` (45), besides `cli` and `sdk-cli` | `rg` over `~/.claude/projects` |
| E11 | `~/.codex/config.toml:8` sets `include_instructions = false`; `pi-explicit-skills.ts` strips skills from Pi prompts | reading both files |
| E12 | 420 of 469 Pi session headers carry `parentSession`, whose value is the parent's file path | `head -1` over the session files |
| E13 | `~/.cursor`, `~/.windsurf`, `~/.kiro` and `~/.config/amp` do not exist; `~/.gemini` holds only `settings.json` | `ls` |
| E14 | Codex forks re-stamp copied records but keep their payloads: of 186 `function_call` records copied from a parent (matched by `call_id`), 0 keep the parent's timestamp and 186 have an identical payload; the same holds for all 14 copied `$name` blocks | a script comparing each forked rollout with its parent over `~/.codex/sessions` |
| E15 | `~/.claude/history.jsonl` holds 105 bare `/argus` lines and 13 `/argus:<cmd>` plugin lines; `skills/argus` existed in git only from 2026-03-13 to 2026-04-01. The skills repo's history also holds `SKILL.md` paths nested under `claude/`, `codex/` and `shared/` | `jq -r .display` with `grep -oE '^/argus[^ ]*'`; `git log --name-only -- skills` |
| E16 | Each agent has a headless flag that writes no session file: `claude --no-session-persistence`, `codex exec --ephemeral`, `pi --no-session`. Claude also has `--json-schema`, `--max-budget-usd`, `--tools ""`, `--system-prompt` and `--safe-mode` (customizations off, auth unchanged), and its `--bare` reads only `ANTHROPIC_API_KEY`, never OAuth; `codex exec` has `--output-schema`, `-o`, `--json`, `--sandbox`, `--skip-git-repo-check` and `--ignore-user-config`, and no system-prompt flag; `pi` takes input as `@file`, and has `--mode json`, `--system-prompt`, `--no-tools`, `--no-extensions`, `--no-skills` and `--no-context-files`, and no schema or budget flag | `--help` of each CLI on 2026-10-14 |
| E17 | All 29 skills carry `interface.default_prompt` in `agents/openai.yaml`; 4 have an examples section in `SKILL.md` (`## Examples` or `## Worked example`) | `grep` over `~/dotfiles/skills` on 2026-10-14 |
| E18 | Version 1 audit: four independent auditors scored 6, 5, 6 and 5 of 10. All four found no progress output on a 10 s cold scan, no description or history in `show`, and the hardcoded policy; three reproduced `sync` refusing a plain skill | the four audit reports of 2026-10-14 |

### 12.2 Open questions

Each will be settled by a run, not by reading.

- **Q1. Codex discovery path.** Does current Codex still discover
  `~/.codex/skills`, or only `~/.agents/skills`? The §10.6 `codex exec` run
  answers it. If the answer is `~/.agents/skills`, the Codex adapter's derived
  skills directory changes, and the config does not.
- **Q2. Work-profile rule file.** Does the work profile need its own `CLAUDE.md`
  for the `AGENTS.md` rule to apply? This is outside skima, which does not
  report it.
- **Q3. Cross-vendor judging.** Settled 2026-10-14: allowed. The owner has no
  client work whose sessions must stay with one vendor, so an audit judges
  every candidate session with the configured judge, whatever agent recorded
  it, and there is no flag to restrict it. `--dry-run` prints each session's
  agent and the judge's.
