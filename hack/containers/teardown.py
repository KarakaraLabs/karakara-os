#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "rich>=13.9",
# ]
# ///
# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

"""Stop and clean up the local Karakara OS runner container, volume, and image."""

from __future__ import annotations

import argparse
import signal
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from hack.containers.logger import log_event
from hack.containers.runtime import (
    DEFAULT_CONTAINER_NAME,
    DEFAULT_IMAGE,
    DEFAULT_VOLUME_NAME,
    detect_runtime,
    remove_container_cmd,
    remove_image_cmd,
    remove_volume_cmd,
    stop_container_cmd,
)

try:
    from rich.console import Console
    from rich.table import Table

    console: Console | None = Console()
    HAS_RICH = True
except ImportError:
    console = None
    HAS_RICH = False


def print_msg(msg: str, style: str = "white") -> None:
    """Print a styled message via Rich when available, or plain stdout."""
    if HAS_RICH and console is not None:
        console.print(f"[{style}]{msg}[/{style}]")
    else:
        print(msg)


def _sigint_handler(_sig: int, _frame: Any) -> None:
    log_event("teardown", "interrupted", {"reason": "SIGINT"})
    print_msg("\n[teardown] Interrupted by user (Ctrl+C). Exiting cleanly.", "yellow")
    sys.exit(130)


signal.signal(signal.SIGINT, _sigint_handler)


def run_cleanup_step(
    label: str, cmd: list[str], dry_run: bool = False
) -> tuple[str, str]:
    """Run a cleanup command; tolerate already-absent resources."""
    if dry_run:
        print_msg(f"[dry-run] {' '.join(cmd)}", "dim")
        return label, "dry-run"

    proc = subprocess.run(
        cmd, cwd=REPO_ROOT, capture_output=True, text=True, check=False
    )
    if proc.returncode == 0:
        return label, "removed"
    stderr = (proc.stderr or "").strip()
    if "no such" in stderr.lower() or "not found" in stderr.lower():
        return label, "not present"
    return label, f"skipped ({stderr or f'exit {proc.returncode}'})"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """Parse CLI arguments for container teardown."""
    parser = argparse.ArgumentParser(
        description="Stop and remove the local Karakara OS runner container."
    )
    parser.add_argument(
        "--runtime",
        choices=("auto", "podman", "docker"),
        default="auto",
        help="Container runtime to use (default: auto-detect podman then docker).",
    )
    parser.add_argument(
        "--name",
        default=DEFAULT_CONTAINER_NAME,
        help=f"Container name (default: {DEFAULT_CONTAINER_NAME}).",
    )
    parser.add_argument(
        "--volume",
        default=DEFAULT_VOLUME_NAME,
        help=f"Named data volume (default: {DEFAULT_VOLUME_NAME}).",
    )
    parser.add_argument(
        "--image",
        default=DEFAULT_IMAGE,
        help=f"Image tag (default: {DEFAULT_IMAGE}).",
    )
    parser.add_argument(
        "--purge-volume",
        action="store_true",
        help="Also delete the persisted /data volume.",
    )
    parser.add_argument(
        "--purge-image",
        action="store_true",
        help="Also delete the built runner image.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print commands without executing them.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    """Execute container teardown and optional volume/image cleanup."""
    args = parse_args(argv)
    try:
        runtime = detect_runtime(args.runtime)
    except (RuntimeError, ValueError) as exc:
        print_msg(f"[teardown] {exc}", "bold red")
        return 1

    steps: list[tuple[str, list[str]]] = [
        (f"stop container ({args.name})", stop_container_cmd(runtime, args.name)),
        (f"remove container ({args.name})", remove_container_cmd(runtime, args.name)),
    ]
    if args.purge_volume:
        steps.append(
            (f"remove volume ({args.volume})", remove_volume_cmd(runtime, args.volume))
        )
    if args.purge_image:
        steps.append(
            (f"remove image ({args.image})", remove_image_cmd(runtime, args.image))
        )

    outcomes: list[tuple[str, str]] = []
    for label, cmd in steps:
        outcomes.append(run_cleanup_step(label, cmd, dry_run=args.dry_run))

    if HAS_RICH and console is not None:
        table = Table(title=f"Karakara Runner Teardown ({runtime})")
        table.add_column("Step", style="cyan")
        table.add_column("Outcome", style="bold")
        for label, status in outcomes:
            table.add_row(label, status)
        console.print(table)
    else:
        for label, status in outcomes:
            print(f"  {label}: {status}")

    log_event("teardown", "completed", {"runtime": runtime, "outcomes": outcomes})
    return 0


if __name__ == "__main__":
    sys.exit(main())
