---
name: retro
description: Perform a retrospective (retro, post-mortem) self-evaluation of a skill that already ran in this conversation. Use when the user asks for a retro, retrospective, or post-mortem of a skill run, or wants to evaluate, grade, or score how a skill execution went. Traces the execution against the skill's specification, scores seven weighted dimensions (goal achievement, workflow discipline, tool efficiency, token economy, output quality, error handling, knowledge generalizability), writes a report to the skill's retros directory under mazo's evals_dir, and can file the recommendations as GitHub issues on the skill's origin repository, or draft one issue for the user to file without write access. Run it in the same conversation as the evaluated skill when possible; when that conversation is gone, durable artifacts support a reduced cross-session retro.
argument-hint: "[optional: skill name to evaluate]"
allowed-tools: Read, Glob, Grep, Bash, Write, Edit, Agent
---

# Skill Retrospective — Evaluator

In Pi, read [the shared runtime contract](../writing-for-agents/references/pi-runtime.md)
before applying this skill; it defines tool, argument and delegation mapping.

You are a meta-evaluator that analyzes how well a previously-executed skill performed in this conversation. You trace execution against the skill's specification, score across seven dimensions, and produce actionable improvement recommendations.

## Ground Rules

- **Conversation context first.** Most evaluation data comes from this conversation's history.
- **Code reading for verification only.** You may `Read` and `Grep` source files strictly to spot-check subagent claims — never to re-do the skill's work.
- **Spot-checks confined to Phase 1.5.** Verification of subagent claims happens exclusively during Phase 1.5 — via spot-check subagents where the platform supports them, or directly by you otherwise. All other phases are single-agent.
- **Evidence-based scoring.** Every score must cite specific tool calls, outputs, or conversation events.
- **Conservative under uncertainty.** If context compression obscured a phase, note it and score conservatively rather than guessing.
- **No self-evaluation loops.** If asked to retro a retro with no prior retro execution, decline gracefully.
- **Recommendations must generalize.** Recommendations must be written as root-cause abstractions or workflow rules, not as benchmark-shaped fixes. Project names, issue IDs, function names, or protocol-specific bug silhouettes may appear as evidence, but not as the recommendation itself unless the change is unavoidably scoped to a named file in the evaluated skill.

## Platform Dispatch

This skill runs on Claude Code, Codex and Pi. Four mechanics differ and are dispatched inline where they occur: skill-invocation markers (Phase 0), subagent detection (Phase 0 step 7), spot-check execution (Phase 1.5), and token measurement (Phase 1 / Token Estimation Strategy). Full details live in [references/platform-notes.md](references/platform-notes.md) — read it when you hit the first dispatch point.

## Phase 0: Skill Identification & Spec Loading

1. Check the argument passed to the skill for an explicit skill name (on Claude Code this arrives as `$ARGUMENTS`; on Codex, it is the text following `$retro`; on Pi, the argument appended after the expanded skill).
2. If not provided, scan the conversation for skill invocation markers. On Claude Code: `<command-name>` tags, `/skill-name` patterns, or Skill tool calls. On Codex: `$skill-name` mentions or injected skill content. On Pi: `/skill:name` or a `<skill name="..." location="...">` block; loading instructions alone does not prove the workflow ran.
3. If **multiple skills** detected — list them and ask the user which to evaluate. Stop and wait.
4. If **no skill** detected — inform the user that no prior skill execution was found in this conversation and halt, unless the user is pointing at durable run artifacts and the Optional issue-swarm and resolve-issue section below is active, in which case run its Artifact-Based Fallback.
5. If a retro of `retro` is requested and no prior retro execution exists — decline: "No prior retro execution found to evaluate."
6. Resolve the evaluated skill's SKILL.md path. Prefer the `location` supplied by the evaluated invocation. Otherwise search these roots, without a filesystem-wide search:
   - Project-local: `skills/`, `.claude/skills/`, `.pi/skills/`, `.agents/skills/`
   - User-level: the configured Pi agent directory's `skills/` (default `~/.pi/agent/skills/`), `~/.agents/skills/`, `~/.claude/skills/`, `~/.codex/skills/`

   If multiple candidates share the name, prefer the one whose content matches the skill text loaded in this conversation; if still ambiguous, ask the user. Then `Read` it.
7. Extract from the SKILL.md:
   - Workflow phases and their ordering
   - Output contract (what the skill promises to produce)
   - Ground rules and constraints
   - Gate points (user gates, blocking decisions)
   - Allowed tools list
   - Whether subagents are used (Agent — a.k.a. Task — tool in allowed-tools, or delegated worker activity in the transcript)
8. Run `mazo show <skill-name> --json` and keep these fields as the **Skill Record**: `content_hash` (written into the report), `origin` (Phase 4's target repository; `null` for a local-only skill), and the newest-first `grades` and `retros` arrays (Phase 0.5). If it exits non-zero, mazo does not know the skill: run the retro, print the report without persisting it, and skip Phase 4 with that reason.

## Phase 0.5: Prior Retro Scan

Scan for prior retrospective reports to enable trend analysis and recommendation tracking.

1. Determine the retro directory: run `mazo evals dir <skill-name>`. It prints two tab-separated lines, `grades<TAB><path>` and `retros<TAB><path>`; `{retros-dir}` is the path on the `retros` line. It sits under the user's `evals_dir`, keyed by the evaluated skill's name, so a retro of a skill installed from elsewhere never writes into a third party's package.
2. Take the prior reports from the Skill Record's `retros` array, which mazo has already sorted newest first and filtered to files with a dated frontmatter.
3. If **empty** → note "First retro for this skill" in the trace. Skip to Phase 1.
4. If **non-empty** → read the YAML frontmatter of the `file` of the **3 most recent** entries. From the **most recent only**, also read the `## Recommendations` section. Store as the Prior Retro Record.

**Token budget**: Frontmatter-only reads for the 2nd and 3rd most recent. Capped at 3 files total.

5. Also take the first entry of the Skill Record's `grades` array (static evaluations written by the `grade` skill). If there is one, read its `file`'s frontmatter and verdict line as the **static baseline**: note in the report where runtime behavior confirmed or contradicted the static grade (e.g., a dimension graded strong that failed in execution). When its `stale` is `true`, the skill changed after it was graded: say so beside every comparison. Cap: 1 file, skip silently if none.

## Phase 1: Execution Trace Reconstruction

Walk through the conversation chronologically and build a structured trace:

1. **Tool calls**: Catalog every tool call — type, purpose, success/failure.
2. **Subagent activity**: For each subagent spawn — prompt summary, expected output, actual output, and token usage from `<usage>` tags when the platform provides them.
3. **Phase mapping**: For each phase in the skill's SKILL.md workflow — mark as: executed / skipped / partial / reordered.
4. **Gate compliance**: For each documented user gate — mark as: respected / bypassed.
5. **Token measurement**: On Claude Code, sum all `<usage>` tags from Agent results — these are exact. If `<usage>` tags are absent (Codex, or a run with no subagents), set `tokens_measured` to "unavailable" and rely on labeled estimates — see Token Estimation Strategy below.
6. **Errors**: Note tool failures, script errors, unexpected results. For each Bash tool call failure, capture: the command (or summary), exit code / stderr snippet, whether it was retried, and whether it caused downstream failures.
7. **Context compression**: Note any markers indicating conversation truncation and which phases are affected.

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

**On Claude Code**: spawn one Agent (subagent_type: `Explore`) per selected claim and launch them in parallel. Each prompt must quote the exact claim, point to the specific file(s) and line range(s) it references, ask the three questions, and request the verdict format below. See [references/platform-notes.md](references/platform-notes.md) for the prompt shape.

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

Read [references/evaluation-rubric.md](references/evaluation-rubric.md) now. Apply the seven-dimension rubric:

| Dimension | What it measures |
|-----------|-----------------|
| D1: Goal Achievement | Did the skill accomplish its stated objectives per SKILL.md output contract? |
| D2: Workflow Discipline | Did execution follow documented phases, ordering, gates, and rules? |
| D3: Tool Efficiency | Were tools used appropriately? Redundant calls? Right tool for the job? |
| D4: Token Economy | Token budget well-spent relative to output quality? Prompt inflation? |
| D5: Output Quality | Is the final output well-structured, accurate, actionable, complete? Includes subagent output verification. |
| D6: Error Handling | How were errors, unexpected inputs, and edge cases handled? |
| D7: Knowledge Generalizability | Are outgoing recommendations root-cause abstractions, and were implemented prior recommendations structural or reactive? |

For each dimension:
1. Gather evidence from the execution trace.
2. Match evidence to the rubric criteria.
3. Assign a score (1-5) with explicit justification citing specific events.

## Phase 3: Synthesis & Report

1. **Composite score**: Weighted mean using the dimension weights and the `sum(score_i × weight_i) / 11` formula defined in [references/evaluation-rubric.md](references/evaluation-rubric.md), rounded to 1 decimal. Also compute the unweighted arithmetic mean as `score_raw` for backward compatibility.
2. **Verdict mapping**:
   - 4.5–5.0 → EXCELLENT
   - 3.5–4.4 → GOOD
   - 2.5–3.4 → ADEQUATE
   - 1.5–2.4 → POOR
   - 1.0–1.4 → FAILED
3. **What went well**: Top 3 positives with evidence citations.
4. **What went wrong**: Issues with evidence and impact assessment.
5. **Recommendations** in three tiers:
   - **Immediate**: Changes for the next run of this skill
   - **Structural**: Skill design changes (SKILL.md, reference files, subagent prompts)
   - **Process**: Workflow or invocation improvements
   Recommendation rules:
   - Format every recommendation bullet as `- **Short title**: prescription`.
   - Make the short title specific and stable enough to reuse as a GitHub issue title.
   - After the title, write 1-3 sentences that define the change and desired outcome. Avoid single-clause bullets that become ambiguous when promoted into issues.
   - Phrase each recommendation as a reusable control, invariant, or process rule.
   - Do not recommend copying a benchmark miss, target-project bug, or protocol-specific example into prompts.
   - If an observed failure came from a specific target, first state the underlying failure mode, then recommend the general fix for that failure mode.
   - Keep evidence separate from prescription. Example names or file paths belong in the supporting evidence, not in the recommendation sentence unless the recommendation is explicitly to edit that file in the evaluated skill.
   - If a fix cannot be stated as a general method, classify it as context/evidence, not as a recommendation.
   - Do not invent recommendations to fill tiers. If the skill executed cleanly, a tier may be empty or the full list may be short. Each recommendation must trace to concrete evidence of a real failure mode or missed opportunity — not to a desire for completeness, symmetry across tiers, or the appearance of thoroughness.
6. **Trend analysis** (skip if Prior Retro Record is empty):
   - Build a score trend table from prior frontmatter (`score`, `verdict`, `date`) plus the current retro's scores.
   - Classify the overall trend by comparing the current weighted score to the most recent prior score:
     - **Improving**: delta >= +0.3
     - **Declining**: delta <= -0.3
     - **Stable**: delta between -0.3 and +0.3
   - Flag any individual dimension with an absolute delta >= 1.0 point between the current and most recent prior retro.
   - If the token measurement basis differs between retros (measured `<usage>` totals vs. estimates), state the basis alongside the trend — D4 deltas across mixed bases are not directly comparable and must not be flagged as real regressions on their own.
7. **Recommendation tracking** (skip if Prior Retro Record has no recommendations):
   - For each recommendation from the most recent prior retro, assess against the current execution trace.
   - Assign a status: **ADDRESSED-STRUCTURAL** / **ADDRESSED-REACTIVE** / **PARTIALLY ADDRESSED** / **NOT ADDRESSED** / **NOT APPLICABLE**.
     - **ADDRESSED-STRUCTURAL**: The fix generalizes — it would prevent the same class of failure in a different codebase or context, not just re-cover the specific incident.
     - **ADDRESSED-REACTIVE**: The fix is narrowly scoped to the specific scenario that prompted the recommendation. It would not transfer to a different codebase or context without further adaptation.
   - Each status must include an evidence citation (specific tool call, phase execution, or output reference).
   - ADDRESSED-REACTIVE counts as negative evidence in D7 scoring.
8. **Assemble the report** using the template from [references/report-template.md](references/report-template.md).
9. **Persist to disk**:
   - **Directory**: `{retros-dir}`, resolved in Phase 0.5 with `mazo evals dir`.
   - **Filename**: `retro-{context-slug}-{YYYY-MM-DD}.md` where `{context-slug}` is a short (1-3 words, underscores) identifier derived from the task context (e.g., the target project, audit name, or key subject). Examples: `retro-uniswap_audit-2026-10-12.md`, `retro-login_refactor-2026-03-20.md`. If context is ambiguous, ask the user for a short label.
   - Collision handling: if slug exists, append `-2`, `-3`, etc.
   - Include YAML frontmatter with machine-readable metadata, including the Skill Record's `content_hash`
   - Create the directory if it doesn't exist
10. **Print to terminal**: The scorecard table + top 3 recommendations as a concise summary.

## Phase 4: GitHub Issue Filing

This phase converts retro recommendations into GitHub issues on the repository the evaluated skill came from. It runs after the report is written to disk and skips gracefully when there is nowhere to file.

Read [references/github-issues.md](references/github-issues.md) now — it defines the label set, issue-body templates, body-content requirements, and report-update mechanics. Do not improvise labels or issue bodies.

### Step 4.1 — Repository Detection

1. Take `origin` from the Skill Record. If it is `null`, print `Phase 4 skipped: {skill-name} has no GitHub origin (local-only skill)` and end Phase 4.
2. `origin.repo` is `https://github.com/{owner}/{name}.git`; strip the `https://github.com/` prefix and the `.git` suffix and store `{owner}/{name}` as `{repo}`.
3. Run:
   ```
   gh repo view {repo} --json nameWithOwner,hasIssuesEnabled,viewerPermission
   ```
4. If the command fails (`gh` not installed or not authenticated, repository gone) or `hasIssuesEnabled` is `false`:
   - Print: `Phase 4 skipped: {reason}` with a specific diagnostic.
   - End Phase 4. The retro is complete without issues.
5. Set the **filing mode**: **write** when `viewerPermission` is `WRITE`, `MAINTAIN` or `ADMIN`; **propose** otherwise.
6. Run `git ls-remote {origin.repo} HEAD` and keep its first field as `{upstream-head}`, the upstream default-branch HEAD. If it fails, use `unknown`.

### Step 4.2 — Recommendation Parsing

From the Phase 3 report (already in memory), extract for each recommendation bullet: **tier**, **title**, **prescription**, **failure summary**, **supporting evidence**, **likely touchpoints**, **acceptance criteria**, and **category label** — following the field definitions and category-keyword rules in [references/github-issues.md](references/github-issues.md).

In write mode each issue gets three labels: `retro` + tier label (`retro:immediate` / `retro:structural` / `retro:process`) + category label. Propose mode uses no labels.

### Step 4.3 — Duplicate Check

For every issue title this phase would file or propose (write mode: `[retro] {title}` per recommendation; propose mode: the same per-recommendation titles plus the consolidated title from [references/github-issues.md](references/github-issues.md)), list the origin's open issues:

```
gh issue list -R {repo} --state open --search "{title} in:title" --json number,title,url
```

A result whose title equals the candidate title exactly, ignoring case, is a duplicate: record its URL, and link it instead of filing or proposing that recommendation. Search hits that differ in any other way are not duplicates.

### Step 4.4 — User Gate

Present a summary table before creating anything:

```
Phase 4: GitHub Issue Filing
Repository: {repo} ({write | propose: no write access})
Installed commit: {origin.commit}  Upstream HEAD: {upstream-head}

| # | Title | Tier | Labels | Duplicate |
|---|-------|------|--------|-----------|
| 1 | Fix report rendering | Immediate | retro, retro:immediate, bug | — |
| 2 | Cap token budget | Structural | — | #17 |
| ... | ... | ... | ... | ... |

(a) Create all · (b) Select specific · (s) Skip
```

In propose mode the Labels column is `—` and (a) reads `Draft all`. When the installed commit differs from the upstream HEAD, the problem may already be fixed upstream: say so under the table.

Wait for user input. This gate is **mandatory** — issues are externally visible artifacts.

- **(a)**: Proceed with all non-duplicate recommendations.
- **(b)**: Ask the user to specify issue numbers (e.g., "1,3,5"), then filter the list.
- **(s)**: Print `Phase 4 skipped by user.` and end.

### Step 4.5 — Issue Creation (write mode)

Create (or update) the four required labels using `gh label create --force -R {repo}`, with the exact names, colors, and descriptions defined in [references/github-issues.md](references/github-issues.md). If any label creation fails, log the warning but continue — issue creation can proceed without the tier label.

For each selected recommendation, create an issue with `gh issue create` using the command template, issue-body template, and body-content requirements from [references/github-issues.md](references/github-issues.md). The issue must be self-contained and understandable without opening the retro report.

Capture the returned issue URL and number. If an individual issue creation fails, log the error and continue with the remaining issues.

### Step 4.6 — Issue Proposal (propose mode)

Create no labels and no issues. Write one consolidated issue covering every selected recommendation, using the consolidated template in [references/github-issues.md](references/github-issues.md):

1. Write its body into the retro report under `## Proposed Issue`, and the same body to `{report-stem}.issue.md` next to the report.
2. Print the command for the user to run:
   ```
   gh issue create -R {repo} --title "{consolidated title}" --body-file {retros-dir}/{report-stem}.issue.md
   ```

### Step 4.7 — Report Update

Update the retro report written in Phase 3 step 9 (use `Edit` for targeted changes), per the mechanics in [references/github-issues.md](references/github-issues.md):

1. **Inline annotation**: Append `[TRACKED](issue-url)` to each recommendation bullet that was filed or matched a duplicate.
2. **Summary table**: In write mode, insert a `## Filed Issues` section after `## Recommendations`.
3. **Frontmatter**: Add `github_repo` and `github_issues` (filed and duplicate issue numbers).

### Step 4.8 — Terminal Summary

Print:
```
Phase 4 complete: Filed {N} issues on {repo}
  - https://github.com/owner/repo/issues/42
  - ...
```

In propose mode, print `Phase 4 complete: drafted 1 issue for {repo}` followed by the `gh issue create` command from Step 4.6.

## Optional: issue-swarm and resolve-issue

Run this section only when both `mazo show issue-swarm` and `mazo show resolve-issue` exit 0; otherwise skip it silently.

### Close The Flywheel

When issues were filed, offer the follow-up explicitly: the `issue-swarm` skill can burn down the retro backlog with `ISSUE_SCOPE=label:retro` (or a single tier, e.g. `label:retro:immediate`), and a single filed issue can go straight to `resolve-issue`. Do not launch either yourself — filing was the retro's job; fixing is a new, user-initiated run.

### Artifact-Based Fallback (cross-session)

The transcript-based flow above requires the evaluated run in this conversation. When the user asks to retro a run whose conversation is gone but which left durable artifacts — Resolution Records v1 under `.git/resolve-issue/` (from `resolve-issue`), an issue-swarm state dir under `.git/issue-swarm/` (state.json, prompts/, validation/), or `.goal/` artifacts — run a reduced-scope retro from those artifacts instead of halting:

1. Read the artifacts; they define what was claimed (contracts, verdicts, verification results, publication states, state transitions).
2. Spot-check claims against the current repository exactly as in Phase 1.5 (same cap of 3).
3. Score only what artifacts support: D1 (claimed outcomes vs re-checked evidence), D5 (artifact completeness and accuracy), and D2 only where the artifact trail shows phase/gate compliance (e.g., swarm state transitions, record presence for every terminal task). Mark D3, D4, D6, D7 `N/A — no transcript` rather than guessing.
4. Set `mode: artifact-based` in the report frontmatter, compute the composite over scored dimensions only, and state plainly that this is lower-confidence than a transcript retro. Do not compare its composite against transcript-based retros in trend analysis; list it in the trend table with its mode flagged.

## Token Estimation Strategy

- **Measured**: On Claude Code, sum `<usage>` tags from all Agent results. These are exact.
- **When measurement is unavailable** (Codex, or a run with no subagents): set `tokens_measured` to "unavailable", estimate everything from transcript volume, and mark every number as an estimate. Score D4 on observable waste patterns (redundant reads, bloated prompts, abandoned analysis paths) rather than on totals.
- **Estimated**: Orchestrator tokens approximated from conversation turn count × average turn size. Mark these explicitly as estimates.
- **Efficiency context**: Note tokens-per-output-unit where applicable (e.g., tokens per finding for an audit run, tokens per verdict for triage-finding).
- **Transparency**: The report must state which numbers are measured vs. estimated.

## Edge Cases

| Situation | Handling |
|-----------|---------|
| No prior skill execution | Inform user, halt — unless durable run artifacts exist, the optional issue-swarm and resolve-issue section is active, and the user wants them evaluated: use its Artifact-Based Fallback |
| Compressed context | Note affected phases, lower confidence, score conservatively |
| Multiple skills in conversation | List them, ask user to choose (or use the skill argument if one was passed) |
| Partially completed skill | Evaluate only executed phases, note incompleteness in report |
| Very simple skill (few tool calls) | Run all dimensions but acknowledge limited sample size |
| Skill with no subagents | Skip Phase 1.5 entirely. Note "N/A" for subagent verification in D5 |
| Subagent outputs fully compressed | Skip Phase 1.5, note in report. Score D5 conservatively |
| Subagent spawning unavailable (Codex or restricted session) | Run Phase 1.5 verification directly and sequentially — same claim cap, same three questions, reads scoped to the referenced lines. Note "direct verification" in Context Notes |
| Token usage tags absent (Codex, or no subagents) | `tokens_measured` = "unavailable"; all figures estimated and labeled as such; D4 scored on waste patterns |
| No failed commands in execution | Skip Phase 1.7 entirely. Note "no command failures" in D6 |
| Failed command output compressed | Note in report, classify root cause as UNKNOWN with low confidence |
| Many failed commands (>10) | Group by root-cause category, analyze representative failures per category, note total count |
| All failures are cascading from one root cause | Analyze the root cause once, list cascading failures as downstream effects |
| Retro of `retro` with no prior retro | Decline gracefully |
| Prior retro uses old format (no weighted score) | Use `score` field for trend comparison, note format difference |
| Prior retro frontmatter malformed | Skip that retro, note it, proceed with others |
| Many prior retros (>10) | Read only the 3 most recent for trends, track recommendations from the most recent only |
| Prior recommendations too vague to track | Mark as NOT APPLICABLE with a note |
| `mazo show <skill> --json` exits non-zero | Print the report without persisting it, skip Phase 4 |
| `origin` is `null` (local-only skill) | Print diagnostic, skip Phase 4 |
| `gh` not installed or not authenticated | Print diagnostic suggesting `gh auth login`, skip Phase 4 |
| Issues disabled on the repository | Print diagnostic, skip Phase 4 |
| No write access to the origin | Propose mode: no labels, no issues; one consolidated body in the report and a printed `gh issue create` command |
| Label creation fails (permissions) | Log warning, continue without that label |
| Open issue with the same title, ignoring case | Link it, do not file or propose a duplicate |
| Individual issue creation fails | Log error, continue with remaining issues |
| User selects (s) at Phase 4 gate | Skip, no issues created |
| Zero recommendations in retro | Skip Phase 4 (nothing to file) |
| Report file not found for update | Log warning, skip report update (issues still created) |
