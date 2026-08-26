# Synapse

**Single-command AI harness installer, auto-updater, and portable OMP stack restore.**

Synapse installs and keeps [herdr](https://github.com/herdrdev/herdr),
[omp](https://omp.sh), and [skillshare](https://github.com/runkids/skillshare) up to date on
macOS, Linux, and WSL2. It also captures OMP plugins and MCP definitions into Skillshare's tracked
Git repository so a new machine can restore the same stack. Nix pins package versions; Git carries
portable configuration.

```bash
synapse install              # interactive installer (TUI)
synapse status               # platform, Nix, installed packages
synapse update --all         # update everything
synapse rollback             # revert to the previous versions
synapse auto-update enable   # schedule daily update checks
synapse stack capture       # capture OMP plugins/MCP into tracked Git
synapse stack restore --trust  # restore after reviewing the captured code/config
```

## Why

Setting up an AI coding environment by hand means installing Nix, sourcing packages for each tool,
wiring up shell PATH and completions, and then remembering to check for updates. Doing it on a
second machine means doing it all again, slightly differently.

Synapse makes it one command, pins exact versions via a committed `flake.lock`, and keeps the last
three versions of every package so a bad update is one command to undo.

## Install

### Public bootstrap

The repository and release assets are public. Anonymous bootstrap was verified end to end against
the published v1.0.0 release:

```bash
curl -fsSL https://synapse.hyberorbit.com/install | sh
```

The portable-stack and dedicated-profile work in this checkout targets v1.1.0 and remains
unreleased until its commit, CI matrix, and release workflow complete.

### Manual / developer install (Nix already installed)

If you already have Nix 2.24+ and prefer to manage Synapse through your Nix
profile:

```bash
nix profile install github:thinhngotony/synapse
synapse install   # open the TUI to install the harness packages
```

`synapse doctor` will tell you if anything is missing or misconfigured.

### Installing Nix

Neither method requires you to install Nix first — the curl bootstrap detects
its absence and gives instructions. If you want to install it yourself, the
Determinate Systems installer is the recommended path:

```bash
curl -fsSL https://install.determinate.systems/nix | sh -s -- install
```

On macOS this creates a dedicated APFS volume and modifies system files; run it
yourself rather than having Synapse do it on your behalf.

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
| `synapse stack capture` | Capture OMP profiles, plugins, MCP definitions, and Skillshare Git source |
| `synapse stack restore --trust` | Restore reviewed executable stack content; `--remote` bootstraps Skillshare |
| `synapse stack status` | Validate the capture and report missing environment variables |
| `synapse version` | Version, git commit, target triple |

Add `--dry-run` to `setup-shell` to see exactly what would change before anything is written.

## What Gets Installed

| Package | What it is | Source |
|---------|-----------|--------|
| herdr | Terminal multiplexer for coding agents | nixpkgs |
| omp | Oh My Pi coding agent CLI | npm, wrapped with Bun |
| skillshare | Sync AI CLI skills across tools | upstream release binaries |

Managed binaries are rooted in `~/.local/share/synapse/profile`; Synapse adds that profile's
`bin` directory to shell PATH. Install, update, rollback, and uninstall all mutate this dedicated
profile and verify the expected executable exists before changing `state.json`.

## Portable AI Stack

Skillshare remains the source of truth for skills, agents, rules, commands, and other file-based
resources. Synapse stores its portable stack at `.synapse/stack` inside Skillshare's configured
`git_root`; capture never commits or pushes.

On the existing machine:

```bash
synapse stack capture
git -C ~/.config/skillshare add .synapse
git -C ~/.config/skillshare commit -m "Update AI stack"
git -C ~/.config/skillshare push
```

Adjust the Git directory when Skillshare uses `git_root: skills`, `agents`, or `extras`.

On a new machine, configure Git/SSH authentication, install Synapse, and clone the stack source:

```bash
synapse stack restore \
  --remote git@github.com:you/ai-stack.git \
  --git-root root
```

That first command clones only. Review `stack.json` and every `local-plugins/` snapshot: plugin
packages and stdio MCP commands execute as your user. Then apply the reviewed content explicitly:

```bash
synapse stack status
synapse stack restore --trust
```

The restore flow:

- lets Skillshare clone and sync its own skills, agents, and extras;
- pins registry plugins to their installed version;
- preserves full-SHA Git origins, and snapshots dirty or unmanaged local plugins;
- restores every OMP profile's plugin enablement and selected features;
- excludes plugin settings because their arbitrary schema cannot be proven credential-free;
- converts MCP environment values and headers to environment references;
- preserves supported credential commands such as `gh auth token`, `security`, `pass`, and `op`;
- rewrites paths under the old home directory to `${HOME}`.

MCP credentials are externalized, and plugin settings are excluded from `stack.json`.
`synapse stack status` lists environment variables required on the new machine. Capture also rejects
likely secret files (`.env`, private keys, credential JSON, package-manager auth files) in local
plugin snapshots, but snapshots are source archives—not a general-purpose secret scanner. Review
them before committing. Unsupported MCP fields, unpinned registry state, escaping symlinks, and
unlocked local dependencies fail capture.

MCP capture currently covers OMP-native user/profile files (`agent/mcp.json`). Skillshare covers
cross-tool skills and agents; native MCP writers for other AI clients are not implemented.

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

Package operations use `github:thinhngotony/synapse` by default, independent of the current
directory. Developers may opt into a reviewed local checkout with `SYNAPSE_FLAKE_DIR`.
Packages are installed into the dedicated Nix profile rather than an unrooted `nix build` result.
The public repository makes the default flake anonymously resolvable.

## How State Works

Synapse tracks what it installed in `~/.config/synapse/state.json`:

- Every write is atomic (temp file + rename), so an interrupted update cannot corrupt it.
- A PID lock at `~/.config/synapse/.lock` prevents concurrent installs. Stale locks from a crashed
  process are detected and cleared automatically.
- The last **3** versions of each package are retained as rollback targets.

Rollback repoints the dedicated profile to the exact recorded Nix store path. Older profile
generations keep those paths rooted; if profile history was explicitly removed, rollback fails
instead of silently rebuilding a different version.

## Shell Integration

`synapse setup-shell` configures bash, zsh, and fish — whichever you actually use. Everything it
writes is wrapped in a marker block:

```bash
# >>> synapse >>>
...managed lines...
# <<< synapse <<<
```

Re-running replaces that block in place rather than appending a second copy, so your PATH never
accumulates duplicates. The block exposes `~/.local/share/synapse/profile/bin` plus the user's Nix
profile. Nothing outside the markers is modified, and `synapse uninstall` removes the block once
no managed packages remain.

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
