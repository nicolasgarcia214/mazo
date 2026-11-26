# 0002. Skill store

Status: Accepted (2026-11-26)

## Context
The store (`src/store/`) installs third-party skills, records them in `mazo.lock`,
vets untrusted ones with a judge, and updates them. Its decisions, and the
reasons behind several tests that exercise it, lived as code comments until the
comment ban (ADR 0001). This record keeps them.

## Decisions

### Io and consent (`store::ask`)
- `Io::confirms_calls` lets the TUI show and confirm the judge-call count for every untrusted add, not only `--all`. `Terminal` returns false, so the CLI confirms only for `--all`.
- `Terminal` asks y/N only while stdin is a terminal and shows the picker only when both stdin and stdout are terminals.
- `Io::yes_no` receives lines the caller has already reported, so `Terminal` ignores them. `Consent::ask` names the default in the prompt (`[Y/n]` or `[y/N]`), and an empty answer takes it.
- `Consent::answerable` refuses as a usage error before any judge call when no y/N can be answered and `--yes` was not passed, so no paid judge call is wasted on an answer nobody can accept. This holds for `--force` and `--project` too.
- `confirm_calls` prints the judge-call count (one per skill), then requires `--yes` or a y/N that defaults to No. Without a terminal, `--yes` is mandatory. Agreeing does not skip the per-skill install prompt.

### Skill selection (`ask::skills_in`, `ask::skill_dirs`)
- `skills_in` installs the root itself when it holds a `SKILL.md`. Otherwise `--skill` matches a path, or a name that no other skill in the source shares. `--all` takes every skill, a single skill is taken as is, and otherwise the `Io` picker asks. With several skills and no picker, the usage error lists them.
- `skills_in` passes its usage errors through `render::inert_line` because the paths it lists are directory names chosen by the source's author; a newline in one cannot forge a line of the message.
- `skills_in` collapses each description's whitespace so a folded YAML description shows as one picker line.
- `skill_dirs` searches at any depth but skips dot directories and `node_modules`, does not follow symlinked directories, and stops descending at a directory that holds a `SKILL.md`. It returns paths sorted, which is the picker's order.

### Bundled skills (`store::bundled`)
- `mazo init` installs the bundled skills at the running version's tag, `v<CARGO_PKG_VERSION>`, and `mazo update` re-pins them to that tag.
- `REPO` is the only source installed trusted without an entry in the trusted list. `bundled::install` adds `skills/` through `add --all --trust` and writes no trust entry to the config.
- `bundled::install` falls back to the default branch's commit, with a warning, when the tag is missing. It skips bundled skills that are already installed, and on failure prints the `mazo add` that retries and exits 1.
- `bundled::install` reloads the inventory before each add, so each add plans its links knowing the skills the previous add installed.
- `bundled::behind` repins only bundled entries pinned to a commit or to a `vX.Y.Z` tag older than the running version, and only once the remote has the tag. Branches, other tags and unpinned entries stay.

### Lockfile (`store::lock`)
- `Locked::origin` records the canonical physical directory for a local-path source.
- `Locked::subpath` is `.` when the origin itself is the skill.
- `Locked::ref` is the branch, tag or commit that was cloned, and `None` means the remote's default branch. An entry pinned to a ref or a commit ignores new commits on the default branch.
- `Locked::installed_at` and `Locked::vetted_at` hold RFC 3339 timestamps. `vetted_at` is absent for a trusted install.
- `Locked::try` holds at most three judge-suggested uses. `audit::check_for_try` enforces the limit when it parses the vet answer.
- `skill_name` rejects lockfile names that fail `policy::valid_name`, because the name is joined to the store as a path and a project's lockfile is untrusted input. This stops a name such as `..` from reaching outside the project through `.old-<name>` on `update --project`.
- `lock::parse` makes every lockfile string inert, including the TOML error text, which quotes the offending line. A project's lockfile is anyone's commit, and its verdicts, origins and refs are printed.

### Copying and hashing (`store::lock`)
- `lock::resolved` canonicalises the source and refuses it when it leaves `Fetched::clone`, so a symlink in a repository cannot pull in files from outside it.
- `lock::checked` lists the tree before reading `SKILL.md`, so a `SKILL.md` that is not a regular file is refused unread.
- `lock::stage` copies the skill into a staged directory. The judge reads that copy and it is renamed into place, so a source changed after it was read cannot install anything unvetted.
- `lock::stage` checks each entry's type on the opened handle rather than on the listed path, so an entry swapped for a symlink after listing is refused rather than followed.
- `lock::stage` copies each file's permissions as `fs::copy` does, so scripts stay executable.
- `lock::unfollowed` opens each path component with `O_NOFOLLOW` and `O_NONBLOCK`, so no symlink is followed and a FIFO is refused instead of blocking while it waits for a writer.
- SAFETY (`libc::openat` in `lock::unfollowed`): the directory handle stays open across the call and the name is NUL-terminated.
- SAFETY (`File::from_raw_fd` in `lock::unfollowed`): the fd was just returned by `openat` and has no other owner.
- `lock::tree` omits `.git` and errors on any symlink, or any entry that is neither a regular file nor a directory.
- `content_hash` is SHA-1 over the files in `Path` (component-wise) order. Each file contributes its path, NUL, an optional `x` NUL executable marker, its decimal length, NUL, and its bytes. Toggling an executable bit therefore counts as a change, which `update` installs and re-vets.

### Add (`store::add`, `store::resolve`, `Outcome`)
- `store::inventory` canonicalises the store path, as it does the source, because ledger event paths are compared in resolved form.
- `store::resolve` treats a source as trusted with `--trust` or a trusted-list match, and refuses an untrusted source as a usage error when the config has no `[judge]`.
- `Outcome` is ordered `Done < Failed < Usage`, so an add or update can report each failure as it happens and still exit with the worst. A refusal for want of a `[judge]` is `Outcome::Usage`, exit 2.
- `store::add` with `--project` fails as a usage error outside a git repository before anything else. It vets an untrusted source before installing, and a failed skill lets the rest install.
- `store::add` sets up the judge backend and checks its login before cloning anything.
- `store::add` defers the answerable check for `--all` to `confirm_calls`, which shows the call count once the source has been read.
- `store::source::local` expands only `~` and `~/` against home, as config paths do, and refuses `~user`. `mazo add` expands the tilde itself because no shell does so when the TUI calls it.

### Install and remove (`store::install`, `Target`, `Fresh`)
- `store::install` leaves the store and lockfile unchanged on a usage error (bad skill, taken name or project link) or on a refusal (refused vet, or a sync that would fail with the skill installed, such as one missing from the Claude settings' `skillOverrides`). For `--project`, a link that cannot be made undoes any link already made.
- `store::install` checks that the name is unclaimed before paying for the judge, and again under the data lock.
- `store::install` with `--project` refuses a name the global store holds, but a global add does not check projects.
- `Target::home_sync` returns an empty plan for a project's store, because homes do not link project skills. A project skill gets its own links in the project skill dirs, and `update` skips the home sync for project skills.
- `Target::remove` deletes links, then the lock entry, then the directory, so a failure leaves at worst an unrecorded skill, never a lock entry for a missing one.
- `Fresh` removes the store directories that staging created, deepest first, when they are still empty after a refused add. `store::install` declares it first so it is dropped last.
- `store::install` accepts a crash window between the rename and the lockfile write. That window leaves an unrecorded store skill, which a re-run refuses as taken and which `sync` links like any other store skill.
- `store::install` replaces the caller's inventory after recording, so the judge vets the next skill of the same add alongside this one.

### Locking, staging and undo (`store::install`, `update::refresh`)
- `store::install` and `update::refresh` stage under a hidden per-process name that no root lists, so the judge and the y/N prompt run without holding the data lock.
- The data lock (`durable::lock`) also guards a project's store and `mazo.lock`, so two adds into one project serialise only when they share a data dir.
- `store::install` and `update::refresh` re-read the inventory under the data lock, so a concurrent retire or add cannot leave the link plan or sync check stale.
- `store::install` and `update::refresh` report a failed undo as the error, saying why the undo ran.

### Project scope (`store::project`)
- `PROJECT_STORE` lives under the git root and is committed with the project. `add --project` writes `mazo.lock` at the git root in the global lockfile's shape, and writes nothing global: no store skill, no global lockfile entry, no home link.
- `PROJECT_LINKS` names Claude Code's project skill dir and the one Codex and Pi share, each two levels under the git root.
- `git_root` finds the nearest ancestor holding `.git` (a directory or a worktree file) without running git, so loading the project adds no git subprocess. It canonicalises first, so a project reached through a symlink is found at its physical root.
- `scoped_inventory` merges install dates from the global and project lockfiles because `Inventory::load_roots` records none.
- `project_links` uses relative targets, so a project link resolves in any checkout of the project.
- `unlink_project` removes only project links that still point at the skill's project store directory.
- Global and project scopes are disjoint. `update` and `retire` without `--project` do not touch project skills, and naming one adds the `--project` pointer from `project_hint`. `retire --project` does not retire a global skill, and a global retire leaves the project untouched.
- `Project::at` treats a git repository without `mazo.lock` as no project, so `status` and `usage` print as they do outside one. Inside a project, `status` reports a missing project link as drift of the project skill.
- `Project::skill_of` resolves only paths that name a project skill, through their deepest existing ancestor, so a removed link under a symlinked root such as macOS `/var` still matches. The scan knows no project and records a read through a project link as other-origin; `skill_of` attributes it.

### Sources (`store::source`)
- `Remote::ref` must be a full 40-hex commit id to be cloned as a commit; `None` means the default branch.
- `Source::parse` treats existing paths, and those starting with `.`, `/` or `~`, as local. It refuses git sources containing `?`, `#` or `\`, because each can make the apparent host github.com while git fetches from another host. `Source::git` refuses `<transport>::` sources because git runs them as remote helpers, and `ext::` executes a command.
- `Source::git` appends `.git` so each repository has one origin, except for `file://` URLs, which name the directory as is.
- `Source::git` cuts the repository path at the first segment ending in `.git`, so only the repository's last segment can carry the suffix.
- `github_repo` normalises only bare github.com remotes, in https, ssh or `git@github.com:` form, to `https://github.com/<owner>/<repo>.git`. It requires a `:`, so `owner/repo` shorthand (which parses as GitHub whatever its origin) and local paths are not taken for GitHub URLs.
- `github_browser_url` parses GitHub tree and blob URLs and drops their query or fragment. Because a ref may contain `/`, it records the ref/path split as a guess (`Remote::browsed`). `Remote::resolve` settles the guess by picking the longest listed branch or tag that whole segments of `<ref>/<path>` start with, else a full commit id. It runs `ls-remote` before the clone, because parsing does not touch the network.
- `Remote::clone_into` fetches a commit into an empty repository, because `clone --branch` does not accept a commit and `--revision` needs git 2.49.
- `Remote::git` disables git and ssh prompting unless the user set `GIT_SSH_COMMAND`, limits transports through `GIT_ALLOW_PROTOCOL`, and kills the process on cancel or after `CLONE_LIMIT`.
- `Scratch::new` clears its own path and every same-prefix directory whose process is gone, since an interrupted run leaves its scratch behind and no later run reuses it. A live process's scratch is kept.
- SAFETY (`libc::kill` in `gone`): signal 0 only probes and takes no pointers.

### Trust (`Remote::trusted_by`, `update`)
- `Remote::trusted_by` matches an entry on host plus owner or owner/repo, case-insensitively. A first segment without a `.` is read as a GitHub owner, so an entry for one host does not trust the same owner on GitHub, and a `file://` URL is never trusted.
- `Locked::trusted` is a record only. `update` re-derives trust on every run from the trusted list and the bundled check, and never grants it from the lockfile, because anyone can commit a project's lockfile. No project holds a bundled skill. A source dropped from the trusted list is untrusted on the next update.

### Update (`store::update`)
- `update` re-vets any skill that is neither on the trusted list nor bundled, regardless of its lock entry, and refuses it with exit 2 when no judge is configured. A failing skill stays unchanged while the rest run. Bundled skills that fall behind the running version move to its tag unvetted.
- `update` runs the home sync under the data lock, on an inventory read under it, as `mazo sync` does.
- `update::refresh` vets a changed copy unlocked and swaps it in under the data lock. On a refused vet, a failing sync or a failed write, it leaves the old copy and the lockfile untouched. Before that, `update::recover` puts back any copy an interrupted swap set aside.
- `update::refresh` refuses the swap when the installed copy's hash changed after the unlocked vet diffed it.
- `update::refresh` removes the set-aside copy only best-effort, since no root lists a hidden leftover.
- `update::recover` restores the set-aside copy when the target is missing, and deletes it only when the target exists.
- `Revet` sets up the judge and checks its login lazily, so an update that touches only trusted skills never contacts the judge.
- `Revet::vet` sends the judge `git diff --no-index --text`, so binary changes cannot hide behind "Binary files differ". It rewrites the staged paths to the installed name and returns the hash of the copy it diffed from.
- `Revet::vet` passes every git diff flag that user config could otherwise change, and accepts exit code 1 because that only means the copies differ.

### Vet gate (`store::vet`)
- `VET_FILE_BYTES` and `VET_TOTAL_BYTES` cap what the judge is sent. A file past either cap, or one that is not valid UTF-8, is withheld from the judge but still installed.
- `vet::gate` treats any file or diff withheld from the judge as a security finding of mazo's own, since nobody reviewed it. It refuses before the judge call unless `--force`. With `--force` the judge sees what fits and the verdict is still shown. The y/N prompt still applies, defaulting to `VetVerdict::default_yes`, unless `--yes`.
- `vet::gate` renders withheld file names inert because the skill's author chose them.

### Work summary (`store::work`)
- `work::summary` builds the judge's view of recent work from the ledger only. It carries no user prompts, so a skill under vet cannot get the judge to copy them into the lockfile verdict. It is deterministic for a given ledger and time.
- `SUMMARY_BYTES` caps the work summary; `capped` cuts whole lines and appends a note saying it was cut.
- `work::summary` counts only strong-channel, main-actor events within the last `DAYS` days.

### Judge detection at init
- `init` counts claude or codex as a judge only with a subscription login (claude.ai or OAuth token for claude, a ChatGPT login for codex), never an API key. Its login checks leave no data directory and no scratch directory behind.

### Status output
- `status` treats a home whose only findings are foreign entries as in sync and exits 0.
- `status` prints entry names through `render::inert_line`, because another installer controls a foreign entry's name and it may carry terminal escapes or a newline that would forge an output line.

### Test design
- `PLAIN_SKILL` violates every configurable policy rule at once, so a default config, with every rule off, is shown to accept it.
- `a_missing_config_exits_2_naming_the_discovered_path` plants a config at `~/dotfiles/skills/mazo.toml` to prove that config discovery never falls back to that former default.
- `a_file_named_like_the_remote_does_not_hide_it` creates its directory relative to the crate root, where tests run, under an owner name unique to the test.
- `hosted_repo` serves a local bare repository under an https URL through a git `insteadOf` rewrite, so the source has a host to trust.
- The `init` helper leaves only git on `PATH` because `mazo init` detects judges from `PATH`.
- `add_in_picker` passes `winsize` as a `*mut` pointer because Apple's `openpty` takes `*mut winsize` while Linux's takes `*const`.
- SAFETY (`openpty` in `add_in_picker` and `add_at_terminal`): it writes two new descriptors, which the helper immediately wraps in `OwnedFd`.
- `an_unlistable_skills_dir_fails_status` checks listability first and skips its assertions when the directory is listable, because root can list a `0o311` directory.
- `status_json_reports_a_name_that_is_not_utf8` returns early on `EILSEQ` because APFS refuses non-UTF-8 names, so macOS never lists one.
- `conflict_in_any_harness_aborts_all_destinations` writes valid settings in both Claude homes, so sync fails for the conflict and not for policy.
- `retire_project_removes_the_project_copy_of_a_name_the_global_store_also_holds` adds the project copy first, because `add --project` refuses a name the global store already holds.
