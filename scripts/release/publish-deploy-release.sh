#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

# Semver release tag helper: verify tag matches Cargo.toml and push tag.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() {
  echo "publish-deploy-release: $*" >&2
  exit 1
}

if [[ "$#" -ne 1 ]]; then
  die "usage: publish-deploy-release.sh <vX.Y.Z>"
fi

GIT_REF="$1"
if [[ "${GIT_REF}" != v* ]] || [[ "${GIT_REF}" == */* ]]; then
  die "expected a semver tag like v0.1.0 (got '${GIT_REF}')"
fi

DRY="${KARAKARA_RELEASE_DRY_RUN:-0}"

if [[ "$DRY" == "1" ]]; then
  echo "publish-deploy-release: DRY-RUN ${GIT_REF}" >&2
  exit 0
fi

cd "${ROOT}"

export RELEASE_TAG="${GIT_REF}"
bash "${ROOT}/scripts/release/verify-release-tag-matches-workspace-version.sh" \
  || die "tag ${GIT_REF} must match Cargo.toml workspace version"
if git rev-parse "${GIT_REF}" >/dev/null 2>&1; then
  if [[ "${KARAKARA_RELEASE_RETAG:-0}" == "1" ]]; then
    git tag -d "${GIT_REF}"
    git push origin ":refs/tags/${GIT_REF}" 2>/dev/null || true
  else
    die "tag ${GIT_REF} already exists (use KARAKARA_RELEASE_RETAG=1 to replace)"
  fi
fi
git tag -a "${GIT_REF}" -m "Release ${GIT_REF}"
branch="$(git rev-parse --abbrev-ref HEAD)"
git push origin "refs/heads/${branch}:refs/heads/${branch}"
git push origin "refs/tags/${GIT_REF}:refs/tags/${GIT_REF}"
echo "publish-deploy-release: pushed tag ${GIT_REF}"
