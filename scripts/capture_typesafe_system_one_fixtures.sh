#!/usr/bin/env bash

# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

if [[ -z "${TYPESAFE_API_KEY:-}" ]]; then
  echo "TYPESAFE_API_KEY must be set" >&2
  exit 2
fi

for command in curl jq; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "$command is required" >&2
    exit 2
  fi
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
base_url="${TYPESAFE_API_BASE_URL:-https://api.typesafe.ai}"
endpoint="${base_url%/}/v1/systemone"
model="${TYPESAFE_MODEL:-jev-1.13.0}"
out_dir="${1:-$repo_root/crates/tools/typesafe/tests/fixtures/live}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
mkdir -p "$out_dir"

capture() {
  local name="$1"
  local request="$out_dir/$name.request.json"
  local response="$out_dir/$name.response.json"
  local raw_response="$tmp_dir/$name.response.json"
  local status

  jq --arg model "$model" '.model = $model' >"$request"
  status="$(
    curl --silent --show-error \
      --output "$raw_response" \
      --write-out '%{http_code}' \
      --request POST "$endpoint" \
      --header "Authorization: Bearer $TYPESAFE_API_KEY" \
      --header 'Content-Type: application/json' \
      --data-binary "@$request"
  )"

  if jq empty "$raw_response" >/dev/null 2>&1; then
    jq --sort-keys . "$raw_response" >"$response"
  else
    cp "$raw_response" "$response"
  fi
  chmod 0644 "$request" "$response"

  if [[ ! "$status" =~ ^2 ]]; then
    echo "$name returned HTTP $status; response saved to $response" >&2
    return 1
  fi

  jq -e --arg model "$model" '
    (.model | type == "string") and
    (.answers | type == "object") and
    (.usage | type == "object")
  ' "$response" >/dev/null
  echo "captured $name with $model"
}

capture choice_tie <<'JSON'
{
  "state": {
    "evidence": "There is no information that distinguishes candidate_alpha from candidate_beta. The candidate names are arbitrary."
  },
  "questions": {
    "tie": {
      "type": "choice",
      "instructions": "Which candidate is better supported by `evidence`?",
      "criteria": {
        "candidate_alpha": null,
        "candidate_beta": null
      }
    }
  }
}
JSON

capture rounded_probabilities <<'JSON'
{
  "state": "The customer says the product is useful but expensive and occasionally unreliable.",
  "questions": {
    "sentiment": {
      "type": "choice",
      "instructions": "What is the customer's overall sentiment?",
      "criteria": {
        "positive": "Mostly favorable",
        "mixed": "Material positive and negative views",
        "negative": "Mostly unfavorable"
      }
    },
    "satisfaction": {
      "type": "score",
      "instructions": "How satisfied is the customer?",
      "criteria": ["Not satisfied", "Partly satisfied", "Satisfied"]
    }
  }
}
JSON

capture structured_score_levels <<'JSON'
{
  "state": "The incident interrupted checkout for 12 minutes and affected about 5% of attempts. Retried purchases succeeded.",
  "questions": {
    "severity": {
      "type": "score",
      "instructions": "Rate the incident severity.",
      "criteria": [
        {"label": "minor", "definition": "No user-visible interruption"},
        {"label": "moderate", "definition": "Limited interruption with a working retry"},
        {"label": "major", "definition": "Broad or sustained outage"}
      ]
    }
  }
}
JSON

capture null_choice_descriptions <<'JSON'
{
  "state": "The record explicitly selects beta.",
  "questions": {
    "selected_label": {
      "type": "choice",
      "instructions": "Which label does the record select?",
      "criteria": {
        "alpha": null,
        "beta": null,
        "gamma": null
      }
    }
  }
}
JSON

capture null_score_levels <<'JSON'
{
  "state": "The evidence does not support any particular point on the scale.",
  "questions": {
    "position": {
      "type": "score",
      "instructions": "Place the state on the three-point scale. The levels have no extra descriptions.",
      "criteria": [null, null, null]
    }
  }
}
JSON

if ! jq -e '
  (.answers.tie.type == "choice") and
  ((.answers.tie.probabilities.candidate_alpha - .answers.tie.probabilities.candidate_beta) | fabs <= 0.01)
' "$out_dir/choice_tie.response.json" >/dev/null; then
  rm -f "$out_dir/choice_tie.request.json" "$out_dir/choice_tie.response.json"
  echo "other fixtures were captured, but the choice prompt did not produce a tie within 0.01" >&2
  exit 1
fi

echo "fixtures written to $out_dir"
