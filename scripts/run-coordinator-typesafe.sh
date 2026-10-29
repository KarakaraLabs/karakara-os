#!/usr/bin/env bash
# Optional wrapper for a local TypeSafe-enabled coordinator stack.
# Replace the placeholder in .env, or export TYPESAFE_API_KEY before running.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

: "${TYPESAFE_API_KEY:=REPLACE_WITH_YOUR_TYPESAFE_API_KEY}"
export TYPESAFE_API_KEY

if [[ "$TYPESAFE_API_KEY" == "REPLACE_WITH_YOUR_TYPESAFE_API_KEY" || "$TYPESAFE_API_KEY" == "apikey_....." ]]; then
  cat >&2 <<'EOF'
Set TYPESAFE_API_KEY in .env before running this script:

  TYPESAFE_API_KEY="REPLACE_WITH_YOUR_TYPESAFE_API_KEY"

Create the key in the TypeSafe console. The coordinator recipe stores enabled=true
in the local runner when this credential is present.
EOF
  exit 1
fi

exec just coordinator-agent
