# Developer Tooling (`hack/`)

Local developer automation scripts managed with [`uv`](https://docs.astral.sh/uv/) (PEP 723 inline script metadata) and [`rich`](https://github.com/Textualize/rich), writing structured JSONL execution logs to `.hack-logs/`.

## Modules

- [`containers/`](containers/) — Build, launch, probe, and tear down the standalone Karakara OS runner + Web UI container (`podman` or `docker`). See [`operations/containers/README.md`](../operations/containers/README.md) for usage and macOS/Linux VM sizing.
  - [`containers/deploy.py`](containers/deploy.py) — Build/start container and verify `/healthz`, `/readyz`, `/agents`, `/openapi.json`, and `/` (Web UI).
  - [`containers/teardown.py`](containers/teardown.py) — Stop/remove container and optionally purge `/data` volume and image.

## Linting & Unit Tests

```bash
uv tool run ruff check hack/
uv tool run ruff format --check hack/
uv run --with rich python3 -B -m unittest discover -s hack -t . -p 'test_*.py' -v
```
