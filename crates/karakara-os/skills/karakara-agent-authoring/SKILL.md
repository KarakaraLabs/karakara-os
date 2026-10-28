---
name: karakara-agent-authoring
description: Author Karakara agents — manifest.json, BAML session plans, runGeneratedStepExecutor, citations. Use when scaffolding or editing agent packages outside the monorepo.
---

# Karakara agent authoring

## When to apply

- Creating or editing an agent package (`manifest.json`, `baml_src/`, `src/index.ts`)
- Wiring tools in manifest allowlist
- Implementing multi-turn flows with `awaitInput`

## Workflow

1. `karak init` then `karak new agent <name>` (or edit existing source)
2. `karak skill install agent` (this skill)
3. Edit **source only** — do not hand-edit `_baml_runtime.baml` or `baml-runtime.d.ts`
4. `karak install agent` — repository builds and deploys
5. `karakara sync-types` after install for editor types (when available)
6. `karak chat` / `karak eval run` to verify

## Hard rules

- Manifest `tools[]` must list exact qualified tool names
- User-facing reply uses `SessionResult.message` / `StructuredReply`, not step telemetry
- `NeedClarification` must call `awaitInput` — never fake-complete
- Citations: `#N` history, `@N` archives — never mix

## Commands

| Command | Purpose |
|---------|---------|
| `karak install agent` | Publish source + deploy (server builds) |
| `karak eval run` | Run eval/cases.toml against deployed agent |
| `karak chat` | Interactive smoke test |
