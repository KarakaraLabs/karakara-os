# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

"""Shared container runtime helpers and HTTP probe checks for hack/ scripts."""

from __future__ import annotations

import http.client
import shutil
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

RuntimeKind = Literal["podman", "docker"]

DEFAULT_DOCKERFILE = Path("operations/containers/runner.Dockerfile")
DEFAULT_IMAGE = "karakara-os:local-test"
DEFAULT_CONTAINER_NAME = "karakara-runner"
DEFAULT_VOLUME_NAME = "karakara-runner-data"
DEFAULT_PORT = 18080


@dataclass(frozen=True)
class ContainerConfig:
    """Immutable configuration for building and running the runner container."""

    runtime: RuntimeKind
    repo_root: Path
    dockerfile: Path = DEFAULT_DOCKERFILE
    image: str = DEFAULT_IMAGE
    container_name: str = DEFAULT_CONTAINER_NAME
    volume_name: str = DEFAULT_VOLUME_NAME
    port: int = DEFAULT_PORT
    jobs: int | None = None
    env_file: Path | None = None


@dataclass(frozen=True)
class ProbeResult:
    """Result of an HTTP endpoint check."""

    path: str
    status: int
    ok: bool
    detail: str = ""


def detect_runtime(preferred: str = "auto") -> RuntimeKind:
    """Resolve container runtime binary (`podman` or `docker`)."""
    if preferred in ("podman", "docker"):
        if shutil.which(preferred) is None:
            raise RuntimeError(
                f"Requested runtime '{preferred}' was not found in PATH."
            )
        return preferred  # type: ignore[return-value]

    if preferred != "auto":
        raise ValueError(
            f"Unsupported runtime '{preferred}'. Expected auto, podman, or docker."
        )

    for candidate in ("podman", "docker"):
        if shutil.which(candidate) is not None:
            return candidate  # type: ignore[return-value]

    raise RuntimeError("Neither 'podman' nor 'docker' was found in PATH.")


def build_image_cmd(cfg: ContainerConfig) -> list[str]:
    """Build the container image build command."""
    cmd = [
        cfg.runtime,
        "build",
        "-f",
        str(cfg.dockerfile),
        "-t",
        cfg.image,
    ]
    if cfg.jobs is not None:
        if cfg.jobs <= 0:
            raise ValueError("jobs must be a positive integer")
        cmd.extend(["--build-arg", f"CARGO_BUILD_JOBS={cfg.jobs}"])
    cmd.append(".")
    return cmd


def run_container_cmd(cfg: ContainerConfig) -> list[str]:
    """Build the detached container run command."""
    cmd = [
        cfg.runtime,
        "run",
        "-d",
        "--name",
        cfg.container_name,
        "-p",
        f"127.0.0.1:{cfg.port}:18080",
        "-v",
        f"{cfg.volume_name}:/data",
    ]
    if cfg.env_file is not None:
        cmd.extend(["--env-file", str(cfg.env_file)])
    cmd.append(cfg.image)
    return cmd


def stop_container_cmd(runtime: RuntimeKind, container_name: str) -> list[str]:
    """Build the container stop command."""
    return [runtime, "stop", container_name]


def remove_container_cmd(runtime: RuntimeKind, container_name: str) -> list[str]:
    """Build the container remove command."""
    return [runtime, "rm", "-f", container_name]


def remove_volume_cmd(runtime: RuntimeKind, volume_name: str) -> list[str]:
    """Build the volume remove command."""
    return [runtime, "volume", "rm", "-f", volume_name]


def remove_image_cmd(runtime: RuntimeKind, image: str) -> list[str]:
    """Build the image remove command."""
    return [runtime, "rmi", "-f", image]


def http_get_status(url: str, timeout_secs: float = 2.0) -> tuple[int, str]:
    """Perform an HTTP GET and return `(status_code, error_detail)`."""
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout_secs) as resp:
            return int(resp.status), ""
    except urllib.error.HTTPError as exc:
        return int(exc.code), str(exc.reason)
    except (urllib.error.URLError, http.client.HTTPException, OSError) as exc:
        return 0, str(exc)


def wait_for_ready(
    base_url: str,
    timeout_secs: int = 60,
    poll_interval_secs: float = 1.0,
    getter: Callable[[str], tuple[int, str]] = http_get_status,
) -> list[ProbeResult]:
    """Poll `/healthz` and `/readyz` until both return 200, then verify API endpoints."""
    deadline = time.monotonic() + timeout_secs
    last_readyz = (0, "not checked")

    while time.monotonic() < deadline:
        health_code, _health_err = getter(f"{base_url}/healthz")
        ready_code, ready_err = getter(f"{base_url}/readyz")
        last_readyz = (ready_code, ready_err)
        if health_code == 200 and ready_code == 200:
            break
        time.sleep(poll_interval_secs)
    else:
        raise TimeoutError(
            f"Runner at {base_url} did not become ready within {timeout_secs}s "
            f"(last /readyz status={last_readyz[0]} detail={last_readyz[1]})"
        )

    results: list[ProbeResult] = []
    for path in ("/healthz", "/readyz", "/agents", "/openapi.json"):
        code, detail = getter(f"{base_url}{path}")
        results.append(
            ProbeResult(path=path, status=code, ok=(code == 200), detail=detail)
        )
    return results
