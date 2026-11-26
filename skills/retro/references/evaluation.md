# Evaluation — Phases 1 to 3

You evaluate one run of a skill and write the retro report. You are here because the retro's orchestrator briefed you, or because you are that orchestrator in in-context mode. The brief names the evaluated skill and its SKILL.md, `{trace}`, `{transcript}`, `{span}`, the skill's `content_hash`, the Prior Retro Record and static baseline, `{report}`, and the platform.

## Ground Rules

- **The trace is the evidence.** `{trace}` holds the run, one or more lines per transcript record, each prefixed `L<n>: `: `user:` for what the user wrote, `assistant:` for the agent's text, `tool <name>:` for a call and `result:` for what came back. Calls are cut to 200 characters and results to 500. The whole record is line `n` of `{transcript}`; read it with `sed -n '<n>p' {transcript} | jq '.message.content'` when the clipped text is not enough, as a long command or a failed one's stderr often is not. Stay inside `{span}`: the rest of the session is other work. With no trace (in-context mode when it did not print, and the artifact-based fallback), your conversation context or the artifacts are the evidence, and wherever this file asks for `L<n>` ids you cite the event or the artifact in prose instead.
- **Cite line ids.** Every score and every recommendation names the `L<n>` lines it rests on. A reviewer checks them.
- **Code reading for verification only.** You may `Read` and `Grep` source files to spot-check subagent claims, and the evaluated skill's own files to tell a fault in its instructions from a fault in the run — never to re-do the skill's work.
- **Spot-checks confined to Phase 1.5.** Verification of subagent claims happens exclusively during Phase 1.5 — via spot-check subagents where the platform supports them, or directly by you otherwise.
- **Conservative under uncertainty.** If a phase of the run cannot be read from the evidence, note it and score conservatively rather than guessing.
- **Recommendations must generalize.** Recommendations must be written as root-cause abstractions or workflow rules, not as benchmark-shaped fixes. Project names, issue IDs, function names, or protocol-specific bug silhouettes may appear as evidence, but not as the recommendation itself unless the change is unavoidably scoped to a named file in the evaluated skill.

Platform mechanics (subagent detection, spot-check execution, token measurement) are in [platform-notes.md](platform-notes.md); read it at the first step that names it.

## Phase 1: Execution Trace Reconstruction

First `Read` the evaluated skill's SKILL.md and extract its **spec**:

- Workflow phases and their ordering
- Output contract (what the skill promises to produce)
- Ground rules and constraints
- Gate points (user gates, blocking decisions)
- Allowed tools list, if it declares one
- Whether subagents are used, as [platform-notes.md](platform-notes.md) §4 defines it

Then walk through `{trace}` in order and build a structured trace:

1. **Tool calls**: Catalog every tool call — type, purpose, success/failure.
2. **Subagent activity**: For each subagent spawn — prompt summary, expected output, actual output, and token usage from `<usage>` tags when the platform provides them.
3. **Phase mapping**: For each phase in the skill's SKILL.md workflow — mark as: executed / skipped / partial / reordered.
4. **Gate compliance**: For each documented user gate — mark as: respected / bypassed.
5. **Token measurement**: On Claude Code, sum all `<usage>` tags from Agent results — these are exact. If `<usage>` tags are absent (Codex, or a run with no subagents), set `tokens_measured` to "unavailable" and rely on labeled estimates — see [platform-notes.md](platform-notes.md) §3.
6. **Errors**: Note tool failures, script errors, unexpected results. For each Bash tool call failure, capture: the command (or summary), exit code / stderr snippet, whether it was retried, and whether it caused downstream failures.
7. **Context compression**: The transcript keeps the records from before a compaction, so the trace has them. A `user:` line that summarises earlier conversation is the harness's compaction summary, not the user's words. In in-context mode without a trace, note any truncation markers and which phases they hide.

Record this trace as structured notes — you will reference it throughout scoring.

## Phase 1.5: Subagent Work Spot-Check

**Skip this phase if the evaluated skill did not use subagents.**

This phase verifies whether subagent outputs hold up against the actual source code. The goal is sampling, not re-execution — catch systematic problems without re-doing the skill's work.

### Selection

1. From the Phase 1 trace, collect all subagent results that made concrete claims (findings, verdicts, code references, invariant assertions).
2. **Select up to 3 claims for verification**, prioritizing:
   - Highest severity or most consequential claims
   - Claims that influenced the final output most
   - Claims that appear suspicious (vague evidence, hedging language, references to nonexistent code)
3. If a skill had many subagents, sample across different subagents rather than concentrating checks on one.

### Verification

Each selected claim gets a focused verification pass answering three questions:

1. Does the referenced code exist at the stated location?
2. Does the code behave as the claim describes?
3. Is the claim's severity/impact assessment reasonable?

**When the cited location has moved** (a worktree gone, commits squashed or merged), check the claim where the change landed, such as `git show <merge-sha>:<path>`. A claim that reading cannot settle, such as a test count, is left out of the sample rather than marked UNVERIFIABLE.

**On Claude Code**: spawn one Agent (subagent_type: `Explore`) per selected claim and launch them in parallel. Each prompt must quote the exact claim, point to the specific file(s) and line range(s) it references, ask the three questions, and request the verdict format below. See [references/platform-notes.md](platform-notes.md) for the prompt shape.

**On Pi with authorized worker tools**: use their actual schemas for the same scoped verification passes and report the actual independence obtained.

**Whenever subagent spawning is unavailable or not permitted**: run the same verification passes yourself, sequentially. `Read` only the specific file(s) and line range(s) each claim references and answer the same three questions. The same claim cap (3) applies, and reads stay strictly verification-scoped — never expand into re-doing the skill's work. Note "direct verification (no subagent isolation)" in the report's Context Notes so scores stay comparable across platforms.

Either way, each pass returns a short verdict: **CONFIRMED**, **PARTIALLY CONFIRMED**, **REFUTED**, or **UNVERIFIABLE** — with a 2-3 sentence justification.

### Recording

For each checked claim, record:
- The original subagent claim (quoted)
- The verification verdict and justification
- Whether the orchestrator caught or missed the issue (if refuted)

Summarize the spot-check results:
- **Confirmation rate**: X/Y claims confirmed
- **Pattern notes**: Any systematic issues (e.g., all path references off-by-one, severity inflation, hallucinated function names)
- **Orchestrator filtering**: Did the orchestrator catch subagent errors before they reached the final output?

These results feed directly into D5 (Output Quality) and D3 (Tool Efficiency) scoring.

## Phase 1.7: Failed Command Analysis

**Skip this phase if no Bash tool calls failed during the evaluated skill's execution.**

This phase performs root-cause analysis on failed command executions and produces fix recommendations. It goes beyond simply noting that errors occurred (Phase 1) by diagnosing *why* commands failed and what would prevent the failure.

### Detection

1. From the Phase 1 trace, collect every Bash tool call that:
   - Returned a non-zero exit code
   - Produced stderr output indicating failure (compilation errors, missing dependencies, permission denied, syntax errors, command not found, etc.)
   - Was retried or abandoned after producing unexpected output
   - Timed out
2. Also collect non-Bash tool failures that stem from a prior command failure (e.g., a Read that fails because a script didn't produce its expected output file).

### Root-Cause Classification

For each failed command, classify the root cause into one of these categories:

| Category | Examples |
|----------|----------|
| **Environment** | Missing dependency, wrong runtime version, PATH issue, permissions |
| **Input** | Malformed input data, missing file, wrong file path, encoding issue |
| **Logic** | Script bug, incorrect regex, wrong flag/option, off-by-one |
| **Resource** | Timeout, OOM, disk full, rate limit |
| **External** | Network failure, API error, unavailable service |
| **Cascading** | Failure caused by a prior failed command (cite the upstream failure) |

For each failure, record:
- **Command**: The exact command or a shortened form if very long
- **Error signal**: Exit code, stderr snippet, or symptom
- **Root cause**: One-sentence diagnosis with the category
- **Cascading?**: Whether this failure caused downstream failures (list them)
- **Was it recovered?**: Whether the orchestrator detected and worked around the failure, and how

### Fix Recommendations

For each non-cascading root cause (cascading failures are fixed by fixing their upstream cause), produce a fix recommendation:

1. **Preventive fix**: What change would prevent this failure from occurring (e.g., add a dependency check, fix the script, validate input before passing it).
2. **Defensive fix**: If prevention isn't feasible, what guard or fallback should the skill add (e.g., retry with backoff, graceful degradation, pre-flight check).
3. **Scope**: Whether the fix belongs in the skill's SKILL.md, a reference script, the invoking workflow, or the environment setup.

### Recording

Store the analysis as structured notes. These feed into:
- **D6 scoring**: Root causes that were correctly diagnosed and recovered from during execution strengthen the score. Undiagnosed failures or silent swallowing weaken it.
- **Recommendations**: Each unresolved root cause becomes a candidate for the Phase 3 recommendations (Immediate tier if it blocked output, Structural if it's a design gap).
- **Report**: The analysis populates the `## Failed Command Analysis` section.

## Phase 2: Dimensional Evaluation

Read [evaluation-rubric.md](evaluation-rubric.md) now and apply its seven-dimension rubric. Score D1–D6 here. D7 is scored in Phase 3 step 5, because its evidence is the recommendations and their tracking statuses, which Phase 3 produces.

For each dimension:
1. Gather evidence from the execution trace.
2. Match evidence to the rubric criteria.
3. Assign a score (1-5) with explicit justification citing specific events.

## Phase 3: Synthesis & Report

1. **What went well**: Top 3 positives with evidence citations.
2. **What went wrong**: Issues with evidence and impact assessment.
3. **Recommendations** in three tiers:
   - **Immediate**: Changes for the next run of this skill
   - **Structural**: Skill design changes (SKILL.md, reference files, subagent prompts)
   - **Process**: Workflow or invocation improvements
   Recommendation rules:
   - Format every recommendation bullet as `- **Short title**: prescription Evidence: L<n>, L<n>.`
   - The evidence names the trace lines that show the failure or the missed opportunity. An observation with no line to cite is context for the sections above, not a recommendation.
   - Make the short title specific and stable enough to reuse as a GitHub issue title.
   - After the title, write 1-3 sentences that define the change and desired outcome. Avoid single-clause bullets that become ambiguous when promoted into issues.
   - State each recommendation as the behaviour the skill should produce on any target. When the change is to the evaluated skill's text, name the step or reference section it rewrites. Recommend a new step only when no existing one can carry the change, and name the step it follows. A fix that belongs in a script, the workflow or the environment names that instead.
   - Do not recommend copying a benchmark miss, target-project bug, or protocol-specific example into prompts.
   - If an observed failure came from a specific target, first state the underlying failure mode, then recommend the general fix for that failure mode.
   - Keep evidence separate from prescription. Example names or file paths belong in the supporting evidence, not in the recommendation sentence unless the recommendation is explicitly to edit that file in the evaluated skill.
   - If a fix cannot be stated as a general method, classify it as context/evidence, not as a recommendation.
   - Do not invent recommendations to fill tiers. If the skill executed cleanly, a tier may be empty or the full list may be short. Each recommendation must trace to concrete evidence of a real failure mode or missed opportunity — not to a desire for completeness, symmetry across tiers, or the appearance of thoroughness.
4. **Recommendation tracking** (skip if Prior Retro Record has no recommendations):
   - For each recommendation from the most recent prior retro, assess against the current execution trace.
   - Assign a status: **ADDRESSED-STRUCTURAL** / **ADDRESSED-REACTIVE** / **PARTIALLY ADDRESSED** / **NOT ADDRESSED** / **NOT APPLICABLE**.
     - **ADDRESSED-STRUCTURAL**: The fix generalizes — it would prevent the same class of failure in a different codebase or context, not just re-cover the specific incident.
     - **ADDRESSED-REACTIVE**: The fix is narrowly scoped to the specific scenario that prompted the recommendation. It would not transfer to a different codebase or context without further adaptation.
   - Each status must include an evidence citation (specific tool call, phase execution, or output reference).
   - ADDRESSED-REACTIVE counts as negative evidence in D7 scoring.
5. **D7 and composite score**: Score D7 from the statuses in step 4, or mark it `N/A` as [evaluation-rubric.md](evaluation-rubric.md) says when there is nothing to score it on. Classify this retro's own recommendations STRUCTURAL or REACTIVE in the report; that classification is a check, not a score. Then compute the weighted mean over the scored dimensions with the weights and formula the rubric defines, rounded to 1 decimal. Also compute the unweighted arithmetic mean of the scored dimensions as `score_raw` for backward compatibility.
6. **Verdict mapping**:
   - 4.5–5.0 → EXCELLENT
   - 3.5–4.4 → GOOD
   - 2.5–3.4 → ADEQUATE
   - 1.5–2.4 → POOR
   - 1.0–1.4 → FAILED
7. **Trend analysis** (skip if Prior Retro Record is empty):
   - Build a score trend table from prior frontmatter (`score`, `verdict`, `date`) plus the current retro's scores.
   - Classify the overall trend by comparing the current weighted score to the most recent prior score:
     - **Improving**: delta >= +0.3
     - **Declining**: delta <= -0.3
     - **Stable**: delta between -0.3 and +0.3
   - Flag any individual dimension with an absolute delta >= 1.0 point between the current and most recent prior retro.
   - If the token measurement basis differs between retros (measured `<usage>` totals vs. estimates), state the basis alongside the trend — D4 deltas across mixed bases are not directly comparable and must not be flagged as real regressions on their own.
   - If the `mode` differs between retros, state each retro's mode in the trend table and say that a delta across modes may be the judge changing, not the skill. A prior retro with no `mode` field ran in-context.
8. **Assemble the report** using the template from [report-template.md](report-template.md).
9. **Persist to disk**: write the report to `{report}`, with YAML frontmatter carrying the machine-readable metadata, the brief's `content_hash` and the `mode` (`fresh` when the orchestrator briefed you, `in-context` when you are the orchestrator) among it.
10. **Return**: the scorecard table and the top 3 recommendations, as a concise summary. In in-context mode, print them to the terminal.

## Edge Cases

| Situation | Handling |
|-----------|---------|
| Partially completed skill | Evaluate only executed phases, note incompleteness in report |
| Very simple skill (few tool calls) | Run all dimensions but acknowledge limited sample size |
| Subagent outputs fully compressed | Skip Phase 1.5, note in report. Score D5 conservatively |
| Failed command output compressed | Note in report, classify root cause as UNKNOWN with low confidence |
| Many failed commands (>10) | Group by root-cause category, analyze representative failures per category, note total count |
| All failures are cascading from one root cause | Analyze the root cause once, list cascading failures as downstream effects |
| Prior retro uses old format (no weighted score) | Use `score` field for trend comparison, note format difference |
| Prior retro frontmatter malformed | Skip that retro, note it, proceed with others |
| Prior recommendations too vague to track | Mark as NOT APPLICABLE with a note |
