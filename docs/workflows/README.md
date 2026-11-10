# GitHub Actions Workflows & CI Guide

This directory is the canonical reference for all GitHub Actions workflows in [`.github/workflows/`](../../.github/workflows/), their runner requirements, secrets/variables, and operational state.

## Documentation Map (`docs/workflows/`)

| Document | Purpose |
|---|---|
| [`README.md`](README.md) | Master inventory of all 9 workflows, triggers, runner classes, and `gh workflow` CLI commands |
| [`runners-and-zero-cost-alternatives.md`](runners-and-zero-cost-alternatives.md) | Runner architecture (`ubuntu-latest` vs self-hosted `arc-runner-set`) and `$0` public-repo migration paths |
| [`secrets-variables-and-external-ties.md`](secrets-variables-and-external-ties.md) | Complete reference for `secrets.*`, `vars.*`, `fnox.toml`, `.env.example`, and external post-fork dependencies |

---

## 1. Master Inventory of All 9 Workflows

Because `KarakaraLabs/karakara-os` is a **public repository**, standard GitHub-hosted Linux x64 and ARM64 runners (`ubuntu-latest`, `ubuntu-22.04`, `ubuntu-22.04-arm`) are **free (`$0`) with unlimited minutes**.

Workflows that depend on unprovisioned self-hosted runners (`arc-runner-set`) or paid LLM tokens (`OPENROUTER_API_KEY`) carry a **dual lock**: their automatic `push` / `pull_request` / `schedule` triggers are commented out in YAML (leaving `workflow_dispatch`) **and** they are manually disabled in GitHub Actions (`disabled_manually` via `gh workflow disable`).

All active workflows share a unified architectural pattern: structured header documentation, least-privilege permissions, descriptive step naming, OS package retry mechanisms (`scripts/ci/apt-update-retry.sh`), and dynamic Markdown status summaries appended to `$GITHUB_STEP_SUMMARY`.

| Workflow Name | File | Active Triggers | Runner(s) | Secrets / Cost | Current State |
|---|---|---|---|---|---|
| **Rust CI (Standard Runner)** | [`rust-ci-standard.yml`](../../.github/workflows/rust-ci-standard.yml) | `push` (`main`), `pull_request`, `workflow_dispatch` | `ubuntu-latest` | `$0` (Standard, union non-LLM) | **ACTIVE** |
| **Secret Scan (Gitleaks)** | [`secret-scan.yml`](../../.github/workflows/secret-scan.yml) | `push` (`main`), `pull_request`, `workflow_dispatch` | `ubuntu-latest` | `$0` (Free, read-only) | **ACTIVE** |
| **License Compliance (REUSE)** | [`reuse.yml`](../../.github/workflows/reuse.yml) | `push` (`main`), `pull_request`, `workflow_dispatch` | `ubuntu-latest` | `$0` (Free, read-only) | **ACTIVE** |
| **Release Please (Automation & Assets)** | [`release-please.yml`](../../.github/workflows/release-please.yml) | `push` (`main`), `workflow_dispatch` | `ubuntu-latest`, `ubuntu-22.04`, `ubuntu-22.04-arm` | `$0` (Free, uses `GITHUB_TOKEN`) | `disabled_manually` (enable when cutting releases) |
| **Release PR Post-Process (Cargo.lock)** | [`release-pr-postprocess.yml`](../../.github/workflows/release-pr-postprocess.yml) | `pull_request` (`release-please--*` branches) | `ubuntu-latest` | `$0` (Free, uses `GITHUB_TOKEN`) | `disabled_manually` (enable with `Release Please`) |
| **Release Publish (Multi-Arch Binaries)** | [`release-publish.yml`](../../.github/workflows/release-publish.yml) | `push` (`v*.*.*` tags), `pull_request` (`scripts/release/**`), `workflow_dispatch` | `ubuntu-latest`, `ubuntu-22.04`, `ubuntu-22.04-arm` | `$0` (Free, uses `GITHUB_TOKEN`) | `disabled_manually` (enable with `Release Please`) |
| **Rust CI** | [`rust-ci.yml`](../../.github/workflows/rust-ci.yml) | `workflow_dispatch` (`push`/`pull_request` commented out) | `ubuntu-latest` (filter + fork PRs), `arc-runner-set` (trusted lane) | Self-hosted + `OPENROUTER_API_KEY` | `disabled_manually` + YAML `workflow_dispatch` (Legacy reference lane) |
| **Doc Sync** | [`claude-md-sync.yml`](../../.github/workflows/claude-md-sync.yml) | `workflow_dispatch` (`push` commented out) | `arc-runner-set` | Self-hosted + `OPENROUTER_API_KEY` | `disabled_manually` + YAML `workflow_dispatch` |
| **K8s Pilot Validation** | [`k8s-pilot-validation.yml`](../../.github/workflows/k8s-pilot-validation.yml) | `workflow_dispatch` (`push`/`pull_request` commented out) | `ubuntu-latest` (filter), `arc-runner-set` (90m k3d) | Self-hosted (high CPU/RAM) | `disabled_manually` + YAML `workflow_dispatch` |

---

## 2. Workflow Groups Explained

### Group A — Always-On Standard CI & Security Guardrails (`ubuntu-latest`, `$0`)
1. **[`rust-ci-standard.yml`](../../.github/workflows/rust-ci-standard.yml) (`Rust CI (Standard Runner)`):**
   - The authoritative routine PR and `main` test suite, split into two concurrent parallel jobs for sub-2min linter feedback and fast warm execution:
     - **Code Formatting & Clippy Linter (`clippy`):** Runs `cargo +nightly fmt --check` and `cargo clippy --workspace --all-targets --locked -D warnings` on `ubuntu-latest` with dedicated `ci-clippy-standard` cache.
     - **Union Nextest Suite (`nextest`):** Runs all 2,116 workspace unit, integration, and property tests via `cargo nextest run --profile ci-merged` using the high-performance `mold` linker and unpacked split debuginfo (`-C link-arg=-fuse-ld=mold -C split-debuginfo=unpacked`) with `ci-nextest-standard` cache.
   - Publishes JUnit test annotations and real-time Markdown summaries with test execution metrics to `$GITHUB_STEP_SUMMARY`.
2. **[`secret-scan.yml`](../../.github/workflows/secret-scan.yml) (`Secret Scan (Gitleaks)`):**
   - Runs `gitleaks` (`v8.30.1`) against the full commit history (`gitleaks git`) and working tree (`gitleaks dir`) using [`.gitleaks.toml`](../../.gitleaks.toml).
   - Completes in ~17s with `contents: read` permissions and zero external dependencies, appending scan results to `$GITHUB_STEP_SUMMARY`.
3. **[`reuse.yml`](../../.github/workflows/reuse.yml) (`License Compliance (REUSE)`):**
   - Runs `fsfe/reuse-action@v5` to enforce [REUSE 3.3](https://reuse.software/) Apache 2.0 licensing metadata across all tracked files (inline `SPDX-FileCopyrightText` + `SPDX-License-Identifier: Apache-2.0` headers or [`REUSE.toml`](../../REUSE.toml) annotations).
   - Completes in ~18s with `contents: read` permissions, appending compliance status to `$GITHUB_STEP_SUMMARY`.

### Group B — Self-Hosted & Paid LLM Workflows (Dual-Locked)
4. **[`rust-ci.yml`](../../.github/workflows/rust-ci.yml) (`Rust CI`):**
   - Splits execution via the `changes` job (`ubuntu-latest`):
     - **Trusted lane (`nextest`):** Targets `arc-runner-set`, generates `fnox.toml` from repository secrets (`OPENROUTER_API_KEY`, etc.), runs `cargo +nightly fmt --check`, `cargo clippy`, and `cargo nextest run` with `CI_FEATURES_TRUSTED` (includes `llm-tests`).
     - **Untrusted lane (`nextest-untrusted`):** Targets free `ubuntu-latest` for fork PRs without secrets using `CI_FEATURES_UNTRUSTED` (omits `llm-tests`).
5. **[`claude-md-sync.yml`](../../.github/workflows/claude-md-sync.yml) (`Doc Sync`):**
   - Calls OpenRouter (`anthropic/claude-sonnet-4` via `OPENROUTER_API_KEY`) on `arc-runner-set` after merges to `main` to propose updates to `CLAUDE.md`, `docs/reference/**`, and `docs/runbooks/**` via an `auto/doc-sync-<sha>` PR.
6. **[`k8s-pilot-validation.yml`](../../.github/workflows/k8s-pilot-validation.yml) (`K8s Pilot Validation`):**
   - Runs [`scripts/verify-k8s-pilot-package.sh`](../../scripts/verify-k8s-pilot-package.sh) on `arc-runner-set` (90-minute timeout, k3d + Argo CD + local registry + Git LFS ONNX models). Local equivalent: `just verify-k8s-pilot-package`.

The microVM tool tests (`--features sandbox-e2e-kvm`) have no CI workflow; they need `/dev/kvm` and run locally via `just test-kvm`.

### Group C — Automated Release Pipeline (`ubuntu-latest` / `ubuntu-22.04` / `arm`, `$0`)
7. **[`release-please.yml`](../../.github/workflows/release-please.yml) (`Release Please (Automation & Assets)`):**
   - Uses `googleapis/release-please-action@v4` with [`release-please-config.json`](../../release-please-config.json) and [`.release-please-manifest.json`](../../.release-please-manifest.json) to maintain a Release PR updating `Cargo.toml`, `operations/deployment/helm/karakara-os/Chart.yaml`, and `CHANGELOG.md`. When merged, it tags `vX.Y.Z`, builds native `x86_64` and `aarch64` tarballs, and uploads them to the GitHub Release with dynamic `$GITHUB_STEP_SUMMARY` reporting.
8. **[`release-pr-postprocess.yml`](../../.github/workflows/release-pr-postprocess.yml) (`Release PR Post-Process (Cargo.lock)`):**
   - Triggered on `release-please--*` PRs; runs `cargo update --workspace` on `ubuntu-latest` and commits the updated `Cargo.lock` to the Release PR branch with exponential rebase retry and execution summaries.
9. **[`release-publish.yml`](../../.github/workflows/release-publish.yml) (`Release Publish (Multi-Arch Binaries)`):**
   - Standalone native binary builder for `v*.*.*` tag pushes, manual `workflow_dispatch` dry-runs/backfills, and `aarch64` native build smoke tests when `scripts/release/**` changes. See [`binary-release-guide.md`](../runbooks/binary-release-guide.md).

---

## 3. Operational CLI Reference (`gh workflow`)

```bash
# List all workflows and their current state (active vs disabled_manually)
gh workflow list -R KarakaraLabs/karakara-os --all

# Enable or disable a specific workflow via GitHub API
gh workflow enable <workflow-file-or-id> -R KarakaraLabs/karakara-os
gh workflow disable <workflow-file-or-id> -R KarakaraLabs/karakara-os

# Inspect recent workflow runs
gh run list -R KarakaraLabs/karakara-os --limit 15
```
