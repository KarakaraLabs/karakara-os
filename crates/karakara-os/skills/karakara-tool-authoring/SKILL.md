---
name: karakara-tool-authoring
description: Author external Karakara tools — tool-manifest.json, JSON-RPC handlers, FSM transitions. Use when scaffolding host tools outside the monorepo.
---

# Karakara tool authoring

## When to apply

- Creating external tools (`tool-manifest.json`, handler source)
- Enabling tools on a running instance
- Referencing tools from agent manifests

## Workflow

1. `karakara new tool <name>`
2. `karakara skill install tool` (this skill)
3. Implement handlers; `karakara check tool <dir>`
4. `karakara install tool <dir>` — enables on runner registry
5. Add exact tool name to agent `manifest.json` → `karakara install agent`

## Hard rules

- `tool-manifest.json` name must match agent manifest allowlist exactly
- Session tools follow Open → Send → SearchRead → Finish/Abort FSM
- Return structured JSON at tool boundaries

## Commands

| Command | Purpose |
|---------|---------|
| `karakara install tool <dir>` | Enable external tool on runner |
| `karakara check tool <dir>` | Validate manifest locally |
