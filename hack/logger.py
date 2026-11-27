# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

"""Structured JSON Lines (JSONL) logger for hack/ container tooling."""

from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
LOGS_DIR = REPO_ROOT / ".hack-logs"


def init_logs_dir(logs_dir: Path = LOGS_DIR) -> Path:
    """Ensure the log directory exists and return its path."""
    logs_dir.mkdir(parents=True, exist_ok=True)
    return logs_dir


def log_event(
    event_type: str,
    action: str,
    details: dict[str, Any],
    log_name: str = "containers.jsonl",
    logs_dir: Path = LOGS_DIR,
) -> None:
    """Append a structured JSONL entry to `.hack-logs/<log_name>`."""
    try:
        target_dir = init_logs_dir(logs_dir)
        log_file = target_dir / log_name
        entry = {
            "timestamp": datetime.now(UTC).isoformat(),
            "event_type": event_type,
            "action": action,
            "details": details,
        }
        with log_file.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry) + "\n")
    except OSError:
        # Logging should never crash core tooling operations.
        pass
