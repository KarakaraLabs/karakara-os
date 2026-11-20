# Getting Started

This guide covers building Karakara OS from source, running the local platform and operator UI, trying example agent flows, running tests, and enabling local observability.

> **Just want the binaries?** Grab prebuilt Linux release tarballs (or `cargo install`) — see [`INSTALL.md`](../../INSTALL.md).

---

## Prerequisites & First-Time Setup

Commands use [`just`](https://github.com/casey/just) (`just --list` for the full set). A `.env` or `fnox.toml` in the repository root supplies API keys for LLM tests and demos — see [`CONTRIBUTING.md`](../../CONTRIBUTING.md).

```bash
just check-host          # Linux deps + TypeScript 6 on PATH
just build-release       # karakara (platform host + SDK)
```

---

## Local Runner & Chat UI

```bash
just web-build           # optional: operator UI at /
just runner              # HTTP A2A on http://127.0.0.1:18080
```

Persist provenance to disk:

```bash
just runner-provenance
```

Publish and deploy agents with `baml-agent-builder` and the runner repository API — see [`docs/reference/agent-runner.md`](../reference/agent-runner.md). For authoring your own agent, start with [`docs/assertions/how-to-write-agents.md`](../assertions/how-to-write-agents.md).

---

## Example Flows (`just`)

| Command | What it does |
|---------|----------------|
| `just notion-demo` | HTTP demo: package + deploy + one Notion agent request (needs `NOTION_API_TOKEN`) |
| `just coordinator-demo` | Coordinator delegates to Notion over A2A |
| `just slack-demo` | Slack todo-extraction demo |
| `just clickup-agent` | Run the ClickUp agent (stdio + HTTP runner) |
| `just list-agents` | List agent packages known to the SDK |
| `just list-tools` | List registered host tools |

Stop background demos with `just notion-demo-stop`, `just coordinator-demo-stop`, etc.

MCP walkthrough (weather agent, no `just` wrapper): [`scripts/meteo_mcp.sh`](../../scripts/meteo_mcp.sh) — run `runner` in one terminal and `chat` in another.

---

## Deployment (Linux Binary & Container)

- **Prebuilt Linux release binaries (`karak`):** [`INSTALL.md`](../../INSTALL.md)
- **Container definitions:** [`operations/containers/`](../../operations/containers/)
- **Kubernetes Helm chart (dormant pilot):** [`operations/deployment/helm/karakara-os/`](../../operations/deployment/helm/karakara-os/) — see [`k8s-pilot-operator-guide.md`](k8s-pilot-operator-guide.md)

---

## Develop and Test

```bash
just fmt
just clippy
just doctor              # workspace integrity (karakara)
just test-unit           # no API keys
just test                # CI-parity nextest (needs OPENROUTER_API_KEY for LLM lane)
```

---

## Observability (Optional)

```bash
just otel-up             # local Prometheus / Tempo / Grafana stack
just otel-summary 15m    # latency snapshot from Prometheus
just otel-down
```

---

## Next Steps

- [Documentation Map](../README.md)
- [How to Write Agents](../assertions/how-to-write-agents.md)
- [Runner Reference](../reference/agent-runner.md)
