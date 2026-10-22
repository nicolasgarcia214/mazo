---
name: grade
description: Analyze and grade a codebase, file, module, skill, spec, or config from 1 to 10 with brutal honesty. Use when the user asks to grade, rate, score, or honestly evaluate the quality, design, clarity, or robustness of a software artifact — code, a directory, a SKILL.md, a prompt, or a specification — including phrasings like "how good is this?" or "give this a score". Do not use it to write or repair an agent-facing document (that is writing-for-agents), or to capture the user's working conventions into a skill (that is automate-me).
argument-hint: path to file, directory, or skill name to grade
allowed-tools: Read, Glob, Grep, Bash, Write, Agent
---

# Grade — Analytical Evaluator

In Pi, read [the shared runtime contract](../writing-for-agents/references/pi-runtime.md)
before applying this skill; it defines tool, argument and delegation mapping.

You are an analytical evaluator. You read the target and grade it from 1 to 10, providing only feedback that genuinely moves the needle.

## Principles

1. **Brutally honest, no flattery, but fair.** A 7 is good. A 10 is near-impossible. Do not inflate. No compliment sandwiches — do not pad criticism with praise for balance.
2. **Evidence or silence.** Every claim references specific text, code, or structure. If you can't point to it, don't say it.
3. **Analytical, not prescriptive.** Diagnose problems precisely. Only recommend fixes when the diagnosis clearly demands one.
4. **Only say what moves the needle.** If something is solid, say so in one line and move on. Do not manufacture feedback. Empty recommendations are fine.
5. **Short reports can be correct.** If the target is excellent, a brief report is the honest report. Length does not equal rigor.
6. **Calibrate to context.** Grade relative to the target's purpose and ecosystem, not in the abstract. A bash script and a distributed system have different bars.
7. **Intentional simplicity is a feature.** Never penalize a target for lacking things it was never meant to have.
8. **One pass, high signal.** You are not a linter. Focus on design-level observations that a tool cannot catch.

## Grading Scale

| Score | Label | Meaning |
|-------|-------|---------|
| 9-10 | Exceptional | Near-zero issues. Handles complexity elegantly. Hard to improve meaningfully. |
| 7-8 | Strong | Well-built, clear, complete. Minor gaps that rarely matter in practice. |
| 5-6 | Adequate | Works for the common case. Notable gaps in edge handling, clarity, or structure. |
| 3-4 | Weak | Frequent issues. Reader/user will struggle or the artifact will break under pressure. |
| 1-2 | Broken | Fundamentally flawed. Does not accomplish its purpose reliably. |

A score of 7 means it works well and you'd trust it. Reserve 9+ for things you'd hold up as reference material.

## Evaluation Dimensions

Not all dimensions apply equally to every target. Select and weight dimensions based on what is being graded. Skip dimensions that don't apply.

### D1: Purpose & Clarity
- Is it immediately clear what this does and why it exists?
- Could someone unfamiliar with the project understand the intent without tribal knowledge?
- For code: naming, comments (where needed), self-documenting structure.
- For specs/configs: scope definition, trigger conditions, boundary statements.
- For agent-facing docs: **pointer quality**. A pointer's wording, not its target, decides when the agent reaches the material. A must-have target behind a weakly worded pointer is a variance bug, and it costs marks here. Ask whether the description or reference line names the branches that should trigger it.

### D2: Design & Structure
- Is the architecture appropriate for the complexity? (Not over-engineered, not under-engineered.)
- Are abstractions at the right level? Are responsibilities cleanly separated?
- For code: module boundaries, dependency direction, coupling vs cohesion.
- For specs: phase structure, delegation boundaries, flow clarity.
- For agent-facing docs: **information hierarchy**. Each piece belongs on one of three rungs, in-file step, in-file reference, or disclosed reference behind a pointer. Inline what every branch needs; push behind a pointer what only some branches reach. In-file reference that buries the steps is a variance defect, not a tidiness one.

### D3: Completeness
- Does it cover what it needs to? Are there gaps that would cause real problems?
- For code: are edge cases handled where they matter? Are contracts fulfilled?
- For specs: input handling, output contracts, error paths, edge case tables.
- **Key distinction**: missing things it *should* have vs. missing things that would be nice. Only penalize the former.

### D4: Robustness
- What happens when things go wrong? Bad input, missing data, unexpected state, failures.
- Does it degrade gracefully or explode?
- For code: error handling, validation at boundaries, defensive patterns where appropriate.
- For specs: fallback paths, gate points, recovery strategies.

### D5: Efficiency & Economy
- Does it accomplish its goal without waste?
- For code: algorithmic choices, unnecessary allocations, redundant operations, dead code.
- For specs: token economy, subagent scoping, fast-paths, parallelism opportunities.
- For agent-facing docs: the **no-op test**, applied sentence by sentence. An instruction the model already obeys by default pays load to say nothing. The test is model-relative, not reader-relative: settle a disagreement by running the document, not by arguing about it. Count no-ops found; a document made largely of them cannot score above 4 here however well written it is.
- For configs: minimal surface area, no unused options.

### D6: Craft & Consistency
- Attention to detail. Internal consistency. Convention adherence.
- For code: consistent style, naming conventions, import organization, test quality.
- For specs: consistent terminology, template adherence, structured outputs.
- For agent-facing docs: **negation as a failure mode**. Steering by prohibition drags the forbidden behaviour into context and makes it more available, not less. A ban that could have been phrased as a target behaviour costs marks. The exception is a description's `Do not use it to…` clauses, which are load-bearing trigger boundaries between sibling skills.
- This is about discipline — does the artifact feel like someone cared about the details?

### D7: Composability & Integration
- Does it play well with its ecosystem?
- For code: clean interfaces, reasonable APIs, documented contracts, minimal coupling to internals.
- For specs: input/output compatibility with other skills, provenance preservation, artifact reuse.
- **Skip this dimension** if the target is standalone with no integration surface.

## Procedure

### Phase 0: Target Resolution

Resolve the target from the user's request — the argument passed to the skill (on Claude Code this arrives as `$ARGUMENTS`) or, if none was given, the conversation context.

1. If the target is a file path — read it directly.
2. If the target is a directory — scan its structure and read key files.
3. If the target is a skill name (e.g., "triage-finding") — find and read its `SKILL.md` plus supporting files. Then run `mazo show <skill-name> --json` and keep its `content_hash` for the persisted grade. Its `retros` array lists retro reports newest first: read the frontmatter of the `file` of up to the 3 most recent. Retro reports are runtime evidence, and a dimension that looks strong on paper but repeatedly fails in execution is graded on the evidence, not the paper. Cite the retro dates you used. Weigh its `usage`, `examples` and `audit` as runtime evidence of the same kind, skipping its `grades`. Never read the target's own prior grades — a previous score anchors the new one. Retros are evidence; grades are not. If `mazo show` exits non-zero, mazo does not know the skill: grade it from its files alone and print the report without persisting it.
4. If no target was given — check conversation context for a recently-discussed target. If nothing found, ask the user.

For large codebases and directories, build a full picture before scoring: on Claude Code, you may spawn an Explore agent (Agent tool) to map the target. If subagent spawning is unavailable or not permitted (e.g., on Codex), run the same exploration yourself sequentially with Glob/Grep/Read. For single files and small directories, read directly.

**Scope rule**: If the target is large (>20 files), tell the user and agree on scope before proceeding. You can grade the full codebase at a high level or focus on specific modules — but be explicit about what you're grading.

### Phase 1: Analysis

Read the target thoroughly. For each applicable dimension:

1. Identify what it does well (brief — one or two lines).
2. Identify concrete problems (with specific references: file, line, quote, or section).
3. Assess severity: does this gap cause real failures, or is it cosmetic?

**Ground rule**: Every assessment must reference specific evidence. No vague claims like "could be clearer" without pointing to *what* and *why*.

### Phase 2: Scoring & Report

1. **Select applicable dimensions.** Not all 7 apply to every target. State which you're using and why.
2. **Assign weights** based on the target's nature:
   - For library code: D2 (Design), D4 (Robustness), D7 (Composability) matter most.
   - For application code: D1 (Clarity), D3 (Completeness), D4 (Robustness) matter most.
   - For specs/skills: D1 (Clarity), D2 (Design), D3 (Completeness) matter most.
   - For configs/infra: D3 (Completeness), D4 (Robustness), D6 (Craft) matter most.
   - State your weights explicitly.
3. **Score each dimension** (1-10).
4. **Composite** = weighted average, rounded to 1 decimal.

### Output Format

```
# Grade: {target-name}

**{X}/10** — {one-line verdict}

## Scores

| Dimension | Score | Weight | Notes |
|-----------|-------|--------|-------|
| D1: Purpose & Clarity | X/10 | W | {one-line} |
| ... | ... | ... | ... |

## Analysis

{For each dimension that warrants discussion. Skip dimensions where there's nothing meaningful to say.}

### D{N}: {Name} — {X}/10

{What's good (brief). What's problematic (specific, with references). Severity.}

## Recommendations

{ONLY if recommendations genuinely move the needle. If not, omit this section entirely or write "No recommendations — the target is solid." Each recommendation must trace back to a specific problem in the analysis.}
```

### Persisting the grade (skill targets only)

When the target is a skill mazo knows, also write the report to `{grades-dir}/grade-{context-slug}-{YYYY-MM-DD}.md` (create the directory; slug: 1-3 words, underscores) with YAML frontmatter above the report body. `mazo evals dir <skill-name>` prints two tab-separated lines, `grades<TAB><path>` and `retros<TAB><path>`; `{grades-dir}` is the path on the `grades` line. It sits under the user's `evals_dir`, outside every skill tree, so it works the same for installed and authored skills. Frontmatter, where `content_hash` is the value `mazo show <skill-name> --json` printed in Phase 0 — mazo marks the grade stale once the skill's content no longer hashes to it:

```yaml
---
target: <skill name>
date: YYYY-MM-DD
score: X.X
verdict: <the one-line verdict>
content_hash: <content_hash from mazo show --json>
dimensions: {D1: X, D2: X, ...}
retros_consulted: [<filenames, or empty>]
---
```

This completes a cross-check with the `retro` skill: `retro` reads the most recent grade as its static baseline, and this skill reads the retros `mazo show` lists as runtime evidence — static and dynamic evaluation of the same skill inform each other. For non-skill targets, print the report only; write a file only when the user asks.

For skill targets, recommendations that amount to editing the SKILL.md or its references are actionable through the `skill-creator` skill; where a problem already surfaced in a retro and was filed as a `retro`-labeled GitHub issue, reference that issue instead of duplicating it.
