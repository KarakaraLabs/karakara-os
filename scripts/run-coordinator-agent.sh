#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

if (( $# > 1 )); then
  echo "Usage: $0 [debug|--debug]" >&2
  exit 2
fi

case "${1:-release}" in
  debug|--debug)
    BUILD_PROFILE="debug"
    KARAK_BIN="${KARAK_BIN:-${CARGO_TARGET_DIR:-target}/debug/karak}"
    ;;
  release)
    BUILD_PROFILE="release"
    KARAK_BIN="${KARAK_BIN:-${CARGO_TARGET_DIR:-target}/release/karak}"
    ;;
  *)
    echo "Usage: $0 [debug|--debug]" >&2
    exit 2
    ;;
esac

RUNNER_URL="${RUNNER_URL:-http://127.0.0.1:18080}"
RUNNER_BIND="${RUNNER_BIND:-127.0.0.1:18080}"
REPOSITORY_URL="${REPOSITORY_URL:-${RUNNER_URL}/repository}"
RUNNER_LOG="${RUNNER_LOG:-/tmp/coordinator-agent-runner.log}"
RUNNER_STATE_DIR="${RUNNER_STATE_DIR:-.runner-state}"
RUNNER_REPOSITORY_DIR="${RUNNER_REPOSITORY_DIR:-.repository}"
PROVENANCE_DB="${PROVENANCE_DB:-provenance.db}"
RUNNER_READY_SECS="${RUNNER_READY_SECS:-180}"
# This script owns a local development runner. Remove any prior deployment before
# installing so durable context pins from an interrupted chat cannot block a restart.
LOCAL_DEV_UNDEPLOY_RATIONALE="${LOCAL_DEV_UNDEPLOY_RATIONALE:-local development runner restart after an interrupted chat}"
# clickup-agent also declares typesafe/system_one in its manifest.
KARAK_FEATURES="${KARAK_FEATURES:-clickup,notion,typesafe}"
RUNNER_PID=""

# Export traces and metrics to the local observability stack unless the caller
# supplied another endpoint. OTEL_SDK_DISABLED=true still disables all export.
export OTEL_TRACES_EXPORTER="${OTEL_TRACES_EXPORTER:-otlp}"
export OTEL_METRICS_EXPORTER="${OTEL_METRICS_EXPORTER:-otlp}"
export OTEL_EXPORTER_OTLP_ENDPOINT="${OTEL_EXPORTER_OTLP_ENDPOINT:-http://localhost:4317}"
export OTEL_EXPORTER_OTLP_PROTOCOL="${OTEL_EXPORTER_OTLP_PROTOCOL:-grpc}"
export RUST_LOG_FMT="${RUST_LOG_FMT:-baml_rt=info,baml_rt_a2a=debug,baml_rt_provenance=debug,baml_rt_quickjs=debug,baml_rt_interceptor=debug,baml_agent_runner=info,quickjs_runtime::quickjsrealmadapter=warn,quickjs_runtime::typescript=warn}"

log() {
  printf '\n==> %s\n' "$*"
}

runner_curl() {
  local args=(-fsS)
  if [[ -n "${RUNNER_TOKEN:-}" ]]; then
    args+=(-H "X-Runner-Token: ${RUNNER_TOKEN}")
  fi
  curl "${args[@]}" "$@"
}

cleanup() {
  if [[ -n "$RUNNER_PID" ]] && kill -0 "$RUNNER_PID" 2>/dev/null; then
    kill "$RUNNER_PID" 2>/dev/null || true
    wait "$RUNNER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

undeploy_local_agent_if_active() {
  local agent_name="$1"
  local deployments active_hash

  deployments="$(runner_curl "$RUNNER_URL/deployments")"
  active_hash="$(
    printf '%s' "$deployments" | python3 -c '
import json
import sys

agent_name = sys.argv[1]
records = json.load(sys.stdin)
active = [
    record for record in records
    if record.get("agent_name") == agent_name and record.get("status") == "active"
]
if active:
    active.sort(key=lambda record: record.get("deployed_at", ""), reverse=True)
    print(active[0].get("content_hash", ""))
' "$agent_name"
  )"

  if [[ -z "$active_hash" ]]; then
    return 0
  fi

  echo "[cleanup] Removing prior local deployment for $agent_name ($active_hash)"
  runner_curl -X POST "$RUNNER_URL/undeploy" \
    -H 'Content-Type: application/json' \
    --data "$(python3 -c 'import json,sys; print(json.dumps({"hash": sys.argv[1], "override_rationale": sys.argv[2]}))' "$active_hash" "$LOCAL_DEV_UNDEPLOY_RATIONALE")" \
    >/dev/null
}

deployments_cleanup() {
  local agent_dir agent_name
  for agent_dir in \
    agents/coordinator-agent \
    agents/notion-agent \
    agents/clickup-agent; do
    agent_name="$(basename "$agent_dir")"
    undeploy_local_agent_if_active "$agent_name"
  done
}

log "Building the $BUILD_PROFILE runner with $KARAK_FEATURES"
build_args=(-p karakara-os --no-default-features --features "$KARAK_FEATURES")
if [[ "$BUILD_PROFILE" == "release" ]]; then
  build_args=(--release "${build_args[@]}")
fi
cargo build "${build_args[@]}"

if [[ ! -x "$KARAK_BIN" ]]; then
  echo "Build did not produce the expected binary: $KARAK_BIN" >&2
  exit 1
fi

log "Checking the runner"
if runner_curl "$RUNNER_URL/readyz" >/dev/null 2>&1; then
  echo "Runner is already running at $RUNNER_URL"
else
  log "Starting the runner"
  echo "Runner log: $RUNNER_LOG"
  "$KARAK_BIN" serve \
    --serve-http "$RUNNER_BIND" \
    --repository-url "$REPOSITORY_URL" \
    --state-dir "$RUNNER_STATE_DIR" \
    --repository-dir "$RUNNER_REPOSITORY_DIR" \
    --provenance-db "$PROVENANCE_DB" \
    >"$RUNNER_LOG" 2>&1 < /dev/null &
  RUNNER_PID=$!

  for ((attempt = 0; attempt < RUNNER_READY_SECS; attempt++)); do
    if runner_curl "$RUNNER_URL/readyz" >/dev/null 2>&1; then
      break
    fi
    if ! kill -0 "$RUNNER_PID" 2>/dev/null; then
      cat "$RUNNER_LOG" >&2
      exit 1
    fi
    sleep 1
  done

  if ! runner_curl "$RUNNER_URL/readyz" >/dev/null 2>&1; then
    echo "Runner did not become ready at $RUNNER_URL. See $RUNNER_LOG" >&2
    exit 1
  fi
  echo "Runner is ready"
fi

log "Cleaning prior local deployments"
deployments_cleanup

log "Installing coordinator agents"
for agent_dir in \
  agents/coordinator-agent \
  agents/notion-agent \
  agents/clickup-agent; do
  "$KARAK_BIN" install agent \
    --path "$agent_dir" \
    --repository-url "$REPOSITORY_URL" \
    --url "$RUNNER_URL"
done

# Priority requests can use Jev only when the matching credential is present.
if [[ -n "${TYPESAFE_API_KEY:-}" ]]; then
  runner_curl -X PUT "$RUNNER_URL/config/typesafe" \
    -H 'Content-Type: application/json' \
    --data '{"enabled":true}' \
    >/dev/null
fi

log "Starting coordinator chat"
"$KARAK_BIN" chat \
  --agent coordinator-agent \
  --url "$RUNNER_URL"
