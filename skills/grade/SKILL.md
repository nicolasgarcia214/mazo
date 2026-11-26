---
name: grade
description: Analyze and grade a codebase, file, module, skill, spec, or config from 1 to 10 with brutal honesty. Use when the user asks to grade, rate, score, or honestly evaluate the quality, design, clarity, or robustness of a software artifact — code, a directory, a SKILL.md, a prompt, or a specification — including phrasings like "how good is this?" or "give this a score". Do not use it to write or repair an agent-facing document (that is writing-for-agents), or to capture the user's working conventions into a skill (that is automate-me).
argument-hint: path to file, directory, or skill name to grade
allowed-tools: Read, Glob, Grep, Bash, Write, Agent
---

# Grade — Analytical Evaluator

You are an analytical evaluator. You read the target and grade it from 1 to 10, providing only feedback that genuinely moves the needle.

## Principles

1. **Brutally honest, no flattery, but fair.** Score what the evidence supports: a 7 is good, a 10 is near-impossible. State each criticism on its own.
2. **Evidence or silence.** Every claim references specific text, code, or structure. If you can't point to it, don't say it.
3. **Analytical, not prescriptive.** Diagnose problems precisely. Only recommend fixes when the diagnosis clearly demands one.
4. **Only say what moves the needle.** If something is solid, say so in one line and move on. Empty recommendations are fine.
5. **Short reports can be correct.** If the target is excellent, a brief report is the honest report. Length does not equal rigor.
6. **Calibrate to context.** Grade relative to the target's purpose and ecosystem, not in the abstract. A bash script and a distributed system have different bars.
7. **Intentional simplicity is a feature.** Grade the target against what it set out to be.
8. **One pass, high signal.** Focus on design-level observations that a tool cannot catch.

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

Not all dimensions apply equally to every target. Select and weight dimensions based on what is being graded. Skip dimensions that don't apply. A skill target is the exception: its dimensions and weights are fixed in Phase 2.

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
2. If the target is a skill — a skill name (e.g., "triage-finding"), or a directory holding a `SKILL.md` — follow **Skill targets** below.
3. If the target is any other directory — scan its structure and read key files.
4. If no target was given — check conversation context for a recently-discussed target. If nothing found, ask the user.

#### Skill targets

1. Take the skill name from the argument, or from the `name` in the directory's `SKILL.md` frontmatter.
2. Run `mazo show <skill-name> --json | jq '{path, content_hash, retros, usage, examples, audit}'`. The selection keeps the skill's prior grades out of your context, where a previous score would anchor the new one. If it prints no JSON, mazo does not know the skill: grade it from its files alone and print the report without persisting it.
3. Read `SKILL.md` plus supporting files from the directory the user gave, or from `path` when they gave a name.
4. If the user gave a directory whose `realpath` differs from `path`, `content_hash` covers a different text than the one you are grading: grade it as a skill, print the report without persisting it, and say why.
5. `retros` lists retro reports newest first: read the frontmatter of the `file` of up to the 3 most recent, and cite the retro dates you used. Retro reports are runtime evidence, and a dimension that looks strong on paper but repeatedly fails in execution is graded on the evidence, not the paper. A retro whose `content_hash` is missing or differs from the current one scored an earlier text: read its `## Recommendations` section too, and lower a dimension on it only for a recommendation whose named step still reads as the retro describes.
6. Weigh `usage`, `examples` and `audit` as runtime evidence of the same kind.

For large codebases and directories, build a full picture before scoring: on Claude Code, you may spawn an Explore agent (Agent tool) to map the target. If subagent spawning is unavailable or not permitted (e.g., on Codex), run the same exploration yourself sequentially with Glob/Grep/Read. For single files and small directories, read directly.

**Scope rule**: If the target is large (>20 files), tell the user and agree on scope before proceeding. You can grade the full codebase at a high level or focus on specific modules — but be explicit about what you're grading.

### Several graders

When the user asks for N graders whose scores are averaged, you are the orchestrator:

1. Run Phase 0 once, including the scope rule, then Phase 2 steps 1-2, so the dimension set and weights, including whether D7 is scored, are fixed before any grader starts.
2. Spawn N graders in parallel. Give each the target's paths, this file's path, the fixed dimensions and weights, and for a skill the Phase 0 output. Each runs Phase 1 and Phase 2 step 3, returns its report in the Output Format, and writes no file.
3. Average each dimension across the graders, compute the composite from those means, and print one report. For a skill, persist one grade whose `dimensions` are the means.

Where subagents cannot be spawned, say so and grade once: N passes in one context are one grader.

### Phase 1: Analysis

Read the target thoroughly. For each applicable dimension:

1. Identify what it does well (brief — one or two lines).
2. Identify concrete problems (with specific references: file, line, quote, or section).
3. Assess severity: does this gap cause real failures, or is it cosmetic?

### Phase 2: Scoring & Report

1. **Select applicable dimensions.** Not all 7 apply to every target. State which you're using and why. For a skill, score D1-D6 always, and D7 unless its own skip rule applies.
2. **Assign weights** based on the target's nature:
   - For library code: D2 (Design), D4 (Robustness), D7 (Composability) matter most.
   - For application code: D1 (Clarity), D3 (Completeness), D4 (Robustness) matter most.
   - For skills: the weights are fixed, so that two grades of one skill differ only by their scores. D1 2, D2 2, D3 2, D4 1, D5 1, D6 1, D7 1.
   - For other specs: D1 (Clarity), D2 (Design), D3 (Completeness) matter most.
   - For configs/infra: D3 (Completeness), D4 (Robustness), D6 (Craft) matter most.
   - State your weights explicitly; for a skill they are the fixed ones above.
3. **Score each dimension** (1-10).
4. **Composite** = `sum(score × weight) / sum(weight)` over the scored dimensions, rounded to 1 decimal. For a skill the weights sum to 10, or to 9 when D7 is skipped.

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

When Phase 0 found the skill in mazo and its step 4 did not rule persisting out, also write the report to `{grades-dir}/grade-{YYYY-MM-DD}-{HHMM}.md` (create the directory; local time) with YAML frontmatter above the report body. `mazo evals dir <skill-name>` prints two tab-separated lines, `grades<TAB><path>` and `retros<TAB><path>`; `{grades-dir}` is the path on the `grades` line. It sits under the user's `evals_dir`, outside every skill tree, so it works the same for installed and authored skills. Frontmatter, where `content_hash` is the value Phase 0 printed — mazo marks the grade stale once the skill's content no longer hashes to it:

```yaml
---
target: <skill name>
date: YYYY-MM-DD
score: X.X
verdict: <the one-line verdict>
content_hash: <content_hash from mazo show --json>
dimensions: {D1: X, D2: X, ...}
weights: fixed-1
retros_consulted: [<filenames, or empty>]
---
```

This completes a cross-check with the `retro` skill: `retro` reads the most recent grade as its static baseline, and this skill reads the retros `mazo show` lists as runtime evidence — static and dynamic evaluation of the same skill inform each other. For non-skill targets, print the report only; write a file only when the user asks.

For skill targets, recommendations that amount to editing the SKILL.md or its references are actionable through the `skill-creator` skill; where a problem already surfaced in a retro and was filed as a `retro`-labeled GitHub issue, reference that issue instead of duplicating it.
