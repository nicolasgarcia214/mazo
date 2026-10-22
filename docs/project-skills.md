# Project skills

This guide covers installing, updating and retiring skills for one project with
`--project`, and how `status` and `usage` show them. Back to the
[README](../README.md).

## Installing for a project

`mazo add --project` installs for one project, the git repository holding the
current directory, instead of for you. It writes three things, and you commit
all three with the project:

- the skill, to `.mazo/skills/<name>/` at the repository's root;
- its entry, to `mazo.lock` there, in the same
  [`[[skill]]` shape](configuration.md#the-data-directory-and-the-store);
- relative links, `../../.mazo/skills/<name>`, in `.claude/skills` (Claude
  Code) and `.agents/skills` (Codex and Pi). mazo never links a project skill
  into the homes.

The links resolve on any checkout. mazo writes no `.gitignore` and fetches
nothing back.

Vetting, `--skill`, `--all`, `--as`, `--trust`, `--yes` and `--force` work as
for a [global add](installing-skills.md#mazo-add). mazo refuses (exit 2) a name
already in the project's store, the global store or `source`, or at either
project link. A refused or failed install leaves no store directory, no entry
and no link.

## Updating and retiring

`mazo update --project [names...]` updates the project's skills from their
origins as `update` does the store's, re-vetting an untrusted one on its diff,
and rewrites the project's `mazo.lock`. The relative links need no change.

`mazo retire --project <name>` removes the project skill's directory, its
`mazo.lock` entry and both of its links, with the same blockers and `--force`
as a global retire. Blockers match usage by skill name. Recent use of a global
skill of the same name therefore also blocks retiring the project copy, and
`--force` overrides that.

Without `--project`, `update` and `retire` act on global skills only; given a
project skill's name, they point to `--project`. Outside a git repository,
`--project` exits 2 and writes nothing.

## Status and usage in a project

Run inside a git repository with a `mazo.lock` at its root, `mazo status` and
`mazo usage` list that project's skills beside the global ones, with
` (project)` after the name.

- `status` checks each one's two project links. It prints
  `project: <name> (project) in sync`, or else one drift line per bad link,
  such as `.agents/skills: <name> (project) is not linked (missing)`. A drift
  line fails `status` as home drift does. `--json` lists every project link
  under `project`.
- `usage` counts a project skill's loads through either project link or its
  `.mazo/skills/<name>` directory. Its audit column is the vet in the project's
  `mazo.lock`, and it has no grade. A name both global and in the project gets
  one row per scope, counted apart.
- The `--json` rows of both commands carry `scope`, `global` or `project`.
  Outside a project that field is the only change.
