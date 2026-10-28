# Karakara CLI Reference

The unified **`karak`** binary is the only user-facing entrypoint for Karakara OS:

- **`karak serve`** — run the platform (HTTP A2A, repository, provenance, config)
- **SDK subcommands** — scaffold, install, chat, eval, registry, sandbox tooling

For HTTP API details (deploy, A2A, auth tiers), see [`agent-runner.md`](agent-runner.md).

## Installation

See [`INSTALL.md`](../../INSTALL.md). From a clone:

```bash
cargo build --release -p karakara-os --all-features
./target/release/karak --help
```

Local dev shortcuts: `just build`, `just runner` (build + start on `127.0.0.1:18080`).

## Migration from `cargo-agent-platform` / `baml-agent-runner`

| Before | After |
|--------|-------|
| Separate `baml-agent-runner` + `cargo-agent-platform` binaries | Single `karak` binary |
| `baml-agent-runner --serve-http …` | `karak serve --serve-http …` |
| `cargo-agent-platform push` | `karakara install agent` (publish + deploy) |
| `cargo-agent-platform build` / `regen` | **Removed** — publish triggers server-side build; monorepo fixture regen stays on `just regen-fixtures` |
| `cargo-agent-platform deploy --hash …` | `karakara deploy --hash … --url …` |

Legacy subcommands **`build`**, **`regen`**, and **`push`** are not registered on `karak`.

## Project config

`karakara init` creates `karakara.toml`:

```toml
[runner]
url = "http://127.0.0.1:18080"
token_env = "RUNNER_TOKEN"

[project]
default_agent = "my-agent"
agent_path = "./my-agent"
```

Resolution order: `--config` flag → `./karakara.toml` → `~/.config/karakara/config.toml`.

```bash
karakara config show
karakara config set runner.url http://127.0.0.1:18080
```

## Platform host

```bash
karak serve --serve-http 127.0.0.1:18080 \
  --repository-dir ./.repository \
  --state-dir ./.runner-state \
  --provenance-db ./provenance.db
```

All former `baml-agent-runner` flags are available on `serve` (flattened runner CLI). Set `RUNNER_TOKEN` (or pass via env) when operator routes should require auth.

## Primary developer loop

```bash
karakara init --with-agent --agent-name support-bot --tags support,demo
karakara skill install agent
# edit agent source only (manifest, baml_src/, src/)
karakara install agent --path ./support-bot
karakara sync-types
karakara chat --agent support-bot --instance default
karakara eval run --min-pass-rate 0.9
```

| Command | Purpose |
|---------|---------|
| `install agent` | `POST /repository/publish` (server build) + `POST /deploy` |
| `install tool` | Approve and register an external tool snapshot |
| `publish` | Publish source only (no deploy); prints `karakara deploy …` hint |
| `deploy --hash HASH --url URL` | Activate a published artifact |
| `sync-types` | Pull `_baml_runtime.baml` + `baml-runtime.d.ts` from `GET /repository/dev-artifacts` |

## Scaffolding

```bash
karakara new-agent my-agent --template simple --tags support,demo
karakara new-tool weather --bundle support --lang rust
karakara new-static-tool github   # platform-internal (monorepo contributors only)
```

Interactive mode: omit positional names (`karakara new-agent`, `karakara new-tool`, …).

## Eval harness

```bash
karakara eval init                    # writes eval/cases.toml from example
karakara eval run --min-pass-rate 0.9
karakara eval run --cases lifecycle-demo,smoke-one-shot
karakara eval report                # prints eval/out/last-run.json
```

Eval talks to a **running** runner over HTTP:

1. **`POST /eval/sessions`** (operator auth) — optional ephemeral model override scope; eval run sends `X-Karakara-Eval-Session` on A2A turns.
2. **`POST /agents/{agent}/a2a`** — chat turns via `message.sendStream` (JSON-RPC `id` must use `corr-<millis>-<counter>` format).
3. **`POST /events/publish`** — ingress turns (fixture JSON on disk).

Manifest syntax: `crates/karakara/eval/cases.toml.example`. Turn modes:

- **`chat`** (default) — A2A user message; assert on task states and streamed text.
- **`ingress`** — publish host event fixture; assert on `contains` / `not_contains` in the publish response (non-empty `failures` fails the turn).

Flags: `--url`, `--model`, `--deploy` (publish+deploy agent path before eval), `--path`, `--runner-token`.

## Registry, sandbox, and diagnostics

Unchanged in role from the former SDK:

- `external-tool enable|inspect|refresh`
- `mcp list|enable|server|versions|tool`
- `sandbox-bind-sync`, `sandbox-oci-prepare`
- `export-snapshot-cache`, `snapshot-report`, `doctor`, `list-*`
- `check-external-tool`

## Skills

```bash
karakara skill install agent   # Cursor skill: agent authoring
karakara skill install tool    # Cursor skill: external tool authoring
```

Bundled skills live under `crates/karakara/skills/`.

## Authentication

Operator routes accept `X-Runner-Token` or `--runner-token` / `RUNNER_TOKEN` env (see `karakara.toml` `runner.token_env`).

Public routes (no token): discovery, A2A, `/events/publish`, read-only `/repository/*` including **`GET /repository/dev-artifacts`**.

## Server-generated dev artifacts

On **`POST /repository/publish`**, the runner captures `_baml_runtime.baml` and `src/baml-runtime.d.ts` from the build workspace and stores them in the repository blob store (keyed by content hash).

```bash
# After publish
karakara sync-types --path ./my-agent
# or
curl 'http://127.0.0.1:18080/repository/dev-artifacts?agent=my-agent'
curl 'http://127.0.0.1:18080/repository/dev-artifacts?hash=<content-hash>'
```

Response `status`: `ok` | `not_found` | `not_implemented` (hash unknown or artifacts not yet published).
