---
name: retro
description: Perform a retrospective (retro, post-mortem) evaluation of a skill that already ran in this conversation. Use when the user asks for a retro, retrospective, or post-mortem of a skill run, or wants to evaluate, grade, or score how a skill execution went. Prints the run from the session transcript, has a fresh agent trace it against the skill's specification and score seven weighted dimensions (goal achievement, workflow discipline, tool efficiency, token economy, output quality, error handling, knowledge generalizability), has the recommendations reviewed against the cited evidence, writes a report to the skill's retros directory under mazo's evals_dir, and can file the recommendations as GitHub issues on the skill's origin repository, or draft one issue for the user to file without write access. Run it in the same conversation as the evaluated skill when possible; when that conversation is gone, durable artifacts support a reduced cross-session retro.
argument-hint: "[optional: skill name to evaluate, and what to focus on]"
allowed-tools: Read, Glob, Grep, Bash, Write, Edit, Agent
---

# Skill Retrospective

You run a retrospective on a skill that already executed in this conversation. You ran that skill, so you are not its judge: you identify the run, hand a line-numbered trace of it to a fresh evaluator, have the evaluator's recommendations reviewed, and file what the user approves.

## Ground Rules

- **The judge did not run the skill.** A fresh agent evaluates the run and another reviews its recommendations. An agent grading its own run is lenient, and after a compaction it no longer holds most of that run. You evaluate it yourself only in in-context mode (Phase 0.7) and in the Artifact-Based Fallback.
- **The trace is the evidence.** `mazo evals trace` prints the run from the session transcript with a line id on every record, compacted history included. Scores and recommendations cite those ids, so a reviewer can check them.
- **The user decides what is published.** Nothing reaches GitHub before the Step 4.4 gate, and a refuted recommendation is shown there, marked, never dropped.

## Phase 0: Skill Identification & Spec Loading

1. Read the argument passed to the skill (on Claude Code this arrives as `$ARGUMENTS`; on Codex, it is the text following `$retro`; on Pi, the argument appended after the expanded skill). It may name the skill to evaluate, and it may say what the user wants the retro to look at. Keep everything in it other than the skill name, verbatim, as `{focus}`.
2. If it names no skill, scan the conversation for skill invocation markers. On Claude Code: `<command-name>` tags, `/skill-name` patterns, or Skill tool calls. On Codex: `$skill-name` mentions or injected skill content. On Pi: `/skill:name` or a `<skill name="..." location="...">` block; loading instructions alone does not prove the workflow ran.
3. If **multiple skills** detected — list them and ask the user which to evaluate. Stop and wait.
4. If **no skill** detected — inform the user that no prior skill execution was found in this conversation and halt, unless the user is pointing at durable run artifacts, in which case run the Artifact-Based Fallback below.
5. If a retro of `retro` is requested and no prior retro execution exists — decline: "No prior retro execution found to evaluate."
6. Run `mazo show <skill-name> --json` and keep these fields as the **Skill Record**: `path` (the copy mazo hashed), `content_hash` (written into the report), `origin` (Phase 4's target repository; `null` for a local-only skill), and the newest-first `grades` and `retros` arrays (Phase 0.5). If it exits non-zero, mazo does not know the skill: skip Phase 0.5, run the retro with `{report}` as `{trace-dir}/retro.md`, and skip Phase 4 with that reason. Step 4.9 prints that report before it deletes the directory.
7. Resolve the evaluated skill's SKILL.md and `Read` it. Take the `location` the evaluated invocation supplied; without one, the Skill Record's `path`. When the file you read is outside that `path`, `content_hash` describes another copy: say so in the report's Context Notes. Only when mazo does not know the skill, search these roots, without a filesystem-wide search:
   - Project-local: `skills/`, `.claude/skills/`, `.pi/skills/`, `.agents/skills/`
   - User-level: the configured Pi agent directory's `skills/` (default `~/.pi/agent/skills/`), `~/.agents/skills/`, `~/.claude/skills/`, `~/.codex/skills/`

   If multiple candidates share the name, prefer the one whose content matches the skill text loaded in this conversation; if still ambiguous, ask the user.

## Phase 0.5: Prior Retro Scan

Scan for prior retrospective reports to enable trend analysis and recommendation tracking.

1. Determine the retro directory: run `mazo evals dir <skill-name>`. It prints two tab-separated lines, `grades<TAB><path>` and `retros<TAB><path>`; `{retros-dir}` is the path on the `retros` line. It sits under the user's `evals_dir`, keyed by the evaluated skill's name, so a retro of a skill installed from elsewhere never writes into a third party's package.
2. Take the prior reports from the Skill Record's `retros` array, which mazo has already sorted newest first and filtered to files with a dated frontmatter.
3. If **empty** → the Prior Retro Record is "first retro for this skill". Go to step 5.
4. If **non-empty** → read the YAML frontmatter of the `file` of the **3 most recent** entries. From the **most recent only**, also read the `## Recommendations` section. Store as the Prior Retro Record.

5. Also take the first entry of the Skill Record's `grades` array (static evaluations written by the `grade` skill). If there is one, read its `file`'s frontmatter and verdict line as the **static baseline**: note in the report where runtime behavior confirmed or contradicted the static grade (e.g., a dimension graded strong that failed in execution). When its `stale` is `true`, the skill changed after it was graded: say so beside every comparison. Cap: 1 file, skip silently if none.

## Phase 0.7: Trace and Dispatch

1. **Name the report.** Make `{trace-dir}` with `mktemp -d`. `{report}` is `{retros-dir}/retro-{context-slug}-{YYYY-MM-DD}.md`, and `{report-stem}` is its file name without `.md`. `{context-slug}` is a short identifier (1-3 words, underscores) for what the run worked on: the target project, the audit name, the key subject. Examples: `retro-uniswap_audit-2026-10-12.md`, `retro-login_refactor-2026-03-20.md`. If the context does not give one, ask the user for a short label. If the file exists, append `-2`, `-3`, and so on. Create the directory if it is missing.
2. **Print the trace.** Take the session id as [references/platform-notes.md](references/platform-notes.md) §3 gives it for your platform, and run:
   ```
   mazo evals trace <skill-name> <session-id> > {trace-dir}/trace.txt
   ```
   `{trace}` is that file. It holds the session's text, which is why it lives outside the repository and outside `evals_dir`. stderr prints the transcript's path and the run's span, as `<path>: L<first>-L<last>, ...`; keep them as `{transcript}` and `{span}`. The trace covers the skill's last run in the session: when the skill ran more than once, tell the user which run the retro covers.
3. **Pick the mode.**
   - **fresh**: the trace printed and you can spawn a subagent. Go to step 4.
   - **in-context**: the session id is unset, `mazo evals trace` failed, or you cannot spawn a subagent. Read [references/evaluation.md](references/evaluation.md) and run it yourself, from `{trace}` when it printed and from your conversation context when it did not. Then continue at Phase 3.5.
4. **Brief the evaluator.** Spawn one fresh agent, one that does not inherit this conversation. Its brief holds these and nothing else:
   - The instruction to read `references/evaluation.md` under this skill's base directory, given as an absolute path, and to follow it.
   - The evaluated skill's name and the path of its SKILL.md.
   - `{trace}`, `{transcript}` and `{span}`.
   - The Skill Record's `content_hash`.
   - The Prior Retro Record and the static baseline from Phase 0.5, as text.
   - `{report}`, the file it writes.
   - The platform the run happened on.
   - `{focus}`, in the user's words, when the request carried one.

   Leave out your own account of how the run went. The evaluator reads the trace; your summary would hand it your conclusions.
5. **Take the result.** The evaluator returns the scorecard and the top three recommendations. Check that `{report}` exists, then print both to the terminal.

Phases 1 to 3 (trace reconstruction, spot-check, failed-command analysis, scoring, report) live in [references/evaluation.md](references/evaluation.md). Read it only in in-context mode and in the Artifact-Based Fallback.

## Phase 3.5: Recommendation Review

Run this phase whenever `{trace}` printed and you can spawn a subagent, whichever agent wrote the report: the reviewer is fresh either way. Skip it when the report has no recommendations, when there is no trace, or when you cannot spawn one; a review by the agent that wrote the recommendations is not a review.

Spawn one fresh agent to review every recommendation in one pass. Give it the report's `## Recommendations` section and no other part of the report, plus `{trace}`, `{transcript}`, `{span}`, and the evaluated skill's directory. Tell it that its job is to refute. For each recommendation it answers:

1. Do the cited `L<n>` lines lie inside `{span}`, and do they show what the recommendation says happened? Where the trace clipped a record, it reads line `n` of `{transcript}`; a cited call's outcome is the record after it.
2. Does the evaluated skill's text bear the claim out? A rule called missing is missing; a step called wrong is written that way.

It returns one verdict per recommendation, with one or two sentences: **SUPPORTED**, **PARTLY SUPPORTED** (the evidence holds but the claim overstates it, as when the skill already has the rule and the run broke it), **REFUTED**, or **UNVERIFIABLE** (nothing cited, or the evidence cannot settle it).

Write the verdicts into `{report}` under `## Recommendation Review` and set the `review` frontmatter field, as [references/report-template.md](references/report-template.md) lays out. Leave every recommendation's text as the evaluator wrote it.

## Phase 4: GitHub Issue Filing

This phase converts retro recommendations into GitHub issues on the repository the evaluated skill came from. It runs after the report is written to disk and skips gracefully when there is nowhere to file.

Read [references/github-issues.md](references/github-issues.md) now. It holds the labels, field definitions, commands, body templates and report-update mechanics this phase uses; take each from it as written.

### Step 4.1 — Repository Detection

1. Take `origin` from the Skill Record. If it is `null`, print `Phase 4 skipped: {skill-name} has no GitHub origin (local-only skill)` and end Phase 4.
2. `origin.repo` is `https://github.com/{owner}/{name}.git`; strip the `https://github.com/` prefix and the `.git` suffix and store `{owner}/{name}` as `{repo}`.
3. Run:
   ```
   gh repo view {repo} --json nameWithOwner,hasIssuesEnabled,viewerPermission,visibility
   ```
4. If the command fails (`gh` not installed or not authenticated, repository gone) or `hasIssuesEnabled` is `false`:
   - Print: `Phase 4 skipped: {reason}` with a specific diagnostic.
   - End Phase 4. The retro is complete without issues.
5. Set the **filing mode**: **write** when `viewerPermission` is `WRITE`, `MAINTAIN` or `ADMIN`; **propose** otherwise. Keep `visibility` as `{visibility}` for the gate and the issue bodies.
6. Run `git ls-remote {origin.repo} HEAD` and keep its first field as `{upstream-head}`, the upstream default-branch HEAD. If it fails, use `unknown`.

### Step 4.2 — Recommendation Parsing

Read `{report}` and extract for each recommendation bullet: **tier**, **title**, **prescription**, **failure summary**, **supporting evidence**, **likely touchpoints**, **acceptance criteria**, and **category label** — following the field definitions and category-keyword rules in [references/github-issues.md](references/github-issues.md).

### Step 4.3 — Duplicate Check

For every issue title this phase would file or propose (write mode: `[retro] {title}` per recommendation; propose mode: the same per-recommendation titles plus the consolidated title from [references/github-issues.md](references/github-issues.md)), list the origin's issues, open and closed:

```
gh issue list -R {repo} --state all --search '{title} in:title' --json number,title,url,state
```

A result whose title equals the candidate title exactly, ignoring case, is a match; search hits that differ in any other way are not.

- An **open** match is a duplicate: record its URL, and link it instead of filing or proposing that recommendation.
- A **closed** match goes in the gate's Duplicate column as `#N (closed)` and stays selectable. The issue may have been fixed or declined, and only the user knows which.
- A recommendation that re-raises a prior-retro recommendation which the report's `## Recommendation Tracking` table marks NOT ADDRESSED or PARTIALLY ADDRESSED, and which carries a `[TRACKED]` link, matches that linked issue whatever its title. Titles are reworded from run to run, so the tracked link is the stable identity. Fetch its state with `gh issue view {number} -R {repo} --json state,url` and apply the open and closed rules above.

### Step 4.4 — User Gate

In write mode, first write each non-duplicate issue's body to `{trace-dir}/{n}.issue.md`, where `{n}` is the recommendation's `#` in the table below, built from the issue-body template and body requirements in [references/github-issues.md](references/github-issues.md), and list those paths under the table. The table shows titles only, so the files are how the user reads what will be published before approving it.

Present a summary table before creating anything:

```
Phase 4: GitHub Issue Filing
Repository: {repo} ({visibility}; {write | propose: no write access})
Installed commit: {origin.commit}  Upstream HEAD: {upstream-head}

| # | Title | Tier | Labels | Review | Duplicate |
|---|-------|------|--------|--------|-----------|
| 1 | Fix report rendering | Immediate | retro, retro:immediate, bug | SUPPORTED | — |
| 2 | Cap token budget | Structural | — | SUPPORTED | #17 |
| 3 | Gate the merge step | Structural | retro, retro:structural, enhancement | REFUTED | — |
| ... | ... | ... | ... | ... | ... |

(a) Create all · (b) Select specific · (s) Skip
```

The Review column carries the Phase 3.5 verdict, or `—` when that phase was skipped. In propose mode the Labels column is `—` and (a) reads `Draft all`. When the installed commit differs from the upstream HEAD, the problem may already be fixed upstream: say so under the table. When Phase 3.5 was skipped, say under the table that no reviewer checked these recommendations.

Wait for user input. This gate is **mandatory** — issues are externally visible artifacts.

- **(a)**: Proceed with every recommendation whose Duplicate column is `—` and whose Review column is `SUPPORTED` or `—`. A `PARTLY SUPPORTED`, `REFUTED`, `UNVERIFIABLE` or `#N (closed)` row is filed only when the user names it through (b). Print the reviewer's reason under the table for every row that is not `SUPPORTED`.
- **(b)**: Ask the user to specify issue numbers (e.g., "1,3,5"), then filter the list.
- **(s)**: Print `Phase 4 skipped by user.` and end.

### Step 4.5 — Issue Creation (write mode)

Create the labels and list the available ones as [references/github-issues.md](references/github-issues.md) defines. Then run its issue-creation command for each selected recommendation, with the body file written in Step 4.4.

Capture the returned issue URL and number. If an individual issue creation fails, log the error, move its body file to `{retros-dir}/{report-stem}.{n}.issue.md` and print that path so the user can retry, then continue with the remaining issues.

### Step 4.6 — Issue Proposal (propose mode)

Create no labels and no issues. Write one consolidated issue covering every selected recommendation, using the consolidated template in [references/github-issues.md](references/github-issues.md):

1. Write its body into the retro report under `## Proposed Issue`, and the same body to `{report-stem}.issue.md` next to the report.
2. Print that section's command for the user to run.

### Step 4.7 — Report Update

Update `{report}` as the Report Update Mechanics in [references/github-issues.md](references/github-issues.md) lay out: the `[TRACKED]` annotations, the `## Filed Issues` section and the frontmatter fields.

### Step 4.8 — Terminal Summary

Print:
```
Phase 4 complete: Filed {N} issues on {repo}
  - https://github.com/owner/repo/issues/42
  - ...
```

In propose mode, print `Phase 4 complete: drafted 1 issue for {repo}` followed by the `gh issue create` command from Step 4.6.

### Step 4.9 — Cleanup

When `{report}` is inside `{trace-dir}`, print the whole report first: this is the only copy. Then delete `{trace-dir}` when Phase 4 ends, whichever way it ends, and when it is skipped. The trace is session text and has no use after the retro, and the issue bodies left in it are on GitHub or were declined.

## Optional: Close The Flywheel

Run this section only when both `mazo show issue-swarm` and `mazo show resolve-issue` exit 0; otherwise skip it silently.

When issues were filed, offer the follow-up explicitly: the `issue-swarm` skill can burn down the retro backlog with `ISSUE_SCOPE=label:retro` (or a single tier, e.g. `label:retro:immediate`), and a single filed issue can go straight to `resolve-issue`. Do not launch either yourself — filing was the retro's job; fixing is a new, user-initiated run.

## Artifact-Based Fallback (cross-session)

Phase 0 step 4 sends you here when the user asks to retro a run whose conversation is gone but which left durable artifacts: Resolution Records v1 under `.git/resolve-issue/` (from `resolve-issue`), an issue-swarm state dir under `.git/issue-swarm/` (state.json, prompts/, validation/), or `.goal/` artifacts. The evaluated skill is the one that wrote them; ask the user when the artifacts leave that open. Run Phase 0 steps 6 and 7, Phase 0.5 and Phase 0.7 step 1 for that skill. Then run this reduced-scope retro yourself in place of the rest of Phase 0.7; [references/evaluation.md](references/evaluation.md) holds the phases and the rubric pointer the steps below name:

1. Read the artifacts; they define what was claimed (contracts, verdicts, verification results, publication states, state transitions).
2. Spot-check claims against the current repository exactly as in Phase 1.5 (same cap of 3).
3. Score only what artifacts support: D1 (claimed outcomes vs re-checked evidence), D5 (artifact completeness and accuracy), and D2 only where the artifact trail shows phase/gate compliance (e.g., swarm state transitions, record presence for every terminal task). Mark D3, D4, D6, D7 `N/A — no transcript` rather than guessing.
4. Set `mode: artifact-based` in the report frontmatter, compute the composite over scored dimensions only, and state plainly that this is lower-confidence than a transcript retro. Do not compare its composite against transcript-based retros in trend analysis; list it in the trend table with its mode flagged.

With no trace there is nothing for a reviewer to check, so Phase 3.5 is skipped. Phase 4 runs as usual.

## Edge Cases

| Situation | Handling |
|-----------|---------|
| `gh` not installed or not authenticated | Print diagnostic suggesting `gh auth login`, skip Phase 4 |
| Zero recommendations in retro | Skip Phase 4 (nothing to file) |
| `mazo evals trace` exits non-zero (no transcript for the id, no load of the skill, or a mazo without the command) | In-context mode |
| The evaluator returns without writing `{report}` | Say so, then run in-context mode |
| The Phase 3.5 reviewer fails, or returns no verdict for a recommendation | That recommendation is `UNVERIFIABLE`, with the reason "no review returned" |
| Every recommendation is REFUTED | Show the gate anyway; (a) files nothing |
