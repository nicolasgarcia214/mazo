# 0007. CLI, configuration and test harness

Status: Accepted (2026-11-26)

## Context
These decisions about the command line, the config file, the sync and lifecycle paths, and the test harness were recorded as code comments. The comment ban (ADR 0001) removes those comments; this record keeps the decisions so the reasons survive the deletion.

## Decisions
### Crate layout and errors (`lib.rs`, `main.rs`)
- mazo has five paths: write (`tail` → `adapters` → `classify` → `ledger`), read (`ledger` + `inventory` + `evals` → `report`), sync (`config` + `inventory` + `policy` → `links`), judge (`ledger` + `adapters` condense → `audit`) and store (source → `audit` vet → store + `mazo.lock` → `links` for `add` and `update`, `lifecycle` → store for `retire`). With `--project`, the store path acts on a project's store and lockfile.
- An `Error` reaches the user as one `ERROR: …` line, and its variant picks the exit code: `Error::Usage` (the command cannot run as invoked or configured) exits 2, `Error::Failed` exits 1.
- `mazo::at` passes the path through `render::inert` before prefixing an error with it, because a skill's author or a transcript may control that path.
- `mazo::unique` combines the pid, the time in nanoseconds and an atomic call count. The count is needed because the clock repeats across threads on platforms where it ticks in microseconds, such as macOS.
- `mazo::stdout` exits 0 quietly on `BrokenPipe` (a reader such as `head` closed stdout early) and exits 1 on any other write error.
- `main` opens the TUI for bare `mazo` only when both stdin and stdout are terminals. Otherwise clap parses the empty command line, prints help and exits 2.
- `run` calls `report::check_skill` for `show` before the scan, so a bad skill name fails without paying for a scan.
- `mazo evals dir` prints the grade and retro directories raw rather than inert, because skills read those lines as the directories to write into.
- `cmd_audit` runs `audit::plan` before the scan so usage errors surface first, and a failed login check stops the run before any judge call.
- `cmd_report` writes to `<data dir>/report.html`, outside any git repository, unless `--out` names another path. It makes an `--out` path absolute so the opener and the printed path do not depend on the cwd.
- `cmd_scan` prints JSON through a `serde_json::Value` so object keys print sorted. After a reparse it prints absolute per-home event counts with the previous count instead of `+N`, because a delta is meaningless after a rewrite.
- `cmd_status` also checks a project's skill links when run inside a project, and `--json` lists them under `project`.
- `cmd_sync` and `lifecycle::retire` take the data lock (`durable::lock`) and re-read the inventory under it before computing the plan they apply, so a concurrent `add`, `update` or `retire` cannot leave that plan stale. `cmd_sync` takes no lock for a dry run, a refusal or an empty plan.

### Config file (`config`)
- `config::path` reads `$MAZO_CONFIG`, else `$XDG_CONFIG_HOME/mazo/config.toml` with `XDG_CONFIG_HOME` defaulting to `~/.config`, and consults no other location.
- `config::load` canonicalises the config path first, so a config symlinked from a skills repo resolves `source` against that repo.
- `source_dir` resolves `Config::source` physically, through symlinks, so a directory alias still links the canonical source. It rejects a source that is not a directory. `Config::source` is `None` for a user with no skills of their own.
- `config::load` resolves a relative home `path` against the config file, as it does `source`. The exception is a Pi home taken from `PI_CODING_AGENT_DIR`, which resolves against the cwd because Pi resolves its own variable that way.
- `Config::trusted` lists owners (`owner`) and repositories (`owner/repo`) whose skills install without vetting. `RawConfig::check` treats an entry whose first segment contains a `.` as host-qualified, which allows `host/owner` and `host/owner/repo`.
- `Home::links` keys are paths relative to the home and values are paths relative to the source. `RawConfig::check` requires every key and value to be plain components (`.` aside), with no root and no `..`, so a link never leaves the directory it is joined to.
- `RawConfig::check` requires each `loose` entry to be exactly one plain path component, because the entry also names its link.
- `Policy::authoring_skills` lists the skills whose strong load marks a root session as authoring.
- `resolve` mirrors Node's POSIX `path.resolve(base, p)` and `file_url_to_path` mirrors Node's POSIX `fileURLToPath` (WHATWG parse, empty or `localhost` host, no encoded `/`, strict percent-decoding to UTF-8). Neither touches the filesystem.

### Judge configuration (`config::Judge`, `JudgeAgent`, `Billing`)
- `Config::judge` is `None` without a `[judge]` table, and mazo then assumes no judge agent.
- `JudgeAgent` covers the homes' agents (`claude`, `codex`, `pi`), `command` (any CLI that reads a prompt on stdin and prints its answer) and `http` (an OpenAI-compatible chat completions endpoint). Config and JSON spell each by `JudgeAgent::name`. `JudgeAgent::home` is true for the agents that are a binary of their own name, which `init` can look for on `PATH` and which are configured by `agent` alone.
- `Judge::model` is passed as `--model` (`-m` for Codex); unset means the agent's own default. `Judge::argv` belongs to the `command` judge only, and `Judge::check` rejects it on any other agent. The `http` judge posts to `<url>/chat/completions`, and an unset `Judge::api_key_env` sends no key.
- `cleartext` counts any plain `http://` URL not to localhost, 127.0.0.0/8 or ::1 as cleartext, and `Judge::check` refuses an `api_key_env` on such a URL. A URL the `url` crate refuses also counts as cleartext, because the request's own parser may still accept it. The test spells `http://evil\@localhost/v1` with a TOML-escaped backslash, because the backslash ends the host and the URL must still count as cleartext.
- Under `Billing::Subscription` every judge call runs, after a login check, without the variables that would bill an API key or a cloud account. Under `Billing::Api` the environment passes unchanged.

### `mazo init` (`config::init`)
- `config::init` writes a config naming `source` absolutely, a home for each agent directory under `~` that exists, every rule off, and a detected or chosen judge. It refuses to overwrite an existing path before detecting or asking, and warns on stderr when it finds no home.
- `detect_judge` tries agents in `JudgeAgent::ALL` order (claude, codex, pi) and picks the first whose binary is on `PATH` and whose subscription login check passes; Pi has no check. The checks read local state only (`claude auth status` and Codex's `auth.json`) and run in a fresh scratch directory under the temp dir, removed afterwards, so detection leaves nothing behind. `InitJudge::Detected` records such a judge; `InitJudge::Chosen` records one the user picked.
- `init` writes the `command` and `http` tables (`COMMAND_PLACEHOLDER`, `HTTP_PLACEHOLDER`) commented out, because they are invalid until their placeholders are filled in.

### Context column (`ctx`)
- The ctx column derives what each agent a skill is linked into keeps of it in context from Claude's `skillOverrides` and the SKILL.md's `disable-model-invocation`. `Ctx` holds an `Invocation` only for the agents a skill is linked into.
- `Invocation` variants are ordered costliest first, so taking the minimum picks the state that keeps the most in context. `Invocation::On` (name and description) covers Claude on, every Codex skill and Pi's default. `Invocation::UserInvocableOnly` (nothing until invoked) covers Claude user-invocable-only and `disable-model-invocation` in Claude or Pi.
- `Invocation::claude` lets a `skillOverrides` value of `off` win over `disable-model-invocation`, which wins over any other override value.
- `Contexts` reads each skill's linked homes and each Claude home's `skillOverrides` once for every skill's `Ctx`. `Contexts::of` gives each agent the costliest state among its homes, so one home's `on` beats another's name-only, and user-invocable-only beats off. A project skill counts as linked into every agent, with its Claude state taken over every Claude home.
- `Contexts::of` ignores a skill's Codex manifest (`agents/openai.yaml`); it never changes the Codex state.
- `Tokens` approximates each agent's listing tokens as the characters `Invocation::chars` keeps of every skill, divided by 4. `Tokens::agents` flags an agent once its tokens reach 80% of its listing budget, a share of `Config::context_window`: 1% for Claude, 2% for Codex, none for Pi. The comparison `tokens * 500 >= window * percent * 4` uses saturating integer arithmetic.

### Evals (`evals`)
- The evals module reads grade and retro frontmatter under `evals_dir` at report time and never copies or writes those files.
- `Eval::date` keeps the first 10 characters of the frontmatter `date` (a `YYYY-MM-DD`), and `Eval::score` keeps the frontmatter score as written (`8.6`, `9`) rather than parsing it.
- `Grade::content_hash` is the skill's `content_hash` when it was graded. A grade is stale when that hash is missing or differs from `Evals::content_hash`, the current hash, which is `None` for a name in no root.
- `read_dir` loads every `*.md` with loadable frontmatter and a `date`, sorted by (date, filename), so `Evals::grades` and `Evals::retros` run oldest first and the last is the latest. It records each skipped file that has frontmatter, with the reason, passing the file name and YAML error through `render::inert` because both are untrusted text.
- `evals::transcript` treats its argument as a path only when it is an existing file under a home's transcripts, because a session id may also name something in the cwd. Otherwise it resolves to the single transcript whose file name contains the session id.

### Examples (`examples`)
- The card's real examples are read from transcripts at display time and never stored.
- `examples::examples` reads only strong events by the `main` actor, newest first, and adds the latest audit's curated examples, which the audit caps at `REPORT_EXAMPLES`.
- `examples::real` takes at most one prompt from each of the `PROJECTS` most recent projects, newest project first. It skips a prompt that is only the invocation or shorter than `MIN_CHARS`, because a reply such as "yes" or "go on" says nothing about the skill, and falls through to the project's next event. An expired transcript ends that project's search.

### Help text (`help`)
- `help` is the single source of the TUI help overlay, the legend under `mazo usage` and `mazo unused`, and the HTML report's legend.
- `USAGE_COLUMNS` and `UNUSED_COLUMNS` list columns in header order, and a test checks them against `USAGE_HEADER` and `UNUSED_HEADER`.
- Each `MARKS` entry names the usage column it appears in, and a mark spelled as its column's name stands for the whole column.

### Inventory (`inventory`)
- `Inventory::roots` lists the source first and the store after it, so `Inventory::root` resolves a name held in both to the source. `Inventory::origins` records every root holding a name, and more than one root for a name is a conflict.
- `Inventory::load_roots` reads git history, `Inventory::known` and `Inventory::worktree` (for the authoring rule) from the source root only. `Inventory::known` unions the canonical skills with every directory that ever held a `SKILL.md` in the source's history.
- `Inventory::load_roots` skips a missing non-source root, because the store does not exist before the first `add`.
- `canonical_skills` skips, with a Debug-escaped warning, any directory name that is not UTF-8 or not already inert, because a project's store can be anyone's commit. Its test skips on `EILSEQ` because APFS refuses non-UTF-8 names, so macOS never lists one.
- `git` passes `--no-optional-locks` so report commands such as `git status` never rewrite the user's index.
- `inventory::citations` draws the held edge A → B when A's citing text (the SKILL.md body plus `references/**/*.md`, backticks removed, whitespace collapsed) contains `the B skill`, `owned by B`, or `B/` as a path component.
- `inventory::citations` draws the named edge A → B for a whole-word, case-sensitive match of B not already a held edge. A mention marks a boundary between siblings and never holds a skill. `names` treats `/b` as a skill name and `b/` as a path, so a match followed by `/` is not a mention.

### Lifecycle (`lifecycle`)
- `lifecycle::new` parses the generated SKILL.md back through `policy::always_on` and checks that the description round-trips, so what it writes passes the rules `sync` runs. A rejected name or description is a usage error; an existing entry or a store skill of that name is a refusal. `new` never runs `sync`, so `status` reports the missing links until `sync` runs.
- `quoted` emits a YAML double-quoted scalar that escapes `"`, `\` and every control or line-separating character as `\uXXXX`, so the value stays on one line.
- `prune` deletes nothing and exits 1 on a refusal or a declined confirmation. Pruning everything refuses as a whole when any entry needs `--force`, so even clean entries survive. `is_entry` accepts only a single plain component naming a directory in the archive, so a name cannot escape it.
- `unsaved` compares each archived file's `git hash-object` blob (which ignores `.gitignore`) against `HEAD` at both the original and the archived path. `link_blob` hashes a symlink as its link text through `git hash-object --stdin`, matching the blob git stores for a symlink. Outside a git repository `unsaved` returns nothing, and a repository with no `HEAD` has nothing saved.
- `retire` scans first and runs every refusal check before any write: a held or named skill, a strong event of the skill, a holder or a namer within `RETIRE_DAYS`, and any conflict or policy error in the sync plan as if the skill were gone. It writes nothing unless it returns `Retired::Done`, never commits, and keeps the scan's warnings even when a later step fails.
- `retire` moves a tracked source skill with `git mv`. `archive` refuses when `.archive/<name>` exists and returns `None` for a store skill, which is deleted along with its lockfile entry.
- `retire` lists `skillOverrides` entries to drop only for global skills, because the homes' settings name only global skills.
- `located` picks the project store root explicitly, because the global store may hold a skill of the same name ahead of it. It requires a source only when the name is not a store skill, since only a store skill retires without a source.

### Links (`links`)
- `links::plan` only reads the filesystem and `links::apply` is the only writer, so every check happens before any write. Any entry in `Plan::conflicts` means no writes.
- `targets` relies on `config::load` having refused `loose` and `links` without a source. `linked` returns the pairs `plan` links regardless of disk state.
- With `gone` (for `retire`), `plan` plans as if that path did not exist, so links into it plan as dangling. Callers must already leave the gone skill out of the inventory.
- `plan` treats a dangling link to `<root>/<its name>` for a root of the inventory as mazo-made, so mazo may remove it, and replaces it with a fresh link; this covers a copy deleted from one root and installed into another. A wrong link whose target exists is a conflict left to the user.
- `plan` merges targets sharing a physical directory, directly or through a symlink such as `PI_CODING_AGENT_DIR`, and plans each physical directory once. A `links` entry naming the same path to a different source is a conflict whichever home claims it.
- `plan` reports a conflict when a home's skills directory resolves to a skill root, because every skill there would look like a real file or a link written into the root.
- `plan` never prunes hidden names.
- `sync_errors` orders its lines as per-skill rules, per-home rules of the selected homes, then the plan's conflicts.
- `apply_after` undoes nothing on a failed write. Its error lists every link and prune made before the failure, including those already in `out`.

### Policy (`policy`)
- `per_skill` runs the always-on then the configured per-skill rules on every `sync` and `status`, whatever `--agent` says. It reports an empty skill set only when a source is configured, because without a source the user's own skill set is empty by design.
- `python_space` matches Python's `str.isspace()`, adding U+001C to U+001F to Unicode whitespace.
- `settings` yields `settings.json` then `settings.local.json`, and the later file wins. `overrides` applies that per skill; a file that does not parse, or whose `skillOverrides` is not an object, sets nothing and yields an `ERROR` line. `per_home` lists a skill not set name-only under the file whose value wins, else `settings.json`.
- `freshness` warns for a Claude home never scanned or last scanned more than its retention minus 7 days ago. Retention is Claude's `cleanupPeriodDays` from the winning settings file, or 30 days when unset.

### Reader grammar (`readcmd`)
- A command is a load only when it chains reader verbs with `&&` or `;` (`cd` rebasing later relative paths), or pipes a reader into stages that only pass lines through. `rg`, `grep`, `wc`, `ls`, `find`, `git` and `apply_patch` are not loads. `Reads::Load` lists the distinct files in order of first read.
- A reader mixed with non-readers returns `Reads::Unknown`, so it counts as unparsed rather than silently dropped.
- `lex` returns `None` on `||`, `&` backgrounding, subshells or unbalanced quotes, because those are not plain reads. It keeps `&` after `>` as part of a redirection such as `2>&1`.
- After a `cd` to a directory that is not `plain` (shell expansion left in it other than a leading `~/`), later relative paths are unknowable while absolute paths still count as reads.
- `reader_files` treats `command cat` and `/usr/bin/cat` as `cat`. `flags_then_files` skips a leading `+N` word as a `tail` offset. `sed_files` counts only `sed -n` with a line-range print script as a read.

### YAML (`yaml`)
- `yaml::load` uses saphyr with the YAML 1.2 core schema, rejects duplicate keys at any depth, and keeps scalars as written so callers apply strictness where a value is used.
- `yaml::load` parses with `early_parse(false)` and runs its own duplicate check, because saphyr's own load resolves scalars and so treats `a`, `'a'` and `!!str a` as the same key.
- `is_plain_false` accepts only the plain, untagged scalar `false`, rejecting `"false"`, `False`, `no` and `!!bool false`.

### Test harness (`tests/`)
- `Probe` builds the live end-to-end fixture directly, never through `mazo sync`, so the live test does not depend on the code it checks. It symlinks the real Claude and Codex credentials rather than copying them so token refreshes reach them, and `tests/live.rs` runs one agent at a time so no two refresh together.
- `Probe::new` and the sync `Fixture` canonicalise their temporary roots, because agents record physical paths and sync links name the physical source.
- `unnonced` strips the frame nonce from tags because each mazo process picks its own.
- `parse_unfiltered` bypasses the scan prefilter so no-event tests reach the parser even when their records carry no marker.
- The test `git` helper runs with a fixed identity and fixed author and committer dates, isolated from the user's git config.
- `as_uri` and `relpath` reproduce Python's `Path.as_uri` and `os.path.relpath`; `relpath` is valid only for absolute, lexically clean paths.
- `pi-explicit-skills.ts` is an empty stand-in for the Pi extension `mazo sync` links, since the sync tests check only its presence and link.
- `a_reader_that_closes_stdout_early_ends_the_command_with_0_and_no_panic` writes about 130 KiB of rows so output overflows a 64 KiB pipe buffer and the write after the reader closes fails.
- The lifecycle `Fixture` puts the skills source one level below the repository root so `git -C <source>` resolution is exercised.
- `new_rejects_a_missing_description_a_bad_name_and_an_existing_entry` expects exit 2 for `-a` because clap parses it as a flag, not a name.
- The retire test accepts without `--force` a skill whose strong events all predate the 30-day window and that no used skill cites.
- `Sourceless::new` runs `init` with a `PATH` holding no agent, so it detects no judge.
