# Runners & Zero-Cost Alternatives (`KarakaraLabs/karakara-os`)

When Karakara OS was forked from `Agentium OS`, four workflows referenced custom self-hosted runner labels (`arc-runner-set` and `kvm-enabled`) and paid LLM API calls (`OPENROUTER_API_KEY`). Because `KarakaraLabs/karakara-os` is a **public repository**, standard GitHub-hosted Linux x64 and ARM64 runners are **100% free (`$0`)** and can replace most self-hosted lanes.

---

## 1. Runner Comparison Matrix

| Runner Label | Type | Hardware (Public Repo) | Cost | Workflows Using It |
|---|---|---|---|---|
| `ubuntu-latest` | GitHub-hosted standard | 4 vCPU, 16 GB RAM, 14 GB SSD (x86_64) | **`$0` (Free)** | `secret-scan.yml`, `reuse.yml`, `release-please.yml`, `release-pr-postprocess.yml`, `release-publish.yml`, plus `changes` & `nextest-untrusted` in `rust-ci.yml` |
| `ubuntu-22.04` | GitHub-hosted standard | 4 vCPU, 16 GB RAM (x86_64, glibc 2.35) | **`$0` (Free)** | `release-please.yml`, `release-publish.yml` (`x86_64-unknown-linux-gnu` binary builds) |
| `ubuntu-22.04-arm` | GitHub-hosted ARM64 | 4 vCPU, 16 GB RAM (aarch64, glibc 2.35) | **`$0` (Free)** | `release-please.yml`, `release-publish.yml` (`aarch64-unknown-linux-gnu` native binary builds) |
| `arc-runner-set` | Self-hosted Kubernetes ARC | Custom K8s pod pool (not provisioned) | **Infra cost** | `rust-ci.yml` (trusted lane), `claude-md-sync.yml`, `k8s-pilot-validation.yml` |
| `kvm-enabled` | Self-hosted KVM host | Linux host with `/dev/kvm` (not provisioned) | **Infra cost** | `sandbox-e2e-kvm.yml` |

---

## 2. Zero-Cost Migration Paths for Self-Hosted Workflows

### A. `rust-ci.yml` — Run Non-LLM `fmt` + `clippy` + `nextest` on Free `ubuntu-latest`
- **Current bottleneck:** Same-repo PRs (`trusted == 'true'`) route to the `nextest` job on `arc-runner-set` and require `OPENROUTER_API_KEY` for `llm-tests`, while external fork PRs (`trusted != 'true'`) route to `nextest-untrusted` on `ubuntu-latest` using `CI_FEATURES_UNTRUSTED` (no secrets, no `llm-tests`).
- **`$0` Alternative:**
  - Switch `runs-on: arc-runner-set` to `runs-on: ubuntu-latest` on `sandbox-adapter`, `sandbox-adapter-image`, and `sandbox-adapter-image-handoff`.
  - Route standard PRs and `main` pushes through the non-LLM `CI_FEATURES_UNTRUSTED` feature set (`baml-rt-tools/http-tools,baml-rt-builder/http-tools,baml-rt-builder/dev-tools,baml-agent-runner/http-tools,baml-agent-runner/memory`) on `ubuntu-latest`.
  - Gate the paid `llm-tests` lane behind manual `workflow_dispatch` (or a dedicated label) so routine PRs get full Rust `fmt` + `clippy` + `nextest` coverage at `$0` cost.
- **Local Verification Alternative:**
  ```bash
  just fmt
  just clippy
  just test-unit
  ```

### B. `sandbox-e2e-kvm.yml` — Use Nested `/dev/kvm` on Standard `ubuntu-latest`
- **Current bottleneck:** Targets `runs-on: kvm-enabled` to access `/dev/kvm` for microsandbox E2E tests (`--features sandbox-e2e-kvm`).
- **`$0` Alternative:** Standard GitHub-hosted `ubuntu-latest` runners (Azure v5 VMs) support hardware nested virtualization and expose `/dev/kvm` out of the box. Changing `runs-on: kvm-enabled` to `runs-on: ubuntu-latest` and adding a single permission step:
  ```yaml
  - name: Enable /dev/kvm permissions on ubuntu-latest
    run: |
      ls -la /dev/kvm
      sudo chmod 666 /dev/kvm
  ```
  allows `sandbox-e2e-kvm.yml` to run for `$0` without self-hosted KVM hardware.

### C. `claude-md-sync.yml` — Switch Runner to `ubuntu-latest` (Keep On-Demand)
- **Current bottleneck:** Uses `runs-on: arc-runner-set` even though the job only invokes `git`, `jq`, `curl`, `python3`, and `gh`.
- **`$0` Compute Alternative:** Replace `runs-on: arc-runner-set` with `runs-on: ubuntu-latest`. Keep the trigger on `workflow_dispatch` (or manual execution) unless an `OPENROUTER_API_KEY` budget is allocated for automated documentation PRs.

### D. `k8s-pilot-validation.yml` — Run Locally via `just`
- **Current bottleneck:** Spins up a multi-node `k3d` cluster, Argo CD, SurrealDB, and local Docker registry for up to 90 minutes, which can CPU-starve standard 4-vCPU runners.
- **`$0` Alternative:** Keep `k8s-pilot-validation.yml` disabled in CI (`workflow_dispatch` only) and run the authoritative validation locally before cutting a release:
  ```bash
  just verify-k8s-pilot-package
  ```

