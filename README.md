# Synapse

**Single-command AI harness installer and auto-updater.**

Synapse installs and keeps the AI coding ecosystem — [herdr](https://github.com/herdrdev/herdr),
[omp](https://omp.sh), and [skillshare](https://github.com/runkids/skillshare) — up to date on
macOS, Linux, and WSL2. Packages are built and pinned with Nix, so every machine gets the same
versions and updates are atomic and reversible.

```bash
synapse install              # interactive installer (TUI)
synapse status               # platform, Nix, installed packages
synapse update --all         # update everything
synapse rollback             # revert to the previous versions
synapse auto-update enable   # schedule daily update checks
```

## Why

Setting up an AI coding environment by hand means installing Nix, sourcing packages for each tool,
wiring up shell PATH and completions, and then remembering to check for updates. Doing it on a
second machine means doing it all again, slightly differently.

Synapse makes it one command, pins exact versions via a committed `flake.lock`, and keeps the last
three versions of every package so a bad update is one command to undo.

## Install

Synapse requires **Nix 2.24 or newer**. It does not install Nix for you: on macOS the official
installer creates a dedicated APFS volume and modifies system files, which is not a change to make
on your behalf.

```bash
# 1. Install Nix (if you don't have it)
curl -fsSL https://install.determinate.systems/nix | sh -s -- install

# 2. Install Synapse
nix profile install github:thinhngotony/synapse

# 3. Install the harness
synapse install
```

`synapse doctor` will tell you if anything is missing or misconfigured.

## Commands

| Command | Description |
|---------|-------------|
| `synapse install` | Interactive TUI: pick packages, watch progress |
| `synapse status` | Platform, Nix version, installed packages, update status |
| `synapse doctor` | Diagnose missing PATH entries, broken symlinks, stale locks |
| `synapse list` | Installed packages with versions and install timestamps |
| `synapse update [pkg]` | Update one package |
| `synapse update --all` | Update every installed package |
| `synapse rollback [pkg]` | Revert to the previous version (last 3 retained) |
| `synapse uninstall --all` | Remove packages from management and clean up shell config |
| `synapse log` | Install and update history |
| `synapse setup-shell` | Configure PATH and shell completions |
| `synapse auto-update …` | `enable`, `disable`, `config`, `now`, `status` |
| `synapse version` | Version, git commit, target triple |

Add `--dry-run` to `setup-shell` to see exactly what would change before anything is written.

## What Gets Installed

| Package | What it is | Source |
|---------|-----------|--------|
| herdr | Terminal multiplexer for coding agents | nixpkgs |
| omp | Oh My Pi coding agent CLI | npm, wrapped with Bun |
| skillshare | Sync AI CLI skills across tools | upstream release binaries |

## Auto-Update

```bash
synapse auto-update enable    # register a scheduled update
synapse auto-update status    # is it actually registered?
synapse auto-update config    # edit the schedule in $EDITOR
synapse auto-update now       # run one immediately
synapse auto-update disable   # unregister
```

Scheduling uses whatever the platform provides:

| Platform | Mechanism |
|----------|-----------|
| macOS | launchd agent (`~/Library/LaunchAgents/`) |
| Linux with systemd | systemd user timer (`~/.config/systemd/user/`) |
| WSL2 / no systemd | cron |

Configuration lives in `~/.config/synapse/auto-update.yaml`:

```yaml
enabled: true
check_interval: 1d
install_interval: 7d
notify: true
preferred_time: "02:00"
max_duration: 30m
```

Scheduled runs take the same lock as interactive ones, so an update can never collide with an
install you started yourself.

## How State Works

Synapse tracks what it installed in `~/.config/synapse/state.json`:

- Every write is atomic (temp file + rename), so an interrupted update cannot corrupt it.
- A PID lock at `~/.config/synapse/.lock` prevents concurrent installs. Stale locks from a crashed
  process are detected and cleared automatically.
- The last **3** versions of each package are retained as rollback targets.

Rolling back reuses the old Nix store path when it is still present, so it is usually instant rather
than a rebuild.

## Shell Integration

`synapse setup-shell` configures bash, zsh, and fish — whichever you actually use. Everything it
writes is wrapped in a marker block:

```bash
# >>> synapse >>>
...managed lines...
# <<< synapse <<<
```

Re-running replaces that block in place rather than appending a second copy, so your PATH never
accumulates duplicates. Nothing outside the markers is ever modified, and `synapse uninstall`
removes the block cleanly.

## Platform Support

| Platform | Status |
|----------|--------|
| macOS aarch64 (Apple Silicon) | Supported |
| macOS x86_64 (Intel) | Supported |
| Linux x86_64 | Supported |
| Linux aarch64 | Supported |
| Windows WSL2 | Supported (cron fallback for auto-update) |
| Windows native | Not supported |

## Requirements

- Nix 2.24+ with flakes enabled
- bash, zsh, or fish
- macOS 13+, or Linux with glibc 2.31+

## Development

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings

nix build .#synapse       # build via the flake
nix build .#harness       # build every managed package
nix develop               # dev shell with the full toolchain
```

CI runs the full test suite, clippy, shell integration, and flake builds across macOS
(Intel + ARM) and Linux (x86_64 + ARM) on every pull request.

## License

MIT
