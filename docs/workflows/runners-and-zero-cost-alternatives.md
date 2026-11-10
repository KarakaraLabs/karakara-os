# Runners & Zero-Cost Alternatives (`KarakaraLabs/karakara-os`)

When Karakara OS was forked from `Agentium OS`, several workflows referenced custom self-hosted runner labels (`arc-runner-set`, and `kvm-enabled` for a since-removed v1 sandbox workflow) and paid LLM API calls (`OPENROUTER_API_KEY`). Because `KarakaraLabs/karakara-os` is a **public repository**, standard GitHub-hosted Linux x64 and ARM64 runners are **100% free (`$0`)** and can replace most self-hosted lanes.

---

## 1. Runner Comparison Matrix

| Runner Label | Type | Hardware (Public Repo) | Cost | Workflows Using It |
|---|---|---|---|---|
| `ubuntu-latest` | GitHub-hosted standard | 4 vCPU, 16 GB RAM, 14 GB SSD (x86_64) | **`$0`** | `rust-ci-standard.yml` (standard PR nextest & fmt/clippy), `secret-scan.yml`, `reuse.yml`, `release-please.yml`, `release-pr-postprocess.yml`, `release-publish.yml` |
| `ubuntu-22.04` | GitHub-hosted standard | 4 vCPU, 16 GB RAM (x86_64, glibc 2.35) | **`$0`** | `release-please.yml`, `release-publish.yml` (`x86_64-unknown-linux-gnu` binary builds) |
| `ubuntu-22.04-arm` | GitHub-hosted ARM64 | 4 vCPU, 16 GB RAM (aarch64, glibc 2.35) | **`$0`** | `release-please.yml`, `release-publish.yml` (`aarch64-unknown-linux-gnu` native binary builds) |
| `arc-runner-set` | Self-hosted Kubernetes ARC | Custom K8s pod pool (not provisioned) | **Infra cost** | `rust-ci.yml` (legacy reference lane), `claude-md-sync.yml`, `k8s-pilot-validation.yml` |

---

## 2. Zero-Cost Migration Paths for Self-Hosted Workflows

### A. Dedicated Standard Workflow: `rust-ci-standard.yml` (Issue #69)
- **Resolved in Issue #69:** Rather than modifying the legacy `rust-ci.yml` (which is kept for reference with its ARC and secret configurations), a new dedicated standard workflow `.github/workflows/rust-ci-standard.yml` runs on GitHub-hosted `ubuntu-latest` ($0) with `CI_FEATURES_UNTRUSTED` (`baml-rt-tools/http-tools,baml-rt-builder/http-tools,baml-rt-builder/dev-tools,baml-agent-runner/http-tools,baml-agent-runner/memory`).
- **Triggers:** Automatically runs on pull requests (`pull_request`), pushes to `main` (`push`), and manual `workflow_dispatch`. Pushing to PR branches triggers solely via `pull_request` to avoid duplicate workflow runs.
- **Standalone:** Requires no third-party API keys and no self-hosted runner infrastructure.
- **Local Verification Alternative:**
  ```bash
  just fmt
  just clippy
  just test-unit
  ```

### B. microVM tool tests (`--features sandbox-e2e-kvm`) — Local Today, `ubuntu-latest` If Added to CI
- **Current state:** No workflow. The tests need `/dev/kvm` and a digest-pinned tool-runtime image; run them locally with `just test-kvm`.
- **`$0` Path if a workflow is added:** Standard GitHub-hosted `ubuntu-latest` runners (Azure v5 VMs) support nested virtualization and expose `/dev/kvm`. A job on `ubuntu-latest` needs one permission step before running the recipe:
  ```yaml
  - name: Enable /dev/kvm permissions on ubuntu-latest
    run: |
      ls -la /dev/kvm
      sudo chmod 666 /dev/kvm
  ```

### C. `claude-md-sync.yml` — Switch Runner to `ubuntu-latest` (Keep On-Demand)
- **Current bottleneck:** Uses `runs-on: arc-runner-set` even though the job only invokes `git`, `jq`, `curl`, `python3`, and `gh`.
- **`$0` Compute Alternative:** Replace `runs-on: arc-runner-set` with `runs-on: ubuntu-latest`. Keep the trigger on `workflow_dispatch` (or manual execution) unless an `OPENROUTER_API_KEY` budget is allocated for automated documentation PRs.

### D. `k8s-pilot-validation.yml` — Run Locally via `just`
- **Current bottleneck:** Spins up a multi-node `k3d` cluster, Argo CD, SurrealDB, and local Docker registry for up to 90 minutes, which can CPU-starve standard 4-vCPU runners.
- **`$0` Alternative:** Keep `k8s-pilot-validation.yml` disabled in CI (`workflow_dispatch` only) and run the authoritative validation locally before cutting a release:
  ```bash
  just verify-k8s-pilot-package
  ```

