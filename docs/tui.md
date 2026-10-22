# The TUI

This guide covers the terminal UI that bare `mazo` opens: its keys, its dialogs
and its background jobs. Back to the [README](../README.md).

Bare `mazo` at a terminal opens a TUI over the same data as `usage`, `unused`
and `show`: the usage table, the unused list and a skill's card. Off a
terminal, bare `mazo` prints the help. The TUI scans first and writes nothing
but what an action writes, and it re-reads the data after each action.

## Keys

| Key | Action |
|---|---|
| arrows, `hjkl` | Move. |
| `Enter` | Open a card. On the unused list, a store skill whose status is `unused` (its `new` window since install has ended) asks to retire it instead. |
| `Esc` | Go back. On a list, close the job pane once the job ends. |
| `Tab` | Switch between usage and unused. |
| `/` | Filter by name. |
| `w` | Cycle the window: 30 days, 60 days, all. |
| `a`, `c` | Cycle the agent and channel filters (not on the unused list). |
| `A` | Toggle authoring reads (not on the unused list). |
| `r` | Retire the selected skill, or the card's. |
| `s` | Show the actions `mazo sync --dry-run` would print, or say the homes are in sync. |
| `u` | Update the selected store skill, or the card's. |
| `U` | Update every store skill. |
| `i` | Add a source. |
| `x` | Audit the selected skill, or the card's. |
| `q` | Quit. |

## Retire

`r` retires as [`mazo retire`](managing-skills.md#mazo-retire) does. When
`retire` would refuse a skill, the dialog lists its blockers. There is no
`--force`.

## Update

`u` and `U` update as [`mazo update`](installing-skills.md#mazo-update) does.
A source skill, or one installed from a local path, shows that it is not
updatable.

The dialog names the judge and its cap and asks "update and ask about each
judge verdict?". The update runs in the background pane. Each re-vet of an
untrusted skill that changed opens the y/N dialog an add uses. Enter takes the
prompt's default, and Esc or `q` answers No.

There is no `--force`, so a security finding still refuses that skill's
update. mazo reports an up-to-date skill without a judge call.

## Add

`i` asks for a source (Enter adds, Esc cancels) and adds it as
[`mazo add <source>`](installing-skills.md#mazo-add) does, in the background
pane.

- An untrusted source with no `[judge]` shows the refusal `add` gives and
  starts nothing.
- A source of several skills opens the picker in a dialog.
- For an untrusted source, the dialog shows the number of judge calls, one per
  picked skill, and asks `make <n> judge calls? [y/N]` before the first. A
  trusted source asks nothing and calls no judge.
- Each verdict then opens a dialog with the `install <name>?` prompt and the
  default `add` gives it. Enter takes the default, `y` and `n` answer, and Esc
  or `q` is No (`q` then asks to quit).
- There is no `--force`, so a security finding refuses that skill.
- A skill reaches the store only once you accept its verdict.

## Audit

`x` audits as [`mazo audit <skill>`](audit.md) does with its default caps. The
dialog shows the cap (20 sessions, and $1 for a judge that reports its cost)
or the refusal `mazo audit` would give. The audit runs in the background, and a
pane above the footer shows its progress and result while the views keep
working.

## Jobs and confirmation

One job, an add, an update or an audit, runs at a time. While a job runs, `r`,
`s`, `i`, `u`, `U`, `x` and `Enter`'s retire on the unused list show that it is
running and do nothing. Quitting while one runs asks first, and on `y` mazo
kills its judge call.

Retiring, applying the plan `s` shows, starting an update and starting an
audit each run only on `y`; any other key cancels.
