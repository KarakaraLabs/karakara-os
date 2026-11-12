# Kubernetes Pilot Operator Guide

> [!WARNING]
> **Status: DORMANT / PARKED PILOT** (see Issue `#44`).
> This guide documents the parked Helm chart at [`operations/deployment/helm/karakara-os/`](../../operations/deployment/helm/karakara-os/). It is **not** an active deployment target today:
> - Primary deployment paths are the Linux `karak` binary ([`INSTALL.md`](../../INSTALL.md)) and standalone container ([`operations/containers/runner.Dockerfile`](../../operations/containers/runner.Dockerfile), tracked in `#29`).
> - The Helm chart does **not** yet support the External Tools v2 Sandbox tier (`/dev/kvm`, tracked in `#33`).
> - The former k3d / Argo CD / load-test script harness (`just up`, `just verify-k8s-pilot-package`, `scripts/e2e-k8s/`, `scripts/k8s-pilot-*`) was pruned under `#44` (preserved in git history). Cluster-free probe validation remains available via `./scripts/test-helm-runner-probes.sh` (`just helm-probe-check`).

Exploratory first-run path for deploying the parked Karakara OS Helm chart to a Kubernetes cluster.

For the HTTP API and CLI reference, see [`docs/reference/agent-runner.md`](../reference/agent-runner.md) and [`docs/reference/karakara-cli.md`](../reference/karakara-cli.md).

## Supported topology

The pilot packages one topology:

- Two-replica runner `StatefulSet` sharing a single cluster-scoped SurrealDB.
- A headless `Service` for pod-to-pod A2A forwarding and a `ClusterIP` API `Service` for operator access.
- `NetworkPolicy` objects: SurrealDB ingress is restricted to runner pods; runner HTTP port accepts cluster-network ingress (not namespace-scoped on the runner Service).
- Operator access over `kubectl port-forward`. Ingress and TLS termination are out of scope for the pilot.
- LLM credentials come from a file-mounted `fnox.toml` (`BAML_FNOX_CONFIG=/config/fnox.toml`). Runners do not read LLM API keys from environment variables.

The parked install surface is the Helm chart at [`operations/deployment/helm/karakara-os/`](../../operations/deployment/helm/karakara-os/).

## Prerequisites

- Kubernetes cluster ≥ 1.24 with a default `StorageClass`.
- Helm 3.x, `kubectl`, `curl`, `jq`.
- Docker (or compatible) to build the runner image.
- Rust toolchain (stable) to run the `karak` CLI. Installed from the repository root.
- Repo checkout. All commands below are run from the repository root.

The Karakara runner image is not published to a public registry. Operators build and push their own image.

## Step 1 — Build and push the runner image

```bash
# Build
docker build -f operations/containers/runner.Dockerfile \
  -t your-registry.example.com/karakara-runner:0.1.0 .

# Push to your registry
docker push your-registry.example.com/karakara-runner:0.1.0
```

> **Note:** [`operations/containers/runner.Dockerfile`](../../operations/containers/runner.Dockerfile) currently has a known `models/fastembed` / `karak` entrypoint issue tracked in `#29`.

Compare the trailing `sha256:…` digests against the one printed by
`docker push` (or the registry's manifest).

## Step 2 — Create the three required objects

The chart references pre-existing objects by name. Create them before installing.

```bash
kubectl create namespace karakara

# SurrealDB credentials
kubectl create secret generic surrealdb-credentials -n karakara \
  --from-literal=username=karakara \
  --from-literal=password="$(openssl rand -hex 32)"

# Runner operator token
kubectl create secret generic runner-token -n karakara \
  --from-literal=token="$(openssl rand -hex 32)"

# fnox.toml (LLM configuration — see below for a minimal example)
kubectl create configmap fnox-config -n karakara \
  --from-file=fnox.toml=./fnox.toml
```

A minimal `fnox.toml` for the smoke path. The smoke fixture does not invoke an LLM, but the runner still loads the file at startup, so either keep a valid file on disk or supply real credentials:

```toml
# fnox.toml — minimal pilot example
[secrets.OPENROUTER_API_KEY]
description = "OpenRouter API key — LLM backend for all agents"
if_missing = "ignore"
# default = "sk-or-v1-..."  # uncomment and set for agents that call an LLM
```

See [`fnox.toml`](../../fnox.toml) in the repo root for the full template.

Store the runner token once and reuse it for the rest of this guide:

```bash
RUNNER_TOKEN="$(kubectl -n karakara get secret runner-token -o jsonpath='{.data.token}' | base64 -d)"
```

## Step 3 — Install the chart

For the design-partner profile, edit [`operations/deployment/helm/karakara-os/examples/design-partner-values.yaml`](../../operations/deployment/helm/karakara-os/examples/design-partner-values.yaml) so `runner.image.repository` and `runner.image.tag` point at your registry, then:

```bash
helm upgrade --install karakara operations/deployment/helm/karakara-os/ \
  --namespace karak --create-namespace \
  -f operations/deployment/helm/karakara-os/examples/design-partner-values.yaml
```

For local k3d:

```bash
helm upgrade --install karakara operations/deployment/helm/karakara-os/ \
  --namespace karak --create-namespace \
  -f operations/deployment/helm/karakara-os/examples/k3d-values.yaml
```

Resource sizing: the chart defaults the runner to 2Gi memory request and 5Gi memory limit. This is the empirically observed floor for publishing a real multi-agent set (e.g. `argument-cleese` + `argument-chapman`) end-to-end — `POST /deploy` runs tar extract, BAML IL load, QuickJS init, and tool registration on top of the resident fastembed ONNX model, SurrealDB client, and provenance backend. Raise `runner.resources.limits.memory` further for heavier workloads; lowering it below 5Gi tends to OOM-kill the runner mid-deploy with a confusing `connection closed before message completed` symptom client-side.

Cluster host memory floor (local k3d): two runner replicas at 2Gi request each plus SurrealDB at 256Mi account for 4.25Gi of memory requests, and kube-system pods consume more on top. On macOS Docker Desktop (default 4 GiB) or colima with similar defaults that's below the floor — pods stay `Pending` with `0/1 nodes are available: 1 Insufficient memory`. Allocate **≥6 GiB** to Docker Desktop / colima before `helm upgrade --install`. This is distinct from the OOMKilled path above (which kicks in only once a pod is running) — see the Troubleshooting table.

## Step 4 — Verify pods are running

```bash
kubectl -n karakara rollout status statefulset/karakara-karakara-os-runner --timeout=180s
kubectl -n karakara rollout status statefulset/karakara-karakara-os-surrealdb --timeout=180s
kubectl -n karakara get pods
```

Expected: two runner pods and one SurrealDB pod, all `Ready`.

## Step 5 — Operator access via port-forward

```bash
kubectl -n karakara port-forward svc/karakara-karakara-os-runner-api 18080:18080
```

Leave this running in a separate terminal. All subsequent commands target `http://localhost:18080`.

Health checks (public, no token required):

```bash
curl -sf http://localhost:18080/healthz && echo ok
curl -sf http://localhost:18080/readyz  && echo ok
```

## Step 6 — Authenticated publish and deploy

Publish the `dispatch-echo` fixture and deploy it in one step using the authenticated CLI path from [#220](https://github.com/semiotic-agentium/agent-platform/issues/220):

```bash
cargo run -p karakara-os -- push \
  --agents tests/fixtures/agents/dispatch-echo \
  --url http://localhost:18080 \
  --repository-url http://localhost:18080/repository \
  --runner-token "$RUNNER_TOKEN"
```

On success the CLI prints `published: dispatch-echo@v<N>`, a content hash, and `deployed: ok`.

## Step 7 — Verify the deployment is visible

```bash
curl -s http://localhost:18080/agents \
  | jq '[.[] | select(.agent_package == "dispatch-echo") | {
      agent_package,
      content_hash: .agent_card.content_hash
    }]'
```

Expected: a non-empty array with one entry whose `agent_package` is `dispatch-echo`. This is the authoritative "package is usable" assertion for the pilot smoke.

## Step 8 — First smoke action (dispatch)

`dispatch-echo` is a deterministic dispatch smoke target, not a conversational chat agent — its chat handler returns the literal string `"dispatch-echo does not handle A2A messages"`. The pilot smoke therefore uses only the operator-visible `POST /agents/{pkg}/dispatch` route.

```bash
SMOKE_ID="k8s-pilot-smoke-$(date +%s)"

curl -s -X POST "http://localhost:18080/agents/dispatch-echo/dispatch" \
  -H 'content-type: application/json' \
  -d "$(jq -n --arg sid "$SMOKE_ID" '{
        routing_key: "pilot.smoke",
        message_type: "k8s-pilot-smoke.v1",
        messages: [],
        task_id: $sid,
        context_id: $sid,
        message_id: $sid
      }')" \
  | jq .
```

Expected:

```json
{
  "accepted": true,
  "detail": "routing_key=pilot.smoke messages=0"
}
```

## Step 9 — Cross-runner A2A verification (optional)

Cluster fallback is implemented on the A2A path, not the dispatch path — `POST /.../dispatch` only resolves against the receiving pod's local registry and returns `AgentNotFound` otherwise. To confirm cross-pod forwarding works, send an A2A chat request to each pod directly and check the HTTP status:

- The owning pod handles the request locally and returns `200`.
- The non-owning pod consults the cluster resolver, forwards to the owner, and also returns `200`.
- If routing is broken, the non-owning pod returns `404` (or another 4xx) and the body is an RFC 7807 problem document naming the missing agent.

```bash
kubectl -n karakara port-forward pod/karakara-karakara-os-runner-0 18081:18080 &
kubectl -n karakara port-forward pod/karakara-karakara-os-runner-1 18082:18080 &

body='{"jsonrpc":"2.0","id":"1","method":"message.sendStream","params":{"message":{"messageId":"cross-runner-1","role":"user","parts":[{"kind":"text","text":"cross-runner smoke"}]}}}'

for port in 18081 18082; do
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 30 \
    -X POST "http://localhost:$port/agents/dispatch-echo/a2a" \
    -H 'Content-Type: application/json' \
    -d "$body")
  echo "port=$port http=$code"
done
```

Expected: both lines print `http=200`. `dispatch-echo` does not implement A2A, so its response body is the literal string `"dispatch-echo does not handle A2A messages"`; the step does not assert anything about the body because the HTTP status alone is sufficient to prove the runner layer routed the request end-to-end.

## Step 10 — Observability sanity check (optional)

If `observability.enabled` is `true` and `observability.otlpEndpoint` points at a reachable collector, OTLP traces and metrics are emitted with the pilot identity contract (`service.name=karakara-runner`, `service.instance.id=<pod-name>`, `k8s.namespace.name=<namespace>`). `deployment.environment` is the first non-empty of `observability.environment`, `global.environment`, or the literal `pilot`. See [`operations/observability/README.md`](../../operations/observability/README.md) for the collector setup and dashboard walkthrough, and [`docs/reference/metrics-inventory.md`](../reference/metrics-inventory.md) for the canonical metric names.

## Troubleshooting

| Symptom | What to check | What to do |
|---|---|---|
| Pods stuck in `Pending` with `pod has unbound immediate PersistentVolumeClaims` event | `kubectl -n karakara describe pod <name>` | Default `StorageClass` missing, or PVC cannot be satisfied. Set `runner.persistence.storageClass` / `surrealdb.persistence.storageClass`. |
| Pods stuck in `Pending` with `0/N nodes are available: N Insufficient memory` event | `kubectl -n karakara describe pod <name>` | Cluster host doesn't have enough schedulable memory. Two runner replicas (2Gi request each) plus SurrealDB (256Mi request) account for 4.25Gi of requests; kube-system pods consume more on top. On local k3d, raise Docker Desktop / colima allocation to ≥6 GiB. Distinct from `OOMKilled` below — this happens before scheduling. |
| Pods in `ImagePullBackOff` | `kubectl -n karakara describe pod <name>` | Wrong `runner.image.repository`/`tag` or registry unreachable. |
| Same-tag rebuild + `rollout restart` reports complete but pods still run the old code | `kubectl -n karakara get pod -l app.kubernetes.io/component=runner -o jsonpath='{.items[*].status.containerStatuses[?(@.name=="runner")].imageID}'` | `runner.image.pullPolicy` is `IfNotPresent` in the values you installed with. Either set it to `Always` (chart default) or use a unique tag per build. |
| `POST /deploy` returns `connection closed before message completed`, or pods show `Reason: OOMKilled` / exit code 137 | `kubectl -n karakara describe pod <name>` | Runner exceeded its memory limit during deploy (tar extract + BAML IL load + QuickJS init + tool registration on top of resident SurrealDB client and provenance backend). The chart default is 5Gi, which fits the documented multi-agent fixtures; raise `runner.resources.limits.memory` if a heavier workload still OOMs. |
| `readyz` returns 503 for more than a minute | `kubectl -n karakara logs statefulset/karakara-karakara-os-runner` | SurrealDB not up, or runner cannot reach it. Verify `surrealdb-credentials` keys match `values.yaml` (`username`/`password`). |
| `401 authentication required` from publish/deploy | `kubectl -n karakara get secret runner-token -o yaml` | `RUNNER_TOKEN` missing or wrong. Re-export from the secret (see Step 2). |
| `400 routing_key must be non-empty` on dispatch | request body | Ensure `routing_key` and `message_type` are present and non-empty strings. |
| `/agents` is empty after publish+deploy | runner logs, `karakara list-deployed-instances --url http://localhost:18080` | Deploy silently failed or ran on the other pod. Check both pods (Step 9). |
| LLM-backed agents fail with `secret not resolved` | runner logs | `fnox.toml` in the ConfigMap does not have a `default = "..."` for the required key. Update the ConfigMap, then `kubectl -n karakara rollout restart statefulset/karakara-karakara-os-runner`. |

Pod logs:

```bash
kubectl -n karakara logs statefulset/karakara-karakara-os-runner --tail=200
kubectl -n karakara logs statefulset/karakara-karakara-os-surrealdb --tail=200
```

## Support matrix and known limitations

Supported in the pilot:

- Two-replica runner `StatefulSet` + shared SurrealDB + `ClusterIP` API service + `kubectl port-forward` operator access.
- `runner-token` secret for operator auth on protected routes.
- Per-agent manifest allowlist as the deny-by-default tool gate, with an optional cluster-wide access-class cap via the `BAML_TOOL_ACCESS_ALLOWLIST` env var. See [Tool Access in agent-runner.md](../reference/agent-runner.md#tool-access) for the full model.
- `fnox-config` ConfigMap mounted at `/config/fnox.toml` for LLM credentials.
- Helm values profiles: [`examples/k3d-values.yaml`](../../operations/deployment/helm/karakara-os/examples/k3d-values.yaml) (local) and [`examples/design-partner-values.yaml`](../../operations/deployment/helm/karakara-os/examples/design-partner-values.yaml) (production-like).
- OpenTelemetry export to an operator-supplied OTLP gRPC endpoint.

Deferred / blocked before production k8s promotion:

- Sandbox tier (`/dev/kvm`) support in the Helm chart — Issue `#33` (paused while the k8s pilot is dormant under `#44`).
- Published runner image. Until then, operators build and push their own cluster-reachable image.
- Ingress / TLS termination. Operators front the API service with their own ingress controller if needed.
- Multi-node SurrealDB HA. The pilot ships a single SurrealDB replica.
