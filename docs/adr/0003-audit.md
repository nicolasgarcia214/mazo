# 0003. Skill audit

Status: Accepted (2026-11-26)

## Context
These decisions were recorded as code comments in `src/audit`, `tests/audit.rs` and
`tests/live.rs` until the comment ban (ADR 0001) removed them. This record keeps
the reasons behind the audit's budget, judge calls, login checks, answer checks and
tests, so the code no longer has to carry them.

## Decisions

### Budget (`plan`, `Audit`, `Run`)
- `plan` gives a budget only to a backend with a `Backend::budget_flag`, and rejects `--max-budget-usd` for any other. The Codex, Pi, command and http judges report no dollar figure, so `--limit` is their only cap. `Planned::budget`, `Audit::budget` and `Run::spent` are micro-dollars, and `Run::spent` stays 0 for a judge that reports no cost.
- `judge_sessions` caps stage 1 at 75% of the budget, so stage 2 always has a share left.

### Money (`parse_usd`, `cents`, `exact`)
- `ClaudeOutput::total_cost_usd` is a `RawValue`, and `parse_usd` computes the amount as digits times 10^shift micro-dollars in integer arithmetic, truncating. The reported cost never passes through a float, so records keep the reported digits.
- `cents` rounds a cap down to whole cents, the form the judge's budget flag takes. `exact` renders micro-dollars as a trimmed exact decimal, the form records store.

### Judge call and retry (`judge`, `retry`, `NoAnswer`)
- `judge` is the call-and-validate step both stages and `vet` share. Each call is capped at the limit minus what was spent, and starts only with a cap of at least `CENT` and an uncancelled run.
- `retry` appends the rejection reason to the input after an invalid answer, and resends the input unchanged after a call that gave no answer. An answer is retried once, then dropped.
- `NoAnswer::Retry` covers answers that are over the cap, past `JUDGE_TIMEOUT` or unparseable. `NoAnswer::Stop` covers login, key and network failures and signals mazo did not send, and stops the run. `exited` reads a subprocess judge that gave no answer as a stop after a nonzero exit and as a retry after a zero one. `stop` cuts the judge's stderr to `STDERR_LINES` lines of `STDERR_CHARS` characters.
- Setting `Pane::cancel` kills the judge call in flight and fails the run with `CANCELLED`.
- `Caller` carries only what a judge call needs (data directory, `[judge]` config, backend, pane), so audit and vet share one call path.

### Login check (`Caller::login`, `Backend::login`)
- `Caller::login` runs the backend's login check once, before a run's first call, only under subscription billing. A failure is `Error::Usage` because no judge was started. `command` runs it whenever `calls` says the run will start a call, so `--resynth` checks even when no session is left to judge.
- The command judge checks no login because mazo knows nothing of the program it runs. The Pi judge checks no login because Pi picks its provider from its own settings.

### Subprocess judges (`spawn`, `wait`, `BILLED`)
- `BILLED` lists the variables that would bill an API key or cloud account instead of the subscription login: Claude's key, token and cloud routes, and the keys Codex and Pi may read. `scrub` removes them under subscription billing.
- `spawn` gives each call its own run directory under `audit-runs`, its own temporary file directory and its own scratch home, and removes all three after, so concurrent calls never share one. `Subprocess::scratch` receives a path of the call's own that does not exist yet.
- `spawn` counts a capped call killed at `JUDGE_TIMEOUT` at its cap, the most it could have spent.
- When `Subprocess::scratch` fails, `spawn` cleans up best-effort and reports the scratch error rather than any cleanup error.
- `SCHEMA_FILE` and `RESULT_FILE` must be whole argv entries, while `INPUT_FILE` may sit inside one (Pi's `@<input file>`), so `spawn` matches `INPUT_FILE` by substring.
- `prompted` sends the prompt, the framed answer schema and the input as one text, for the command and http judges, which have no schema flag of their own.
- `wait` sends SIGTERM and waits `KILL_GRACE` before SIGKILL, because launchers such as Codex's npm shim forward SIGTERM to the worker and SIGKILL would orphan it. It reads the pipes on detached threads with a one-second post-exit timeout, so a grandchild holding a pipe costs its output, not a hang.
- `wait` ignores a failed stdin write, because a judge that exits without reading its input says why in its own output.
- SAFETY: `wait` calls `kill(2)` only on its own unreaped child, so the pid cannot have been reused.

### Claude judge (`Claude`)
- `Claude::login` runs `claude auth status` in a fresh `run_dir` with `BILLED` scrubbed, the same conditions the judge calls get, so it checks the login those calls will use. It removes that directory after, and accepts only the `claude.ai` and `oauth_token` methods.
- `Claude::reply` counts a call that reported no cost at its cap, so the budget still binds. It stops an `api_error` reply at the reported cost or $0, a reply that is not JSON at $0, and a call killed by a signal at its cap.

### Codex judge (`Codex`)
- `Codex::argv` disables `shell_tool`, `view_image`, `multi_agent` and web search. The read-only sandbox blocks only writes and the network, and those tools would read any file the user can or carry the input out. Subagents of a v2 model keep their own, with the same tool set.
- `Codex::scratch` makes `CODEX_HOME` beside the run directory under the data directory, holding only a symlink to `auth.json`. It never uses the system temporary directory, because Codex refuses to create its helper binaries there.
- `Codex::login` reads `auth.json` from the configured Codex home, not `~/.codex`. It requires a ChatGPT login, and refuses with the reason when the file is unreadable or not JSON.

### Http judge (`Http`)
- `Http` keeps the API key in-process. It sends the key only as a Bearer token and redacts it from error text, so nothing mazo writes or prints carries it.
- `Http::call` stops the run on a connection failure or non-2xx status, and retries on timeout or a reply without a JSON answer. It ignores cancel, because an in-flight request holds no process to orphan.

### Framing (`NONCE`, `frame`, `enclose`)
- `NONCE` is 64 bits from a `RandomState`'s OS-seeded keys, fresh per process, and suffixes every frame's tag name. `frame` replaces `NONCE` in the framed text with `[nonce]`, so no input can close its frame or forge another. `enclose` takes only nested frames or text already cut by `frame`, so the nonce never appears in an untrusted body.

### Stage 1 selection and input (`select`, `input`)
- `select` takes sessions with a strong main or headless load of the skill that have no session record, newest first and round-robin across projects up to the limit. The n-th newest session of every project comes before any project's (n+1)-th, with projects ordered by their newest session. A dropped session writes no record, so the next run judges it again.
- `select` condenses each session from its earliest strong load that still condenses. A session with none (expired, or a transcript no longer on disk) is skipped before the limit is counted. A typed load that points at `history.jsonl`, which holds no window, falls through to the session's later tool load.
- `select` pulls each round lazily with `take(room)`, so queues past the limit never pop and lose a session.
- `input` gives the judge the skill as it was when the session started (via `skill_at`), then the condensed session, each in its own frame.

### No-quote rule (`quoting`, `validate`)
- `quoting` flags the first answer field that shares `QUOTE_CHARS` consecutive characters with a user, assistant or result record, or that contains a whole user message whose length falls in `SHORT_MESSAGE`. The quote it reports runs on to at most `SHARED_CHARS`.
- `normalize` makes session text inert, as the checked answer already is, then lowercases it and collapses whitespace, so quoting compares like with like and catches recased or respaced copies.

### Stage 1 run (`judge_sessions`, `Run`)
- `judge_sessions` appends each valid answer to `audits.jsonl` at once. A judge that fails without an answer stops the run: records already written stay, and stage 2 does not run.
- When `Run::stopped` is set, `Run::left` starts at the session whose judge failed. A stage-2 stop leaves `Run::stopped` unset, so sessions the budget left stay reported as budget-left.
- `Run::ok` is false, and the run exits 1, when stage 1 dropped every session it judged, a judge stopped the run, or stage 2 was due and wrote no record.

### Stage 2 (`synthesize`, `Rates`, `SkillReport`)
- `synthesize` reports over every session record of the skill, not only the ones the run added. `run` calls it only when stage 1 wrote a record or `--resynth` is set, and `calls` starts no judge call when nothing is new and `--resynth` is absent.
- `synthesize` caps stage 2 at the whole budget minus what stage 1 spent. Under a cent it returns `Synthesis::Skipped`, which never occurs without a budget, and the run exits 1. A judge that fails without an answer stops the run as in stage 1.
- `Rates` are computed by mazo from session records, never taken from the judge. `SKILL_SCHEMA` refuses judge-supplied counts.
- `SkillReport::improvements` is ranked most important first.
- `SkillReport::for` is optional with a serde default because skill records written before the field lack it. A new answer must still carry it: `check_for_try` refuses a missing `for` or more than `TRIES` try items.
- `synthesis_input` leaves out any grade or retro file that no longer reads or parses, rather than failing.

### Records (`append`, `JudgeRecord`, `latest`)
- `append` writes audit records under the ledger lock, `scan.lock`.
- `audits.jsonl` is append-only. `latest` keeps the last skill record per skill in file order, and older skill records stay in the file.
- `JudgeRecord::billing` is optional with a serde default because records written before mazo recorded billing lack it. `None` means unknown.

### Vet (`vet`)
- `vet` caps a cost-reporting judge at `DEFAULT_BUDGET`, and retries an invalid answer once.

### Scan (`Classifier`)
- `Classifier::event` drops a session whose cwd lies under `audit-runs`, because the judge's own runs are never usage. The same transcript from any other project counts.

### Display (`usd`, `dry_run`)
- `usd` labels dollar figures "API-equivalent" under subscription billing, because there they are Claude's client-side estimate, not a charge.
- `dry_run` estimates tokens at four condensed characters each. `estimate_cost` prices a run at the cost per condensed character of earlier session records.

### Audit tests (`tests/audit.rs`)
- `Fixture` stubs `claude`, `codex` and `pi`. Each call records its argv, cwd, directory listing, stdin and named files under `calls/<n>/`, runs `scripts/<n>`, and answers from `replies/<n>`, `replies/skill` or `replies/default`. `claude auth status` is recorded separately and defaults to a claude.ai login.
- `Fixture::mazo_env` unsets every variable in `PROXIES`, in both cases, so an http judge reaches its local stub server directly.
- The unreachable-endpoint test binds port 0 and releases it, to get a local port that nothing listens on.
- The `wait` test's launcher reaps its worker itself, so `kill -0` cannot mistake an unreaped zombie for a live worker.

### Live tests (`tests/live.rs`)
- The live tests are `#[ignore]`d and run with `cargo test --test live -- --ignored`. The three gating tests share one run through `gate`. `assert_recorded` tries up to three times and reports skipped when the model never acts.
- `gate` builds the gating run once per test binary in a `OnceLock` and catches its panic, so a failed setup fails every gating test without rerunning the agents.
- The `LIVE` mutex serializes every agent run, because the probes link the real credentials and two agents refreshing one token at once could corrupt it.
- `run` spawns the agent in its own process group, so a timeout kill reaches every process it started. It feeds stdout and stderr into one channel, so every wait uses `recv_timeout` and stays bounded. It stops waiting at a failed `response` line.
- `run` ignores stdin write errors, because a failed write surfaces as a missing event in the assertions. It closes stdin so an RPC agent can exit on its own, kills whatever still runs after a grace period, then drains the channel for at most one second because a grandchild may hold the pipes open.
- SAFETY: `libc::kill` in `run` takes no pointers, and the negated child pid targets the child's process group.
- `codex` passes `--skip-git-repo-check` because the probe work directory is not a git repository.
- `pi` sends one RPC prompt with session persistence on, answered by the faux `probe` provider rather than a real model.
