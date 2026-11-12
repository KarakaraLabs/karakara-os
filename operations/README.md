# Operations

This directory contains the first-party configuration used to deploy and operate Karakara.

- [`containers/`](containers/) contains first-party OCI container image definitions (`runner.Dockerfile` and `tool-runtime/` microVM guest images).
- [`deployment/`](deployment/) contains the parked/experimental Helm chart ([`deployment/helm/karakara-os/`](deployment/helm/karakara-os/)).
- [`observability/`](observability/) contains the local OpenTelemetry, Prometheus, Tempo, and Grafana stack.

Runtime implementation code remains under `crates/`. In particular, `crates/baml-rt-observability/` is application code rather than operator configuration.
