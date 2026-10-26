// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use futures_util::StreamExt;

use crate::{
    Result,
    a2a_wire::{A2aStreamChunk, A2aWireRequest},
    bus::BusStream,
    deferred::DeferredHolder,
    ids::{AgentId, ContextId, TaskId},
};

/// Host-only delegation envelope for `system/internal_a2a` dispatch.
///
/// Every field is known to the in-process host before dispatch: the running
/// caller's stable identity and scope, the child task the caller minted for the
/// tool session, and the session id that owns the request/reply exchange. It is
/// deliberately not derived from the A2A request body, so a caller cannot
/// manufacture lineage by varying `params.metadata.kind`, `referenceTaskIds`, or
/// an `a2a:` context prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationCallContext {
    /// Host-generated identifier for this delegated tool session.
    pub delegation_id: String,
    pub tool_session_id: String,
    pub caller_agent_id: AgentId,
    pub caller_context_id: ContextId,
    /// Caller task that opened the delegation. Absent only when the caller runs
    /// outside a task scope.
    pub caller_task_id: Option<TaskId>,
    pub child_task_id: TaskId,
    /// Package-name snapshot of the delegated target, resolved by the host.
    pub target_name: String,
}

/// Shared A2A request handling abstraction used by both transport and tools.
#[async_trait]
pub trait A2aRequestHandler: Send + Sync {
    /// Canonical streaming entrypoint for A2A handling.
    async fn handle_a2a_stream(&self, request: A2aWireRequest)
    -> Result<BusStream<A2aStreamChunk>>;

    /// Delegated `system/internal_a2a` entrypoint carrying the host-only
    /// [`DelegationCallContext`].
    ///
    /// Handlers that do not record delegation lineage fall back to plain
    /// dispatch, which keeps in-memory test hosts working unchanged.
    async fn handle_delegated_a2a_stream(
        &self,
        request: A2aWireRequest,
        _delegation: DelegationCallContext,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.handle_a2a_stream(request).await
    }
}

#[async_trait]
impl A2aRequestHandler for DeferredHolder<dyn A2aRequestHandler> {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.get()?.handle_a2a_stream(request).await
    }
}

/// Marker sub-trait: full A2A **chat parity** (HTTP-equivalent QuickJS wiring).
///
/// Only types that always build a [`baml_rt_quickjs::QuickJSBridge`] through
/// [`baml_rt_quickjs::QuickJSBridge::register_baml_functions`] should implement this.
/// That path verifies [`baml_rt_quickjs::a2a_chat_surface::A2A_CHAT_HOST_GLOBALS`] at the end
/// of registration, so the machine-spirit cannot drift silently.
///
/// Use this trait as a **type constraint** for in-memory test clients when
/// execution-session + step-executor behaviour must match the HTTP host — not as a dynamic
/// runtime probe (QuickJS eval futures are not `Send`-safe across `async_trait` boundaries).
pub trait A2aJsChatHost: A2aRequestHandler {}

#[async_trait]
impl A2aRequestHandler for DeferredHolder<dyn A2aJsChatHost> {
    async fn handle_a2a_stream(
        &self,
        request: A2aWireRequest,
    ) -> Result<BusStream<A2aStreamChunk>> {
        self.get()?.handle_a2a_stream(request).await
    }
}

impl A2aJsChatHost for DeferredHolder<dyn A2aJsChatHost> {}

/// Explicit one-shot/test boundary: collect a stream into memory.
///
/// Use only where buffering is the contract (e.g. stdio compatibility or tests). Live HTTP/SSE
/// forwarding must preserve the stream and must not call this helper.
pub async fn collect_a2a_stream_one_shot(
    mut stream: BusStream<A2aStreamChunk>,
) -> Vec<A2aStreamChunk> {
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        out.push(item);
    }
    out
}

/// Explicit one-shot/test boundary: collect until a predicate matches.
pub async fn collect_a2a_stream_until_one_shot<F>(
    mut stream: BusStream<A2aStreamChunk>,
    mut should_stop: F,
) -> Vec<A2aStreamChunk>
where
    F: FnMut(&A2aStreamChunk) -> bool,
{
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        let stop = should_stop(&item);
        out.push(item);
        if stop {
            break;
        }
    }
    out
}
