# Auditing a skill

This guide covers `mazo audit`: how it judges a skill's real sessions, what it
costs, who pays, and what it prints. Back to the [README](../README.md).

## Grade, retro and audit

Three reviews judge a skill, each at a different time:

- **grade** is a static review of the SKILL.md: is it well written.
- **retro** judges one run in context, right after it ran.
- **audit** judges many past runs offline and takes the retros as input.

A retro feeds the audit rather than replacing it. The TUI's help (`H`) states
the same roles.

## How an audit runs

`mazo audit <skill>` sends each session not yet audited to one call of the
[`[judge]`](configuration.md#judge) agent, newest first and round-robin across
projects. It sends at most `--limit` sessions (default 20), or every one with
`--all`.

- Claude and Pi run with tools off.
- Codex runs without its shell, image viewer or web search, in a read-only
  sandbox with a scratch `CODEX_HOME`. Its v1 subagents are off too; a model
  on v2 keeps its own subagents, with the same tools off.
- A command judge gets the prompt, the answer schema and the input on stdin,
  and its stdout must be the answer as JSON.
- An http judge gets the same text as one user message, and its reply's
  `choices[0].message.content` must be the answer as JSON. A connection
  failure or a non-2xx status stops the run, naming the status and the
  endpoint.

Every call runs in an empty directory under the data directory, and mazo writes
nothing inside the source.

mazo checks each session answer before it keeps it. The answer must match the
schema and its length caps, and every finding must cite a line that is in the
condensed session. No field may quote the session: a quote is 40 characters in
a row shared with a user message, an assistant message or a tool result, or a
whole user message of 12 to 39 characters. mazo retries an answer that fails a
check once, telling the judge what failed, then drops it. It appends each
answer that passes to `audits.jsonl`.

When the run wrote any session record, or with `--resynth`, one more call
writes the skill report over every session record of the skill. The report
holds a verdict, patterns, ranked improvements, curated examples, what the
skill is for, and up to three uses to try that are not among the uses the
session records show. mazo computes the rates `followed`, `achieved` and
`abandoned or redone` itself, as `n of N`.

## Budget

With the Claude judge, the session calls share 75% of `--max-budget-usd`
(default 1, at least 0.04) and the skill report gets the rest. No call starts
with less than a cent left. Codex, Pi, a command judge and an http judge report
no cost, so `--limit` is their only cap, `--max-budget-usd` is a usage error,
and their records' `cost_usd` is null.

## Who pays

`[judge] billing` is `"subscription"`, the default, or `"api"`. Any other value
is a config error.

Under `"subscription"`, every judge call runs without `ANTHROPIC_API_KEY`,
`ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
`CLAUDE_CODE_USE_FOUNDRY`, `OPENAI_API_KEY` and `CODEX_API_KEY`. Before its
first call the run checks the login once, with the same variables removed, and
exits 2 when the check fails:

- `claude auth status` must report a claude.ai login or an OAuth token. An
  `apiKeyHelper` fails, since no environment change removes it.
- The Codex home's `auth.json`, else `~/.codex/auth.json`, must hold a ChatGPT
  login (`auth_mode` `chatgpt`).
- mazo does not check Pi. Pi picks its provider from its own settings, and a
  provider that logs in, such as OpenRouter, can still bill per token. Check
  the provider and model that `--dry-run` prints.
- mazo does not check a command judge either. It only removes the variables
  above from its environment.
- An http judge runs no process, so `billing` changes nothing for it.

A dry run, or a run with nothing to judge, starts no call and checks nothing.

mazo labels a Claude dollar figure `API-equivalent`. It is Claude's own
estimate, not a charge, and `--max-budget-usd` still caps it. A run stopped by
a judge failure names `billing = "api"` as the way to use an API key instead.

Under `"api"` the environment passes unchanged and nothing is checked.

Each record in `audits.jsonl` names its billing under `judge.billing`. A
record written before mazo recorded it has none, and the card's audit line
shows its billing as `unknown`.

## Dry run

`--dry-run` lists the sessions with their agent, project and condensed size. It
also prints:

- the judge command: a command judge's `argv`, or an http judge's endpoint and
  key variable;
- the billing;
- for Pi and http, the model;
- an input-token estimate and, once records exist, a dollar estimate.

It starts no judge.

## Audit exit codes

A run exits 1 when any of these happens:

- it drops every session it judged;
- it drops or skips the skill report;
- it meets a judge that fails without an answer, from a login, key or network
  failure or a signal mazo did not send.

A judge that fails without an answer stops the run at once. mazo keeps the
records already written and names the sessions left for the next run, as it
does when the budget runs out.

## JSON output

`--json` prints the new skill record, the session records written and dropped,
the spend and the sessions left. The spend is null for every judge but Claude,
since Codex, Pi, a command judge and an http judge report no cost.
