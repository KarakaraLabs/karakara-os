# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

"""Unit tests for hack/ container deploy, teardown, and logging helpers."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from hack import deploy, logger, runtime, teardown


class RuntimeTests(unittest.TestCase):
    def test_detect_runtime_prefers_podman_in_auto_mode(self) -> None:
        with mock.patch("shutil.which", side_effect=lambda name: f"/usr/bin/{name}"):
            self.assertEqual(runtime.detect_runtime("auto"), "podman")

    def test_detect_runtime_falls_back_to_docker(self) -> None:
        with mock.patch(
            "shutil.which",
            side_effect=lambda name: "/usr/bin/docker" if name == "docker" else None,
        ):
            self.assertEqual(runtime.detect_runtime("auto"), "docker")

    def test_detect_runtime_raises_when_missing(self) -> None:
        with (
            mock.patch("shutil.which", return_value=None),
            self.assertRaises(RuntimeError),
        ):
            runtime.detect_runtime("auto")

    def test_build_image_cmd_with_jobs(self) -> None:
        cfg = runtime.ContainerConfig(
            runtime="podman",
            repo_root=Path("/repo"),
            image="karakara-os:test",
            jobs=4,
        )
        self.assertEqual(
            runtime.build_image_cmd(cfg),
            [
                "podman",
                "build",
                "-f",
                "operations/containers/runner.Dockerfile",
                "-t",
                "karakara-os:test",
                "--build-arg",
                "CARGO_BUILD_JOBS=4",
                ".",
            ],
        )

    def test_build_image_cmd_rejects_non_positive_jobs(self) -> None:
        cfg = runtime.ContainerConfig(runtime="docker", repo_root=Path("/repo"), jobs=0)
        with self.assertRaises(ValueError):
            runtime.build_image_cmd(cfg)

    def test_run_container_cmd_binds_localhost_and_data_volume(self) -> None:
        cfg = runtime.ContainerConfig(
            runtime="docker",
            repo_root=Path("/repo"),
            port=19080,
            volume_name="custom-vol",
            env_file=Path(".env"),
        )
        self.assertEqual(
            runtime.run_container_cmd(cfg),
            [
                "docker",
                "run",
                "-d",
                "--name",
                "karakara-runner",
                "-p",
                "127.0.0.1:19080:18080",
                "-v",
                "custom-vol:/data",
                "--env-file",
                ".env",
                "karakara-os:local-test",
            ],
        )

    def test_wait_for_ready_retries_503_then_verifies_endpoints(self) -> None:
        calls: list[str] = []
        readyz_attempts = 0

        def fake_get(url: str) -> tuple[int, str]:
            nonlocal readyz_attempts
            calls.append(url)
            if url.endswith("/readyz"):
                readyz_attempts += 1
                if readyz_attempts == 1:
                    return 503, "Service Unavailable"
            return 200, ""

        results = runtime.wait_for_ready(
            "http://127.0.0.1:18080",
            timeout_secs=5,
            poll_interval_secs=0.0,
            getter=fake_get,
        )
        self.assertEqual(len(results), 4)
        self.assertTrue(all(item.ok and item.status == 200 for item in results))
        self.assertGreaterEqual(readyz_attempts, 2)

    def test_http_get_status_handles_connection_reset(self) -> None:
        with mock.patch(
            "urllib.request.urlopen", side_effect=ConnectionResetError(54, "reset")
        ):
            status, detail = runtime.http_get_status("http://127.0.0.1:18080/healthz")
        self.assertEqual(status, 0)
        self.assertIn("reset", detail)

    def test_wait_for_ready_raises_on_timeout(self) -> None:
        with self.assertRaises(TimeoutError):
            runtime.wait_for_ready(
                "http://127.0.0.1:18080",
                timeout_secs=0,
                poll_interval_secs=0.0,
                getter=lambda _url: (503, "starting"),
            )


class CliAndLoggerTests(unittest.TestCase):
    def test_deploy_dry_run_exits_zero(self) -> None:
        with mock.patch("hack.deploy.detect_runtime", return_value="podman"):
            code = deploy.main(["--dry-run", "--jobs", "4"])
        self.assertEqual(code, 0)

    def test_teardown_dry_run_exits_zero(self) -> None:
        with mock.patch("hack.teardown.detect_runtime", return_value="podman"):
            code = teardown.main(["--dry-run", "--purge-volume", "--purge-image"])
        self.assertEqual(code, 0)

    def test_log_event_writes_valid_jsonl(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            logs_dir = Path(tmp)
            logger.log_event(
                "deploy",
                "test_action",
                {"port": 18080},
                log_name="test.jsonl",
                logs_dir=logs_dir,
            )
            lines = (logs_dir / "test.jsonl").read_text(encoding="utf-8").splitlines()
            self.assertEqual(len(lines), 1)
            payload = json.loads(lines[0])
            self.assertEqual(payload["event_type"], "deploy")
            self.assertEqual(payload["action"], "test_action")
            self.assertEqual(payload["details"]["port"], 18080)


if __name__ == "__main__":
    unittest.main()
