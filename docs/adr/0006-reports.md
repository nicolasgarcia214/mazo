# 0006. Reports and rendering

Status: Accepted (2026-11-26)

## Context
The report commands (`usage`, `unused`, `show`, `report`), the HTML page and the
terminal renderer carried their design decisions as code comments until the comment
ban (ADR 0001). This record keeps those decisions: what each count means, which
window it covers, where untrusted text is made inert, and why the tests are built
the way they are.

## Decisions
### report
- `Input` carries `now` and `tz` as injected values instead of reading the clock and the zone, so report tests are deterministic.
- `Input::tz` is the one zone for both bucketing days and showing times. `html::render` shows every time in the model's zone: the events table, the usage `last` column, the card's last use and its histogram.
- `UsageArgs::new` is the default filter, and both `show` and the report's cards count with it.
- `SHOW_LIMIT` is `show`'s default `--limit` and also the number of events the report's and the TUI's cards list.
- `legend` explains only the help marks that the rendered cells show, and returns nothing when they show none. It, the TUI help and the HTML page all take their marks and glyphs from `help::legend`.
- `json_line` serialises through a `serde_json::Value` so the emitted keys come out sorted.
- `report` lists grades due only for inventory skills, because rows outside the inventory have no grade and nothing to grade.

### report::view
- Reports collapse a skill's events by `Key`, the (agent, session, channel) triple. Repeated events with the same key, such as a second chunk of the same file read, count once.
- `Filter::default` counts only canonical-origin events, excludes probe actors and excludes authoring reads.
- `View::new` matches `policy.authoring_skills` against the name after the last `:`, so a plugin skill such as `skill-creator:skill-creator` counts.
- `View::scope` treats an event as a project skill's when its id is in a project skill's directory, and every other event as global.
- `View::is_authoring` decides authoring at report time. A weak event is authoring when it was stored so by the cwd rule, or when its root session holds a strong authoring-skill event anywhere in the ledger. Strong events are never authoring.
- `View::kept` treats a project skill's event as canonical whatever origin the scan gave it, because the scan knows no project.
- `View::coverage` gives each home's effective start as the later of the report window and the home's first transcript. It also reports the earliest typed event when that reaches further back, because history backfill reaches back before the first transcript.
- `Unit::of` buckets a card's activity chart by day for windows of 30 days or fewer, by ISO week from Monday for longer windows, and by calendar month for all time. Each bar counts distinct (agent, session) pairs with a strong event, and today's bucket comes last and is partial.

### report::usage
- `UsageRow::sessions_7d` and `UsageRow::sessions_30d` count strong sessions over the last 7 and 30 local days whatever the report window, under the usage filters. They use local days, not 24-hour spans, so an event within 30×24 hours but 30 local days ago falls outside `sessions_30d`.
- `UsageRow::trend` compares `sessions_30d` with the 30 days before it. `Trend::of` returns up at +20% or more, down at -20% or less, flat in between, new when the earlier period is zero, and none when both are zero.
- `vet_of` returns a store skill's lockfile entry only when no source skill of the same name shadows it.
- `verdict_of` marks the audit column `!` when the lockfile vet found a security issue, else `✓` when an audit judged the skill, else `vet` when only the vet did, else `-`. The verdict comes from the latest audit, else from the vet with any security finding leading it, and its source is reported apart. An audit wins over the vet's verdict but never over its security mark. Project skills have no audit and read the project's lockfile.
- `usage_rows` emits one row per global skill and one per project skill, so a name in both gets two rows, sorted most used first. Outside a project every row is global.
- With `--all`, `usage_rows` adds every global skill in the ledger so retired and pruned skills keep a row, except names only the project has, which keep only their project row.
- `usage_rows` loads grades only for global skills in the inventory, because only source and store skills have grades. A global skill's grade never applies to a project skill of the same name. A store skill's grade comes from `evals_dir` as a source skill's does, and `<source>/.grades` is not read.
- A grade shows its score alone while its `content_hash` matches the skill, and `stale` once the skill changes. A grade with no hash is stale.

### report::unused
- A skill is used only when it has a strong event inside the window. An older strong event does not count.
- `UnusedRow::holders` lists the citers used in the window. Held status is not transitive: a skill whose only citer is held, not used, stays unused, and `UnusedRow::cluster` lists those unused citers.
- New status wins over held, so an uncommitted skill cited by a used skill reports new.
- `UnusedRow::named_by` lists skills used in the window that name this one, but being named never makes a skill held. A mention by a skill not used in the window gives no note.
- `inventory::mentions` counts whole-word names that are not already `fan_in` edges. A path such as `x-goal-plan/` is not a word match.
- `unused_of` dates a store skill from its install and a source skill from its first git commit. Outside a git worktree no source skill is new.
- `retire_blockers` refuses to retire a skill that has a strong event in the last `days` days, or that a citing or naming skill with such an event holds.

### report::card
- `CardUsage::strong` counts strong events in the window, collapsed by `Key` and grouped per agent and channel.
- `CardUsage::last_use` is the latest strong event in the whole ledger, not only the window.
- `CardUsage::projects` lists the top three cwds in the window, ranked by distinct root sessions with a strong event.
- `Card::strong_since_retro` counts collapsed strong events on days after the latest retro, over the whole ledger.
- `Card::grades` and `Card::retros` list newest first.
- `Manual::guide` takes For and Try from the latest skill audit when it has a For and is newer than the vet or there is no vet. Otherwise For comes from the lockfile vet, falling back to the frontmatter description, and Try comes from the vet or is empty. A source skill with no audit keeps its description.
- `MANUAL_WIDTH`, `FOR_LINES` and `ITEM_LINES` bound the manual so For plus three Your use and three Try items fit in 15 lines of 80 columns, cutting with `…`. Your use takes each prompt's first line cut to `USE_CHARS`; empty parts are left out and the manual opens the card.
- `SkillOrigin` comes from a store skill's lockfile entry, or for a source skill from the source repository's `origin` remote at HEAD. Both paths normalise `SkillOrigin::repo` through `store::github_repo`.
- `origin_of` reads only the lockfile and git config, never the network. It returns none for local installs, non-GitHub origins and source repositories without a github.com `origin` remote.
- `origin_of` caches the source repository's remote and HEAD in `View::upstream`, so git runs once per report however many cards it builds.
- `Card`'s line methods are shared by `show` and the HTML report so both render the same fields.
- `Card::history_line` prints the first ten characters of the commit's ISO date, which is the committer's own local day, without converting it to the report zone. Outside a git worktree the card omits history and still reads the grade.
- `AuditLines::rates` shows the rates mazo computed, not figures the judge reported.
- `check_skill` accepts any name in `Inventory::known`, which includes canonical names.
- `card` places a name found only in git history under the source root, so it keeps its path in the source.
- `card` marks a cited or citing skill as held when a skill citing it was used in the window.
- `card` computes ctx with `Scope::Global` because `show` accepts only known names, never a project-only one.
- `usage_section` serialises a card's usage section over `show`'s default window for the audit's stage 2 to read.
- `tilde` shows `$HOME` as `~` and passes the path through `inert_line`, because the cwd comes from a transcript and is untrusted. Text output uses `tilde` and short locators; `--json` keeps full paths.

### examples
- `real` takes the most recent informative prompt per project from the three most recent projects. A prompt under 20 characters or one that is only the bare invocation gives way to the project's next event, and an expired transcript ends that project's search.

### evals
- `date` accepts a retro or grade file's date only when it is a real calendar date, not just the `YYYY-MM-DD` shape.

### html
- `html::render` produces one self-contained file with inline CSS and inline SVG from the `Report` model alone, so the page shows nothing the JSON lacks. `Report`'s usage serialises as `usage --all --json` and each card as `show --json`, so the page cannot drift from either.
- `html::write` writes the page atomically with mode 0600, replacing any previous file, because the page shows cwds and prompt excerpts and is the one place prompt text is written.

### render
- `status_colour` paints `unused`, `ERROR:` and the audit security mark `!` red, and `held`, `new`, `warning:`, `named by` notes and a grade's `stale` mark yellow. `NO_COLOR` turns all colour off.
- `unprefixed` strips `ERROR:` or `warning:` for JSON output, whose key already says which.
- `table` measures cells with `width`, which counts display columns minus `ESC [ … m` colour sequences, so painted and wide cells stay aligned. It trims trailing spaces from every line.
- `inert` replaces C0, DEL and C1 controls (except newline and tab), bidi controls, the zero-width space, the BOM and the line and paragraph separators with U+FFFD, so skill or judge text cannot drive or reorder the terminal. Zero-width joiners stay because scripts and emoji need them. A skill description's controls print as U+FFFD in the card and in `--json`.
- `inert_line` also replaces newline and tab, so a name cannot forge a line of output.
- `InertPath` shows every path through `inert_line`, and `clippy.toml` bans `Path::display` so no path bypasses it.
- `inert_value` makes every string inert, keys included, because a judge's answer or a committed lockfile is untrusted.

### Test design
- `fan_in_on_the_fixture_tree_matches_each_rule` uses a fixture tree with one skill per citation rule, plus a description, an `owned by graders` mention and self-mentions that must yield no edges.
- `fan_in_on_a_real_repo_has_no_self_edges_and_only_skill_targets` runs only with `MAZO_TEST_SKILLS` set and checks only invariants that hold on any skills repository.
- `grade_of` builds a fresh report so the skill is hashed again after the test edits it.
- `the_usage_guide_sample_is_current` pins the usage guide's `mazo usage` sample to the binary's output for its scenario.
- `reports_match_their_golden_files` compares every rendered report and the judge schemas `audit::SESSION_SCHEMA` and `audit::SKILL_SCHEMA` byte for byte with `tests/golden/`, so a renderer or schema change is deliberate. `UPDATE_GOLDEN=1` rewrites the files so the change shows as a diff, and JSON outputs are pretty-printed so that diff is readable.
- `reports_match_their_golden_files` uses an HTML-escaped placeholder for `.html` outputs so the page renders the placeholder as written.
- `scrub` asserts the tempdir name is gone after replacement. This relies on the name holding no character HTML escapes, and keeps golden files machine-independent.
- `assert_self_contained` scans for `src` and `href` only inside tags, because escaped text has no `<` to open one.
