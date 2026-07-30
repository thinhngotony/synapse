# Synapse

**Universal AI tool sync — one config, one command, zero drift.**

Synapse replaces [skillshare](https://github.com/runkids/skillshare) and extends it to sync **skills, agents, MCP configs, and plugins** across Oh My Pi, Claude Code, Copilot CLI, Cursor, OpenCode, and more.

```bash
synapse sync      # sync everything to all targets
synapse list      # inventory of skills, MCP servers, plugins
synapse validate  # pre-flight checks
synapse doctor    # diagnose broken state
```

## Why

AI coding tools use 4+ different config formats, directory structures, and plugin systems. Installing a plugin means copying it to each tool. Adding an MCP server means hand-editing different JSON schemas. Skillshare solved skills + agents — Synapse solves the rest.

## Status

**Design phase.** Spec and implementation plan complete. Not yet implemented.

- [Spec](docs/superpowers/specs/2026-07-29-synapse-design.md)
- [Implementation Plan](docs/superpowers/plans/2026-07-29-synapse.md)

## What It Syncs

| Resource | Source | Targets |
|----------|--------|---------|
| Skills | `~/.config/synapse/skills/` | omp, Copilot, Cursor, OpenCode |
| Agents | `~/.config/synapse/agents/` | omp, Copilot, Cursor, OpenCode |
| MCP configs | `~/.config/synapse/mcp/*.yaml` | omp, Copilot, Cursor, OpenCode |
| Plugins | `~/.config/synapse/plugins/` (git clones) | omp, Copilot, Cursor |

## Quick Start (planned)

```bash
# Install
go install github.com/thinhngotony/synapse@latest

# Migrate from skillshare
synapse init --from-skillshare

# Install plugins
synapse install https://github.com/obra/superpowers
synapse install https://github.com/Egonex-AI/Understand-Anything

# Sync everything
synapse sync
```

## Compared To

| Feature | skillshare | Ruler | Synapse |
|---------|-----------|-------|---------|
| Skills | Yes | Yes | Yes |
| Agents | Yes | Yes | Yes |
| MCP configs | No | Yes (30+ tools) | Yes (4 tools) |
| Plugins | No | No | **Yes** |
| Rules | No | Yes | Yes |
| Format | Go binary | npm package | Go binary |

## License

MIT
