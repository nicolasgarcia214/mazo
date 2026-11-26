# Report Template

Use this template to structure the retrospective report. Fill in all bracketed sections.

```
---
skill: "[skill-name]"
date: "[YYYY-MM-DD]"
content_hash: "[content_hash from mazo show <skill-name> --json]"
mode: "[fresh | in-context | artifact-based]"
verdict: "[EXCELLENT | GOOD | ADEQUATE | POOR | FAILED]"
score: [X.X weighted composite]
dimensions:
  goal_achievement: [X]
  workflow_discipline: [X]
  tool_efficiency: [X]
  token_economy: [X]
  output_quality: [X]
  error_handling: [X]
  knowledge_generalizability: [X, or "n/a"]
tokens_measured: [sum from <usage> tags; "unavailable" when there are none (no subagents, or the platform provides no usage tags)]
tokens_estimated: [total estimate including orchestrator]
spot_check_claims: [X/Y confirmed, or "n/a" if no subagents]
review: "[X/Y supported; written by the orchestrator after Phase 3.5, omitted when that phase was skipped]"
prior_retro_count: [N]
prior_retro_latest: "[YYYY-MM-DD or none]"
trend: "[improving | stable | declining | first_run]"
github_repo: "[owner/repo or omit if Phase 4 skipped]"
github_issues: "[filed and duplicate issue numbers, or omit if Phase 4 skipped]"
---

# Retrospective: [skill-name]

## Executive Summary

[2-3 sentences: what skill ran, what it was supposed to do, overall assessment. State the verdict.]

## Scorecard

| Dimension | Score | Weight | Weighted | Label |
|-----------|-------|--------|----------|-------|
| Goal Achievement | X/5 | 2.5 | [X.XX] | [label] |
| Output Quality | X/5 | 2.0 | [X.XX] | [label] |
| Workflow Discipline | X/5 | 1.5 | [X.XX] | [label] |
| Tool Efficiency | X/5 | 1.5 | [X.XX] | [label] |
| Error Handling | X/5 | 1.5 | [X.XX] | [label] |
| Token Economy | X/5 | 1.0 | [X.XX] | [label] |
| Knowledge Generalizability | [X/5, or N/A] | 1.0 | [X.XX, or —] | [label] |
| **Composite** | | | **X.X/5** (weighted, over the scored weights) | **[VERDICT]** |

## Dimension Analysis

### D1: Goal Achievement (X/5 — [Label])

**Output contract**: [List each item from SKILL.md and delivery status]

[Assessment with evidence]

### D2: Workflow Discipline (X/5 — [Label])

**Phase execution**:
| Phase | Status |
|-------|--------|
| [phase name] | [executed / skipped / partial / reordered] |

**Gate compliance**: [For each gate: respected / bypassed]

[Assessment with evidence]

### D3: Tool Efficiency (X/5 — [Label])

**Tool call summary**: [Count by type]
**Redundancies identified**: [List or "none"]
**Parallelism**: [Opportunities used / missed]

[Assessment with evidence]

### D4: Token Economy (X/5 — [Label])

**Measured tokens**: [total from `<usage>` tags; or "unavailable — no usage tags (no subagents, or the platform provides none), all figures below are estimates"]
**Estimated orchestrator tokens**: [estimate]
**Efficiency metric**: [tokens per output unit, if applicable]

[Assessment with evidence]

### D5: Output Quality (X/5 — [Label])

[Assessment of structure, accuracy, completeness, actionability]

**Spot-Check Verification** (if Phase 1.5 executed — subagent or direct):
| # | Claim (quoted) | Source | Verdict | Impact on Final Output |
|---|----------------|--------|---------|----------------------|
| 1 | [subagent claim] | [file:line] | [CONFIRMED/PARTIALLY CONFIRMED/REFUTED/UNVERIFIABLE] | [how it affected the output] |

- **Confirmation rate**: X/Y
- **Orchestrator filtering**: [Did the orchestrator catch/miss subagent errors?]
- **Systematic patterns**: [Any recurring issues across subagents, or "none"]

### D6: Error Handling (X/5 — [Label])

**Errors encountered**: [List or "none"]
**Handling assessment**: [Per-error evaluation or general assessment]

**Failed Command Analysis** (if Phase 1.7 executed):
| # | Command | Error Signal | Root Cause | Category | Cascading | Recovered? |
|---|---------|-------------|------------|----------|-----------|------------|
| 1 | [command or summary] | [exit code / stderr snippet] | [one-sentence diagnosis] | [Environment/Input/Logic/Resource/External/Cascading] | [downstream failures or "no"] | [yes/no — how] |

- **Total failed commands**: [N]
- **Root-cause distribution**: [e.g., "3 Logic, 1 Environment, 2 Cascading"]
- **Diagnosis quality**: [Did the orchestrator correctly identify root causes during execution, or retry blindly?]

**Fix Recommendations** (from Phase 1.7):
| # | Root Cause | Preventive Fix | Defensive Fix | Scope |
|---|-----------|----------------|---------------|-------|
| 1 | [root cause summary] | [what prevents it] | [fallback if prevention isn't feasible] | [SKILL.md / script / environment] |

[If Phase 1.7 was skipped: "N/A — no command failures during execution."]

### D7: Knowledge Generalizability ([X/5 — Label], or N/A — no implemented prior recommendation)

**Outgoing recommendation classification** (a check, not scored):
| # | Recommendation (short title) | Classification | Rationale |
|---|------------------------------|---------------|-----------|
| 1 | [title] | [STRUCTURAL / REACTIVE] | [why it transfers or doesn't] |

**Prior recommendation adoption**:
| # | Prior Recommendation | Status | Adoption Quality |
|---|---------------------|--------|-----------------|
| 1 | [title] | [ADDRESSED-STRUCTURAL / ADDRESSED-REACTIVE / ...] | [STRUCTURAL / REACTIVE / N/A] |

**Structural ratio**: X/Y recommendations structural (outgoing); X/Y adoptions structural (prior)

[Assessment]

## What Went Well

[Up to 3; fewer when fewer hold up.]

1. [Positive with evidence]

## What Went Wrong

1. [Issue with evidence and impact]
[Additional issues if applicable]

## Recommendations

[Omit a tier subsection with no recommendation. With none at all, write one plain sentence saying why, not a bullet.]

### Immediate (before next run)
- **[Short action title]**: [The behaviour the next run should show, and the step that changes to produce it. 1-3 sentences.] Evidence: [L<n>, L<n>].

### Structural (skill design changes)
- **[Short action title]**: [The step or reference section to rewrite, and the behaviour the rewritten text should produce. A new step only when no existing one can carry the change. 1-3 sentences.] Evidence: [L<n>, L<n>].

### Process (workflow improvements)
- **[Short action title]**: [The change to how the skill is invoked or to the workflow around it, and the behaviour it should produce. 1-3 sentences.] Evidence: [L<n>, L<n>].

## Recommendation Review

[Written by the orchestrator after Phase 3.5. Omit this section when that phase was skipped.]

| # | Recommendation | Verdict | Reviewer's reason |
|---|----------------|---------|-------------------|
| 1 | [recommendation title] | [SUPPORTED / PARTLY SUPPORTED / REFUTED / UNVERIFIABLE / IMMATERIAL] | [one or two sentences] |

## Filed Issues

[Omit this section entirely if Phase 4 was skipped or no issues were created.]

Each recommendation filed as an issue, or matched to a duplicate, also gets an inline `[TRACKED](issue-url)` marker appended to its bullet above.

| # | Recommendation | Issue | Labels |
|---|----------------|-------|--------|
| 1 | [recommendation title] | [#N](issue-url) | [comma-separated labels] |

## Proposed Issue

[Omit this section unless Phase 4 ran in propose mode. The consolidated issue body from references/github-issues.md, followed by the `gh issue create -R {repo}` command printed for the user.]

## Score Trend

[Omit this section entirely on first run.]

| Date | Mode | Verdict | Score | D1 | D5 | D2 | D3 | D6 | D4 | D7 |
|------|------|---------|-------|----|----|----|----|----|-----|----|
| [prior date] | [mode; in-context when the retro has none] | [verdict] | [score] | [X] | [X] | [X] | [X] | [X] | [X] | [X] |
| [current date] | [mode] | [verdict] | [score] | [X] | [X] | [X] | [X] | [X] | [X] | [X] |

**Trend**: [improving / stable / declining] — [one-line evidence, e.g., "weighted score rose from 3.2 to 3.8 (+0.6), driven by D1 improvement"]
**Dimension flags**: [dimensions with |delta| >= 1.0, or "none"]
**Judge**: [Omit if all compared retros share one mode. Otherwise say that a delta across modes may be the judge changing, not the skill.]
**Measurement basis**: [Omit if all compared retros share the same token measurement basis. Otherwise state which used measured `<usage>` totals vs. estimates — D4 deltas across mixed bases are not directly comparable.]

## Recommendation Tracking

[Omit this section entirely on first run.]

| # | Prior Recommendation | Status | Adoption Quality | Evidence |
|---|---------------------|--------|-----------------|----------|
| 1 | [recommendation text] | [ADDRESSED-STRUCTURAL / ADDRESSED-REACTIVE / PARTIALLY ADDRESSED / NOT ADDRESSED / NOT APPLICABLE] | [STRUCTURAL / REACTIVE / N/A] | [evidence citation] |

**Closure rate**: X/Y addressed (structural: X, reactive: X)

## Context Notes

[The mode and the evidence read: the trace's span, or conversation context. Compression status, estimation confidence, evaluation limitations, incomplete phases. If Phase 1.5 ran in direct mode, note "direct verification (no subagent isolation)".]
```
