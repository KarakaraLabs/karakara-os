# Operations

This directory contains the first-party configuration used to deploy and operate Karakara.

- [`containers/`](containers/) contains first-party OCI container image definitions.
- [`deployment/`](deployment/) contains the Helm chart, Argo CD configuration, k3d setup, values, and legacy Kubernetes manifests.
- [`observability/`](observability/) contains the local OpenTelemetry, Prometheus, Tempo, and Grafana stack.

Runtime implementation code remains under `crates/`. In particular, `crates/baml-rt-observability/` is application code rather than operator configuration.
