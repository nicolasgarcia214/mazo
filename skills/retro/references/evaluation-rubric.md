# Evaluation Rubric

This rubric defines the seven dimensions used to evaluate a skill's execution. Each dimension is scored 1-5 with defined criteria and evidence requirements.

## Score Labels

| Score | Label |
|-------|-------|
| 5 | Optimal |
| 4 | Strong |
| 3 | Adequate |
| 2 | Weak |
| 1 | Failed |

## Dimension Weights

Dimensions are grouped into four tiers reflecting their relative importance: **Outcome** (did it work and is it good?), **Process** (was the execution disciplined?), **Efficiency** (was it economical?), and **Integrity** (does the learning it produces generalize?). The weighted composite prioritizes delivering correct results over doing so efficiently — a skill that achieves everything sloppily scores higher than one that achieves nothing efficiently.

| Dimension | Weight | Tier |
|-----------|--------|------|
| D1: Goal Achievement | 2.5 | Outcome |
| D5: Output Quality | 2.0 | Outcome |
| D2: Workflow Discipline | 1.5 | Process |
| D3: Tool Efficiency | 1.5 | Process |
| D6: Error Handling | 1.5 | Process |
| D4: Token Economy | 1.0 | Efficiency |
| D7: Knowledge Generalizability | 1.0 | Integrity |

**Weighted composite formula**: `sum(score_i × weight_i) / 11`, rounded to 1 decimal. Weights sum to 11. Verdict mapping uses the weighted composite score.

## D1: Goal Achievement

**What it measures**: Did the skill accomplish its stated objectives per the SKILL.md output contract?

| Score | Criteria |
|-------|----------|
| 5 | All output contract items delivered completely. Output exceeds specification quality. |
| 4 | All major outputs delivered. Minor gaps or imperfections that don't affect usefulness. |
| 3 | Core output delivered but with notable gaps. Some contract items missing or incomplete. |
| 2 | Partial output only. Key deliverables missing or substantially incomplete. |
| 1 | Output contract not met. Skill failed to produce its primary deliverable. |

**Evidence required**: List each output contract item from SKILL.md and whether it was delivered. Cite the specific output or its absence.

## D2: Workflow Discipline

**What it measures**: Did execution follow the documented phases, ordering, gates, and ground rules?

| Score | Criteria |
|-------|----------|
| 5 | All phases executed in order. All gates respected. All ground rules followed. |
| 4 | Phases followed with minor reordering that didn't affect quality. All gates respected. |
| 3 | Most phases followed. One gate bypassed or one ground rule violated without severe impact. |
| 2 | Multiple phases skipped or reordered. Gates bypassed. Ground rules violated. |
| 1 | Workflow largely ignored. Execution bears little resemblance to the specification. |

**Evidence required**: Map each SKILL.md phase to its execution status (executed/skipped/partial/reordered). For each gate, cite whether it was respected or bypassed.

## D3: Tool Efficiency

**What it measures**: Were tools used appropriately? Were there redundant calls, wrong tool choices, or missed opportunities for parallelism?

| Score | Criteria |
|-------|----------|
| 5 | Every tool call purposeful. Parallelism maximized. No redundant reads or searches. |
| 4 | Tools well-chosen overall. Minor inefficiencies (1-2 redundant calls or missed parallel opportunities). |
| 3 | Mostly appropriate tool usage. Some clear inefficiencies (repeated reads, grep-then-read-same-file, unnecessary bash). |
| 2 | Frequent tool misuse. Many redundant calls. Subagents spawned for work the orchestrator could have done (or vice versa). |
| 1 | Tools used inappropriately throughout. Massive redundancy or critical tools not used at all. |

**Evidence required**: Count total tool calls by type. Identify specific redundancies or missed parallelism opportunities. For subagent-based skills, assess prompt quality and delegation appropriateness.

## D4: Token Economy

**What it measures**: Were tokens well-spent relative to output quality? Was there prompt inflation, unnecessary verbosity, or wasteful patterns?

| Score | Criteria |
|-------|----------|
| 5 | Excellent token-to-value ratio. Lean prompts, focused subagent work, no wasted context. |
| 4 | Good efficiency. Minor prompt inflation or slightly verbose subagent prompts. |
| 3 | Acceptable but notable waste. Some subagents received bloated prompts or produced largely unused output. |
| 2 | Poor efficiency. Significant token waste from unfocused prompts, redundant context passing, or abandoned work paths. |
| 1 | Extreme waste. Majority of tokens spent on work that didn't contribute to the final output. |

**Evidence required**: Report measured token totals from `<usage>` tags when available. When measurement is unavailable (Codex, or a run with no subagents), state that explicitly and use labeled estimates instead. Note estimated orchestrator tokens. Identify specific waste patterns (e.g., large context passed to subagents that wasn't used, abandoned analysis paths, verbose prompts).

**Scoring guidance**: Scale expectations to skill complexity. A simple skill using 50k tokens is judged differently than a multi-subagent audit using 500k. Focus on the ratio of useful output to tokens consumed, not absolute numbers. When totals are unavailable, score on observable waste patterns rather than volume.

## D5: Output Quality

**What it measures**: Is the final output well-structured, accurate, actionable, and complete? For subagent-based skills, this includes whether subagent work product holds up under verification.

| Score | Criteria |
|-------|----------|
| 5 | Output is clear, well-structured, accurate, actionable, and complete. Spot-checked subagent claims all confirmed. No significant errors or omissions. |
| 4 | Output is good overall. Minor structural issues or small inaccuracies. Spot-checked claims mostly confirmed (≥2/3). |
| 3 | Output is usable but has notable quality issues. One spot-checked claim refuted or multiple partially confirmed. |
| 2 | Output quality is low. Multiple spot-checked claims refuted. Significant errors, poor organization, or key information missing. |
| 1 | Output is unusable or fundamentally flawed. Majority of spot-checked claims refuted. Major errors or critical omissions. |

**Evidence required**: Assess the final output against its intended purpose. Cite specific structural issues, inaccuracies, or quality highlights. For skills producing reports, evaluate sections individually.

**Subagent verification evidence** (when Phase 1.5 was executed):
- Report the confirmation rate (X/Y claims confirmed).
- For each refuted or partially confirmed claim: quote the claim, state the verification verdict, and assess whether it materially affected the final output.
- Note whether the orchestrator caught subagent errors before incorporating them.
- If Phase 1.5 was skipped (no subagents or compressed context), state "N/A — no subagent verification performed" and score based on output assessment alone.

## D6: Error Handling

**What it measures**: How were errors, unexpected inputs, and edge cases handled during execution?

| Score | Criteria |
|-------|----------|
| 5 | All errors handled gracefully. Recovery paths followed. Failures communicated clearly. No silent swallowing. |
| 4 | Errors handled well overall. One minor error not optimally addressed but no downstream impact. |
| 3 | Some errors handled, others ignored or poorly addressed. At least one error caused downstream quality issues. |
| 2 | Poor error handling. Multiple errors ignored or caused cascading issues. |
| 1 | Errors not handled at all. Failures silently swallowed or caused skill to produce invalid output. |

**Evidence required**: List each error encountered during execution (tool failures, script errors, unexpected results). For each, describe how it was handled and whether the handling was appropriate. If no errors occurred, score based on proactive edge-case handling (e.g., input validation, graceful handling of missing data).

**Failed command analysis evidence** (when Phase 1.7 was executed):
- Report the total count of failed commands and their root-cause category distribution.
- For each root cause: state whether the orchestrator correctly diagnosed the failure, whether recovery was attempted, and whether the fix was effective.
- Note any undiagnosed failures where the orchestrator silently moved on without understanding why a command failed.
- Note any cascading failure chains where an early failure caused multiple downstream failures that could have been prevented.
- If Phase 1.7 was skipped (no command failures), state "N/A — no command failures" and score based on proactive handling alone.

**Scoring guidance**: If the skill executed without any errors or edge cases, a score of 4 is appropriate (no opportunity to demonstrate error handling, but no failures either). Reserve 5 for executions that actively encountered and overcame problems. When failed commands occurred, the score should reflect both the *handling* of failures (did the orchestrator recover?) and the *diagnosis quality* (did it understand the root cause, or just retry blindly?)

## D7: Knowledge Generalizability

**What it measures**: Are the skill's outgoing recommendations root-cause abstractions that transfer to unseen contexts, and were prior recommendations implemented in a structural way (generalizable) or a reactive way (only recovers the specific incident that prompted them)?

| Score | Criteria |
|-------|----------|
| 5 | All outgoing recommendations are root-cause abstractions with no project-specific language in the prescription. All implemented prior recommendations are structural — they would catch variant bugs in unrelated codebases. |
| 4 | Recommendations mostly generalized; 1–2 contain project-specific language in the description but the underlying rule is transferable. Implemented prior recommendations are mostly structural. |
| 3 | Mix: some recommendations abstract the root cause, others directly copy the missed-finding's shape. At least one implemented prior recommendation would only re-find the exact incident that prompted it. |
| 2 | Most recommendations are benchmark-reactive — stated in terms of specific contracts, function names, or issue numbers. Implemented prior recommendations are narrowly scoped and would not transfer to a different codebase. |
| 1 | All recommendations function only as pattern-matching against the evaluated context. Prior implementations show no generalization — they are verbatim copies of the missed finding's surface form. |

**Evidence required**: For each outgoing recommendation, classify as **STRUCTURAL** (general rule applicable across unrelated codebases) or **REACTIVE** (only recovers the specific scenario that prompted it). For each prior recommendation marked ADDRESSED-STRUCTURAL or ADDRESSED-REACTIVE, state the evidence. Report the structural ratio (STRUCTURAL / total).

**Scoring guidance**: REACTIVE implementations count as negative evidence. A skill with a 100% closure rate on prior recommendations but 80% REACTIVE adoptions should not score above 2. The goal is durable improvement, not metric-gaming.
