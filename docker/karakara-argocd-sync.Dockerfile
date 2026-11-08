# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

# Ephemeral image: workspace copy of operations/deployment/helm + operations/deployment/values for `argocd app sync --local` inside a Job.
ARG ARGOCD_BASE=quay.io/argoproj/argocd
ARG ARGOCD_TAG=v2.13.3
FROM ${ARGOCD_BASE}:${ARGOCD_TAG}
WORKDIR /workspace
COPY operations/deployment/helm /workspace/operations/deployment/helm
COPY operations/deployment/values /workspace/operations/deployment/values
