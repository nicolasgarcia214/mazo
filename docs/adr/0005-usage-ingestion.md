# 0005. Usage ingestion

Status: Accepted (2026-11-26)

## Context
Usage ingestion turns Claude, Codex and Pi transcripts into ledger events: adapters parse records, the classifier assigns identity and origin, and the scan reads files incrementally and appends under the append rule. Until the comment ban (ADR 0001), the reasons behind these modules lived in code comments. This record keeps those reasons.

## Decisions

### Adapter contract
- Adapters only turn transcript records into `Raw` events. Identity, actor, root session, origin and the authoring tag are computed once, in `Classifier::event`, so every adapter gets the same rules.
- `Key::Plain` holds an id that is final at parse time (`cc:`, `cch:`, `pi:`). `Key::Codex` defers the `cx:<root session>:<hash>:<index>` id to `Classifier::event`, because the root session is known only after classification.
- `ActorInputs::subagent` comes from a Claude sidechain or `/subagents/` file, a Codex `source.subagent`, or a Pi `parentSession`.
- Every adapter keeps a record with a missing timestamp and passes the empty string, so `Classifier::event` drops the load as `Dropped::Timestamp` and the scan counts it in `HomeReport::bad_timestamps`. The adapter never skips such a load silently.
- A session's identity comes from line 1 of the file, which `Adapter::parse` receives as `header`: `tail::read` keeps it as `Chunk::header` when it reads from the start, and `first_line` reads it otherwise. A Codex child copies its parent's `session_meta` onto line 2; parsing ignores it.
- `Parsed::skipped` marks a file from an excluded session: a Codex guardian review (`Header::guardian`), skipped wholesale because it replays other sessions' transcripts as evidence, or a Pi file whose line 1 is not a session header (such as a herdr run-state record). A skipped file adds nothing to the malformed count or the canary.
- Each adapter counts its malformed lines in `Parsed::malformed` while it parses, by reading the chunk through `adapters::values`, so a line is parsed once. `values` counts a line only when it is not JSON at all; a line that is JSON but not a `Value`, such as one with a lone surrogate, is dropped without being counted. An adapter must drain `values` on every path that does not set `skipped`, including the Codex file with no header.
- `MARKERS` holds each adapter's skill-load marker, text a transcript contains only where a skill loaded. `Prefilter` matches these, the generic `Prefilter::MARKERS`, the source root and every home skills dir, so the parser sees almost no lines. `has_canary` flags a file whose lines hold a marker but which yielded no event, and the scan lists it in `HomeReport::canary` as a sign the parser has missed a format change.

### Claude adapter
- The `Claude` adapter derives events only from structured harness records: a Skill result's `toolUseResult.commandName`, an `isMeta` record carrying `Base directory for this skill`, or an assistant `Read`/`Bash` tool call. It never reads `tool_result` text, `<command-name>` records or skill attachments.
- A Skill result counts only with `success: true`.
- `Claude::parse` keeps a `loads` map from tool_use id to the event that id produced. The meta record of a Skill call (`sourceToolUseID`) fills that event's path instead of emitting a second event, so a plugin skill takes its name from `commandName` and its path from the meta record. The meta record stands alone as a Tool event when the result is in another chunk or has no `toolUseResult` (subagents). A built-in skill's meta record has no base directory, so its event has no path.
- A slash load comes from the meta record alone, so a scan that stops between the `<command-name>` record and the meta record loses nothing.
- `backfill` emits one-time `typed` events from `history.jsonl` only for lines whose `display` is `/<known name>` followed by whitespace or the end, and whose session has no transcript under the home. History therefore never double-counts a load a transcript already holds.

### Codex adapter
- The `Codex` adapter keys each event by a SHA-1 hash of the record payload, which excludes the record timestamp, so the copies of a record that forked rollouts repeat (re-stamped) collapse to one event.
- `Codex::parse` counts only `response_item` records, the model-facing copy: user messages, `function_call` and `exec` custom tool calls. It ignores `event_msg` duplicates (including `item_completed`), `*_output` records, developer listings, `compacted` replacement history and the `session_meta` copies in forks.
- `Codex::parse` pushes an `exec` input to `unparsed` at most once per record, however many of its calls are opaque, and only when the input names a skill document.

### Codex exec scanner
- `exec_calls` returns `Err` for a `tools.exec_command` call whose `cmd` is not a string literal or a substitution-free template literal. `literal` treats anything that follows the string before the expression ends (such as `"a" + b`) the same way. The command such a call runs is unknown, not guessed.
- The scanner must terminate on any input, because it runs while the scan holds `data.lock` through `durable::lock`. `object_call` stops at `)` or `]`, which mean the object literal was never closed, and breaks out when an entry consumes no input.
- `skip_braces` tracks open `{` and template backticks on a heap `Vec` instead of recursing, so no nesting depth can overflow the call stack. Recursion once let 120k nested `${` abort `mazo scan`.
- `unescape` keeps any unrecognised escaped byte as itself, so an escaped multi-byte UTF-8 character passes through whole.

### Pi adapter
- The `Pi` adapter keys events by entry id and timestamp, so the copies of an entry that forked sessions repeat collapse to one event.
- `Entry::slash` accepts a `<skill …>` block only when its location is an absolute path ending in `/SKILL.md`, which rejects placeholder locations.

### Skill files
- `SkillRoots::classify` resolves a path against the cwd, so a relative read inside a skill directory counts. A path ending in `/SKILL.md` is a Read load. Otherwise the path must lie inside a `<name>/` child of a home skills dir or the source root, where `<name>` is a known skill or a non-hidden directory containing `SKILL.md`. A root's direct children (the skill directory itself or a loose file such as `SKILLS_USAGE.md`) do not count. Only Markdown files count as `Ref` loads, because scripts, tests and manifests carry no instruction text.
- `SkillRoots::shell_reads` sends a command that names a skill's Markdown file but is neither a recognised load nor a known non-load to `unparsed`, such as a command chained with one outside the grammar. `SkillRoots::names_skill_doc` tests only for Markdown files, because running a skill's scripts reads no instructions.

### Prompt and judge views
- `Adapter::prompt_at` reads the user-authored text behind a load at display time and never stores it. It walks back from the load's own record, itself included, because a `/skill:` message holds its own prompt.
- A bare invocation (Claude `/name`, Codex `$name`, Pi `/skill:name`) counts as no authored text, so the prompt walk continues to the message before it. With arguments, it reads as `/name args` (`authored`, `user_turn`).
- Claude `authored` skips `tool_result` and `isMeta` records. Codex `user_turn` reads both the `event_msg` `user_message` and the `item_completed` `UserMessage` that replaced it from 2026-08, and never attributes a `<skill>` injection to the user.
- `INJECTED` strips the blocks the harness inserts into a user turn (system reminders, task notifications, local command output, pasted content), so only text the user wrote counts as authored, and a record holding only such blocks is walked past.
- `message_text` joins the `text` of every block, so an image beside the text does not hide it.
- Each adapter's `render` keeps user text, assistant text, tool calls and tool results, and drops skill bodies: Claude keeps nothing of `isMeta` records, Codex takes user turns from `event_msg` and keeps nothing of `response_item` user messages (they repeat the turns or carry `<skill>` bodies), and Pi strips `/skill:` bodies.
- `Adapter::condense` gives the judge a window from the prompt behind a strong load up to the next strong load of a different skill, the `WINDOW_MESSAGES`th authored record, or the end of file, capped at `max_chars` with a cut marker. A repeat load of the same skill does not end the window.
- `Adapter::trace` takes the last run of a skill before its reviewer's last load, ignoring reviewer loads that precede the skill's load. It ends before the reviewer's first load after the run, so an earlier review stays out, or before the prompt that asked for the review when that prompt names the reviewer. A bare `/retro` carries no prompt, so the run keeps its own last prompt. The trace is uncapped.
- `prompt_at`, `condense` and `trace` read a transcript through `tail::whole`, which shares the scan's guarded open, so a FIFO or device is refused without blocking. A line over `MAX_LINE` is skipped and later lines keep their absolute numbers.
- `prompt_at` and `condense` take a `Transcripts` memo owned by the caller, so one report, show, card or audit selection reads each file at most once. The memo lives only as long as that run, never in a static, so a later run sees the file as it is then. `trace` reads one file once per call and takes no memo.
- `strong_loads` parses with empty `SkillRoots`, because strong loads come from the harness's own markers and need no skill roots.

### Classification
- `Classifier::event` drops every record whose session cwd is inside `<data dir>/audit-runs/` (`Dropped::AuditRun`), because the judge's own runs are never usage. The scan canonicalizes the data dir before the comparison, because agents record their resolved cwd.
- `SKEW` lets a record be dated up to 24 hours after `Classifier::now`, the scan start, to accept transcripts synced from a machine whose clock runs ahead. A later or unparseable timestamp drops the load as `Dropped::Timestamp`.
- `Classifier::name_origin` decides origin from the name alone, for Claude Skill results and typed history (`Raw::name_origin`). A Skill result needs a known name, so skills renamed or pruned but still in git history keep their events. A typed history line needs a skill canonical now, because history has no path and a name such as `argus` can be an old skill or a later plugin command.
- `Classifier::event` keeps a Skill result's name-based origin even after its meta record adds a path, so a merged event equals a split-then-upgraded one.
- `Classifier::path_origin` canonicalises the path and treats it as canonical under any skill root, the store included, whether reached through a home link or a symlinked data dir.
- Only weak reads can be authoring. The stored `authoring` flag is the cwd rule alone (cwd inside the worktree or a home skills dir); a strong load in the worktree is a real use.
- `Classifier::event` stores every transcript-derived text field (skill, path, cwd, `src`) through `render::inert`, because transcripts are text anyone can write. No report, card or TUI can then print a control character from one, including a control character in a transcript's own file name. `inert` keeps newline and tab, which JSON output escapes.

### Root sessions
- `Roots` persists every root a walk reaches in `roots.json` (Codex rollout id or Pi session id to root session id) and reuses it without walking again, so a session keeps its root after a file in its chain is deleted. Header lookups are cached only for one scan.
- `Roots::root` stores the root for every passed session when the walk reaches a parentless or stored session, and stores nothing when it reaches a missing file. A walk is capped at 64 steps and stores nothing when cut off.
- `Roots::session` derives a missing Pi parent's session id from its file name `<ts>_<id>.jsonl`, and `Roots::codex_file` takes the trailing 36-character UUID of a `rollout-<start time>-<id>` stem as the Codex rollout id.

### Git history
- `History` runs one `git log` and one `git status` per report and derives every skill's first and last commit and uncommitted changes from them.
- `History::load` reads the log with `git log -m`, so `parse_log` sees a merge's header once per parent and records one diff each. `Commit::touches` counts a merge as touching a path only when the path differs from every parent.
- `History::commits` is ordered newest first, which `History::first` and `History::last` rely on.
- `History::load` returns `None` when git cannot read the source, and an empty log for a repository with no commit yet.
- `History::load` strips the source's worktree prefix from status paths, because porcelain paths are relative to the worktree whatever the working directory. `parse_status` consumes the original path that follows a rename or copy.
- `History::skill_names` takes the basename of every `SKILL.md` path's parent, so nested layouts such as `claude/goal-audit` yield `goal-audit`.

### Ledger and derived state
- `Channel::strong` marks Slash, Tool and Typed as the only channels that feed the headline `sess` column.
- `Event`'s field declaration order is the serialised key order of a ledger line, so reordering fields changes the ledger format.
- `Ledger::events` keeps the last line per id, in the order each id first appeared; every reader sees the ledger that way.
- `Ledger::admit` is the append rule: it appends a new id, or upgrades a stored `path: null` event by copying it and setting only `path`. Every other duplicate is dropped.
- `durable::lock` takes a blocking flock on `data.lock`, and every writer of the data directory holds it until the file is dropped. `Ledger` and `Roots::save` write without locking of their own for that reason.
- `Ledger::load` skips a torn or unparseable line (including legacy lines whose `ts` does not parse) and sets `unreadable`. `Ledger::append` then rewrites the whole file without those lines, and `scan --reparse` re-derives their events.
- `append_lines` starts the new lines on their own line when the log ends in a torn line, so a crash mid-append costs only that line and later runs read past it.
- `jsonl` splits lines as bytes, so a torn write that cuts a UTF-8 character costs that line, not the file.
- `write_atomic` writes a fresh `<path>.new`, fsyncs it, renames it over the path and fsyncs the parent. A crash leaves the old or the new file, and a symlink at either path is replaced, never followed.
- Cursors, roots and coverage are derived state. `read_derived` returns `None` for a missing file, and `None` with a warning for an unparseable one, and the scan rebuilds it: roots from every session on disk, coverage from the transcripts still on disk (so a home whose oldest transcripts are gone reports a later start).
- `Coverage::first_at` treats an unparseable first timestamp as absent, so the next scan fills it again.

### Scan
- `scan_with` takes the adapter lookup as a parameter so a test can stand in a changed parser rule for `scan --reparse`.
- On reparse, `scan_with` seeds a fresh ledger with every typed event, every event whose transcript this run does not list, and the events of a transcript it cannot read, then admits the parse under the append rule. A stored event whose file was deleted survives when a fork copy shares its id.
- `scan_with` decides a home's first scan from both coverage and the ledger, so a lost or rebuilt `coverage.json` does not backfill history loads again for transcripts deleted since.
- `scan_with` visits a session's root when its header is first read, after the read so the header is at least what the read consumed. The root then survives a parent deleted before the session's first load.
- `scan_with` sets the coverage start to the earliest of the recorded start, the first timestamps read and the loads admitted, so it never moves later; a load from an older transcript added since the first scan moves it back.
- `first_timestamp` takes the earliest parseable timestamp among the first lines, compared as timestamps, not strings, because records are in time order except a Skill result and its meta record a few milliseconds apart.
- `scan_with` drops cursors for transcripts no longer listed, since the agent cleaned them up.
- `HomeReport::malformed` counts prefiltered lines that are not JSON, plus lines too long to read, even in files that yielded events. A line is read once, so `ScanReport::warnings` reports it once, or again on `--reparse` or with a fresh data directory.

### Incremental reading
- `read` resumes from the stored cursor only when device, inode, a length at least the offset and the hash of the `WINDOW` bytes before the offset (`Cursor::window`) all match; otherwise it reads from the start. A file rewritten in place, even with the same inode and length, is read again. `Cursor::line` stores the line count before the offset so `src` can name a line without recounting.
- `Chunk::cursor` only advances past a newline, so a line without a trailing newline is treated as still being written and re-read next time.
- `MAX_LINE` caps a kept line at 64 MiB, because real transcript lines reach a few MB. A longer complete line is skipped as malformed, not buffered; one without a newline yet stops the read without advancing.
- `open` uses `O_NONBLOCK` and accepts only regular files, so a FIFO is refused rather than waited on. `head` reads through it with bounded lines, because a Pi header names its parent's file and that path is untrusted.

### Tests
- Adapter test fixtures are real local records trimmed to the fields the adapter reads, with prompt and tool text scrubbed. A fixture composed from real records exists only where the corpus has no real example, and its test says so.
- `parse_all` bypasses the scan prefilter so fixtures whose records carry no prefilter marker (such as `/clear` and refused Skill calls) still reach the parser; `parse_from` goes through the prefilter from a cursor.
- `golden_codex_id` pins the Codex id hash to a Python `sha1` of sorted compact JSON computed outside mazo. Its `create_time` value also pins serde_json's `float_roundtrip` feature, which real Codex values need.
- `Env` writes the config once, because rewriting it per CLI call truncates it under a child already spawned and reading it.
- Permission-based I/O error tests probe readability after `chmod 000` or return early, because the file stays readable when running as root.
