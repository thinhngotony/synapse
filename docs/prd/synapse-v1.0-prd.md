# Synapse v1.0 — Product Requirements Document

**Status**: Approved for Development  
**Target Release**: 2026-09-15 (6 weeks)  
**Owner**: Tony (Product)  
**Team**: Manager, Dev, QA  

---

## Executive Summary

Synapse v1.0 is a **single-command AI harness installer and manager** that installs, configures, and auto-updates the entire AI coding ecosystem (herdr, omp, skillshare) across macOS, Linux, and Windows.

**One command. Zero config. Daily updates.**

---

## Problem Statement

Today, setting up an AI coding environment requires:
1. Installing Nix manually
2. Finding/building packages for herdr, omp, skillshare
3. Configuring MCP servers (5+ JSON files)
4. Setting up authentication (GitHub PAT, Jira tokens, etc)
5. Manually checking for updates weekly
6. Repeating on every new machine

**Pain points**:
- Takes 2-4 hours per machine
- Easy to miss steps or use wrong versions
- No automated updates
- No migration/backup story

---

## Success Metrics

| Metric | Target |
|--------|--------|
| **Install time** | < 10 minutes (fresh machine) |
| **Setup steps** | 1 command only |
| **Platform coverage** | macOS (Intel/ARM), Linux (Ubuntu/NixOS), Windows (WSL2) |
| **Update frequency** | Daily auto-check, weekly auto-install |
| **User errors** | < 5% failed installs (CI-tested) |
| **Adoption** | 50+ installs first month |

---

## Target Users

### Primary: AI coding tool users
- Use herdr/omp for AI agent orchestration
- Need reproducible environments
- Work across multiple machines (laptop, desktop, cloud)

### Secondary: Platform engineers
- Set up dev environments for teams
- Need automated provisioning
- Want centralized version control

---

## User Stories

### Epic 1: Installation
**As a developer**, I want to install the entire AI harness with one command, so I can start coding in minutes instead of hours.

**Acceptance criteria**:
- [ ] Run `curl -fsSL https://synapse.hyberorbit.com/install | sh` on fresh machine
- [ ] Installer detects OS and architecture
- [ ] Installer prompts for package selection (herdr, omp, skillshare)
- [ ] Installer shows progress with ETAs
- [ ] Installer completes in < 10 minutes
- [ ] Installer verifies all packages work (`synapse doctor`)
- [ ] Installer sets up shell integration (PATH, completions)

---

### Epic 2: Update Management
**As a developer**, I want my AI tools to stay up-to-date automatically, so I always have the latest features and fixes.

**Acceptance criteria**:
- [ ] Daily background check for updates (non-blocking)
- [ ] Weekly auto-install (user-configurable)
- [ ] Update notification in terminal
- [ ] Manual update: `synapse update --all`
- [ ] Rollback support: `synapse rollback`
- [ ] Update log: `synapse log` shows update history

---

### Epic 3: Platform Support
**As a platform engineer**, I want Synapse to work on all major platforms, so I can standardize our team setup.

**Acceptance criteria**:
- [ ] macOS Intel (x86_64) tested
- [ ] macOS Apple Silicon (aarch64) tested
- [ ] Linux x86_64 (Ubuntu 22.04+) tested
- [ ] Linux aarch64 (Raspberry Pi, AWS Graviton) tested
- [ ] Windows WSL2 (Ubuntu) tested
- [ ] NixOS native support
- [ ] CI tests all platforms on every commit

---

### Epic 4: State Management
**As a developer**, I want to see what's installed and check for issues, so I can debug problems myself.

**Acceptance criteria**:
- [ ] `synapse status` shows installed packages, versions, update availability
- [ ] `synapse doctor` diagnoses common issues (missing paths, broken symlinks)
- [ ] `synapse list` shows all managed packages
- [ ] `synapse version` shows Synapse version + build info

---

## Features

### P0 (Must-have for v1.0)

#### 1. One-command Installation
```bash
curl -fsSL https://synapse.hyberorbit.com/install | sh
```
- Detects OS/arch automatically
- Installs Nix if missing
- Interactive TUI for package selection
- Parallel package builds (Nix)
- Progress indicators with ETAs
- Post-install verification

#### 2. Package Management
Supported packages:
- **herdr** (v0.5.2+): AI agent orchestrator
- **omp** (v1.2.0+): Oh My Pi CLI
- **skillshare** (v0.20.23+): Skill/agent sync tool

Commands:
```bash
synapse install herdr              # Install one package
synapse update herdr               # Update one package
synapse update --all               # Update everything
synapse remove herdr               # Uninstall package
```

#### 3. Auto-update
```bash
synapse auto-update enable         # Enable daily checks
synapse auto-update disable        # Disable
synapse auto-update config         # Configure schedule
```

Configuration:
```yaml
# ~/.config/synapse/auto-update.yaml
enabled: true
check_interval: 1d                 # Check daily
install_interval: 7d               # Auto-install weekly
notify: true                       # Desktop notifications
preferred_time: "02:00"            # 2 AM local time
max_duration: 30m                  # Timeout
```

Uses native schedulers:
- macOS: launchd
- Linux: systemd timers
- Windows: Task Scheduler

#### 4. Status & Diagnostics
```bash
synapse status                     # Show system status
synapse doctor                     # Diagnose issues
synapse list                       # List packages
synapse log                        # Update history
synapse version                    # Synapse version
```

#### 5. Platform Support
- macOS 13+ (Intel + Apple Silicon)
- Ubuntu 22.04+ (x86_64 + aarch64)
- Debian 12+
- NixOS 23.11+
- Windows 11 WSL2 (Ubuntu)

#### 6. CI/CD Testing
- GitHub Actions matrix:
  - `macos-13` (Intel)
  - `macos-14` (ARM)
  - `ubuntu-22.04` (x86_64)
  - `ubuntu-22.04-arm` (aarch64)
- Tests on every PR:
  - Fresh install
  - Update existing install
  - Rollback
  - Uninstall

---

### P1 (Nice-to-have, defer to v1.1)
- Cloud sync (backup/restore state)
- Export/import bundles
- Supermemory skill indexing
- MCP server auto-configuration — includes **supermemory** (hosted endpoint `https://mcp.supermemory.ai/mcp` + `SUPERMEMORY_API_KEY`, config entry only, nothing to build). MCP v1 package repo is deprecated; it is not installable software, so it is out of v1.0 package scope.
- Team profiles (pre-configured package sets)

---

## Technical Architecture

### Stack
- **Language**: Rust 1.78+
- **CLI**: clap v4
- **TUI**: ratatui
- **Package backend**: Nix 2.24+
- **HTTP**: reqwest
- **Async**: tokio

### Nix Strategy
```nix
# flake.nix
{
  outputs = { nixpkgs, ... }: {
    packages = {
      herdr = pkgs.herdr;                  # already in nixpkgs
      omp = /* buildNpmPackage + bun wrapper */;
      skillshare = /* fetchurl release tarball */;
    };
  };
}
```

**Build optimization**:
- Use `nix-fast-build` for parallel builds
- Pre-built binary cache (Cachix)
- Incremental builds (reuse common dependencies)

### State Management
```
~/.config/synapse/
├── state.json              # Installed packages, versions
├── auto-update.yaml        # Auto-update config
├── install.log             # Installation log
└── .lock                   # Concurrency lock
```

### Install Flow
```
1. Detect OS/arch
2. Check Nix installed → install if missing
3. Show TUI package selector
4. Build packages in parallel (Nix)
5. Verify installations
6. Set up shell integration
7. Enable auto-update (optional)
8. Show success message + next steps
```

---

## User Experience

### Install Flow (TUI)
```
╭─ 🧠 Synapse Installer ─────────────────────────────────────╮
│                                                             │
│  Detected: macOS 14.5 (arm64)                              │
│  Nix: Not installed (will install automatically)           │
│                                                             │
│  Select packages to install:                               │
│                                                             │
│  ☑ herdr              AI agent orchestrator                │
│  ☑ omp                Oh My Pi CLI                         │
│  ☐ skillshare         Skill/agent sync tool                │
│                                                             │
│                                                             │
│  Download size: ~150 MB                                    │
│  Install time: ~5 minutes                                  │
│                                                             │
│  [Space] Toggle  [Enter] Install  [Esc] Cancel             │
╰─────────────────────────────────────────────────────────────╯
```

### Progress
```
╭─ 📦 Installing packages ───────────────────────────────────╮
│                                                             │
│  ✓ Nix 2.24.0 installed                                    │
│  ✓ herdr 0.5.2 installed                                   │
│  ⠹ Building omp 1.2.0...                     [████░░] 75%  │
│    Est. 1m 30s remaining                                   │
│  ✓ skillshare 0.20.23 installed                            │
│                                                             │
│  [Ctrl+C] Cancel                                           │
╰─────────────────────────────────────────────────────────────╯
```

### Status
```bash
$ synapse status

🧠 Synapse v1.0.0

Platform:
  ✓ macOS 14.5 (arm64)
  ✓ Nix 2.24.0

Packages:
  ✓ herdr 0.5.2 (up to date)
  ✓ omp 1.2.0 (update available: 1.2.1)
  ✓ skillshare 0.20.23 (up to date)

Auto-update:
  ✓ Enabled (daily check, weekly install)
  ✓ Last check: 2 hours ago
  ✓ Next check: in 22 hours

Run 'synapse update --all' to update omp
```

---

## Testing Requirements

### Unit Tests
- [ ] CLI argument parsing
- [ ] OS/arch detection
- [ ] State file read/write
- [ ] Lock file handling
- [ ] Version comparison

### Integration Tests
- [ ] Fresh install on each platform
- [ ] Update existing install
- [ ] Rollback to previous version
- [ ] Uninstall
- [ ] Auto-update scheduler setup
- [ ] Concurrent install detection (lock file)

### E2E Tests (CI)
```yaml
# .github/workflows/ci.yml
matrix:
  os:
    - macos-13           # Intel
    - macos-14           # ARM
    - ubuntu-22.04       # x86_64
    - ubuntu-22.04-arm   # ARM64
  test:
    - fresh-install
    - update
    - rollback
    - doctor
```

### Manual QA Checklist
- [ ] Install on fresh macOS (Intel)
- [ ] Install on fresh macOS (ARM)
- [ ] Install on fresh Ubuntu 22.04
- [ ] Install on Ubuntu with existing Nix
- [ ] Update with `synapse update --all`
- [ ] Rollback with `synapse rollback`
- [ ] Verify auto-update cron/launchd job
- [ ] Verify `synapse doctor` catches issues
- [ ] Verify uninstall removes all files

---

## Release Criteria

### Must pass before v1.0 release:
- [ ] All P0 features implemented
- [ ] All unit tests passing
- [ ] All integration tests passing
- [ ] All E2E tests passing on all platforms
- [ ] Manual QA checklist 100% complete
- [ ] Documentation complete (README, install guide)
- [ ] Install script tested on 3+ fresh machines
- [ ] Performance: install completes in < 10 min
- [ ] Zero critical bugs
- [ ] Security: no hardcoded secrets, TLS for downloads

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Nix install fails on some platforms | High | Detect and show clear error + manual steps |
| Packages fail to build | High | Pre-built binary cache (Cachix) |
| Network failures during install | Medium | Retry with exponential backoff |
| Stale lockfile from crashed install | Medium | Auto-detect stale PID and clear |
| Auto-update breaks system | High | Rollback support, conservative defaults (weekly) |
| Platform-specific bugs | Medium | CI matrix tests all platforms |

---

## Timeline

### Week 1: Foundation
- Rust project scaffold
- CLI structure (clap)
- OS/arch detection
- Nix flake.nix

### Week 2: Installer
- TUI installer (ratatui)
- Nix package builds (herdr, omp, skillshare)
- Progress tracking
- State management

### Week 3: Update Management
- Update commands
- Auto-update scheduler (launchd/systemd)
- Rollback support
- Version tracking

### Week 4: Platform Support + CI
- Test on all platforms
- Fix platform-specific bugs
- CI pipeline (GitHub Actions)
- E2E tests

### Week 5: Polish + QA
- `synapse doctor` diagnostics
- Error messages
- Documentation
- Manual QA on all platforms

### Week 6: Release
- Final testing
- Release notes
- Publish install script
- Announce v1.0

---

## Success Criteria

### Launch (Week 6):
- [ ] v1.0 released
- [ ] Install script live at https://synapse.hyberorbit.com/install (Cloudflare Worker: worker.js + wrangler.toml; deploy pending two Cloudflare secrets from user)
- [ ] Documentation published
- [ ] CI green on all platforms

### Post-launch (Month 1):
- [ ] 50+ installs
- [ ] < 5% failed installs
- [ ] < 3 critical bugs reported
- [ ] 90%+ uptime for install script

---

## Out of Scope (v1.0)

Deferred to v1.1+:
- Cloud sync/backup
- Export/import bundles
- Supermemory integration (beyond basic install)
- MCP server auto-config
- Team profiles
- Web dashboard
- Plugin management (beyond core packages)

---

## Appendix: Command Reference

```bash
# Install
curl -fsSL https://synapse.hyberorbit.com/install | sh

# Package management
synapse install <package>          # Install one package
synapse update <package>           # Update one package
synapse update --all               # Update all packages
synapse remove <package>           # Remove package

# Status
synapse status                     # Show system status
synapse list                       # List installed packages
synapse version                    # Show Synapse version

# Diagnostics
synapse doctor                     # Diagnose issues
synapse log                        # Show update history

# Auto-update
synapse auto-update enable         # Enable auto-update
synapse auto-update disable        # Disable auto-update
synapse auto-update config         # Configure schedule
synapse auto-update now            # Run update now

# Advanced
synapse rollback                   # Rollback to previous version
synapse uninstall                  # Remove Synapse completely
```

---

**Approved by**: Tony  
**Date**: 2026-08-01  
**Next Review**: 2026-08-15 (Sprint checkpoint)
