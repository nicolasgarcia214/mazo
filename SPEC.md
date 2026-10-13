# skima: specification

skima is a personal skill manager for one person's coding agents. It answers
three questions from files already on disk:

1. Which of my skills actually load, on which agent, through which channel, and when?
2. Which skills are dead weight?
3. Does every agent home see exactly the canonical skill set, with the invocation policy intact?

It is inspired by [MoltenBase](https://www.moltenbase.com/), which offers usage
tracking, cross-agent sync, team policy and local-first storage. skima keeps
usage tracking, sync and local-first storage, and drops teams. It adds what one
person with a git-tracked skills repo needs: a durable usage ledger, provenance
for every number, and a join with the existing `grade` and `retro` skills.

This spec merges six independent plans written on 2026-10-12, each based on
read-only inspection of this machine. §12 lists every fact the design rests
on, numbered E1–E15, and how each was observed. Other sections cite those numbers instead of
repeating them.

---

## 1. Decisions

| # | Decision | Why |
|---|---|---|
| D1 | **Transcripts are the only telemetry source. skima installs no hooks.** | Claude Code, Codex and Pi each write a structured record when a skill loads (§4). Transcripts cover every load channel, subagents included, and give months of history on the first run. A Claude `PreToolUse:Skill` hook would see one channel on one agent, starting the day it was installed, and would be a second event stream to deduplicate. |
| D2 | **skima keeps its own append-only ledger.** | Claude Code deletes transcripts after `cleanupPeriodDays`, which defaults to 30 and is unset here (E2). skima does not ask for longer retention. A transcript only has to survive until the next scan, and the ledger is the record that survives any retention setting and any future change to the harness's cleanup. |
| D3 | **Every usage report scans first (`usage`, `unused`, `show`, `runs`). No timer, no daemon.** | A usage report is never stale. `status` and `sync` read links and settings, not transcripts, so they do not scan. Data is lost only if no scan runs for a whole retention window. `status` and every usage report warn before that happens (§5.4). |
| D4 | **skima absorbs `skills/install.sh` and deletes it in the same change.** | Two tools managing the same links would drift. The hard-coded target list in `install.sh` already caused a real bug (E1). The user's rules forbid keeping a legacy path beside its replacement. |
| D5 | **Symlinks only. No copy mode, no frontmatter transforms.** | All three hosts read the same `SKILL.md` and follow symlinks. Per-host differences already live in configuration: `skillOverrides` for Claude, `agents/openai.yaml` for Codex, `pi-explicit-skills.ts` for Pi. skima validates those and generates nothing. |
| D6 | **Rust, one binary, with clap. CLI only. JSONL storage, no SQLite.** | §8 gives the reasons. |
| D7 | **Every count traces to a `file:line` in a transcript.** | This follows the user's rule that every claim carries its evidence. `skima show` prints the locators, and `retro` can open them. |
| D8 | **"Unused" is reported with its coverage window.** | "Unused since 2026-09-10 on Claude and since 2026-03-29 on Codex" is a different claim from "never used". Reports say which one they mean. |

## 2. Scope

**Kept from MoltenBase:**
- invocation counts, last-used timestamps and the never-called report
- a 30-day trend
- a retire action
- a single source of truth with per-agent targets
- drift detection
- plain local files

**Dropped:**
- Team policy, accounts, cloud sync and sharing. Multi-machine sync is git's job.
- A web dashboard or TUI. About 30 skills fit one terminal table, and the other
  consumers are agents (`retro`, `grade`) that want `--json`.
- Copy-based sync and per-agent transforms (D5).
- Adapters for agents that are not installed (E13). An adapter is written only
  against real transcripts it can be tested on.

**Added:**
- the durable ledger (D2), plus a one-time backfill of typed `/name` commands
  from `~/.claude/history.jsonl`, which goes back to 2026-02-24 (E3)
- channel and actor on every event (§4.1)
- provenance per event (D7)
- coverage windows (D8)
- reference awareness, so a hub skill that live skills depend on is never
  offered for retirement (§5.3)
- a join with `.grades/` and `.retros/` frontmatter (§6.3)

**Non-goals:**
- Writing harness settings files. skima prints the edits they need, as
  `install.sh` does today.
- Judging skill quality, or whether a loaded skill was executed well. `grade`
  and `retro` own that; skima counts loads.
- Storing prompt text, command arguments or skill bodies.
- Managing project-scope skills (`<repo>/.claude/skills`).
- Windows, macOS and network access.

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
  - `last`: the time of the last strong event
  - `30d`: a sparkline of strong events per day
  - `grade`: the latest grade score, with `*` if the skill changed after that
    grade (§6.3)
- Strong and weak channels (§4.1) get separate columns and are never summed.
- A footer prints the coverage window for each home.
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
- On today's machine, it reports exactly one error:
  `claude-work: missing break-invariants` (E1).

**F6. `skima sync`: replaces `install.sh`.**
- Builds the whole plan, policy checks included, before the first write. Any
  conflict or policy failure aborts with zero writes in every home.
- A second run prints nothing.
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

### Later

Each of these is built only when a real need shows up.

**F7. `skima retire <skill>`.**
- Runs `git mv <skill> .archive/<skill>`. Hidden entries are never installed.
- Then runs `sync`, which prunes the now-dangling links.
- Prints the `skillOverrides` keys and the `SKILLS_USAGE.md` lines to remove.
- Refuses when the skill is `held`, when it has a strong event in the last 60
  days, or when its path has uncommitted changes. `--force` overrides the first
  two refusals, never the third.
- Never commits.

**F8. `skima runs <skill> --json`.** Emits transcript locators for the `retro`
skill's cross-session mode. The pointer to it is added to `retro/SKILL.md`
through the user's normal skill-editing flow, not by skima.

**F9. Policy-leak report.** Classifies each `tool` or `read` load by its cause.
`requested` means the user's prompt for that turn names the skill. `chain` means
a skill loaded earlier in the session names it. `rule` means `AGENTS.md` names
it. `unprompted` means none of these, which is a leak in the name-only policy.
This would replace the hand-run probes behind the README line "loaded 10 of 14
unprompted runs". It is deferred because the classification is lexical: a
paraphrased request lands in `unprompted`. It ships only after its precision is
measured on real sessions.

**F10. Adapters for other agents** (Gemini CLI, Cursor, Amp, Windsurf, Kiro).
Each is built against fixtures taken from a real session on this machine.

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
| `ref` | weak | The model read another file inside the skill directory, such as `references/*.md`. |

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
- the session's cwd is inside the dotfiles worktree that contains the source
  root, or
- its root session holds a strong event for `grade`, `retro`,
  `writing-for-agents` or `skill-creator`, anywhere in the ledger.

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

**Homes:** `~/.claude`, and `~/.claude-work`, which the `cw` alias selects with
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
- `cc:<assistant uuid>:<content index>:<file index>` for reads.

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
- After that, `history.jsonl` is never read again. Every later session is
  recorded from its own transcript, well inside retention.

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
  `<call index>.<file index>`: an `exec` wrapper can call
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
  event.
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

## 5. Sync, drift and policy

### 5.1 Source of truth

The source is `~/dotfiles/skills/`. A skill is any non-hidden child directory
that contains `SKILL.md`, which is the rule `install.sh` uses. Hidden entries
(`.grades`, `.retros`, `.dev`, `.archive`) are never installed. The loose files
`SKILLS_USAGE.md` and `GOAL_SKILLS_USAGE.md` are linked next to the skills.

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

F3 and F7 use the graph only through "held", which is non-static: a citation
protects B only while A is itself used (F3). On 2026-10-12 the rules produce
these edges:
- Into `blast-radius`, seven of them (E4).
- Into `goal-smart`, eight of them, from the goal suite and
  `writing-for-agents`.
- Into `writing-for-agents`, twenty of them, through the
  `../writing-for-agents/references/pi-runtime.md` pointer.

16 of the 29 skills have at least one incoming edge (E4). A fixture test snapshots the
full edge set of the real repo, so a change in the matcher or in the skills
shows up as a diff.

### 5.4 Policy

`validate_policy` from `install.sh` moves into `policy.rs`. Its strict
`PolicyLoader` (`install.sh:106-132`) is rewritten as `strict_yaml.rs`, a small
loader over `saphyr-parser` events that keeps the same contract:
- A plain scalar resolves as PyYAML's `SafeLoader` resolves it (null, int,
  float, timestamp, string), except that only the exact plain scalars `true`
  and `false` become booleans. `yes`, `on`, `True` and the like stay strings.
- An explicit `!!bool` tag on anything other than `true` or `false` is an
  error.
- A duplicate key, or a merge key `<<`, is an error. Keys are compared after
  resolution, as Python compares them: `1` and `1.0` collide, `true` and `1`
  collide, `~` and `null` collide.
- A plain `=` (the YAML 1.1 value tag) is an error, as are unknown tags and
  more than one document.
- A quoted scalar without an explicit tag is a string. (`!!bool "true"` is a
  boolean, as in PyYAML.)
- Everything else follows PyYAML's `SafeLoader`, and the differential test is
  the arbiter. Its corpus includes at least: anchors and aliases (accepted),
  non-scalar keys (rejected), `!!binary`, `!!set` and `!!omap` (accepted), a
  tab inside a plain scalar (`a: b\tc`, rejected), two `.nan` keys
  (duplicates), an integer above 2^64 (accepted), and `a: <<` (rejected).

A permanent differential test runs both loaders over the same corpus of inputs
(§10.5), keeping a verbatim copy of the Python loader as the reference. The rules come in two scopes.

**Per-skill rules** run on every `sync` and `status`, whatever `--agent` says:
- `name` equals the directory name, matches `[a-z0-9]+(-[a-z0-9]+)*`, and is at
  most 64 characters.
- `description` is a string, non-empty after trimming, and at most 1024
  characters. Characters are Unicode code points (`chars().count()`), as in
  Python's `len()`, not bytes.
- The frontmatter is extracted as `install.sh:141-145` does: a leading BOM is
  dropped, the text is split into lines on every separator Python's
  `splitlines()` accepts (`\n`, `\r\n`, `\r`, `\v`, `\f`, `\x1c`–`\x1e`,
  `\x85`, U+2028, U+2029), line 1 must be exactly `---`, and the frontmatter
  ends at the next line that is exactly `---`. A missing closing line is an
  error. The YAML text is the lines between them joined with `\n`.
- The frontmatter has no `disable-model-invocation` key.
- `agents/openai.yaml` sets `policy.allow_implicit_invocation: false`.

**Per-home rules** apply to the homes selected by `--agent`:
- **Claude:** `<home>/settings.json` exists. A missing file is an error and
  `sync` aborts before any write. This is stricter than `install.sh`, which
  skips a missing profile and fails only when none exists (`install.sh:87-92`).
- **Claude:** `<home>/settings.json` maps every canonical skill to `"name-only"`
  in `skillOverrides`.
- **Any home with `links`:** every entry resolves to its source file.

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

The rules are code, not configuration: there is one user and one policy. When a
setting is missing, skima prints the exact JSON line to add and leaves the file
alone.

### 5.5 Conflicts

skima never overwrites, never backs up and has no `--force`. It prints every
conflicting path with the `mv` or `rm` that would resolve it, then exits 1
without writing anything.

Error lines go to stderr and start with `ERROR: `. Where `install.sh` had a
message, skima keeps its wording, because the ported tests match on it:

| Case | Message |
|---|---|
| A path conflicts with a canonical entry | `ERROR: <path> conflicts with a canonical entry; preserved (resolve manually)`, then the `mv`/`rm` hint |
| A home or `links` directory, or its first existing ancestor, is not a directory | `ERROR: <path> is not a directory` |
| A Claude home has no `settings.json` | `ERROR: <home path>: no Claude settings profile` |
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
PI_CODING_AGENT_DIR: <reason>` (§5.5). The cases in `test_install.py:294-359` are the
acceptance tests, and a one-shot parity check (§10.5) runs the Node snippet and
`config.rs` over the same inputs before the snippet is deleted along with
`install.sh`. Those inputs include every case above.

### 5.7 Migration from `install.sh`

The migration is one dotfiles commit, made after F6's tests pass:
1. **Prove parity.** Run the one-shot checks in §10.5 and paste their output into
   the commit message.
2. **Delete** `skills/install.sh`.
3. **Port the tests.** Each of the 18 cases in `skills/tests/test_install.py`
   becomes a Rust integration test in `tests/sync_port.rs` that runs the built
   `skima` binary; then the Python file is deleted. The port follows four
   rules:
   - **Fixture.** Each test writes a `skima.toml` next to the fixture skills
     and points `SKIMA_CONFIG` at it. It lists four homes with `~`-relative
     paths, so a subtest that changes `HOME` moves them too: `claude`
     (`~/.claude`), `claude-work` (`~/.claude-work`), `codex` (`~/.codex`) and
     `pi` (`~/.pi/agent`, overridden by `PI_CODING_AGENT_DIR` as today, with the
     `extensions/pi-explicit-skills.ts` entry in `links`).
   - Valid `settings.json` files are written into both Claude homes before
     each run, including in every subtest of
     `test_conflict_in_any_harness_aborts_all_destinations` (`:268-284`), so a
     run fails for the reason the test names and not for missing settings.
     That port also asserts the `conflicts with a canonical entry` needle. The
     one exception is the case that deletes a profile to provoke
     `no Claude settings profile` (`:125`). `add_skill` updates
     `skillOverrides` in both homes. Tests that break a settings file break
     `<home>/settings.json`, so the needle `settings.work.json` (`:118`)
     becomes `.claude-work/settings.json`.
   - The skill list (`SKILLS`, `PI_SKILLS`) is read at test time from the
     skills directory named by `SKIMA_TEST_SKILLS`, defaulting to
     `~/dotfiles/skills`, as the Python test reads its own parent (`:12-16`).
   - **Arguments.** `--pi` becomes `--agent pi`. No arguments stays plain
     `sync`. `--unknown` and `--pi extra` become `--unknown` and
     `--agent pi extra`; clap's error output contains `Usage:` (`:291`).
   - **"Nothing was written"** is asserted as equality of a snapshot of `$HOME`
     and the Pi directory (every path, file type, link text and the SHA-1 of
     every regular file) taken before and after the run. This replaces the assertions that a directory is empty
     or absent, because the fixture's settings files now live inside `$HOME`:
     `:81-82` (for `--agent pi`, the `.claude` and `.codex` homes are
     unchanged), `:107`, `:247` and `:283-284`. Assertions on the Pi directory
     not existing (`:63`, `:108`, `:246`, `:292`, `:321`) are kept,
     because the fixture creates nothing there.
   - **Alias.** `test_installing_through_a_directory_alias_still_links_canonical_source`
     (`:130-139`) points `SKIMA_CONFIG` at `<alias>/skima.toml`, where the
     alias is a symlink to the fixture skills directory, and runs from a cwd
     outside both. Links must still name the physical source, so an
     implementation that skips the realpath of `source` fails it.
   - **Output.** A run with nothing to do prints nothing, so `"done.\n"`
     (`:86`, `:265`) becomes `""`. skima's error lines keep the wording the
     tests look for (§5.5), so every other `stderr` needle stays as it is.
   - `test_pi_integration.py` stays in dotfiles. Its calls to
     `install.sh --pi` (`test_pi_integration.py:123, 188, 241, 258`) become
     `skima sync --agent pi`; plain `sync` would also touch the real or
     temporary Claude and Codex homes. It imports `PI_SKILLS` and `SKILLS` from
     `test_install` today (`:18`), so those two constants move into it. It sets
     `HOME` to a temporary directory (`:186`), so it also sets `SKIMA_CONFIG`
     to the real `skills/skima.toml`.
4. **Update every reference** to the old installer: `skills/README.md` (the
   install commands, "Verification", and step 4 of "Adding a skill"),
   `skills/GOAL_SKILLS_USAGE.md:46`, `skills/tests/test_pi_integration.py`, and
   `dotfiles/README.md:54`. Done when both commands exit 1:
   - `rg -n 'install\.sh' ~/dotfiles/skills -g '!.grades' -g '!.retros'`
   - `rg -n 'skills/install\.sh' ~/dotfiles/README.md`

   The search does not catch prose. These mentions of the installer and its
   Python, PyYAML and Node requirements are rewritten by hand in the same
   commit: `skills/README.md:11, 20, 45, 61, 72, 129, 138`,
   `GOAL_SKILLS_USAGE.md:49`, `SKILLS_USAGE.md:36`,
   `writing-for-agents/SKILL.md:30, 270` and
   `writing-for-agents/references/skill-mechanics.md:50, 56`.
5. **Bootstrap.** Add a step to the top-level dotfiles `install.sh`, which today
   excludes `skills/` (`~/dotfiles/install.sh:39`). The step:
   1. clones skima's git remote into `~/projects/skima` if that directory is
      missing;
   2. installs it with `cargo install --locked --path ~/projects/skima`;
   3. runs `skima sync`.

   The "Deliberately NOT in this repo" section of `dotfiles/README.md` gains a
   skima line.

This is the cost of D4. Today a fresh machine installs skills from the dotfiles
checkout alone. After the migration it also needs a second repo and a Rust
toolchain. A tool versioned on its own is worth that. Vendoring it into dotfiles
would tie its tests and releases to unrelated config churn.

## 6. Data model and storage

### 6.1 Configuration

The configuration lives at `~/dotfiles/skills/skima.toml`, tracked with the
skills, because the homes are part of the collection's definition. skima looks
for `$SKIMA_CONFIG` first, then that path, and reads it with the `toml` crate.

```toml
source = "."                      # relative to this file, resolved physically (realpath)
loose  = ["SKILLS_USAGE.md", "GOAL_SKILLS_USAGE.md"]

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

Each adapter derives everything else from `path`:
- **Claude:** `skills/`, `settings.json`, `projects/` and `history.jsonl`.
- **Codex:** `skills/` and `sessions/`.
- **Pi:** `skills/`, and `sessions/`, unless `PI_CODING_AGENT_SESSION_DIR` is
  set.

Adding a home therefore turns on linking, policy checks and ingest together. E1
happened because these lists were kept separately; this layout makes that
impossible.

### 6.2 State

State lives in `${XDG_DATA_HOME:-~/.local/share}/skima/`. It is data, not
cache: once Claude cleans up a transcript, the ledger holds the only copy of
its events. It is kept out of git, because event `cwd` values include client
audit paths under NDA, and the dotfiles repo is pushed to GitHub. Because the
ledger cannot be rebuilt, the directory belongs in the user's existing
home-directory backup.

```
events.jsonl     the ledger
cursors.json     {"<abs path>": {"dev": …, "ino": …, "offset": …, "line": …}}
coverage.json    {"<home name>": {"first": "<ts>", "last_scan": "<ts>"}}
scan.lock        flock target
```

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

- **Latest.** Frontmatter is read with `strict_yaml.rs`. A file it rejects is
  skipped with a warning; all 39 current grade and retro files load. `date` is
  normalised to a `YYYY-MM-DD` string, whether it loaded as a YAML timestamp
  (38 files) or a quoted string (1 retro). The latest grade or retro has the
  greatest such string, with ties broken by filename.
- **Grades.** skima reads the frontmatter `target`, `date` and `score`. Let `G`
  be the last commit that touched the grade file
  (`git log -1 --format=%H -- <grade file>`). The grade is stale when
  `git rev-list G..HEAD -- <skill>` is non-empty, that is, when a later commit
  changed the skill. An edit committed together with the grade does not count.
  An uncommitted grade is not stale. Uncommitted changes to the skill
  (`git status --porcelain -- <skill>` non-empty) make any grade stale, because
  the text being used is not the text that was graded.
- **`baseline_commit`** is deliberately unused. Only some grades carry it, and
  the current `grade` template no longer writes it (E9).
- **Retros.** skima reads the frontmatter `skill`, `date`, `score` and
  `verdict`.

## 7. Interface

```
skima scan    [--reparse] [--report] [--json]
skima usage   [--since 30d|all] [--agent A] [--channel C] [--all] [--authoring] [--json]
skima unused  [--since 60d] [--json]
skima show    <skill> [--since 30d] [--limit 20] [--json]
skima status  [--agent A] [--json]
skima sync    [--agent A] [--dry-run]
skima retire  <skill> [--force]                 # F7
skima runs    <skill> [--since D] [--json]      # F8
```

- `usage`, `unused`, `show` and `runs` scan first. Tests call the `report`
  functions of the library crate directly on a fixture ledger, so no flag exists to skip the scan.
- `--all` adds `other` origins and `probe` actors. `--authoring` adds authoring
  reads.
- `scan` prints one summary line, the number of new events for each home.
  `--report` adds the per-channel counts, the unparsed samples and the parser
  canary.
- Exit codes: 0 for success or clean, 1 for drift, a conflict or a policy
  failure, 2 for usage errors.
- Output is plain aligned text, bold only when stdout is a TTY. The sparkline
  uses `▁▂▃▄▅▆▇█`, one cell per day.

Example `usage` layout (the numbers are illustrative, not measured):

```
skill                   sess slash tool typed read ref agents last    30d                             grade
validate-resolve-issue    70    67    0     0   25   3 X      2d ago  ▁▁▃▇▅▂▁▁▁▃▅▇▅▂▁▁▁▁▁▃▅▇▅▂▁▁▁▁▃▅  8.9
unslop                   120     1   75     0  140   0 CXP    today   ▅▇▇▅▇▅▇▇▅▇▅▇▇▅▇▅▇▇▅▇▅▇▇▅▇▅▇▇▅▇  9.1*
coverage: claude since 2026-09-10 (typed since 2026-02-24) · claude-work since … · codex since 2026-03-29 · pi since 2026-08-29
```

## 8. Tech stack

**Rust, edition 2024, built with the installed cargo 1.98.1.** One binary,
installed with `cargo install --locked --path ~/projects/skima`.

| Crate | Use |
|---|---|
| `clap` 4 (derive) | the CLI (§7) |
| `serde`, `serde_json` | transcript records, the ledger, cursors, `--json` output |
| `toml` | `skima.toml` |
| `saphyr-parser` | YAML events for `strict_yaml.rs` (§5.4) and frontmatter |
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
- The cost is §5.4: PyYAML's strict loader cannot move over unchanged, so
  `strict_yaml.rs` is new code with a differential test against it.

## 9. Architecture

```
skima/
  Cargo.toml  Cargo.lock
  src/
    lib.rs          the library crate: every module below except main.rs
    main.rs         clap dispatch, exit codes
    render.rs       aligned tables, sparklines, --json
    config.rs       skima.toml → Config { source, loose, homes }; path expansion and Pi normalisation
    strict_yaml.rs  PolicyLoader port (§5.4)
    inventory.rs    canonical skills, frontmatter, known names from git history, fan-in graph
    policy.rs       per-skill and per-home rules (§5.4), scan-freshness warning
    links.rs        drift classification; plan() (pure) and apply() (the only writer)
    tail.rs         incremental JSONL reader: cursors, partial-line safety, byte prefilter
    ledger.rs       Event, append rule, lock, coverage, reparse
    adapters/
      mod.rs        trait Adapter { files(home) -> Vec<PathBuf>; parse(path, start, line) -> Parsed }; the header line is always read
      claude.rs     meta records, Skill results, Read/Bash reads, history backfill
      codex.rs      session_meta, <skill> blocks, exec/function_call reads, parent lookup
      pi.rs         session header, <skill name location> blocks, read/bash tool calls
    readcmd.rs      shared reader grammar (§4.5)
    classify.rs     origin, actor, root session, authoring tag
    evals.rs        .grades/.retros frontmatter, grade staleness
    report.rs       usage, unused, show, runs; collapse, held and cluster
  tests/
    fixtures/{claude,codex,pi}/   one trimmed real record per positive and negative case
    policy_loader.py              verbatim PyYAML PolicyLoader, the reference for the YAML differential
    adapters.rs  ingest.rs  inventory.rs  policy.rs  sync_port.rs  reports.rs
```

The code splits into three paths:
- **Write:** `tail → adapters → classify → ledger`.
- **Read:** `ledger + inventory + evals → report`.
- **Sync:** `config + inventory + policy → links`.

Adapters only turn records into events. Identity, actor, root session and
authoring are computed once, in `classify.rs`. Adding an agent means one adapter
module and one `Agent` variant.

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

**Fan-in:** the full edge set of the real repo, kept as a snapshot. Besides the
snapshot, the test asserts the §5.3 counts independently: the seven
`blast-radius` edges (E4) by source skill, 8 edges into `goal-smart`, 20 into
`writing-for-agents`, 16 of 29 skills with an incoming edge, and no edge from
the `interrogate/SKILL.md:12` description clause.

**Held:** in a fixture ledger, the goal suite is unused and `writing-for-agents`
is held by one used skill. `unused` lists the goal skills with a `cluster` note
and leaves `writing-for-agents` out.

**Evals:**
- a grade without frontmatter, and `.grades/GRADES.md`: both skipped
- two grades for one skill: the later `date` wins
- a grade committed together with a skill edit: not stale
- a later skill commit: stale
- a quoted and an unquoted `date`: compared correctly

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
- The same pass counts how many read events the `grade`/`retro` authoring rule
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
  `skima sync` against a temporary `HOME` (§5.7 step 3).
- **Strict YAML differential.** A corpus of frontmatter and `openai.yaml`
  inputs covering every boolean spelling, duplicate and merge keys, explicit
  tags, quoted and plain scalars, timestamps and numbers, plus every real
  skill's files. The test feeds each input to `strict_yaml.rs` and to the
  Python `PolicyLoader` (copied verbatim into `tests/policy_loader.py`) and
  requires the same accept/reject verdict and the same value types. The corpus
  names each case in §5.4: the colliding key pairs, `=`, verbatim
  `!<tag:yaml.org,2002:bool>` tags, YAML 1.1 octal and sexagesimal numbers. The
  test fails when `python3` with PyYAML is absent, unless
  `SKIMA_SKIP_PYDIFF=1` is set.
- **Golden Codex id.** The pinned value is computed once by an independent
  Python script (`json.dumps(sort_keys=True, separators=(",", ":"),
  ensure_ascii=False)`), not by skima. One fixed payload with nested, unsorted and non-ASCII
  keys, a float and an integer above 2^53 must hash to a pinned `cx:` id.
  `serde_json` is used with default features only (no `preserve_order`,
  `arbitrary_precision` or `float_roundtrip`). The pin exists so a change in serialisation (for
  example, a dependency turning on `serde_json/preserve_order`) fails a test
  instead of silently duplicating every Codex event.
- A conflict in the last home leaves zero writes in every home, checked with
  tree snapshots.
- `~/.claude-work`, a home the old installer lacked, gets linked and
  policy-checked.
- `sync --agent pi` runs the per-skill rules and skips the Claude settings.
- `last_scan` 24 days old with `cleanupPeriodDays` unset: `status` warns and exits 0; 22 days old: no warning.

`test_pi_integration.py`, pointed at `skima sync`, stays in dotfiles' own suite
(§5.7 step 3).

**One-shot checks**, run once in M3. Their output goes into the migration
commit, and their code is deleted in that same commit, because it executes
`install.sh`.

- **Differential.** Runs the old `install.sh` and `skima sync` in two temporary
  HOMEs built from the same fixture tree, in two modes: the default mode, and
  `--pi` against `--agent pi`.
  - **Tree contents, two separate runs.** Run 1 has a foreign link and an owned
    dangling link, so both tools link and prune and the trees are compared.
    Run 2 adds an obstruction or a per-skill policy violation, one at a time;
    both tools must exit non-zero with the trees untouched. One tree cannot test
    both, because `install.sh` exits in `validate_policy` before linking
    (`install.sh:235`).
  - **Policy files:** the tree carries the sibling `.claude/` directory that
    `install.sh` reads. Each temporary Claude home's `settings.json` is a
    symlink into that directory, so both tools see the same settings.
  - **Homes:** the fixture `skima.toml` lists only the homes `install.sh`
    knows: `~/.claude`, `~/.codex` and the Pi directory.
  - **Comparison:** the check diffs `find -printf '%p %l\n'` and the exit codes.
- **Node versus Rust Pi paths** (§5.6). The same inputs go to the Node snippet
  and the new function, and the outputs must be equal.

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

1. `skima status` reports `claude-work: missing
   break-invariants` and no other error. Info-level `foreign` lines may appear.
2. `skima sync`, then `skima status`, exits 0.
3. `skima sync --dry-run` prints nothing.

## 11. Milestones

The ledger ships first, because Claude history is still expiring: each day
before the first real scan loses the oldest day of Claude transcripts (E2).
Typed `/name` commands from those days are still recovered from
`history.jsonl` (§4.2). skima never edits settings, so retention stays at the
default 30 days.

| M | Scope | Done when |
|---|---|---|
| M0 (2–3 days) | `config`, the git known-names part of `inventory`, fixtures, the three adapters, `readcmd`, `tail`, `ledger`, `classify`, `scan` with history backfill; the first real scan | §10.1 (detection fixtures), §10.2 and §10.4 pass, and the ledger holds the current Claude window |
| M1 (2 days) | `strict_yaml` (needed by `evals`), `usage`, `unused`, `show`, `evals`, the fan-in graph with held and cluster, `scan --report` | §10.1 (fan-in, held, evals fixtures) and §10.3 pass, with the precision recorded; the §10.6 gating runs pass and the model-dependent runs are recorded |
| M2 (1 day) | `policy`, the drift part of `links`, `status`, all read-only | §10.7 step 1 passes |
| M3 (2 days) | `sync`, the test port, the one-shot checks, deleting `install.sh`, the references and the bootstrap (§5.7) | §10.5, then §10.7 steps 2 and 3, pass; one dotfiles commit deletes `install.sh` |
| M4 (on demand) | F7 `retire`, F8 `runs` with the `retro` pointer, F9 leak report, F10 adapters | Each item's acceptance criteria are met |

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

### 12.2 Open questions

Each will be settled by a run, not by reading.

- **Q1. Codex discovery path.** Does current Codex still discover
  `~/.codex/skills`, or only `~/.agents/skills`? The §10.6 `codex exec` run
  answers it. If the answer is `~/.agents/skills`, the Codex adapter's derived
  skills directory changes, and the config does not.
- **Q2. Work-profile rule file.** Does the work profile need its own `CLAUDE.md`
  for the `AGENTS.md` rule to apply? This is outside skima, which does not
  report it.
