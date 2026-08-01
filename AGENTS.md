# AGENTS.md

Orientation for an agent or engineer picking this repo up cold. Read this before
touching code. It records what is built, what is proven, what is deliberately
deferred, and the traps that already cost time.

Last updated: 2026-08-01, after the SYN-10 platform-assumption audit. Pushed at
`6e8b5a8` on branch `feat/v1-installer-platform`; the audit fixes below landed
after that commit and are not yet pushed.

---

## What Synapse is

One command installs and version-manages an AI coding harness, then keeps it
current. Nix does package resolution so every machine resolves identical
versions.

```
curl -sfS https://synapse.hyberorbit.com/install | sh   # not live yet, see Blockers
synapse install                                          # TUI package selector
synapse auto-update enable                               # daily scheduler
```

Managed packages, each acquired differently on purpose:

| Package | Version | How, and why that way |
|---|---|---|
| herdr | 0.7.5 | `pkgs.herdr` from nixpkgs. Upstream already packages it; forking it would mean maintaining a derivation for no gain. |
| omp | 17.2.2 | npm `@oh-my-pi/pi-coding-agent`, bun-wrapped. Published to npm only, `dist/cli.js` is a prebundled bun script. Per-platform natives resolve through `optionalDependencies`. |
| skillshare | 0.20.23 | Prebuilt GitHub release tarball, sha256 from upstream `checksums.txt`. Chosen over `buildGoModule` to avoid re-pinning `vendorHash` on every bump. |

`supermemory-mcp` is deliberately **not** a managed package. MCP v1 is deprecated
upstream; the current form is a hosted endpoint (`https://mcp.supermemory.ai/mcp`)
consumed as config plus an API key. There is nothing to build. It belongs to
v1.1 MCP auto-configuration. Do not re-add it to the package list.

---

## Status

v1.0 code is complete. 109 tests pass (106 unit + 3 integration), clippy
`-D warnings` clean, fmt clean.

**Verified by execution on aarch64-darwin:**

- Real TUI install of all 3 packages through Nix; `state.json` + `install.log` written; exit 0
- All 4 lifecycle stages under `env -i PATH=/usr/bin:/bin` (launchd-minimal environment)
- launchd `enable → now → disable`, checked against `launchctl list`; plist removed on disable
- Installer harness 26/26, including corrupted-checksum rejection, wget fallback, missing-sha256-tool abort
- Worker routing 7/7, including GitHub API failure fallback and sh-safe error bodies

**Not verified anywhere yet:** x86_64-darwin, x86_64-linux, aarch64-linux. No
Intel Mac and no Linux host was available. CI exists to produce that evidence and
had not completed at the time of writing. Do not claim platform coverage until
the `nix-flake` legs are green — those are the first real test of the hashes
transcribed from upstream checksums and of whether omp's per-platform natives
resolve off this one machine.

---

## Layout

```
src/
  main.rs          clap command tree, the only place subcommands are wired
  platform.rs      OS/arch detection, compile-time cfg!
  nix.rs           Nix discovery + version floor (2.24). resolve_bin() lives here
  state.rs         state.json read/write + PID lock. Atomic write-then-rename
  shell.rs         rc-file splicing with sentinel markers, bash/zsh/fish
  tui.rs           ratatui installer: selector, progress, per-package build
  test_utils.rs    XDG_ENV_LOCK — see Traps
  commands/
    status.rs list.rs log.rs doctor.rs version.rs
    update.rs rollback.rs uninstall.rs setup.rs auto_update.rs
nix/
  skillshare.nix   prebuilt tarball per system, hashes from upstream checksums.txt
  omp.nix          buildNpmPackage + bun wrapper, per-platform natives asserted
  bun.nix          pinned bun; omp rejects nixpkgs' current version at runtime
  omp/             package.json + lock so npm resolves the full runtime tree
flake.nix          packages.{herdr,skillshare,omp,synapse,harness}, devShell
install.sh         POSIX sh installer. Not bash. Verifies sha256 before install
worker.js          Cloudflare Worker serving the stable install URL
ci/
  lifecycle.sh         install/update/rollback/uninstall, asserts outcomes
  offline-commands.sh  read-only commands with no config and no network
  shell-check.sh       generated rc is valid, sets PATH, idempotent
  install-check.sh     installer against a localhost fake release
  worker-check.mjs     worker routing with stubbed fetch
  check-nix-version.sh runner Nix satisfies the same floor synapse enforces
tests/lock_contention.rs  cross-process lock behaviour
```

Roughly 5.1k lines of Rust across `src/`.

---

## Contracts

Break any of these and the pieces stop fitting together.

**Release assets.** `synapse-<version>-<target>.tar.gz`, bare crate version with
no leading `v` (tag is `v1.0.0`, asset is `synapse-1.0.0-...`). Exactly these 4
triples: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`. Each tarball holds one
executable named `synapse` at archive root, nothing else. `checksums.txt` sits
alongside in `sha256sum` format, bare filenames, no path prefixes.

**Install destination.** `$HOME/.local/bin/synapse`, override via
`SYNAPSE_INSTALL_DIR`.

**Installer env vars.** `SYNAPSE_VERSION` pins a tag. `SYNAPSE_BASE_URL` points
at an alternate asset host and is the seam `ci/install-check.sh` uses — https
anywhere, plain http only against 127.0.0.1/localhost, anything else refused.

**state.json.** Additive only. Unknown keys are ignored on read, so adding a
field is safe and removing one is not. Rollback history keeps 3 versions
(`HISTORY_LIMIT`). Always go through `set_package_with_path`; never insert a
`PackageRecord` literal, or history maintenance is silently skipped.

**Managed rc blocks.** Sentinel-delimited. Synapse never writes a user's main rc
outside its own markers. `install_uninstall_cycle_is_lossless` defends this.

---

## Traps

Each of these was a real bug, not a hypothetical.

**Never `Command::new("nix")`.** Use `nix::resolve_bin()`. launchd and cron run
with a minimal environment that excludes the Nix profile, so PATH-only lookup
fails with ENOENT under the scheduler while working perfectly in an interactive
shell — silently breaking the daily auto-update, which is a core v1 requirement.
Verify any change here with `env -i PATH=/usr/bin:/bin`, not just your shell.
The same suspicion applies to anything spawning a binary, reading an env var a
scheduler won't set, or assuming a TTY.

**Know what launchd actually provides.** Measured with a probe job, not assumed:
`HOME`, `USER`, `SHELL`, `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, and `PWD=/`. It
does **not** provide `XDG_CONFIG_HOME` or the Nix profile bin dir. Every
scheduler bug below follows from one of those absences.

**Never fabricate a path from a missing env var.** `unwrap_or_default()` on
`HOME` yields a *relative* path that resolves against the CWD — and launchd runs
with `PWD=/`. That wrote plist files launchd never reads. The systemd branch was
worse: it fell back to `/tmp`, writing unit files systemd never loads while
reporting success. `state::config_dir()` had the same shape, which would have put
installed-package state in a world-writable directory that vanishes on reboot.
All three now resolve HOME, then the passwd database via `getpwuid`
(`state::home_dir()`), and error explicitly rather than inventing a location.
A wrong path that reports success is worse than a clean failure.

**Never trust the CWD.** `locate_flake_dir()` once preferred it, so a scheduled
`update --all` from `PWD=/` reported "nix build exited 1" for every package. It
now checks `SYNAPSE_FLAKE_DIR`, walks up from the exe, and only accepts the CWD
if it actually contains a `flake.nix`.

**One env lock, process-wide.** `crate::test_utils::XDG_ENV_LOCK`. Env vars are
process-global; a per-module `static Mutex` only excludes that module, so two
test modules each holding their own lock still race on `XDG_CONFIG_HOME`. Any
test touching it must hold *this* lock.

**Unique temp dirs need a counter, not a clock.** `tmpdir()` once keyed on
`subsec_nanos()` and parallel tests collided on one path, stomping each other's
lock files. It uses an `AtomicU64` now. Run the suite 10x before believing a
flake is fixed; a single green run proves nothing.

**`nix build` writes `./result`.** Gitignored. Do not commit it.

**Read-only commands must not create a lock.** `ci/offline-commands.sh` asserts
this. `status`, `list`, `log`, `version`, `doctor` stay lock-free.

**The TUI's final screen waits for `q`.** It is not hung. Driving it in
automation means sending a real keypress on a PTY; piping keys into a
non-terminal gets you "Device not configured (os error 6)", which is the correct
error, not a bug.

---

## Verify before claiming done

Every command below was run and passed on aarch64-darwin. Reproduce them, do not
trust this file.

```sh
. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

cargo test                                    # 106 unit + 3 integration
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo build --release

# Nix packages actually build and the binaries run
nix build .#herdr .#skillshare .#omp --no-link --print-out-paths

# Read-only commands, empty config, no network
XDG_CONFIG_HOME=$(mktemp -d) bash ci/offline-commands.sh ./target/release/synapse

# Lifecycle under a scheduler-minimal environment — this is the one that matters
export XDG_CONFIG_HOME=$(mktemp -d)
for s in fresh-install update rollback uninstall; do
  env -i HOME="$HOME" PATH=/usr/bin:/bin XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
    SYNAPSE_BIN="$PWD/target/release/synapse" bash ci/lifecycle.sh "$s"
done

bash ci/install-check.sh                      # 26 assertions
node ci/worker-check.mjs                      # 7 cases
sh -n install.sh && shellcheck -s sh install.sh
```

Do not add a test that cannot fail. Both harnesses here were mutation-checked:
stubbing out the installer's checksum comparison breaks 5 assertions, hardcoding
the wrong target breaks 13. If a new test passes against deliberately broken
code, it is decoration — delete it.

---

## CI

`.github/workflows/ci.yml`, 8 jobs. `ci-passed` is the single required check.

| Job | Covers |
|---|---|
| lint | clippy `--all-targets -D warnings` |
| test | build + tests on 4 platforms, asserts arch in `version` output |
| shell-integration | bash/zsh/fish rc generation, macOS + Linux |
| nix-flake | flake builds per system, binaries run. **The platform proof.** |
| lifecycle | install/update/rollback/uninstall, needs `nix-flake` |
| coverage | 80% floor, excludes `tui.rs`/`main.rs`/`build.rs` (need a pty harness) |
| installer | `sh -n`, shellcheck, `ci/install-check.sh` |

`.github/workflows/release.yml` triggers on `v*` tags. `version` gate fails the
build if the tag disagrees with `Cargo.toml` — a mismatched tag would publish
assets the installer cannot resolve. Then a 4-target native build matrix (no
cross-compilation; each target has its own runner), then collect, checksum, and
publish. `contents: write` is scoped to the release job only.

---

## Blockers

**Worker not deployed.** Written and tested, never shipped. Needs two secrets
only the repo owner can supply: `CLOUDFLARE_API_TOKEN` (Edit Cloudflare Workers
template, scoped to the hyberorbit.com zone) and `CLOUDFLARE_ACCOUNT_ID`. Steps
in `docs/deploy-worker.md`. Until it is live,
`curl -sfS https://synapse.hyberorbit.com/install | sh` does not resolve.

**Platform coverage unobserved.** See Status. Only CI can close this.

---

## v1.1 and beyond

Ordered by dependency, not desirability. `docs/superpowers/specs/2026-08-01-synapse-v2-platform.md`
holds the fuller vision; treat its Rust-rewrite framing as historical — the
rewrite already happened, this repo *is* Rust.

1. **MCP auto-configuration.** The original reason this project exists: sync
   skills, agents, MCP configs, and plugins across omp/Claude/Copilot/Cursor/
   OpenCode. Full design already written in
   `docs/superpowers/specs/2026-07-29-synapse-design.md` — canonical YAML per
   server, per-tool JSON adapters, `_synapse_managed` ownership marker,
   `envFrom` for secrets so tokens never enter config files. supermemory lands
   here as its first real consumer.
2. **Native Windows.** v1 covers Windows through WSL2, which is Linux, so one
   POSIX installer serves all four platforms. Native support means
   `install.ps1`, a `*-pc-windows-msvc` target, and Task Scheduler in place of
   launchd/systemd. `auto_update.rs` already has the backend seam.
3. **Cloud sync / export-import.** Bundle state, config, skills, and plugins;
   restore on a new machine. Needs a conflict-resolution story before any code.
4. **Supermemory skill indexing.** Depends on (1).

Ignore any earlier plan that assigns v1 a Go implementation or a `synapse.io`
install URL. Both are stale: the language is Rust, the host is
`synapse.hyberorbit.com`.

---

## House rules

- Never add AI attribution to commits — no `Co-Authored-By`, no `Made-with`.
  Check `git log -1 --format=%B` before pushing.
- Never commit without the owner's explicit approval. Leave work as unstaged
  changes.
- Never push straight to `main`.
- Fix causes, not symptoms. Grep every caller before changing a shared function;
  one guard in the shared path beats a guard in each caller, and patching only
  the reported path leaves sibling callers broken.
- Reach for stdlib and prebuilt artifacts before new dependencies. The 8 current
  crates are all load-bearing.
- Mark deliberate shortcuts `// ponytail:` naming the ceiling and the upgrade
  path, so a later reader can tell intent from ignorance.
