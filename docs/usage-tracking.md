# Usage tracking

This guide covers how mazo counts skill loads and the commands that report
them: `mazo scan`, `mazo usage`, `mazo unused`, `mazo show` and `mazo report`.
Back to the [README](../README.md).

## Example

```console
$ mazo usage
skill         sess 7d 30d trend slash tool typed read ref agents ctx last   grade audit
release-notes    3  2   3 new       3    0     0    0   0 C      C●  1d ago     - -
triage           1  1   1 new       1    0     0    0   0 C      C●  4d ago     - -
tidy-imports     0  0   0           0    0     0    0   0        C●  -          - -
coverage: claude since 2026-09-14
legend: agents C Claude, X Codex, P Pi; ● name and description in context: Claude on, every Codex skill, Pi by default
```

This example uses sample data: three skills, with recorded invocations for two.

## How mazo counts loads

`mazo scan` reads each home's transcripts from a per-file cursor and appends
the skill loads it finds to the ledger. Every event has a channel, the way the
agent loaded the skill:

| Channel | Strength | Meaning |
|---|---|---|
| `slash` | strong | The user invoked the skill, as `/name` in Claude, `$name` in Codex or a `<skill>` block in Pi. |
| `tool` | strong | Claude called its Skill tool on the skill. |
| `typed` | strong | A `/name` line in Claude's `history.jsonl` whose session transcript is gone; a one-time backfill. |
| `read` | weak | The agent read the skill's `SKILL.md` with a file or shell tool. |
| `ref` | weak | The agent read another Markdown file inside the skill's directory. |

A strong load means the skill was used. Only strong loads count towards
`sess`, `7d`, `30d`, `trend` and `last`, and only they make a skill used for `unused`
and `retire`. A weak load means the agent looked at the skill, which may be
authoring rather than following it.

`usage`, `unused`, `show`, `report`, `audit` and `retire` scan first, so their
numbers are current. Every report prints how far back each home's logs go:
"unused since 2026-09-10" is not "never used".

On a terminal, `usage`, `unused` and `status` colour their status tokens:
`unused`, `ERROR:` and the audit column's `!` in red; `held`, `new`, the `named by` note, `warning:`
and a stale grade's `stale` mark in yellow. Piped output, or any output with
`NO_COLOR` set to a non-empty value, has no colour.

Under the table, `usage` and `unused` print a legend of only the marks and
glyphs their table shows, and none when it shows none. The HTML report's
legend lists them all.

### Actors

The actor is who ran the session: `main` (you), `subagent`, `headless` (Claude
`sdk-cli` or `codex exec`) or `probe` (any other Claude entrypoint). `usage`
leaves out `probe` sessions and `other`-origin loads (from outside mazo's
skill roots, or of a name mazo does not know) unless `--all`, and leaves out
authoring reads unless `--authoring`. `report`'s usage table is `usage --all`,
so it includes probe sessions. `unused`, `show` and the report's cards always leave it out.

## `mazo scan`

Reads new log lines into the ledger. `--report` adds per-channel counts,
unparsed commands and the parser canary. `--reparse` re-derives every event
from the logs on disk. See [how accurate the counts are](#how-accurate-the-counts-are)
for what a scan can miss.

## `mazo usage`

The window is `--since <N>d` or `--since all`. It defaults to 30 days for
`usage`, `show` and `report`, and to 60 days for `unused`. `usage` also takes
`--agent claude|codex|pi` and `--channel slash|tool|typed|read|ref` to keep
only those events, `--authoring` to count authoring reads, and `--json`.
`show --limit <N>` sets how many recent events the card lists (default 20).

One row per skill: sessions in the window, sessions over the last 7 and 30
local days whatever the window, the trend of the last 30 days against the 30
before (`↑` at +20% or more, `↓` at -20% or less, `→` otherwise, `new` from
none, blank for none in either), loads per channel, agents, ctx, last use, the
latest grade, and the audit column. The ctx column gives, for each agent the
skill is linked into, its letter and then a glyph for what that agent keeps
of the skill in context:

- `●`: its name and description: Claude on, every Codex skill, Pi by default;
- `◐`: its name only: Claude `name-only`;
- `○`: nothing until you invoke it: Claude `user-invocable-only` or
  `disable-model-invocation`, Pi `disable-model-invocation`;
- `×`: nothing: Claude `off`.

Claude's state comes from `skillOverrides` in `settings.json` and
`settings.local.json`, where the local file wins, and the costliest state
across an agent's homes counts. Codex lists every skill, even under
`allow_implicit_invocation: false`.

The audit column is a mark, the first of these
that holds:

1. `!`: the verdict `mazo add` recorded when it vetted the skill has a security
   finding;
2. `✓`: an audit judged it;
3. `vet`: only the vet judged it;
4. `-`: neither.

The card (`mazo show`) prints the vet verdict and the latest audit's.
`usage --json` carries the `verdict`, the audit's when there is one, else the vet's
led by any security finding, and its `verdict_source` (`audit`, `vet`, or null
when not judged). Project skills get their own rows; see
[project skills](project-skills.md#status-and-usage-in-a-project).

A grade cell in `usage` is the score as written, then ` stale` when the
skill's content changed after the grade, such as `8.6 stale`: the grade's
frontmatter `content_hash` differs from the skill's, or the grade has none.
Grades live under [`evals_dir`](configuration.md#evals_dir).

## `mazo unused`

Lists the skills with no strong load in the window, each with one status and
any of the notes below.

| Status | Meaning |
|---|---|
| `unused` | Nothing holds it. |
| `held` | A skill that cites it was used in the window; the note names the holders. |
| `new` | It arrived inside the window: a source skill's first commit is in the window or it is not committed yet (outside a git repository no source skill is new), and a store skill was installed in the window. |

| Note | Meaning |
|---|---|
| `cluster` | Skills that cite it, none of them used in the window either, so they may go together. |
| `weak-only` | It has weak loads in the window and no strong one. |
| `named by` | A skill used in the window names it in prose without citing it. It never makes the skill held, but `retire` refuses it without `--force` when the namer was used in the last 30 days. |

## `mazo show`

`mazo show <skill>` prints the skill card. `--json` emits the whole card.

The card opens with a manual of at most 15 lines at 80 columns, in three parts.
An empty part is left out.

- **For** is what the skill is for.
- **Your use** is the first line of up to three real prompts, cut to 100
  characters.
- **Try** is up to three uses to try.

For and Try come from the latest skill audit when it is newer than the vet or
there is no vet, else from the vet in the lockfile. Without either, For is the
frontmatter description.

After the manual come:

- the daily histogram and recent events, each with the log line it came from;
- the description, git history, every grade and retro, cites and cited-by;
- usage by agent, channel and project;
- examples: the invocation on each agent, the manifest's `default_prompt`, up
  to three real prompts read from transcripts at display time and never
  stored, and up to three curated examples from the latest audit;
- that audit's date, session count, verdict, rates and top three improvements.

## `mazo report`

Writes one self-contained HTML page: the `usage --all` table with each skill's
audit verdict in full, the grades to run as `/grade <skill>`, and every
skill's card. The page goes to `report.html` in the data directory with mode
`0600`, and each run replaces it, a symlink there included. `--out <path>`
writes it elsewhere and warns inside a git worktree, because the page shows
cwds; it renames a new file over `<path>`, so it cannot write to a device such
as `/dev/stdout`. `--open` opens it.

## How accurate the counts are

These numbers come from the author's logs, measured on 2026-10-13.

**Strong loads.** Two independent counting scripts, written from the spec
without reading mazo's code, matched mazo id for id on `slash`, `tool` and
`typed`. The counts were 191 slash, 137 tool and 644 typed events.

**Precision.** Two reviewers each sampled 20 strong events, 20 reads, and 20
log lines that mention a skill but produced no event. They checked each sample
against the source records.

- Strong loads: 40 of 40 correct.
- Reads: 40 of 40 correct.
- Missed loads among the 40 near misses: none.

**Reads are weak evidence.** In those samples, 19 of 40 reads were authoring:
editing or grading the skill rather than following it. mazo tags a read as
authoring when its session:

- runs inside the git repository that holds the skills directory,
- runs inside a home's skills directory, or
- loaded a skill named in `[policy] authoring_skills`.

The sampled ledger predates the skills-directory rule. It tagged 13 of the 19
authoring reads. The skills-directory rule covers 5 of the other 6, all run
from `~/.codex/skills`. Reads are never added to the headline `sess` column.

**What a scan misses.** mazo misses reads whose commands its grammar does not
parse (`awk`, globs, heredocs). `mazo scan --report` counts them and prints
samples.

A transcript line that mentions a skill but is not JSON yields no event. A line
that mentions none is never parsed, so it cannot hide a load. A line longer
than 64 MiB is skipped unread and counted as malformed once its newline is
written.

The scan that first reads a malformed line warns with the count per home.
`scan --json` reports that count as `malformed`, and `scan --report` prints it.
The next scan does not repeat the warning, since the line is behind the
cursor, but `scan --reparse` reads it again.

The parser canary in `scan --report` covers the other failure: a file with a
skill marker that yields no event at all.
