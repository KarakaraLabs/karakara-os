# Releasing Karakara OS

Releases are driven by Release Please. One SemVer must stay consistent across:

- `Cargo.toml` `[workspace.package].version`
- `Cargo.lock` workspace package entries
- `operations/deployment/helm/karakara-os/Chart.yaml` `version` and `appVersion`
- `.release-please-manifest.json`

## Normal release flow

1. Merge feature/fix PRs to `main` using Conventional Commits (`feat:`, `fix:`, etc.).
2. Release Please opens or updates a release PR.
3. Release PR postprocess workflow updates `Cargo.lock` on that PR.
4. Verify release PR CI is green.
5. Merge release PR.
6. Release Please creates tag `vX.Y.Z` and a GitHub Release.
7. Same Release Please workflow builds Linux binaries and uploads assets to that release.

Do not manually bump versions or create release tags during normal releases.

Expected release assets:

- `karakara-os-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- `karakara-os-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz`
- `SHA256SUMS`

## Manual fallback

Use only if Release Please is unavailable.

```bash
git checkout main
git pull
just bump-version patch   # or minor | major
git add Cargo.toml Cargo.lock operations/deployment/helm/karakara-os/Chart.yaml
git commit -m "release: v$(just workspace-version)"
git push origin main

VERSION=$(bash scripts/release/workspace-version.sh)
git tag "v${VERSION}"
git push origin "v${VERSION}"
```

The tag must point at the commit containing all version and lockfile updates.

## Helm chart probe check (cluster-free)

```bash
just helm-probe-check
```

## Parked Kubernetes pilot & remote registry publish

The local k3d / Argo CD validation harness was pruned and the Kubernetes pilot is dormant (Issue `#44`). GHCR container publishing and remote GitOps deploy ref updates remain deferred.
