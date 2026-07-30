# Synapse — Universal AI Tool Sync

**Status**: Approved (pair-reviewed 2026-07-29, 20 findings resolved)
**Replaces**: skillshare (`~/.config/skillshare/`)

---

## Problem

Managing AI coding tools across multiple CLIs requires duplicating skills, agents, MCP server
configurations, and plugins in 4+ different formats and locations. skillshare solved skills +
agents but left MCP configs and plugin directories unmanaged.

### Today's fragmentation (real state from machine audit)

| What | omp | Copilot | Cursor | OpenCode |
|------|-----|---------|--------|----------|
| Skills | `~/.claude/skills/` | `~/.copilot/skills/` | `~/.cursor/skills/` | `~/.config/opencode/skills/` |
| Agents | `~/.omp/agent/agents/` | `~/.copilot/agents/` | `~/.cursor/agents/` | `~/.config/opencode/agents/` |
| MCP | — | `~/.copilot/mcp.json` | `~/.cursor/mcp.json` | `opencode.json` |
| Plugins | `~/.claude/plugins/cache/` | `~/.copilot/installed-plugins/` | `~/.cursor/hooks/` | — |
| Settings | `~/.claude/settings.json` | `~/.copilot/settings.json` | — | — |

3 plugins, 2 MCP servers, 12 shared skills, 5 agents — all manually kept in sync across 4 tools.

---

## Solution

**Synapse** — a single Go binary. One YAML config. One command.

All AI tool configuration lives in `~/.config/synapse/` as the single source of truth.
`synapse sync` writes to each tool's native paths. No duplication, no drift.

### Directory Structure

```
~/.config/synapse/
├── synapse.yaml                  # master config: targets, MCP servers, options
├── skills/                       # SKILL.md files (one dir per skill)
│   ├── build/SKILL.md
│   ├── debug/SKILL.md
│   └── ...
├── agents/                       # agent definitions (*.agent.md)
│   ├── dev.agent.md
│   └── ...
├── mcp/                          # canonical MCP server configs (YAML)
│   ├── atlassian.yaml
│   └── supermemory.yaml
├── plugins/                      # plugin source dirs (git clones)
│   ├── superpowers/              # git clone of obra/superpowers
│   ├── understand-anything/      # git clone of Egonex-AI/Understand-Anything
│   └── warp/                     # git clone of warpdotdev/claude-code-warp
└── rules/                        # optional: shared instruction files
    ├── AGENTS.md
    └── CLAUDE.md
```

### CLI Commands

```
synapse sync [--target <name>] [--dry-run] [--force]  # sync all or one target
synapse install <repo-url> [--tag <version>]            # git clone a plugin, optionally pin to tag
synapse update [<plugin>]                                # git pull plugin(s), then resync
synapse remove <plugin>                                  # delete source clone + all target symlinks
synapse list [--skills|--plugins|--mcp|--targets]       # inventory
synapse validate                                          # pre-flight checks (envFrom vars, paths, etc.)
synapse init [--from-skillshare]                         # bootstrap ~/.config/synapse/
synapse doctor                                           # diagnose broken symlinks, dirty repos
```

`--dry-run`: show what would change without applying. `--force`: skip CRITICAL validation
and sync anyway (use with caution).

`synapse remove <plugin>` deletes the git clone from `~/.config/synapse/plugins/<name>`
AND cleans all target symlinks that resolve to it. Warns if source has uncommitted changes.
### Config Format
version: 1   # rejected if version changes — upgrade synapse first
# ~/.config/synapse/synapse.yaml
version: 1
source: ~/.config/synapse
targets:
  omp:
    skills: ~/.claude/skills
    agents: ~/.omp/agent/agents
    mcp: ~/.claude/.mcp.json
    plugins: ~/.claude/plugins/cache
    settings: ~/.claude/settings.json
    rules_managed: ~/.claude/synapse.md
  copilot:
    skills: ~/.copilot/skills
    agents: ~/.copilot/agents
    mcp: ~/.copilot/mcp.json
    plugins: ~/.copilot/installed-plugins
    settings: ~/.copilot/settings.json
    rules_managed: ~/.copilot/synapse.md
  cursor:
    skills: ~/.cursor/skills
    agents: ~/.cursor/agents
    mcp: ~/.cursor/mcp.json
    plugins: ~/.cursor/hooks
  opencode:
    skills: ~/.config/opencode/skills
    agents: ~/.config/opencode/agents
    mcp: ~/.config/opencode/opencode.json

mcp:
  atlassian:
    command: uvx
    args: [mcp-atlassian, --read-only]
    env:
      JIRA_URL: https://sedona.atlassian.net
      JIRA_USERNAME: phucngo@cisco.com
    envFrom: [JIRA_API_TOKEN]
  supermemory:
    command: node
    args: [/Users/tony/.config/supermemory/mcp-proxy.mjs]
    envFrom: [SUPERMEMORY_API_KEY]

plugins:
  superpowers:
    repo: https://github.com/obra/superpowers
    targets: [omp, copilot, cursor]
  understand-anything:
    repo: https://github.com/Egonex-AI/Understand-Anything
    targets: [omp]
  warp:
    repo: https://github.com/warpdotdev/claude-code-warp
    targets: [omp, copilot]

ignore:
  - "**/.DS_Store"
  - "**/.git/**"
  - "**/node_modules/**"

audit:
  block_threshold: CRITICAL
```

### Design Decisions

| Decision | Choice | Why |
|----------|--------|-----|
| **Sync method** | Dir-level symlinks (like skillshare) | Zero-copy, instant, no per-file tracking, C1/C2/C3 resolved |
| **MCP format** | YAML canonical → generated JSON per target | Each tool has different MCP JSON schema. Adapter per tool |
| **MCP ownership** | `_synapse_managed` marker in JSON | Distinguishes managed entries from user-manual. Enables cleanup |
| **Plugin sync** | Git clones in source, symlinks to targets | Git pull for updates, targets just link. Per-target VersionTarget() path |
| **Config format** | Single YAML file | User already knows YAML, skillshare uses it |
| **Language** | Go (single binary) | No runtime dependency, fast, skillshare precedent |
| **Settings entries** | Auto-merge `enabledPlugins` | Adding a plugin = symlink + one JSON key. Removing = delete symlink + key |
| **Secrets** | `envFrom` reads from shell env | Never stored in config files. Tokens stay in `.zshrc`/keychain |
| **Secrets lint** | Validate warns on secret-like keys in `env:` | Keys matching `*TOKEN*`, `*SECRET*`, `*KEY*`, `*PASSWORD*` trigger WARNING |
| **Version gate** | Reject unknown config versions | `version: 1` only; future versions get clear upgrade message |
| **Migration** | `synapse init --from-skillshare` | Reads skillshare config, writes synapse.yaml, copies skills/agents |
| **Locking** | Lockfile with stale-PID detection | Prevents concurrent sync; auto-clears dead PIDs |
| **Rules** | `rules_managed` separate file with sentinel markers | Never overwrites user's main rules file |
| **Atomicity** | Write-then-rename for individual ops | Filesystem-level atomic; crash leaves at most one broken symlink |

### MCP Adapter Differences

| Feature | Copilot (`mcp.json`) | Cursor (`mcp.json`) | OpenCode (`opencode.json`) | Claude (`.mcp.json`) |
|---------|---------------------|---------------------|---------------------------|----------------------|
| Wrapper key | `mcpServers` | `mcpServers` | `mcp` | `mcpServers` |
| Type field | `"type": "stdio"` | — | — | `"type": "stdio"` |

### Plugin Version Resolution

Plugin source dirs contain `.claude-plugin/plugin.json` (or similar). Synapse reads
`name` and `version` to construct the correct versioned target path:

```
source: ~/.config/synapse/plugins/superpowers/
        plugin.json → { name: "superpowers", version: "5.0.7" }

target: ~/.claude/plugins/cache/superpowers-marketplace/superpowers/5.0.7/
```

### Sync Engine

```
synapse sync
├─ Skills processor:   symlink source → targets, clean orphans
├─ Agents processor:   symlink + per-target transforms
├─ MCP processor:      canonical YAML → target-specific JSON via adapters
├─ Plugin processor:   symlink dirs + settings merge
└─ Rules processor:    concat and write to target
```


### MCP Ownership: `_synapse_managed` Marker

MCP config adapters write an ownership marker alongside tool-specific JSON:

```json
{
  "mcpServers": {
    "atlassian": { "command": "uvx", "args": ["mcp-atlassian", "--read-only"] },
    "supermemory": { "command": "node", "args": ["..."] }
  },
  "_synapse_managed": ["atlassian", "supermemory"]
}
```

On subsequent syncs, the adapter:
- Removes entries listed in `_synapse_managed` that are no longer in the canonical YAML
- Adds/updates entries that ARE in the canonical YAML
- Preserves entries NOT in `_synapse_managed` (user-manual additions)
- Updates `_synapse_managed` to match the current canonical set

The `_synapse` namespace prefix avoids collision with tool-specific keys. The marker
is invisible to tools (they only parse `mcpServers`). User can manually edit
`_synapse_managed` to transfer ownership of an entry.


### Validation Severity Taxonomy

| Severity | Meaning | Behavior |
|----------|---------|----------|
| CRITICAL | Blocks sync — cannot proceed safely | `synapse validate` exits 1; `synapse sync` refuses to run (use `--force` to override) |
| WARNING | Advisory issue — sync proceeds, user should review | `synapse validate` exits 0 with warnings printed; `synapse sync` proceeds |

### Doctor Command

`synapse doctor` diagnoses common broken states:

- Broken symlinks in target dirs
- Plugin git repos with uncommitted changes (dirty)
- MCP commands not resolvable in PATH
- Missing target directories
- Stale lockfiles from crashed sync processes

Doctor reports issues but does not fix them. Re-running `synapse sync` repairs most
issues. Plugin dirty state requires manual `git` resolution.

### Sync Idempotency

Running `synapse sync` twice must produce identical state. The second run reports
0 created and 0 removed for skills and agents (dir-level symlinks already point
correctly). MCP configs are stable (managed entries match canonical set). Plugins
are stable (symlinks and settings entries already exist). This property is defended
by a test: run sync, assert changes > 0; run sync again, assert changes == 0.
### Atomicity & Locking

Synapse uses a lockfile at `~/.config/synapse/synapse.lock` to prevent concurrent sync.
File operations use write-then-rename (symlink to temp path, then `os.Rename`) for
atomicity at the filesystem level. A crashed sync leaves at most one broken symlink;
re-running the sync repairs it. Stale lockfiles (PID no longer running) are detected
and cleared automatically.

### Rules: Managed Block Separation

Synapse never overwrites a tool's main rules file (e.g. `~/.claude/CLAUDE.md`).
Instead, each target has a `rules_managed` key — a separate file path that synapse
writes exclusively. Example:

```yaml
targets:
  omp:
    rules_managed: ~/.claude/synapse.md   # synapse writes here only
```

Synapse writes rules into `rules_managed` using sentinel markers:

```
<!-- SYNAPSE_MANAGED_START -->
... concatenated rules from source ...
<!-- SYNAPSE_MANAGED_END -->
```

Content outside the markers is never touched. The user optionally references
`rules_managed` from their main rules file via tool-specific import mechanisms
(e.g. `@.claude/synapse.md` in newer Claude Code versions).

### Plugin Version Target Adapters

Each target adapter implements a `VersionTarget() string` method that constructs
the correct versioned path for plugin symlinks:

| Target | Version Path Pattern |
|--------|---------------------|
| omp | `<pluginDir>/<marketplace>/<name>/<version>/` |
| Copilot | `<pluginDir>/<marketplace>/<name>/` |
| Cursor | `<pluginDir>/<name>/` (flat hooks dir) |

The plugin resolver reads `plugin.json` from the source. If `plugin.json` is absent:
hard fail with a clear error. If the target in `pluginCfg.targets` has no adapter
for the plugin type, skip with a warning.

### Trust Model

Synapse does not verify plugin contents. `synapse install` clones from the provided
Git URL. Git's SHA integrity provides transport protection against tampering, but not
against repository compromise. Users MUST install plugins only from trusted sources.
Pin to a specific tag with `synapse install <url> --tag <version>` for supply-chain
conscious workflows.

### Secrets: `env:` vs `envFrom:` Validation

- `env:` is for non-sensitive values only (URLs, usernames). Synapse validate warns
  if it detects keys matching `*TOKEN*`, `*SECRET*`, `*KEY*`, `*PASSWORD*` in `env:`.
- `envFrom:` reads from shell environment variables at sync time. Synapse validate
  checks every `envFrom` var exists in the process environment. Missing vars produce a
  CRITICAL error. Synapse sync warns on missing envFrom vars but continues (writes
  config without the missing env entry). The user runs `synapse validate` as a pre-flight
  gate before sync.
### Security: Never Store Secrets

MCP servers often need API tokens. Synapse never writes tokens to config files.
The `envFrom` key reads from shell environment variables — tokens stay in `.zshrc`/`.env`.

### Conflict Resolution

- Synapse-managed entries always win (they're the source of truth)
- Non-synapse entries in settings/MCP configs are preserved (merge, don't clobber)
- `synapse sync --dry-run` shows what would change
- Conflicts are reported, not silently resolved

### Validation Pipeline

```
synapse validate
├─ Skills: SKILL.md has required frontmatter, name matches dir
├─ Agents: permission grants are valid, tool references exist
├─ MCP: envFrom vars exist in environment, commands are resolvable
├─ Plugins: plugin.json present and valid, git remote reachable
└─ Targets: paths exist, permissions are writable
```

### What We Don't Build (YAGNI)

| Feature | Why not | When to add |
|---------|---------|-------------|
| Desktop GUI | CLI covers 100% of use cases, SkillDock exists | If non-technical team members need it |
| Marketplace discovery | Git is the canonical source, `synapse install <url>` suffices | If plugin count exceeds 10 |
| Nested rule loading | Project-level rules out of scope (user-global sync) | If monorepo teams adopt it |
| Remote MCP servers (SSE/HTTP) | Current MCP servers are all stdio | When remote servers are used |
| Subagent sync | Ruler handles this, not needed yet | If used across tools |
