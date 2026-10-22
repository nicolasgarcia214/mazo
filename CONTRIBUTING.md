# Contributing

This guide covers running the checks CI runs and adding support for a new
agent. Back to the [README](README.md).

## Checks

CI runs these on Linux and macOS:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
TZ=Pacific/Kiritimati cargo test --locked
TZ=Pacific/Pago_Pago cargo test --locked
```

The two `TZ` values are UTC+14 and UTC-11.

The suite includes 18 tests ported from the old `install.sh` and ingest
properties such as "a fresh scan equals an incremental one". The tests read the synthetic
skills tree in `tests/fixtures/skills`. Set `MAZO_TEST_SKILLS` to run them
against a real skills directory instead.

## Adding an agent

Each agent is:

- one module in `src/adapters/` implementing `Adapter`;
- one module in `src/audit/judges/` implementing `Backend`, with its arm in
  `judges::backend`;
- one `Agent` variant and one `JudgeAgent` variant.

Adapters only turn transcript records into events, prompts and condensed
sessions. `classify.rs` computes identity, actor, root session and authoring
once, for every agent.

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

A judge that is a process implements `Subprocess`, which gives it `Backend`;
a judge with no process, like `http`, implements `Backend` directly. A
`Subprocess` holds what differs between agents when mazo calls one:

```rust
pub trait Subprocess {
    /// The argv of a call, with stand-ins for the files the call writes.
    fn argv(&self, stage: &Stage) -> Vec<String>;
    /// What the call reads on stdin; none closes it.
    fn stdin(&self, stage: &Stage, input: &str) -> Option<String>;
    /// Under `subscription`, the login check made once before the first call.
    fn login(&self, data: &Path, cancel: &dyn Fn() -> bool) -> Result<()>;
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
named in the argv. `src/audit/judges/mod.rs` holds the shared parts: running
the call, its timeout, the billing scrub and the cleanup. A judge that bills an
API key through its own environment variable adds that variable to `BILLED`
there.
