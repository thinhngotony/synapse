# Synapse v1.0

**Single-command AI harness installer and auto-updater.**

Synapse installs herdr, omp, and skillshare across macOS, Linux, and WSL2, pins them with Nix, and
keeps them updated. Setting up a new machine goes from a couple of hours of manual work to one
command.

## Highlights

**One-command install.** `synapse install` opens a TUI, shows your detected platform and Nix
version, lets you pick packages, and reports live progress per package. Ctrl+C aborts cleanly at any
point without leaving a half-configured system.

**Reproducible by default.** All packages resolve through a committed `flake.lock`, so every machine
installing the same Synapse revision gets byte-identical packages. No floating `nixos-unstable`
resolution per install.

**Rollback that doesn't rebuild.** The last three versions of each package are retained. Because Nix
keeps old store paths until garbage collection, `synapse rollback` usually just re-points at the
previous closure instead of rebuilding it.

**Auto-update on native schedulers.** launchd on macOS, systemd user timers on Linux, cron on WSL2.
Synapse verifies the scheduler actually accepted the unit rather than assuming a written file took
effect.

**Shell integration that respects your dotfiles.** PATH and completions for bash, zsh, and fish, all
inside a marker block. Re-running replaces that block in place — your PATH never accumulates
duplicates, and nothing outside the markers is touched.

## Commands

| Command | Description |
|---------|-------------|
| `install` | Interactive TUI installer |
| `status` | Platform, Nix version, installed packages |
| `doctor` | Diagnose PATH, symlink, and lock problems |
| `list` | Installed packages, versions, install timestamps |
| `update [pkg]` / `update --all` | Update one or all packages |
| `rollback [pkg]` | Revert to the previous version |
| `uninstall --all` | Unmanage packages, clean up shell config |
| `log` | Install and update history |
| `setup-shell` | Configure PATH and completions (`--dry-run` supported) |
| `auto-update enable\|disable\|config\|now\|status` | Manage scheduled updates |
| `version` | Version, git commit, target triple |

## Packages Managed

| Package | Version | Source |
|---------|---------|--------|
| herdr | 0.7.5 | nixpkgs (`pkgs.herdr`) |
| omp | 17.2.2 | npm `@oh-my-pi/pi-coding-agent`, Bun-wrapped |
| skillshare | 0.20.23 | upstream release binaries, checksum-verified |

Synapse also pins Bun to 1.3.14, because omp version-checks its runtime at startup and rejects the
1.3.13 currently in nixpkgs.

## Platform Support

| Platform | Status |
|----------|--------|
| macOS aarch64 | Supported |
| macOS x86_64 | Supported |
| Linux x86_64 | Supported |
| Linux aarch64 | Supported |
| Windows WSL2 | Supported (cron fallback for scheduling) |
| Windows native | Not supported |

## Requirements

- **Nix 2.24+** with flakes enabled. Synapse does not install Nix for you — on macOS the installer
  creates a dedicated APFS volume and edits system files, which should be run deliberately rather
  than as a side effect. `synapse doctor` prints the exact command if Nix is missing.
- bash, zsh, or fish
- macOS 13+, or Linux with glibc 2.31+

## Safety Properties

These are enforced and covered by tests, not aspirations:

- **Atomic state writes.** `state.json` is written to a temp file and renamed, so an interrupted
  update cannot leave it truncated or half-parsed.
- **Concurrency locking.** A PID lock prevents two Synapse processes from installing at once. The
  lock uses `O_CREAT|O_EXCL`, so the race is decided by the kernel; the loser is told which PID holds
  the lock rather than being handed a raw filesystem error.
- **Stale lock recovery.** A lock left behind by a crashed process is detected via a liveness check
  and cleared automatically — no manual `rm` needed.
- **Scheduled runs take the same lock.** An auto-update firing at 02:00 cannot collide with an
  install you started by hand.
- **rc files are never clobbered.** Only lines inside Synapse's marker block are ever removed.
  Content between two managed blocks — which happens when dotfiles are merged across machines — is
  preserved.

## Not in v1.0

Deferred to keep the release scoped:

- **Cloud sync of config across machines** — planned for v1.1.
- **Automatic MCP server configuration.** `supermemory-mcp` is a hosted endpoint consumed as a config
  entry, not installable software, so it does not belong in a package installer. MCP auto-config is
  v1.1 scope.
- **Windows native support.** WSL2 is supported; native Win32 is not planned.

## Verification

- 92 unit tests, 3 integration tests
- `cargo clippy --all-targets -- -D warnings` clean
- Flake packages built and smoke-run on aarch64-darwin against Determinate Nix 3.21.9
- CI matrix covers macOS 13/14 and Ubuntu 22.04 x86_64/ARM: build, test, clippy, shell integration
  across all three shells, flake realisation per platform, and the full
  install → update → rollback → uninstall lifecycle
- Coverage gate at 80% lines

## Known Limitations

- **First install is not instant.** With a warm binary cache the harness installs in a couple of
  minutes; a cold cache that has to build omp's native dependencies takes longer. The <10 minute
  target holds with the Cachix substituter configured.
- **`omp` carries a large closure.** Its npm dependency tree includes optional native packages
  (onnxruntime, sharp, libvips) that Nix retains even when unused.
- **Bun is pinned by Synapse, not nixpkgs.** Once nixpkgs ships Bun ≥ 1.3.14 the override in
  `nix/bun.nix` should be deleted in favour of `pkgs.bun`.
