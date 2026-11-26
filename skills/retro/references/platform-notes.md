# Platform Notes

The retro skill runs on Claude Code, Codex and Pi. The evaluation workflow, rubric, report format, and persistence contract are identical everywhere. Three mechanics differ here; invocation markers are in SKILL.md Phase 0 and spot-check execution in evaluation.md Phase 1.5.

## 1. Token Measurement (Phase 1 / D4)

- **Claude Code**: Agent results carry `<usage>` tags. Summing them gives exact subagent token totals — report these as measured. Orchestrator tokens come from `message.usage` on the span's assistant records when you sum them, and are estimated otherwise; say which. The trace clips results, so read the figures from the transcript, over `{span}`:
  ```
  sed -n '<first>,<last>p' {transcript} | jq -r 'select(.type=="user" or .type=="attachment") | tostring | scan("(?:<task-id>|agentId: )([a-z0-9]+).*?subagent_tokens[>: ]*([0-9]+)") | "\(.[0]) \(.[1])"' | awk '{t[$1]=$2} END{for(k in t){print k, t[k]; s+=t[k]} print "total", s}'
  ```
  Each line is one subagent's id and total, and the last line is their sum. Other record types repeat the same tags, which is why the filter keeps two. An agent that reported more than once shows a figure per report, and the `awk` keeps its last.
- **Codex, or any run without `<usage>` tags** (including Claude runs with no subagents): there is no exact measurement source. Set `tokens_measured` to "unavailable" in the report frontmatter, estimate all figures from transcript volume (turn count × average turn size), and label every number as an estimate. Score D4 on observable waste patterns — redundant reads, bloated prompts, abandoned analysis paths — rather than on totals.
- **Trend caution**: D4 scores built on estimates are not numerically comparable to measured runs. When prior retros of the same skill mix measurement bases, state the basis in the Score Trend section and do not flag D4 deltas across mixed bases as real regressions.

Pi session JSONL can contain `message.usage` for assistant turns and usage reported
by tools. Use those numbers only when the complete relevant session/branch and
worker records are available. Avoid double-counting inherited history, nested
usage and compaction totals; do not substitute the current session's total for a
single skill's cost. Otherwise use the unavailable-measurement fallback above.
Session format and workspace privacy are defined in the installed Pi documentation.

## 2. Subagent Detection (Phase 1 spec)

When determining whether the evaluated skill uses subagents:

- **Claude Code**: the Agent tool (historically named Task — treat "Task" in older skill specs as the same tool) in the skill's allowed-tools, Agent calls in the transcript, or any other delegated worker the run started, such as an agent CLI launched in a terminal pane. Workers started outside the Agent tool leave claims to spot-check but no `<usage>` tags.
- **Codex and Pi**: inspect actual delegated or parallel worker activity, such as installed subagent-tool calls, Herdr panes or separate sessions. A skill's `allowed-tools` field is not evidence that those tools existed or ran. If no delegation occurred, Phase 1.5 is skipped per its own rule.

On Claude Code a subagent's own transcript is `<{transcript} without .jsonl>/subagents/agent-<id>.jsonl`. Its last assistant record is the report it returned; read that when the trace clipped the result a spot-check needs.

## 3. Session Id (Phase 0.7)

`mazo evals trace` takes a session id that the transcript's file name holds, or the transcript's path.

- **Claude Code**: `$CLAUDE_CODE_SESSION_ID`. The transcript is `<id>.jsonl`.
- **Codex**: `$CODEX_THREAD_ID`. The rollout's file name ends with it.
- **Pi**: `$PI_SESSION_FILE`, the transcript's path.

A subagent's shell carries the same variable as its parent's. When the variable is unset or empty, the transcript cannot be found from inside the session: run in-context mode.
