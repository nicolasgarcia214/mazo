# Managing skills

This guide covers linking skills into the homes and managing the skills of
`source` with `mazo status`, `mazo sync`, `mazo new`, `mazo retire` and
`mazo prune`. Back to the [README](../README.md).

## `mazo status`

Reports drift and policy for every home and, inside a project, its skills'
links (see [project skills](project-skills.md#status-and-usage-in-a-project)).
Writes nothing, and exits 1 on any error. `--agent <claude|codex|pi>` checks
only that agent's homes. `--json` prints the `drift`, `policy`, `conflicts` and
`warnings` lists.

## `mazo sync`

Links every skill, from `source` and the store, into every home. It checks
everything first and writes nothing if any check fails. `--dry-run` prints the
actions without taking them, and `--agent <claude|codex|pi>` syncs only that
agent's homes.

A link to the same-named skill directory of `source` or the store, whose
target is gone, is mazo's own, so `sync` removes it. When another root now
holds a skill of that name, as after you delete your copy and `mazo add` one,
`sync` and `add` link the name to it. Any other entry in a skill's place, a link elsewhere or a real
file, stops the sync with the command that clears it.

While it writes, `sync` holds the data lock, and it re-plans under the lock. A
dry run, a refusal or an empty plan takes no lock.

## `mazo new`

`mazo new <name> --description <text>` creates `<name>/SKILL.md` in `source`,
plus `agents/openai.yaml` when `codex_manifest` is on. When `claude_name_only`
is on, it prints the `skillOverrides` line to add to each Claude home. It
refuses a name that already has an entry or a store skill, and it does not
sync.

## `mazo retire`

`mazo retire <name>` moves a skill of `source` to `.archive/` (`git mv` when
tracked) and prunes its links. A store skill is removed instead: mazo deletes
its links, its `mazo.lock` entry and its store directory. When
`claude_name_only` is on, either kind then prints the `skillOverrides` entry
each Claude home's settings can now drop.

It refuses a skill used in the last 30 days, or one held or named by a skill
used in that time, unless `--force`. It holds the data lock while it writes. It
writes nothing if the resulting sync would fail, and it never commits.
`--project` retires a project skill instead (see
[project skills](project-skills.md)).

## `mazo prune`

`mazo prune [<name>]` permanently deletes archived skills from `.archive/`. It
asks first, and a declined prompt exits 1. `--yes` skips the prompt and is
required when stdin is not a terminal. In a git repository it refuses a file whose
content is in no `HEAD` path unless `--force`.
