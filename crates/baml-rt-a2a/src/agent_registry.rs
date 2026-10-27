// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Internal agent routing surface: listing and dispatching A2A by route key.
//! Consumed by the HTTP API surface (baml-rt-api); implemented by the runner.

use async_trait::async_trait;
use baml_rt_core::{
    A2aStreamChunk, A2aWireRequest, AgentDispatchAck, AgentDispatchRequest, AgentLister,
    AgentRouteKey, BusStream, Result,
};

/// Registry of running agents: list and dispatch A2A by strict route key.
/// Implemented by the runner; consumed by the HTTP API surface.
#[async_trait]
pub trait AgentRegistry: AgentLister + Send + Sync {
    /// Resolve an agent by route key and handle an A2A JSON-RPC request.
    /// Returns 404-equivalent when the key is unknown (caller maps to HTTP 404).
    async fn handle_a2a_stream(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>>;

    /// Handle A2A with an optional exact-identity precondition.
    ///
    /// Implementations that can resolve the routed booted agent must reject a
    /// mismatch of either identifier before creating a task or emitting
    /// execution provenance. The agent identity and the artifact hash are
    /// separate claims: two artifacts can share one `AgentId`, so an ID-only
    /// check would accept a runner carrying different bytes.
    async fn handle_a2a_stream_with_precondition(
        &self,
        key: &AgentRouteKey,
        request: A2aWireRequest,
        _expected_agent_id: Option<&baml_rt_core::AgentId>,
        _expected_artifact_hash: Option<&baml_rt_core::ArtifactHash>,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.handle_a2a_stream(key, request).await
    }

    /// Resolve an agent by route key and deliver a deterministic non-conversational dispatch.
    async fn handle_dispatch(
        &self,
        key: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck>;
}
