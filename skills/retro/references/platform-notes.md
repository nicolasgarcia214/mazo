# Platform Notes

The retro skill runs on Claude Code, Codex and Pi. The evaluation workflow, rubric, report format, and persistence contract are identical everywhere. Five mechanics differ.

## 1. Invocation Markers (Phase 0 detection)

When scanning the conversation for which skill previously ran:

- **Claude Code**: look for `<command-name>` tags, `/skill-name` slash-command patterns, and Skill tool calls.
- **Codex**: look for `$skill-name` mentions in user prompts and injected skill content (a SKILL.md body pasted into the context by the harness).

- **Pi**: look for `/skill:name` commands or expanded `<skill name="..." location="...">` blocks. Arguments follow the expanded block. Prefer the supplied location for spec lookup. The configured agent directory (default `~/.pi/agent`) and `~/.agents/skills/` are fallback skill roots. An invocation marker proves loading, not execution; reconstruct the actual tool turns.

The same applies to detecting how the retro itself was invoked (`/retro`, `$retro`, or `/skill:retro`).

## 2. Spot-Check Execution (Phase 1.5)

### Claude Code — subagent mode

Spawn one Agent per selected claim (subagent_type: `Explore`), all launched in parallel. Each prompt must:

- State the **exact claim** being verified (quote the subagent's output verbatim).
- Point the agent to the **specific file(s) and line range(s)** the claim references.
- Ask it to answer three questions:
  1. Does the referenced code exist at the stated location?
  2. Does the code behave as the claim describes?
  3. Is the claim's severity/impact assessment reasonable?
- Instruct the agent to return a short verdict — **CONFIRMED**, **PARTIALLY CONFIRMED**, **REFUTED**, or **UNVERIFIABLE** — with a 2-3 sentence justification.

### Pi — capability-based verification

Use authorized installed worker tools when available, with the same claim cap and
brief. Separate sessions provide context independence, not necessarily different
models. Follow actual schemas and report the achieved independence. Without those
tools, use direct mode below; a separately required independent audit stays pending.

### Direct mode

Whenever subagent spawning is unavailable or not permitted, on any platform, run the identical procedure yourself, sequentially:

- Same claim cap (up to 3) and same selection priorities.
- `Read` only the specific file(s) and line range(s) each claim references — reads stay strictly verification-scoped; never expand into re-doing the skill's work.
- Answer the same three questions and record the same verdict format.

Direct mode loses the independence of a separate verifier (the same model that summarized the trace also checks the claims). Note **"direct verification (no subagent isolation)"** in the report's Context Notes so scores remain interpretable across platforms.

## 3. Token Measurement (Phase 1 / D4)

- **Claude Code**: Agent results carry `<usage>` tags. Summing them gives exact subagent token totals — report these as measured. Orchestrator tokens come from `message.usage` on the span's assistant records when you sum them, and are estimated otherwise; say which. The trace clips results, so read the figures from the transcript, over `{span}`:
  ```
  sed -n '<first>,<last>p' {transcript} | jq -r 'select(.type=="user" or .type=="attachment") | tostring' | grep -o 'subagent_tokens[>: ]*[0-9]*' | grep -o '[0-9]*$' | sort -u
  ```
  Each number is one subagent's total. Other record types repeat the same tags, which is why the filter keeps two. An agent that reported more than once shows a figure per report: count its last one.
- **Codex, or any run without `<usage>` tags** (including Claude runs with no subagents): there is no exact measurement source. Set `tokens_measured` to "unavailable" in the report frontmatter, estimate all figures from transcript volume (turn count × average turn size), and label every number as an estimate. Score D4 on observable waste patterns — redundant reads, bloated prompts, abandoned analysis paths — rather than on totals.
- **Trend caution**: D4 scores built on estimates are not numerically comparable to measured runs. When prior retros of the same skill mix measurement bases, state the basis in the Score Trend section and do not flag D4 deltas across mixed bases as real regressions.

Pi session JSONL can contain `message.usage` for assistant turns and usage reported
by tools. Use those numbers only when the complete relevant session/branch and
worker records are available. Avoid double-counting inherited history, nested
usage and compaction totals; do not substitute the current session's total for a
single skill's cost. Otherwise use the unavailable-measurement fallback above.
Session format and workspace privacy are defined in the installed Pi documentation.

## 4. Subagent Detection (Phase 1 spec)

When determining whether the evaluated skill uses subagents:

- **Claude Code**: the Agent tool (historically named Task — treat "Task" in older skill specs as the same tool) in the skill's allowed-tools, Agent calls in the transcript, or any other delegated worker the run started, such as an agent CLI launched in a terminal pane. Workers started outside the Agent tool leave claims to spot-check but no `<usage>` tags.
- **Codex and Pi**: inspect actual delegated or parallel worker activity, such as installed subagent-tool calls, Herdr panes or separate sessions. A skill's `allowed-tools` field is not evidence that those tools existed or ran. If no delegation occurred, Phase 1.5 is skipped per its own rule.

On Claude Code a subagent's own transcript is `<{transcript} without .jsonl>/subagents/agent-<id>.jsonl`. Its last assistant record is the report it returned; read that when the trace clipped the result a spot-check needs.

## 5. Session Id (Phase 0.7)

`mazo evals trace` takes a session id that the transcript's file name holds, or the transcript's path.

- **Claude Code**: `$CLAUDE_CODE_SESSION_ID`. The transcript is `<id>.jsonl`.
- **Codex**: `$CODEX_THREAD_ID`. The rollout's file name ends with it.
- **Pi**: `$PI_SESSION_FILE`, the transcript's path.

A subagent's shell carries the same variable as its parent's. When the variable is unset or empty, the transcript cannot be found from inside the session: run in-context mode.
