# Karakara OS Containers

First-party OCI container image definitions for running Karakara OS locally or standalone on Linux and macOS (Docker / Podman), plus microVM guest images for external tools.

> [!IMPORTANT]
> **Platform Validation Status:** Local builds, Web UI bundling, and `hack/containers/` lifecycle workflows are currently validated on **macOS (`aarch64` Apple Silicon with Podman)**. Native **Linux (`x86_64` / `aarch64`) validation** is queued next. See the full architecture diagram and runbook in [`docs/runbooks/local-containers.md`](../../docs/runbooks/local-containers.md).

- [`runner.Dockerfile`](runner.Dockerfile) — Multi-stage image bundling `karak serve`, Node.js 22 + TypeScript 6 (for `POST /repository/publish`), and the built Vue Web UI (`/app/web/dist`).
- [`tool-runtime/`](tool-runtime/README.md) — MicroVM guest images for the External Tools v2 sandbox tier.

---

## Local Container Quickstart (macOS & Linux)

### Resource Requirements (macOS Container VMs)
Compiling the release Rust workspace (`surrealdb`, `quickjs`, `baml`) inside a Linux VM requires sufficient memory:
- **Recommended VM memory:** `8–10 GiB` RAM (e.g. Podman machine, Docker Desktop, OrbStack, or Colima).
- On `aarch64` (Apple Silicon) with `6 CPUs` and `~9.3 GiB` RAM, a cold build takes ~22 minutes (`cargo build` ~16m with `lld`); subsequent incremental builds reuse BuildKit/Podman cache mounts.
- On memory-constrained VMs, cap Cargo parallelism via `--jobs 4` (or `--build-arg CARGO_BUILD_JOBS=4`).

### Option A — Automated Deploy & Teardown (`hack/containers/`)
Requires [`uv`](https://docs.astral.sh/uv/) and either `podman` or `docker`:

```bash
# Build image, start container on 127.0.0.1:18080, mount /data volume,
# and verify /healthz, /readyz, /agents, /openapi.json, and / (Web UI)
uv run hack/containers/deploy.py

# Start existing image without rebuilding (optional .env for LLM keys)
uv run hack/containers/deploy.py --skip-build --env-file .env

# Stop and remove container (add --purge-volume / --purge-image as needed)
uv run hack/containers/teardown.py --purge-volume
```

### Option B — Direct Podman / Docker Commands
```bash
# Build (replace podman with docker if using Docker)
podman build -f operations/containers/runner.Dockerfile -t karakara-os:local-test .

# Run with persistent /data state (.runner-state + .repository)
podman run -d --name karakara-runner \
  -p 127.0.0.1:18080:18080 \
  -v karakara-runner-data:/data \
  karakara-os:local-test
```

Open `http://127.0.0.1:18080` to view the Web UI.

---

## Deploying Agents & LLM Requirements

A fresh container starts with **0 deployed agents** (`existing_deployments=0`), so the Web UI loads with an empty agent list until you install an agent.

### 1. Zero-LLM Verification (No API Key or Host Build Required)
Pure QuickJS agents run without any LLM provider or API key. Deploy the deterministic [`stream-js-tool`](../../tests/fixtures/agents/stream-js-tool/src/index.ts) fixture agent using the `karak` binary already inside the container image (replace `podman` with `docker` if using Docker):

```bash
podman run --rm \
  --network container:karakara-runner \
  -v "$(pwd):/workspace:ro" \
  -w /workspace \
  --entrypoint /usr/local/bin/karak \
  karakara-os:local-test \
  install agent \
    --path /workspace/tests/fixtures/agents/stream-js-tool \
    --repository-url http://127.0.0.1:18080/repository \
    --url http://127.0.0.1:18080
```

Then open `http://127.0.0.1:18080`, select `stream-js-tool`, and send a message containing `stream-task`.

### 2. LLM-Backed Product Agents (`agents/*`)
Product agents under [`agents/`](../../agents/) (`coordinator-agent`, `notion-agent`, `slack-agent`, `clickup-agent`, `extrospection-agent`) can be deployed and selected in the Web UI without a key, but executing a chat turn invokes BAML LLM functions and requires provider credentials (e.g. `OPENROUTER_API_KEY`) passed at container startup via `--env-file .env`.
