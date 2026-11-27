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

"""Build, start, and validate the Karakara OS runner container (Podman / Docker)."""

from __future__ import annotations

import argparse
import signal
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from hack.logger import log_event
from hack.runtime import (
    DEFAULT_CONTAINER_NAME,
    DEFAULT_IMAGE,
    DEFAULT_PORT,
    DEFAULT_VOLUME_NAME,
    ContainerConfig,
    ProbeResult,
    build_image_cmd,
    detect_runtime,
    remove_container_cmd,
    run_container_cmd,
    wait_for_ready,
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
    log_event("deploy", "interrupted", {"reason": "SIGINT"})
    print_msg("\n[deploy] Interrupted by user (Ctrl+C). Exiting cleanly.", "yellow")
    sys.exit(130)


signal.signal(signal.SIGINT, _sigint_handler)


def render_probe_table(results: list[ProbeResult], base_url: str) -> None:
    """Display endpoint verification results in a Rich table or plain text."""
    if HAS_RICH and console is not None:
        table = Table(title=f"Karakara Runner Verification ({base_url})")
        table.add_column("Endpoint", style="cyan", no_wrap=True)
        table.add_column("HTTP Status", justify="right")
        table.add_column("Result", style="bold")
        for item in results:
            status_str = str(item.status) if item.status else "ERR"
            badge = (
                "[green]PASS[/green]" if item.ok else f"[red]FAIL ({item.detail})[/red]"
            )
            table.add_row(item.path, status_str, badge)
        console.print(table)
        return

    for item in results:
        badge = "PASS" if item.ok else f"FAIL ({item.detail})"
        print(f"  {item.path:15s} -> {item.status} {badge}")


def run_cmd(cmd: list[str], cwd: Path, dry_run: bool = False) -> None:
    """Execute a subprocess command without shell interpolation."""
    if dry_run:
        print_msg(f"[dry-run] {' '.join(cmd)}", "dim")
        return
    subprocess.run(cmd, cwd=cwd, check=True)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """Parse CLI arguments for container deployment."""
    parser = argparse.ArgumentParser(
        description="Build, start, and verify the Karakara OS runner container."
    )
    parser.add_argument(
        "--runtime",
        choices=("auto", "podman", "docker"),
        default="auto",
        help="Container runtime to use (default: auto-detect podman then docker).",
    )
    parser.add_argument(
        "--image", default=DEFAULT_IMAGE, help=f"Image tag (default: {DEFAULT_IMAGE})."
    )
    parser.add_argument(
        "--name",
        default=DEFAULT_CONTAINER_NAME,
        help=f"Container name (default: {DEFAULT_CONTAINER_NAME}).",
    )
    parser.add_argument(
        "--volume",
        default=DEFAULT_VOLUME_NAME,
        help=f"Named data volume mounted at /data (default: {DEFAULT_VOLUME_NAME}).",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=DEFAULT_PORT,
        help=f"Host port mapped to container 18080 (default: {DEFAULT_PORT}).",
    )
    parser.add_argument(
        "--jobs",
        type=int,
        default=None,
        help="Optional CARGO_BUILD_JOBS cap for memory-constrained VMs.",
    )
    parser.add_argument(
        "--env-file",
        type=Path,
        default=None,
        help="Optional .env file passed to container run.",
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="Skip container image build and start the existing image.",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=60,
        help="Seconds to wait for /healthz and /readyz (default: 60).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print commands without executing them.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    """Run container build, launch, and HTTP verification."""
    args = parse_args(argv)
    try:
        runtime = detect_runtime(args.runtime)
    except (RuntimeError, ValueError) as exc:
        print_msg(f"[deploy] {exc}", "bold red")
        return 1

    if args.env_file is not None and not args.env_file.exists():
        print_msg(f"[deploy] Env file not found: {args.env_file}", "bold red")
        return 1

    cfg = ContainerConfig(
        runtime=runtime,
        repo_root=REPO_ROOT,
        image=args.image,
        container_name=args.name,
        volume_name=args.volume,
        port=args.port,
        jobs=args.jobs,
        env_file=args.env_file,
    )

    log_event(
        "deploy",
        "start",
        {
            "runtime": runtime,
            "image": cfg.image,
            "container": cfg.container_name,
            "port": cfg.port,
            "skip_build": args.skip_build,
            "dry_run": args.dry_run,
        },
    )
    print_msg(
        f"[deploy] Using runtime={runtime} image={cfg.image} port={cfg.port}",
        "bold cyan",
    )

    try:
        if not args.skip_build:
            print_msg("[deploy] Building runner image...", "cyan")
            run_cmd(build_image_cmd(cfg), cwd=REPO_ROOT, dry_run=args.dry_run)

        # Replace any existing container with the same name so re-runs are idempotent.
        if args.dry_run:
            run_cmd(
                remove_container_cmd(runtime, cfg.container_name),
                cwd=REPO_ROOT,
                dry_run=True,
            )
        else:
            subprocess.run(
                remove_container_cmd(runtime, cfg.container_name),
                cwd=REPO_ROOT,
                capture_output=True,
                check=False,
            )

        print_msg(f"[deploy] Starting container '{cfg.container_name}'...", "cyan")
        run_cmd(run_container_cmd(cfg), cwd=REPO_ROOT, dry_run=args.dry_run)

        if args.dry_run:
            print_msg("[deploy] Dry run complete.", "green")
            return 0

        base_url = f"http://127.0.0.1:{cfg.port}"
        if HAS_RICH and console is not None:
            with console.status(
                f"[cyan]Waiting for runner probes at {base_url}...[/cyan]"
            ):
                results = wait_for_ready(base_url, timeout_secs=args.timeout)
        else:
            print_msg(f"[deploy] Waiting for runner probes at {base_url}...", "cyan")
            results = wait_for_ready(base_url, timeout_secs=args.timeout)

        render_probe_table(results, base_url)
        all_ok = all(item.ok for item in results)
        log_event(
            "deploy",
            "verified" if all_ok else "probe_failed",
            {"base_url": base_url, "results": [item.__dict__ for item in results]},
        )
        if not all_ok:
            print_msg("[deploy] One or more HTTP checks failed.", "bold red")
            return 1

        print_msg(f"[deploy] Runner is live at {base_url}", "bold green")
        return 0
    except (subprocess.CalledProcessError, TimeoutError, ValueError) as exc:
        log_event("deploy", "error", {"error": str(exc)})
        print_msg(f"[deploy] Failed: {exc}", "bold red")
        return 1


if __name__ == "__main__":
    sys.exit(main())
