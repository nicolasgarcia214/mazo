# mazo

See which skills Claude Code, Codex and Pi actually load, and keep one skills
directory linked into all of them.

mazo reads the session logs those agents already write and stores each skill
load in its own ledger. It links your skills, and skills you install from git,
into every agent home. Before it installs a skill from a source you have not
trusted, it has a judge of your choice vet it.

## What it does

- Counts every skill load per session, channel and agent, with a 30-day
  sparkline and the last use.
- Lists skills with no strong load in a window, and retires them.
- Links your skills directory and the skills you install into each configured
  agent home, such as `~/.claude`, `~/.codex` and `~/.pi/agent`, and reports
  drift.
- Installs skills from a git URL, a GitHub `owner/repo` or a local path into a
  store pinned by `mazo.lock`, for you or for one project.
- Sends an untrusted skill to a judge agent before installing it, and again
  when an update changes it.
- Audits how a skill performs in your real sessions and writes a report with
  ranked improvements.
- Shows all of this in a terminal UI and in one self-contained HTML page.

## Install

mazo runs on Linux and macOS (x86_64 and aarch64) and needs at least one of
Claude Code, Codex or Pi.

No release has been published yet. A release will ship a prebuilt binary
through npm, Homebrew and a shell installer. These download from the
repository's GitHub releases, so they work once a release is published and
readable:

```sh
npx mazo                          # or: npm install -g mazo
brew install nicolasgarcia214/tap/mazo
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/nicolasgarcia214/mazo/releases/latest/download/mazo-installer.sh | sh
```

### Build from source

Until then, or to build from source with a Rust toolchain, let Cargo fetch the
repository with your git credentials by setting
`CARGO_NET_GIT_FETCH_WITH_CLI=true`:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true \
  cargo install --locked --git https://github.com/nicolasgarcia214/mazo
```

## Quick start

For your own skills you need a skills directory: one subdirectory per skill,
each holding a `SKILL.md`. If you only install other people's skills with
`mazo add`, you need none.

```sh
mazo init                         # write the config; add --source <skills dir> for your own skills
mazo sync                         # link every skill into every agent home
mazo usage                        # see which skills your agents load
mazo unused                       # list skills with no strong load in the window
mazo add owner/repo               # install a skill from GitHub, vetted unless trusted
mazo                              # open the terminal UI
```

## Commands

| Command | What it does |
|---|---|
| [`mazo init`](docs/configuration.md#mazo-init) | Write the config. |
| [`mazo scan`](docs/usage-tracking.md#mazo-scan) | Read new log lines into the ledger. |
| [`mazo usage`](docs/usage-tracking.md#mazo-usage) | One row per skill with its loads, last use and quality. |
| [`mazo unused`](docs/usage-tracking.md#mazo-unused) | Skills with no strong load in the window. |
| [`mazo show <skill>`](docs/usage-tracking.md#mazo-show) | The skill card. |
| [`mazo report`](docs/usage-tracking.md#mazo-report) | One self-contained HTML page of usage and cards. |
| [`mazo status`](docs/managing-skills.md#mazo-status) | Drift and policy for every home. Writes nothing. |
| [`mazo sync`](docs/managing-skills.md#mazo-sync) | Link every skill into every home. |
| [`mazo new <name>`](docs/managing-skills.md#mazo-new) | Create a skill in `source`. |
| [`mazo add <source>`](docs/installing-skills.md#mazo-add) | Install a skill into the store, vetting an untrusted one. |
| [`mazo update [names...]`](docs/installing-skills.md#mazo-update) | Re-fetch store skills from their origins. |
| [`mazo retire <name>`](docs/managing-skills.md#mazo-retire) | Archive a skill of `source`, or remove a store skill. |
| [`mazo prune [<name>]`](docs/managing-skills.md#mazo-prune) | Delete archived skills from `.archive/`. |
| [`mazo audit <skill>`](docs/audit.md) | Have an agent judge how the skill performs in real sessions. |
| [`mazo`](docs/tui.md) | Open the TUI at a terminal; print the help off one. |

`add`, `update` and `retire` take `--project` to act on one project's skills;
see [project skills](docs/project-skills.md). Exit codes are 0 on success, 1
when a command failed or found a problem, and 2 for a usage error; the
[exit code table](docs/configuration.md#exit-codes) lists the cases.

## Configuration

mazo reads `$MAZO_CONFIG`, else `$XDG_CONFIG_HOME/mazo/config.toml`
(`~/.config/mazo/config.toml` when `XDG_CONFIG_HOME` is unset). `mazo init`
writes it.

```toml
source = "."                      # your skills directory, relative to this file; optional
loose  = ["SKILLS_USAGE.md", "GOAL_SKILLS_USAGE.md"]
trusted = ["owner", "owner/repo"] # sources whose skills install without vetting

[[home]]
name  = "claude"
agent = "claude"
path  = "~/.claude"

[[home]]
name  = "pi"
agent = "pi"
path  = "~/.pi/agent"
links = { "extensions/pi-explicit-skills.ts" = "pi-explicit-skills.ts" }

[judge]                           # the agent mazo audit calls; none without this table
agent = "claude"                  # claude (default), codex, pi, command or http
model = "sonnet"                  # optional; unset means the agent's own default
billing = "subscription"          # subscription (default) or api
```

- [`source`](docs/configuration.md#source): your own skills directory.
- [`loose` and `links`](docs/configuration.md#loose-and-home-links): extra
  files to link into each home.
- [`[[home]]`](docs/configuration.md#home): one table per agent home.
- [`trusted`](docs/configuration.md#trusted): owners and repositories whose
  skills install without vetting.
- [`[policy]`](docs/configuration.md#policy): optional rules that `sync` and
  `status` check.
- [`[judge]`](docs/configuration.md#judge): the agent that vets and audits,
  one of claude, codex, pi, command or http, and who pays for it.

The [configuration reference](docs/configuration.md) also covers the data
directory, the store and the `mazo.lock` format.

## TUI

Bare `mazo` at a terminal opens a TUI with the usage table, the unused list and
each skill's card. The main keys:

| Key | Action |
|---|---|
| `Enter`, `Esc` | Open a card, go back. |
| `Tab` | Switch between usage and unused. |
| `/` | Filter by name. |
| `i`, `u`, `x` | Add a source, update a store skill, audit a skill. |
| `q` | Quit. |

The [TUI guide](docs/tui.md) lists every key and dialog.

## Privacy

mazo collects no telemetry. It sends data only to the `[judge]` you configure
(see [vetting a skill](docs/installing-skills.md#vetting-a-skill)). Its only
other network use is the git clone that `mazo add` and `mazo update` make of
the source you name.

## Development

CI runs these on Linux and macOS:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
TZ=Pacific/Kiritimati cargo test --locked
TZ=Pacific/Pago_Pago cargo test --locked
```

[CONTRIBUTING.md](CONTRIBUTING.md) covers the test suite and adding an agent.

## License

[MIT](LICENSE)
