// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! system/internal_a2a tool: session-based A2A conversation call.

use std::{collections::VecDeque, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    A2aRequestHandler, A2aWireRequest, Result, context,
    ids::{TaskId, UuidId},
    is_history_infrastructure_notice,
};
use baml_rt_tools::{
    BundleName, ToolBundle, ToolBundleMetadata, ToolFailure, ToolHandler, ToolSession,
    ToolSessionError, ToolStep, opaque_json_map_from_object,
    tools::{
        HistoryContextSessionOp, HistoryContextStatus, HistoryContextV1, ToolFunctionMetadata,
        ToolSessionContext, validate_open_input,
    },
};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::{
    metadata::system_internal_a2a_metadata,
    tools::{
        ConversationChunk, ConversationMessage, ConversationPart, InternalA2aCompletion,
        InternalA2aNextOutput, InternalA2aOpenInput, InternalA2aSendInput, InternalA2aTarget,
    },
};

/// System bundle exposing the system/internal_a2a tool.
pub struct A2aSessionBundle {
    handler: Arc<dyn A2aRequestHandler>,
}

impl A2aSessionBundle {
    pub fn new(handler: Arc<dyn A2aRequestHandler>) -> Self {
        Self { handler }
    }
}

impl ToolBundle for A2aSessionBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = BundleName::new("system".to_string()).expect("system bundle name must be valid");
        ToolBundleMetadata {
            name,
            description: "System tools (e.g. agent-to-agent session)".to_string(),
            config_schema: None,
            secret_requests: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn ToolHandler>> {
        vec![Arc::new(A2aSessionToolHandler {
            handler: self.handler.clone(),
            metadata: system_internal_a2a_metadata(),
        })]
    }
}

struct A2aSessionToolHandler {
    handler: Arc<dyn A2aRequestHandler>,
    metadata: ToolFunctionMetadata,
}

#[async_trait]
impl ToolHandler for A2aSessionToolHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn describe_invocation(&self, content: &Value) -> String {
        let step = content.get("step").unwrap_or(content);
        let op = match step.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return "delegated agent: call".to_string(),
        };
        match op {
            "Open" => {
                if let Some(open) = step
                    .get("input")
                    .and_then(|v| serde_json::from_value::<InternalA2aOpenInput>(v.clone()).ok())
                {
                    format!("delegating to agent '{}'", open.target.agent_package)
                } else {
                    "delegating to agent".to_string()
                }
            }
            "Send" => {
                if let Some(send) = step
                    .get("input")
                    .and_then(|v| serde_json::from_value::<InternalA2aSendInput>(v.clone()).ok())
                {
                    let text = send.parts.first().and_then(|p| p.text.as_deref());
                    match text {
                        Some(t) if t.len() > 60 => {
                            format!("sending message to delegated agent: '{}...'", &t[..57])
                        }
                        Some(t) => format!("sending message to delegated agent: '{t}'"),
                        None => "sending message to delegated agent".to_string(),
                    }
                } else {
                    "sending message to delegated agent".to_string()
                }
            }
            "SearchRead" | "PageRead" => "reading delegated agent output".to_string(),
            "Finish" => "finished delegated agent session".to_string(),
            "Abort" => "aborted delegated agent session".to_string(),
            other => format!("delegated agent: {other}"),
        }
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        validate_open_input::<InternalA2aOpenInput>(open_input.clone())?;
        let open: InternalA2aOpenInput =
            serde_json::from_value(open_input).map_err(baml_rt_core::BamlRtError::Json)?;
        Ok(Box::new(A2aSession {
            ctx,
            handler: self.handler.clone(),
            target: open.target,
            child_task_id: TaskId::for_delegated_child(UuidId::new(uuid::Uuid::new_v4())),
            // One delegation identity per tool session: every send in this
            // session re-links the same typed lineage record.
            delegation_id: uuid::Uuid::new_v4().to_string(),
            queue: VecDeque::new(),
            output_rx: None,
            stream_handle: None,
            seen_output: false,
            empty_stream_notice_emitted: false,
            closed: false,
            read_hop: 0,
            accumulated_outputs: Vec::new(),
        }))
    }
}

struct A2aSession {
    ctx: ToolSessionContext,
    handler: Arc<dyn A2aRequestHandler>,
    target: InternalA2aTarget,
    /// Stable delegated child task id for this tool session; reused across send/resume turns.
    child_task_id: TaskId,
    /// Host-generated delegation identity recorded as typed provenance lineage.
    delegation_id: String,
    queue: VecDeque<InternalA2aNextOutput>,
    output_rx: Option<async_channel::Receiver<InternalA2aNextOutput>>,
    /// JoinHandle for the task that consumes the A2A stream. Aborted in Drop so the task
    /// does not outlive the session and trigger "context is being shutdown" panics.
    stream_handle: Option<JoinHandle<()>>,
    seen_output: bool,
    empty_stream_notice_emitted: bool,
    closed: bool,
    read_hop: u32,
    /// Accumulates all Streaming outputs across Read hops so the final Done hop can carry
    /// the full conversation payload into provenance.
    accumulated_outputs: Vec<InternalA2aNextOutput>,
}

fn parse_send_input(raw: Value) -> std::result::Result<Vec<ConversationPart>, String> {
    if raw.is_null() {
        return Err(
            "Invalid system/internal_a2a Send input: expected { parts: [{ text: '...' }] } — got null".to_string()
        );
    }
    // Accept legacy { text: "..." } shorthand and convert to parts.
    if let Some(text) = raw.get("text").and_then(|v| v.as_str())
        && !text.trim().is_empty()
    {
        return Ok(vec![ConversationPart {
            text: Some(text.to_string()),
            ..Default::default()
        }]);
    }
    match serde_json::from_value::<InternalA2aSendInput>(raw) {
        Ok(send) if !send.parts.is_empty() => Ok(send.parts),
        Ok(_) => Err("system/internal_a2a Send input.parts must not be empty".to_string()),
        Err(err) => Err(format!(
            "Invalid system/internal_a2a Send input: expected {{ parts: [{{ text: '...' }}] }} ({err})"
        )),
    }
}

fn build_send_stream_request(
    parts: Vec<ConversationPart>,
    target: &InternalA2aTarget,
    caller_context_id: &baml_rt_core::ids::ContextId,
    child_task_id: &TaskId,
    parent_task_id: Option<&TaskId>,
) -> Value {
    let reference_task_ids = parent_task_id
        .map(|task_id| vec![task_id.as_str().to_string()])
        .unwrap_or_default();
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "message.sendStream",
        "id": serde_json::Value::Null,
        "params": {
            "message": {
                "messageId": format!("system-a2a-{id}", id = uuid::Uuid::new_v4()),
                "role": "ROLE_USER",
                "parts": parts,
                "contextId": caller_context_id.as_str(),
                "taskId": child_task_id.as_str(),
                "referenceTaskIds": reference_task_ids
            },
            "metadata": {
                "kind": "agent-to-agent",
                "callerContextId": caller_context_id.as_str(),
                "target": {
                    "agent_package": target.agent_package
                }
            }
        }
    })
}

fn current_parent_task_id() -> Option<TaskId> {
    context::current_scope()
        .ok()
        .and_then(|scope| scope.task_id_opt().cloned())
}

fn extract_chunk_value(value: Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value;
    };

    if let Some(result) = obj.get("result").and_then(|v| v.as_object())
        && let Some(chunk) = result.get("chunk")
    {
        return chunk.clone();
    }

    if let Some(chunk) = obj.get("chunk") {
        return chunk.clone();
    }

    value
}

fn to_conversation_chunk(value: Value) -> ConversationChunk {
    let candidate = extract_chunk_value(value);
    if let Some(obj) = candidate.as_object() {
        let message = obj.get("message").and_then(|msg| {
            let role = msg
                .as_object()
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let parts = msg
                .as_object()
                .and_then(|m| m.get("parts"))
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| serde_json::from_value::<ConversationPart>(p.clone()).ok())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if parts.is_empty() {
                None
            } else {
                Some(ConversationMessage { role, parts })
            }
        });

        let task = obj.get("task").map(|v| v.to_string());
        let status_update = obj.get("statusUpdate").map(|v| v.to_string());
        let artifact_update = obj.get("artifactUpdate").map(|v| v.to_string());
        if message.is_some()
            || task.is_some()
            || status_update.is_some()
            || artifact_update.is_some()
        {
            return ConversationChunk {
                message,
                task,
                status_update,
                artifact_update,
            };
        }
    }

    ConversationChunk {
        message: Some(ConversationMessage {
            role: None,
            parts: vec![ConversationPart {
                text: Some(candidate.to_string()),
                ..Default::default()
            }],
        }),
        task: None,
        status_update: None,
        artifact_update: None,
    }
}

/// Detect a terminal A2A task state on a chunk, returning the mapped completion plus the
/// child-provided status message when one is present.
fn chunk_terminal_state(value: &Value) -> Option<(InternalA2aCompletion, Option<String>)> {
    let status = value
        .get("task")
        .and_then(|t| t.get("status"))
        .or_else(|| value.get("statusUpdate").and_then(|s| s.get("status")));
    let state = status
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let message = status
        .and_then(|s| s.get("message"))
        .and_then(|m| m.get("parts"))
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .find_map(|part| part.get("text").and_then(Value::as_str))
        })
        .filter(|text| !text.trim().is_empty())
        .map(ToOwned::to_owned);
    let completion = match state.as_deref() {
        Some("TASK_STATE_COMPLETED") => InternalA2aCompletion::Completed,
        Some("TASK_STATE_FAILED") => InternalA2aCompletion::Failed,
        Some("TASK_STATE_CANCELED") => InternalA2aCompletion::Canceled,
        Some("TASK_STATE_REJECTED") => InternalA2aCompletion::Rejected,
        Some("TASK_STATE_INPUT_REQUIRED") => InternalA2aCompletion::InputRequired,
        Some("TASK_STATE_AUTH_REQUIRED") => InternalA2aCompletion::AuthRequired,
        _ => return None,
    };
    Some((completion, message))
}

/// Returns true when a chunk carries actual conversational content from the agent.
///
/// Filters out infrastructure noise: task-state transitions, status updates, and
/// model/tool invocation notices ("Calling model: ...", "Invoking tool: ...").
/// Only chunks with a non-empty message part that isn't a system notice are kept.
fn is_conversational_chunk(chunk: &ConversationChunk) -> bool {
    let Some(ref message) = chunk.message else {
        return false;
    };
    message.parts.iter().any(|part| {
        part.text
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .filter(|t| !is_history_infrastructure_notice(t))
            .is_some()
            || part.data.is_some()
    })
}

/// Child-internal refs the delegated agent emitted in a conversational reply.
///
/// The caller's ref table is scoped to `(context_id, caller_agent_id)` and cannot resolve the
/// child's `#N` history refs or `@prefix/local` archive refs. Any such token in a child reply is a
/// handoff gap: preserve the text for audit, deny resolution, and log a bounded diagnostic telling
/// the child to materialize the referenced content in its reply instead of leaking an ambient ref.
fn leaked_child_refs(parts: &[ConversationPart]) -> Vec<String> {
    let mut found = std::collections::BTreeSet::new();
    for part in parts {
        if let Some(text) = part.text.as_deref() {
            found.extend(baml_rt_tools::citations::scan_wire_citations(text));
        }
    }
    found.into_iter().collect()
}

/// Log one bounded warning per chunk that leaks child-internal refs. Body text is never logged; only
/// the offending ref tokens (capped) and the stable identities needed to correlate the turn are.
fn warn_on_leaked_child_refs(
    context_id: &baml_rt_core::ids::ContextId,
    caller_agent_id: &baml_rt_core::ids::AgentId,
    child_task_id: &TaskId,
    target_name: &str,
    chunk: &ConversationChunk,
) {
    let Some(ref message) = chunk.message else {
        return;
    };
    let leaked = leaked_child_refs(&message.parts);
    if leaked.is_empty() {
        return;
    }
    const MAX_REFS_LOGGED: usize = 8;
    let shown = &leaked[..leaked.len().min(MAX_REFS_LOGGED)];
    tracing::warn!(
        context_id = %context_id.as_str(),
        caller_agent_id = %caller_agent_id.as_str(),
        child_task_id = %child_task_id.as_str(),
        target = %target_name,
        leaked_ref_count = leaked.len(),
        leaked_refs = ?shown,
        "system/internal_a2a child reply carried internal refs; preserved for audit but denied resolution — child must materialize referenced content in its reply"
    );
}

/// Priority for merging completions: failure states dominate, then user-action suspensions,
/// then normal completion.
fn terminal_priority(completion: &InternalA2aCompletion) -> u8 {
    match completion {
        InternalA2aCompletion::Failed
        | InternalA2aCompletion::Canceled
        | InternalA2aCompletion::Rejected => 3,
        InternalA2aCompletion::InputRequired | InternalA2aCompletion::AuthRequired => 2,
        InternalA2aCompletion::Completed | InternalA2aCompletion::Done => 1,
    }
}

fn merge_outputs(outputs: Vec<InternalA2aNextOutput>) -> InternalA2aNextOutput {
    let mut chunks = Vec::new();
    let mut completion: Option<InternalA2aCompletion> = None;
    for out in outputs {
        chunks.extend(out.chunks);
        if let Some(next) = out.completion {
            completion = Some(match &completion {
                Some(current) if terminal_priority(current) >= terminal_priority(&next) => {
                    current.clone()
                }
                _ => next,
            });
        }
    }
    InternalA2aNextOutput {
        chunks,
        completion,
        history_context: None,
    }
}

fn completion_failure_message(output: &InternalA2aNextOutput) -> String {
    for chunk in &output.chunks {
        if let Some(message) = &chunk.message {
            for part in &message.parts {
                if let Some(text) = &part.text {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                }
            }
        }
    }
    "system/internal_a2a stream failed".to_string()
}

#[async_trait]
impl ToolSession for A2aSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        if self.closed {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "A2A session {session_id} is closed",
                session_id = self.ctx.session_id
            ))));
        }
        let parts = parse_send_input(input)
            .map_err(|msg| ToolSessionError::Tool(ToolFailure::invalid_input(msg)))?;
        self.seen_output = false;
        self.empty_stream_notice_emitted = false;
        if let Some(rx) = &self.output_rx
            && rx.is_closed()
            && rx.is_empty()
        {
            self.output_rx = None;
        }
        if self.output_rx.is_some() {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                "system/internal_a2a session: send only valid once after open".to_string(),
            )));
        }
        if let Some(handle) = self.stream_handle.take()
            && !handle.is_finished()
        {
            handle.abort();
        }
        let parent_task_id = current_parent_task_id();
        let request = build_send_stream_request(
            parts,
            &self.target,
            &self.ctx.context_id,
            &self.child_task_id,
            parent_task_id.as_ref(),
        );
        // Typed lineage is written by the trusted router from host-known values.
        // Without a caller task scope there is no parent to link, so fall back to
        // plain dispatch rather than inventing one.
        let delegation = match parent_task_id {
            Some(caller_task_id) => Some(baml_rt_core::DelegationCallContext {
                delegation_id: self.delegation_id.clone(),
                tool_session_id: self.ctx.session_id.to_string(),
                caller_agent_id: self.ctx.agent_id.clone(),
                caller_context_id: self.ctx.context_id.clone(),
                caller_task_id: Some(caller_task_id),
                child_task_id: self.child_task_id.clone(),
                target_name: self.target.agent_package.clone(),
            }),
            None => None,
        };
        let handler = self.handler.clone();
        let (tx, rx) = async_channel::unbounded::<InternalA2aNextOutput>();
        self.output_rx = Some(rx);
        // Stable identities for the leaked-ref diagnostic, cloned before the spawned stream
        // consumer outlives the borrow of `self`.
        let caller_context_id = self.ctx.context_id.clone();
        let caller_agent_id = self.ctx.agent_id.clone();
        let child_task_id = self.child_task_id.clone();
        let target_name = self.target.agent_package.clone();
        let handle = tokio::spawn(async move {
            let result = match delegation {
                Some(delegation) => {
                    handler
                        .handle_delegated_a2a_stream(A2aWireRequest::from(request), delegation)
                        .await
                }
                None => {
                    handler
                        .handle_a2a_stream(A2aWireRequest::from(request))
                        .await
                }
            };
            match result {
                Ok(stream) => {
                    futures_util::pin_mut!(stream);
                    while let Some(response) = stream.next().await {
                        let value = response.into_inner();
                        let chunk = extract_chunk_value(value.clone());
                        let terminal = chunk_terminal_state(&chunk);
                        let completion =
                            terminal.as_ref().map(|(completion, _)| completion.clone());
                        let mut conversational = to_conversation_chunk(value);
                        if let Some((completion, message)) = &terminal
                            && matches!(
                                completion,
                                InternalA2aCompletion::Failed
                                    | InternalA2aCompletion::Canceled
                                    | InternalA2aCompletion::Rejected
                            )
                        {
                            // Surface the child's failure as the conversational text so it is not
                            // filtered out as an infra status update and never archived as "{}".
                            let text = message.clone().unwrap_or_else(|| {
                                format!("delegated agent ended with {completion:?}")
                            });
                            conversational.message = Some(ConversationMessage {
                                role: None,
                                parts: vec![ConversationPart {
                                    text: Some(text),
                                    ..Default::default()
                                }],
                            });
                        }
                        warn_on_leaked_child_refs(
                            &caller_context_id,
                            &caller_agent_id,
                            &child_task_id,
                            &target_name,
                            &conversational,
                        );
                        let out = InternalA2aNextOutput {
                            chunks: vec![conversational],
                            completion,
                            history_context: None,
                        };
                        if tx.send(out).await.is_err() {
                            break;
                        }
                        if terminal.is_some() {
                            break;
                        }
                    }
                }
                Err(err) => {
                    let fallback = InternalA2aNextOutput {
                        chunks: vec![ConversationChunk {
                            message: Some(ConversationMessage {
                                role: None,
                                parts: vec![ConversationPart {
                                    text: Some(format!(
                                        "system/internal_a2a execution failed: {err}"
                                    )),
                                    ..Default::default()
                                }],
                            }),
                            task: None,
                            status_update: None,
                            artifact_update: None,
                        }],
                        completion: Some(InternalA2aCompletion::Failed),
                        history_context: None,
                    };
                    if tx.send(fallback).await.is_err() {
                        tracing::warn!(
                            "system/internal_a2a failed to emit synthetic error chunk (receiver dropped)"
                        );
                    }
                }
            }
            tx.close();
        });
        self.stream_handle = Some(handle);
        Ok(())
    }

    async fn read(&mut self, _input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        if let Some(output) = self.queue.pop_front() {
            let mut batch = vec![output];
            while let Some(next) = self.queue.pop_front() {
                batch.push(next);
            }
            let merged = merge_outputs(batch);
            self.read_hop = self.read_hop.saturating_add(1);
            let mut merged = merged;
            merged.history_context = Some(HistoryContextV1 {
                hop: self.read_hop,
                op: HistoryContextSessionOp::PageRead,
                status: match merged.completion.as_ref() {
                    Some(
                        InternalA2aCompletion::InputRequired | InternalA2aCompletion::AuthRequired,
                    ) => HistoryContextStatus::Suspended,
                    Some(
                        InternalA2aCompletion::Failed
                        | InternalA2aCompletion::Canceled
                        | InternalA2aCompletion::Rejected,
                    ) => HistoryContextStatus::Error,
                    _ => HistoryContextStatus::Streaming,
                },
                truncated: false,
                cursor: None,
                payload: Some(opaque_json_map_from_object(serde_json::json!({
                    "chunkCount": merged.chunks.len(),
                    "completion": merged.completion.as_ref().map(|c| format!("{:?}", c)),
                }))),
            });
            let value = serde_json::to_value(&merged).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::execution_failed(format!(
                    "Invalid A2A output: {error}",
                    error = e
                )))
            })?;
            let step = if matches!(
                merged.completion.as_ref(),
                Some(InternalA2aCompletion::InputRequired | InternalA2aCompletion::AuthRequired)
            ) {
                self.output_rx = None;
                ToolStep::Suspended { output: value }
            } else if matches!(
                merged.completion.as_ref(),
                Some(
                    InternalA2aCompletion::Failed
                        | InternalA2aCompletion::Canceled
                        | InternalA2aCompletion::Rejected
                )
            ) {
                self.output_rx = None;
                ToolStep::Error {
                    error: ToolFailure::a2a_delegation_failed(completion_failure_message(&merged)),
                }
            } else {
                self.accumulated_outputs.push(merged.clone());
                ToolStep::Streaming { output: value }
            };
            self.seen_output = true;
            return Ok(step);
        }
        if let Some(rx) = &self.output_rx {
            match rx.recv().await {
                Ok(output) => {
                    let mut batch = vec![output];
                    while let Ok(next) = rx.try_recv() {
                        batch.push(next);
                    }
                    let merged = merge_outputs(batch);
                    self.read_hop = self.read_hop.saturating_add(1);
                    let mut merged = merged;
                    merged.history_context = Some(HistoryContextV1 {
                        hop: self.read_hop,
                        op: HistoryContextSessionOp::PageRead,
                        status: match merged.completion.as_ref() {
                            Some(
                                InternalA2aCompletion::InputRequired
                                | InternalA2aCompletion::AuthRequired,
                            ) => HistoryContextStatus::Suspended,
                            Some(
                                InternalA2aCompletion::Failed
                                | InternalA2aCompletion::Canceled
                                | InternalA2aCompletion::Rejected,
                            ) => HistoryContextStatus::Error,
                            _ => HistoryContextStatus::Streaming,
                        },
                        truncated: false,
                        cursor: None,
                        payload: Some(opaque_json_map_from_object(serde_json::json!({
                            "chunkCount": merged.chunks.len(),
                            "completion": merged.completion.as_ref().map(|c| format!("{:?}", c)),
                        }))),
                    });
                    let value = serde_json::to_value(&merged).map_err(|e| {
                        ToolSessionError::Tool(ToolFailure::execution_failed(format!(
                            "Invalid A2A output: {error}",
                            error = e
                        )))
                    })?;
                    let step = if matches!(
                        merged.completion.as_ref(),
                        Some(
                            InternalA2aCompletion::InputRequired
                                | InternalA2aCompletion::AuthRequired
                        )
                    ) {
                        self.output_rx = None;
                        ToolStep::Suspended { output: value }
                    } else if matches!(
                        merged.completion.as_ref(),
                        Some(
                            InternalA2aCompletion::Failed
                                | InternalA2aCompletion::Canceled
                                | InternalA2aCompletion::Rejected
                        )
                    ) {
                        self.output_rx = None;
                        ToolStep::Error {
                            error: ToolFailure::a2a_delegation_failed(completion_failure_message(
                                &merged,
                            )),
                        }
                    } else {
                        self.accumulated_outputs.push(merged.clone());
                        ToolStep::Streaming { output: value }
                    };
                    self.seen_output = true;
                    return Ok(step);
                }
                Err(_) => {
                    self.output_rx = None;
                    if !self.seen_output && !self.empty_stream_notice_emitted {
                        self.empty_stream_notice_emitted = true;
                        self.read_hop = self.read_hop.saturating_add(1);
                        let out = InternalA2aNextOutput {
                            chunks: vec![ConversationChunk {
                                message: Some(ConversationMessage {
                                    role: None,
                                    parts: vec![ConversationPart {
                                        text: Some(
                                            "system/internal_a2a stream ended without output"
                                                .to_string(),
                                        ),
                                        ..Default::default()
                                    }],
                                }),
                                task: None,
                                status_update: None,
                                artifact_update: None,
                            }],
                            completion: None,
                            history_context: Some(HistoryContextV1 {
                                hop: self.read_hop,
                                op: HistoryContextSessionOp::PageRead,
                                status: HistoryContextStatus::Streaming,
                                truncated: false,
                                cursor: None,
                                payload: Some(opaque_json_map_from_object(serde_json::json!({
                                    "chunkCount": 1,
                                    "completion": null
                                }))),
                            }),
                        };
                        let value = serde_json::to_value(&out).map_err(|e| {
                            ToolSessionError::Tool(ToolFailure::execution_failed(format!(
                                "Invalid A2A output: {error}",
                                error = e
                            )))
                        })?;
                        return Ok(ToolStep::Streaming { output: value });
                    }
                }
            }
        }
        // Carry the conversation payload into the Done hop for provenance.
        // Strip infrastructure noise (task state, model/tool invocation notices) so
        // only actual agent messages reach the tool_result archive.
        let final_output = if self.accumulated_outputs.is_empty() {
            None
        } else {
            let mut merged = merge_outputs(std::mem::take(&mut self.accumulated_outputs));
            merged.chunks.retain(is_conversational_chunk);
            if merged.chunks.is_empty() {
                None
            } else {
                serde_json::to_value(&merged).ok()
            }
        };
        Ok(ToolStep::Done {
            output: final_output,
        })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        Ok(())
    }
}

impl Drop for A2aSession {
    fn drop(&mut self) {
        if let Some(h) = self.stream_handle.take() {
            h.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::{ContextId, ExternalId};

    use super::*;

    fn responder_target() -> InternalA2aTarget {
        InternalA2aTarget {
            agent_package: "responder-agent".to_string(),
        }
    }

    #[test]
    fn leaked_child_refs_scans_history_and_archive_refs_without_body_text() {
        let parts = vec![
            ConversationPart {
                text: Some("see #7 and @2/5 for the task list".to_string()),
                ..Default::default()
            },
            ConversationPart {
                text: Some("plain text with no ref".to_string()),
                ..Default::default()
            },
        ];
        let refs = leaked_child_refs(&parts);
        assert_eq!(refs, vec!["#7".to_string(), "@2/5".to_string()]);

        let chunk = ConversationChunk {
            message: Some(ConversationMessage {
                role: None,
                parts: vec![ConversationPart {
                    text: Some("materialize the content, do not leak @3".to_string()),
                    ..Default::default()
                }],
            }),
            task: None,
            status_update: None,
            artifact_update: None,
        };
        let no_message = ConversationChunk {
            message: None,
            task: None,
            status_update: None,
            artifact_update: None,
        };
        // No panic and no body logging for status-only chunks.
        warn_on_leaked_child_refs(
            &ContextId::new(1, 1),
            &baml_rt_core::ids::AgentId::from_uuid(baml_rt_core::ids::UuidId::new(
                uuid::Uuid::new_v4(),
            )),
            &TaskId::from_external(ExternalId::new("child")),
            "clickup-agent",
            &chunk,
        );
        warn_on_leaked_child_refs(
            &ContextId::new(1, 1),
            &baml_rt_core::ids::AgentId::from_uuid(baml_rt_core::ids::UuidId::new(
                uuid::Uuid::new_v4(),
            )),
            &TaskId::from_external(ExternalId::new("child")),
            "clickup-agent",
            &no_message,
        );
    }

    #[test]
    fn conversation_chunk_preserves_structured_a2a_parts() {
        let chunk = to_conversation_chunk(serde_json::json!({
            "message": {
                "role": "ROLE_AGENT",
                "parts": [
                    { "text": "Three tasks found" },
                    {
                        "data": { "items": [{ "id": "task-1" }, { "id": "task-2" }, { "id": "task-3" }] },
                        "media_type": "application/json"
                    }
                ]
            }
        }));

        let message = chunk.message.expect("conversation message");
        assert_eq!(message.parts[0].text.as_deref(), Some("Three tasks found"));
        assert_eq!(
            message.parts[1]
                .data
                .as_ref()
                .expect("structured data part")
                .as_value(),
            &serde_json::json!({
                "items": [{ "id": "task-1" }, { "id": "task-2" }, { "id": "task-3" }]
            })
        );
        assert_eq!(
            message.parts[1].media_type.as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn build_send_stream_request_preserves_context_and_child_task_identity() {
        let target = responder_target();
        let caller_context_id = ContextId::new(1_700_000_000_000, 7);
        let parent_task_id = TaskId::from_external(ExternalId::new("parent-task".to_string()));
        let first_child = TaskId::from_external(ExternalId::new("a2a-child-first".to_string()));
        let second_child = TaskId::from_external(ExternalId::new("a2a-child-second".to_string()));
        let parts = vec![ConversationPart {
            text: Some("delegated query".to_string()),
            ..Default::default()
        }];

        let first = build_send_stream_request(
            parts.clone(),
            &target,
            &caller_context_id,
            &first_child,
            Some(&parent_task_id),
        );
        let second = build_send_stream_request(
            parts,
            &target,
            &caller_context_id,
            &second_child,
            Some(&parent_task_id),
        );

        for request in [&first, &second] {
            let wire_context = request
                .pointer("/params/message/contextId")
                .and_then(serde_json::Value::as_str)
                .expect("contextId on wire");
            assert_eq!(wire_context, caller_context_id.as_str());
            assert!(!wire_context.starts_with("a2a:"));
            assert_eq!(
                request
                    .pointer("/params/message/referenceTaskIds/0")
                    .and_then(serde_json::Value::as_str),
                Some(parent_task_id.as_str())
            );
        }
        assert_eq!(
            first
                .pointer("/params/message/taskId")
                .and_then(serde_json::Value::as_str),
            Some(first_child.as_str())
        );
        assert_eq!(
            second
                .pointer("/params/message/taskId")
                .and_then(serde_json::Value::as_str),
            Some(second_child.as_str())
        );
        assert_ne!(first_child, second_child);
    }

    #[test]
    fn chunk_terminal_state_maps_all_a2a_terminal_states() {
        let cases = [
            ("TASK_STATE_COMPLETED", InternalA2aCompletion::Completed),
            ("TASK_STATE_FAILED", InternalA2aCompletion::Failed),
            ("TASK_STATE_CANCELED", InternalA2aCompletion::Canceled),
            ("TASK_STATE_REJECTED", InternalA2aCompletion::Rejected),
            (
                "TASK_STATE_INPUT_REQUIRED",
                InternalA2aCompletion::InputRequired,
            ),
            (
                "TASK_STATE_AUTH_REQUIRED",
                InternalA2aCompletion::AuthRequired,
            ),
        ];
        for (state, expected) in cases {
            let chunk = serde_json::json!({
                "statusUpdate": {
                    "status": {
                        "state": state,
                        "message": { "parts": [{ "text": "child status message" }] }
                    }
                }
            });
            let (completion, message) = chunk_terminal_state(&chunk).expect("terminal state");
            assert_eq!(completion, expected);
            assert_eq!(message.as_deref(), Some("child status message"));
        }

        // Non-terminal states and non-status chunks are ignored.
        let working =
            serde_json::json!({ "statusUpdate": { "status": { "state": "TASK_STATE_WORKING" } } });
        assert!(chunk_terminal_state(&working).is_none());
        let missing = serde_json::json!({ "message": { "parts": [{ "text": "hi" }] } });
        assert!(chunk_terminal_state(&missing).is_none());
    }
}
