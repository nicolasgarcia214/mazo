# 0001. Code comments are banned

Status: Accepted (2026-11-26)

## Context

A comment makes a claim about the code next to it, and nothing checks that claim when the code changes. An audit of 1980 comment blocks in this repository found 975 that only restated the code, 16 that described an implementation the code no longer has, and 1 that covered for a shortcut. The other 917 recorded real decisions, and scattering those through the source made them hard to find and easy to let rot.

## Decision

The source carries no comments. `scripts/lint_comments.js` enforces this, and CI runs `node scripts/lint_comments.js` in the `test` job.

Each comment is sorted into one of four categories, and each category has one destination:

| Category | Test | Goes to |
|---|---|---|
| Tautology | The code already says it | deleted |
| Stale | It describes history or an implementation the code no longer has | deleted |
| Excuse | It explains a shortcut that has a concrete fix | a GitHub issue, then deleted |
| Decision record | A deliberate, non-obvious choice | an ADR in `docs/adr/`, anchored to a symbol name, then deleted |

The lint exempts comments that a tool reads:

- shebangs, linter and formatter switches, and release markers, listed in `DIRECTIVES` in the lint script;
- licence and copyright headers.

The `///` doc comments on the `Cmd`, `EvalsCmd` and `AuditArgs` variants and fields in `src/main.rs` are also directives, because clap derive turns them into the `--help` text and `every_argument_has_help_text` requires them. The lint's regexes cannot tell them apart from ordinary doc comments, so they stay in `scripts/comment-baseline.json` instead of in `DIRECTIVES`.

`scripts/comment-baseline.json` maps each file to the number of comment lines it may hold. A file not in the baseline may hold none. The lint fails when any file's count differs from its entry. `--tighten` lowers entries to the current counts and never raises them, so the baseline only goes down.

The lint scans Rust, JavaScript and TypeScript, Python, and shell. The other tracked kinds are Markdown, JSON, JSONL, TOML, YAML and HTML; they hold data or prose and are not scanned. The two HTML files have no comments.

## Consequences

- A reason that belongs with the code goes into an ADR, and a shortcut goes into an issue.
- The decisions recorded during the first audit are in ADRs 0002 to 0008.
- A change that adds a comment fails CI until the comment is moved to its destination, or until it is a directive and has been added to `DIRECTIVES`.
