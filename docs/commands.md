# Commands

| Command | What it does |
|---|---|
| [`mazo init`](configuration.md#mazo-init) | Write the config and install the bundled grade and retro skills (`--no-skills` skips them). |
| [`mazo scan`](usage-tracking.md#mazo-scan) | Read new log lines into the ledger. |
| [`mazo usage`](usage-tracking.md#mazo-usage) | One row per skill with its loads, last use and audit verdict. |
| [`mazo unused`](usage-tracking.md#mazo-unused) | Skills with no strong load in the window. |
| [`mazo show <skill>`](usage-tracking.md#mazo-show) | Everything mazo knows about one skill. |
| [`mazo evals dir <skill>`](configuration.md#evals_dir) | Print the skill's grades and retros directories. |
| [`mazo report`](usage-tracking.md#mazo-report) | One self-contained HTML page of usage and cards. |
| [`mazo status`](managing-skills.md#mazo-status) | Drift and policy for every home. Writes nothing. |
| [`mazo sync`](managing-skills.md#mazo-sync) | Link every skill into every home. |
| [`mazo new <name>`](managing-skills.md#mazo-new) | Create a skill in `source`. |
| [`mazo add <source>`](installing-skills.md#mazo-add) | Install a skill into the store, vetting an untrusted one. |
| [`mazo update [names...]`](installing-skills.md#mazo-update) | Re-fetch store skills from their origins. |
| [`mazo retire <name>`](managing-skills.md#mazo-retire) | Archive a skill of `source`, or remove a store skill. |
| [`mazo prune [<name>]`](managing-skills.md#mazo-prune) | Delete archived skills from `.archive/`. |
| [`mazo audit <skill>`](audit.md) | Have an agent judge how the skill performs in real sessions. |
| [`mazo`](tui.md) | Open the TUI; print the help when not on a terminal. |

`add`, `update` and `retire` take `--project` to act on one project's skills;
see [project skills](project-skills.md). Exit codes are 0 on success, 1
when a command failed or found a problem, and 2 for a usage error; the
[exit code table](configuration.md#exit-codes) lists the cases.

Back to the [README](../README.md).
