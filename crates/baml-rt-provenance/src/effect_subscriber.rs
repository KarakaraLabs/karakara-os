// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Provenance subscriber: converts EffectEvent to ProvEvent.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_core::{
    bus::{EffectEvent, EffectSubscriber, EffectSubscriberTier},
    ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
    llm_text::{extract_response_text, llm_completion_should_materialize_assistant_message},
};
use baml_rt_observability::metrics::{self, LlmCallMetrics};
use serde_json::Value;

use crate::{
    events::{
        BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR, CallScope, LlmUsage, PlanStepSpec, ProvEvent,
        ProvEventData,
    },
    store::ProvenanceWriter,
};

/// Event type for provenance event construction
#[derive(Debug, Clone, Copy)]
enum ProvenanceEventType {
    ToolCall,
    LlmCall,
}

impl ProvenanceEventType {
    fn as_str(self) -> &'static str {
        match self {
            ProvenanceEventType::ToolCall => "Tool call",
            ProvenanceEventType::LlmCall => "LLM call",
        }
    }
}

/// Helper to build provenance events with task/global branching.
fn build_prov_event<F, G>(
    context_id: &ContextId,
    metadata: &Value,
    event_type: ProvenanceEventType,
    build_task: F,
    build_global: G,
) -> baml_rt_core::Result<ProvEvent>
where
    F: FnOnce(ContextId, TaskId) -> ProvEvent,
    G: FnOnce(ContextId, MessageId) -> ProvEvent,
{
    Ok(match resolve_call_scope(context_id, metadata, event_type) {
        CallScopeResolution::Task(task_id) => build_task(context_id.clone(), task_id),
        CallScopeResolution::Message(message_id) => build_global(context_id.clone(), message_id),
    })
}

/// Helper for completion events — always emits, using a synthetic message_id
/// fallback when both task_id and message_id are absent from metadata.
fn build_prov_event_completion<F, G>(
    context_id: &ContextId,
    metadata: &Value,
    event_type: ProvenanceEventType,
    build_task: F,
    build_global: G,
) -> Option<ProvEvent>
where
    F: FnOnce(ContextId, TaskId) -> ProvEvent,
    G: FnOnce(ContextId, MessageId) -> ProvEvent,
{
    Some(match resolve_call_scope(context_id, metadata, event_type) {
        CallScopeResolution::Task(task_id) => build_task(context_id.clone(), task_id),
        CallScopeResolution::Message(message_id) => build_global(context_id.clone(), message_id),
    })
}

enum CallScopeResolution {
    Task(TaskId),
    Message(MessageId),
}

fn synthetic_context_message_id(context_id: &ContextId) -> MessageId {
    MessageId::from_external(ExternalId::new(format!("ctx-msg:{}", context_id.as_str())))
}

fn resolve_call_scope(
    context_id: &ContextId,
    metadata: &Value,
    event_type: ProvenanceEventType,
) -> CallScopeResolution {
    if let Some(task_id) = task_id_from_metadata(metadata) {
        return CallScopeResolution::Task(task_id);
    }
    let mut message_id = message_id_from_metadata(metadata);
    if message_id.is_none() {
        tracing::error!(
            event_type = event_type.as_str(),
            context_id = %context_id,
            "effect metadata missing both task_id and message_id; using synthetic fallback"
        );
        message_id = Some(synthetic_context_message_id(context_id));
    }
    CallScopeResolution::Message(message_id.expect("synthetic fallback always set"))
}

/// Adapter that subscribes to effect events and emits provenance events.
pub type ActionDescriber = dyn Fn(Option<&str>, &serde_json::Value) -> Option<String> + Send + Sync;

pub struct ProvenanceEffectSubscriber {
    writer: Arc<dyn ProvenanceWriter>,
    action_describer: Option<Arc<ActionDescriber>>,
    archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
}

impl ProvenanceEffectSubscriber {
    pub fn new(writer: Arc<dyn ProvenanceWriter>) -> Self {
        Self {
            writer,
            action_describer: None,
            archive_ref_tables: None,
        }
    }

    pub fn set_action_describer(&mut self, describer: Arc<ActionDescriber>) {
        self.action_describer = Some(describer);
    }

    pub fn set_archive_ref_tables(
        &mut self,
        tables: Arc<baml_rt_tools::archive_refs::ContextRefTables>,
    ) {
        self.archive_ref_tables = Some(tables);
    }

    fn invalidate_context_ref_cache(&self, context_id: &ContextId, agent_id: Option<&AgentId>) {
        if let Some(tables) = &self.archive_ref_tables {
            match agent_id {
                Some(agent) => {
                    baml_rt_tools::archive_refs::invalidate_ref_table(
                        tables,
                        context_id.as_str(),
                        agent.as_str(),
                    );
                }
                None => {
                    baml_rt_tools::archive_refs::invalidate_ref_tables_for_context(
                        tables,
                        context_id.as_str(),
                    );
                }
            }
        }
    }
}

#[async_trait]
impl EffectSubscriber for ProvenanceEffectSubscriber {
    fn name(&self) -> &'static str {
        "provenance"
    }

    /// Awaited because the next `conversation_context` read structurally
    /// depends on this subscriber's Surreal write being committed.
    fn tier(&self) -> EffectSubscriberTier {
        EffectSubscriberTier::Awaitable
    }

    async fn on_effect(&self, event: &EffectEvent) -> baml_rt_core::Result<()> {
        let prov_event = match event {
            EffectEvent::ToolStarted {
                context_id,
                metadata,
            } => build_prov_event(
                context_id,
                &metadata.metadata,
                ProvenanceEventType::ToolCall,
                |ctx_id, task_id| {
                    ProvEvent::tool_call_started_task(
                        ctx_id,
                        task_id,
                        metadata.tool_name.clone(),
                        metadata.function_name.clone(),
                        metadata.args.clone(),
                        metadata.metadata.clone(),
                        metadata.delegation_target.clone(),
                    )
                    .with_tool_backend_digest(
                        metadata.tool_backend.clone(),
                        metadata.tool_digest.clone(),
                    )
                },
                |ctx_id, msg_id| {
                    ProvEvent::tool_call_started_global(
                        ctx_id,
                        msg_id,
                        metadata.tool_name.clone(),
                        metadata.function_name.clone(),
                        metadata.args.clone(),
                        metadata.metadata.clone(),
                        metadata.delegation_target.clone(),
                    )
                    .with_tool_backend_digest(
                        metadata.tool_backend.clone(),
                        metadata.tool_digest.clone(),
                    )
                },
            )?,
            EffectEvent::ToolCompleted {
                context_id,
                metadata,
                duration_ms,
                outcome,
                result,
            } => {
                // Merge the result (if any) into metadata so the provenance store can write
                // it to the tool_result payload. Reserved anchor (if present) is consumed for
                // event-id assignment and removed from persisted metadata.
                let mut map = match &metadata.metadata {
                    serde_json::Value::Object(m) => m.clone(),
                    _ => serde_json::Map::new(),
                };
                let reserved_anchor = map
                    .get(BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ActivityAnchorId::from);
                map.remove(BAML_PROV_RESERVED_TOOL_COMPLETION_ANCHOR);
                if let Some(result_value) = result {
                    map.insert("result".to_string(), result_value.clone());
                }
                let enriched_metadata = serde_json::Value::Object(map);
                let event = match build_prov_event_completion(
                    context_id,
                    &enriched_metadata,
                    ProvenanceEventType::ToolCall,
                    |ctx_id, task_id| {
                        let event = if let Some(id) = reserved_anchor.clone() {
                            ProvEvent::tool_call_completed_task_with_id(
                                id,
                                ctx_id,
                                task_id,
                                metadata.tool_name.clone(),
                                metadata.function_name.clone(),
                                metadata.args.clone(),
                                enriched_metadata.clone(),
                                *duration_ms,
                                *outcome,
                                metadata.delegation_target.clone(),
                            )
                        } else {
                            ProvEvent::tool_call_completed_task(
                                ctx_id,
                                task_id,
                                metadata.tool_name.clone(),
                                metadata.function_name.clone(),
                                metadata.args.clone(),
                                enriched_metadata.clone(),
                                *duration_ms,
                                *outcome,
                                metadata.delegation_target.clone(),
                            )
                        };
                        event.with_tool_backend_digest(
                            metadata.tool_backend.clone(),
                            metadata.tool_digest.clone(),
                        )
                    },
                    |ctx_id, msg_id| {
                        let event = if let Some(id) = reserved_anchor.clone() {
                            ProvEvent::tool_call_completed_global_with_id(
                                id,
                                ctx_id,
                                msg_id,
                                metadata.tool_name.clone(),
                                metadata.function_name.clone(),
                                metadata.args.clone(),
                                enriched_metadata.clone(),
                                *duration_ms,
                                *outcome,
                                metadata.delegation_target.clone(),
                            )
                        } else {
                            ProvEvent::tool_call_completed_global(
                                ctx_id,
                                msg_id,
                                metadata.tool_name.clone(),
                                metadata.function_name.clone(),
                                metadata.args.clone(),
                                enriched_metadata.clone(),
                                *duration_ms,
                                *outcome,
                                metadata.delegation_target.clone(),
                            )
                        };
                        event.with_tool_backend_digest(
                            metadata.tool_backend.clone(),
                            metadata.tool_digest.clone(),
                        )
                    },
                ) {
                    Some(event) => event,
                    None => return Ok(()), // Skip on missing message_id
                };
                tracing::debug!(
                    event = "provenance_emit",
                    source = "effect_subscriber.tool_completion",
                    prov_event_id = %event.id(),
                    tool_name = %metadata.tool_name,
                    function_name = ?metadata.function_name,
                    context_id = %context_id,
                    task_id = ?task_id_from_metadata(&metadata.metadata),
                    "Emitting tool completion provenance event from effect-subscriber path"
                );
                event
            }
            EffectEvent::LlmStarted {
                context_id,
                metadata,
            } => {
                let prompt = normalized_prompt(&metadata.prompt);
                build_prov_event(
                    context_id,
                    &metadata.metadata,
                    ProvenanceEventType::LlmCall,
                    |ctx_id, task_id| {
                        ProvEvent::llm_call_started_task(
                            ctx_id,
                            task_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            metadata.metadata.clone(),
                        )
                    },
                    |ctx_id, msg_id| {
                        ProvEvent::llm_call_started_global(
                            ctx_id,
                            msg_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            metadata.metadata.clone(),
                        )
                    },
                )?
            }
            EffectEvent::LlmCompleted {
                context_id,
                metadata,
                usage,
                result_payload,
                duration_ms,
                outcome,
                rejection_reason,
            } => {
                let prov_usage = match usage {
                    Some(baml_rt_core::bus::LlmUsage::Known {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        cached_input_tokens,
                    }) => LlmUsage::Known {
                        prompt_tokens: *prompt_tokens,
                        completion_tokens: *completion_tokens,
                        total_tokens: *total_tokens,
                        cached_input_tokens: *cached_input_tokens,
                    },
                    Some(baml_rt_core::bus::LlmUsage::Unknown) | None => LlmUsage::Unknown,
                };
                let prov_usage_clone = prov_usage.clone();
                let prompt = normalized_prompt(&metadata.prompt);
                let task_id = task_id_from_metadata(&metadata.metadata);
                let result_label = if bool::from(*outcome) {
                    "success"
                } else {
                    "error"
                };
                let (tokens_in, tokens_out) = usage_tokens(&prov_usage);
                let completion_metadata = match &metadata.metadata {
                    Value::Object(map) => {
                        let mut out = map.clone();
                        if let Some(result_payload) = result_payload.clone() {
                            out.insert("result".to_string(), result_payload);
                        }
                        Value::Object(out)
                    }
                    _ => metadata.metadata.clone(),
                };
                let Some(completed_event) = build_prov_event_completion(
                    context_id,
                    &completion_metadata,
                    ProvenanceEventType::LlmCall,
                    |ctx_id, task_id| {
                        ProvEvent::llm_call_completed_task(
                            ctx_id,
                            task_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            completion_metadata.clone(),
                            prov_usage.clone(),
                            *duration_ms,
                            *outcome,
                        )
                    },
                    |ctx_id, msg_id| {
                        ProvEvent::llm_call_completed_global(
                            ctx_id,
                            msg_id,
                            metadata.client.clone(),
                            metadata.model.clone(),
                            metadata.function_name.clone(),
                            prompt.clone(),
                            completion_metadata.clone(),
                            prov_usage_clone,
                            *duration_ms,
                            *outcome,
                        )
                    },
                ) else {
                    return Ok(()); // Skip on missing message_id
                };
                let prompt_bytes_for_metrics = match completed_event.data() {
                    ProvEventData::LlmCallCompleted {
                        prompt_serialized_utf8_bytes,
                        ..
                    } => *prompt_serialized_utf8_bytes as usize,
                    _ => 0,
                };
                metrics::record_llm_call(&LlmCallMetrics {
                    function_name: &metadata.function_name,
                    client: &metadata.client,
                    model: &metadata.model,
                    result: result_label,
                    turn: baml_rt_observability::turn::current_turn(context_id),
                    duration: std::time::Duration::from_millis(*duration_ms),
                    prompt_bytes: prompt_bytes_for_metrics,
                    tokens_in,
                    tokens_out,
                });
                let completed_id = completed_event.id().clone();
                let client_alias = metadata
                    .metadata
                    .get("client_alias")
                    .and_then(Value::as_str)
                    .unwrap_or("-");
                let model_alias = metadata
                    .metadata
                    .get("model_alias")
                    .and_then(Value::as_str)
                    .unwrap_or("-");
                tracing::debug!(
                    event = "provenance_emit",
                    source = "effect_subscriber.llm_completion",
                    prov_event_id = %completed_id,
                    function_name = %metadata.function_name,
                    client = %metadata.client,
                    model = %metadata.model,
                    client_alias = client_alias,
                    model_alias = model_alias,
                    context_id = %context_id,
                    task_id = ?task_id,
                    "Emitting LLM completion provenance event from effect-subscriber path"
                );
                self.writer
                    .add_event_with_logging(completed_event, "effect subscriber")
                    .await;
                if bool::from(*outcome)
                    && llm_completion_should_materialize_assistant_message(result_payload.as_ref())
                {
                    emit_llm_assistant_transcript_message(
                        self.writer.as_ref(),
                        context_id,
                        &completion_metadata,
                        result_payload.as_ref(),
                        &completed_id,
                        *duration_ms,
                    )
                    .await;
                }
                self.invalidate_context_ref_cache(
                    context_id,
                    agent_id_from_metadata(&metadata.metadata).as_ref(),
                );
                if !bool::from(*outcome) && rejection_reason.as_deref().is_some() {
                    let reason = rejection_reason.clone().unwrap_or_default();
                    tracing::warn!(
                        reason = %reason,
                        "Prompt output rejected; emitting PromptRejected in provenance"
                    );
                    let rejected_event = build_prov_event(
                        context_id,
                        &metadata.metadata,
                        ProvenanceEventType::LlmCall,
                        |ctx_id, task_id| {
                            ProvEvent::prompt_rejected_task(
                                ctx_id,
                                task_id,
                                completed_id.clone(),
                                reason.clone(),
                            )
                        },
                        |ctx_id, msg_id| {
                            ProvEvent::prompt_rejected_global(
                                ctx_id,
                                msg_id,
                                completed_id.clone(),
                                reason.clone(),
                            )
                        },
                    )?;
                    self.writer
                        .add_event_with_logging(rejected_event, "effect subscriber")
                        .await;
                    self.invalidate_context_ref_cache(
                        context_id,
                        agent_id_from_metadata(&metadata.metadata).as_ref(),
                    );
                }
                return Ok(());
            }
            // A2A effects are primarily for liveness gating, not provenance
            // Skip provenance emission for A2A lifecycle events.
            EffectEvent::A2aStarted { .. }
            | EffectEvent::A2aCompleted { .. }
            | EffectEvent::ContextHistorySettled { .. } => {
                return Ok(());
            }
            EffectEvent::IntentResolved {
                context_id,
                task_id,
                intent_id,
                description,
                citations,
                supersession,
                epoch: _,
            } => ProvEvent::intent_resolved(
                context_id.clone(),
                task_id.clone(),
                intent_id.clone(),
                description.clone(),
                citations.clone(),
                *supersession,
                None,
            ),
            EffectEvent::PlanGenerated {
                context_id,
                task_id,
                intent_id,
                plan_id,
                steps,
                supersession,
                epoch: _,
            } => {
                let steps: Vec<PlanStepSpec> =
                    serde_json::from_value(steps.clone()).map_err(|e| {
                        baml_rt_core::BamlRtError::InvalidArgument(format!(
                            "plan generated effect steps must decode as PlanStepSpec[]: {e}"
                        ))
                    })?;

                ProvEvent::plan_generated(
                    context_id.clone(),
                    task_id.clone(),
                    intent_id.clone(),
                    plan_id.clone(),
                    steps,
                    *supersession,
                )
            }
            EffectEvent::PlanStepStatusChanged {
                context_id,
                task_id,
                intent_id,
                plan_id,
                step_id,
                old_status,
                new_status,
                citations,
                epoch: _,
            } => ProvEvent::plan_step_status_changed(
                context_id.clone(),
                task_id.clone(),
                intent_id.clone(),
                plan_id.clone(),
                step_id.clone(),
                old_status.clone(),
                new_status.clone(),
                citations.clone(),
            ),
            // Tool stream chunks are relay-only; tools are already recorded via the tool interceptor
            EffectEvent::ToolStreamChunk { .. } => return Ok(()),
            EffectEvent::ToolSessionStep {
                context_id,
                tool_name,
                session_id,
                op,
                task_id,
            } => {
                // Task-scoped runs: tie session steps to the task so task-filtered episode
                // transcripts include Open / SendDone / SearchRead / PageRead rows. Otherwise fall back to
                // message scope (synthetic id when the context has no messages yet).
                let scope = if let Some(tid) = task_id {
                    CallScope::Task {
                        task_id: tid.clone(),
                    }
                } else {
                    self.writer
                        .context_messages(context_id, Some(1))
                        .await
                        .ok()
                        .and_then(|msgs| msgs.into_iter().next())
                        .map(|m| CallScope::Message {
                            message_id: m.message_id,
                        })
                        .unwrap_or_else(|| CallScope::Message {
                            message_id: synthetic_context_message_id(context_id),
                        })
                };
                ProvEvent::tool_session_step(
                    context_id.clone(),
                    scope,
                    tool_name.clone(),
                    session_id.clone(),
                    op,
                )
            }
        };

        let ctx_for_cache = prov_event.context_id_opt().cloned();
        let cache_agent = prov_event.message_agent_id().cloned();
        self.writer
            .add_event_with_logging(prov_event, "effect subscriber")
            .await;
        if let Some(ctx) = ctx_for_cache.as_ref() {
            self.invalidate_context_ref_cache(ctx, cache_agent.as_ref());
        }
        Ok(())
    }
}

/// Boundary validation: extract a typed [`MessageId`] from untyped EffectEvent metadata.
///
/// Returns `None` when `message_id` is absent or non-string. Callers that require
/// a message_id for correctness must treat `None` as a validation rejection at the
/// boundary — downstream provenance code must not re-parse this field.
fn message_id_from_metadata(metadata: &Value) -> Option<MessageId> {
    metadata
        .get("message_id")
        .and_then(|value| value.as_str())
        .map(|value| MessageId::from_external(ExternalId::new(value.to_string())))
}

/// Boundary validation: extract a typed [`TaskId`] from untyped EffectEvent metadata.
///
/// Returns `None` when `task_id` is absent or non-string. Callers that require
/// a task_id for correctness must treat `None` as a validation rejection at the
/// boundary — downstream provenance code must not re-parse this field.
fn task_id_from_metadata(metadata: &Value) -> Option<TaskId> {
    metadata
        .get("task_id")
        .and_then(|value| value.as_str())
        .map(|value| TaskId::from_external(ExternalId::new(value.to_string())))
}

fn agent_id_from_metadata(metadata: &Value) -> Option<AgentId> {
    metadata
        .get("agent_id")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(|uuid| AgentId::from_uuid(UuidId::new(uuid)))
}

fn llm_assistant_transcript_message_id(
    metadata: &Value,
    context_id: &ContextId,
    completed_anchor: &ActivityAnchorId,
) -> MessageId {
    message_id_from_metadata(metadata).map_or_else(
        || {
            MessageId::from_external(ExternalId::new(format!(
                "ctx-msg:{}:llm:{}",
                context_id.as_str(),
                completed_anchor.as_str()
            )))
        },
        |base| {
            MessageId::from_external(ExternalId::new(format!(
                "{}:llm:{}",
                base.as_str(),
                completed_anchor.as_str()
            )))
        },
    )
}

async fn emit_llm_assistant_transcript_message<W: ProvenanceWriter + ?Sized>(
    writer: &W,
    context_id: &ContextId,
    metadata: &Value,
    result_payload: Option<&Value>,
    completed_anchor: &ActivityAnchorId,
    timestamp_ms: u64,
) {
    let Some(task_id) = task_id_from_metadata(metadata) else {
        return;
    };
    let Some(agent_id) = agent_id_from_metadata(metadata) else {
        return;
    };
    let response_text = result_payload
        .map(extract_response_text)
        .unwrap_or_default();
    if response_text.trim().is_empty() {
        return;
    }
    let message_id = llm_assistant_transcript_message_id(metadata, context_id, completed_anchor);
    let msg_event = ProvEvent::message_sent_task(
        context_id.clone(),
        task_id,
        message_id,
        "assistant".to_string(),
        vec![response_text],
        None,
        agent_id,
        timestamp_ms,
        Vec::new(),
    );
    writer
        .add_event_with_logging(msg_event, "llm assistant transcript")
        .await;
}

fn usage_tokens(usage: &LlmUsage) -> (Option<u64>, Option<u64>) {
    match usage {
        LlmUsage::Known {
            prompt_tokens,
            completion_tokens,
            ..
        } => (Some(*prompt_tokens), Some(*completion_tokens)),
        LlmUsage::Unknown => (None, None),
    }
}

fn normalized_prompt(prompt: &Value) -> Value {
    if prompt.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        prompt.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use baml_rt_conversation::view::{ProvenanceContextMessage, ProvenanceConversationContextItem};
    use baml_rt_core::{
        Outcome,
        bus::{EffectEvent, LlmEffectMetadata},
    };
    use serde_json::json;

    use super::*;
    use crate::{
        events::ProvEventData,
        store::{ProvenanceContextReader, ProvenanceWriter},
    };

    struct RecordingWriter {
        events: Mutex<Vec<ProvEvent>>,
        seed_messages: Vec<ProvenanceContextMessage>,
        history_reads: std::sync::atomic::AtomicUsize,
    }

    impl Default for RecordingWriter {
        fn default() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                seed_messages: Vec::new(),
                history_reads: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl RecordingWriter {
        fn note_history_read(&self) {
            self.history_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl ProvenanceContextReader for RecordingWriter {
        async fn context_messages(
            &self,
            _context_id: &ContextId,
            _limit: Option<usize>,
        ) -> crate::error::Result<Vec<ProvenanceContextMessage>> {
            self.note_history_read();
            Ok(self.seed_messages.clone())
        }

        async fn conversation_context(
            &self,
            _context_id: &ContextId,
            _limit: Option<usize>,
        ) -> crate::error::Result<Vec<ProvenanceConversationContextItem>> {
            self.note_history_read();
            Ok(Vec::new())
        }

        async fn conversation_context_with_task(
            &self,
            _context_id: &ContextId,
            _limit: Option<usize>,
            _task_id: Option<&TaskId>,
        ) -> crate::error::Result<Vec<ProvenanceConversationContextItem>> {
            self.note_history_read();
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl ProvenanceWriter for RecordingWriter {
        async fn add_event(&self, event: ProvEvent) -> crate::error::Result<()> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }

        async fn add_event_fenced(
            &self,
            event: ProvEvent,
            _fence: crate::ContextAgentFence,
        ) -> crate::error::Result<()> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }

    #[tokio::test]
    async fn tool_started_without_scope_ids_uses_synthetic_message_fallback() {
        let writer = Arc::new(RecordingWriter::default());
        let subscriber = ProvenanceEffectSubscriber::new(writer.clone());
        let context_id = ContextId::from("ctx-gate-synth");
        let event = EffectEvent::ToolStarted {
            context_id: context_id.clone(),
            metadata: baml_rt_core::bus::ToolEffectMetadata {
                tool_name: "support/clickup".to_string(),
                function_name: None,
                args: json!({}),
                metadata: json!({ "agent_id": uuid::Uuid::new_v4().to_string() }),
                delegation_target: None,
                tool_backend: None,
                tool_digest: None,
            },
        };
        subscriber.on_effect(&event).await.unwrap();
        let events = writer.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].data(),
            ProvEventData::ToolCallStarted { .. }
        ));
    }

    fn llm_completed(result_payload: serde_json::Value) -> EffectEvent {
        EffectEvent::LlmCompleted {
            context_id: ContextId::from("ctx-1"),
            metadata: LlmEffectMetadata {
                client: "c".into(),
                model: "m".into(),
                function_name: "f".into(),
                prompt: json!({}),
                metadata: json!({
                    "task_id": "task-1",
                    "message_id": "msg-1",
                    "agent_id": uuid::Uuid::new_v4().to_string(),
                }),
                tool_name: baml_rt_core::bus::ToolNameResolution::NotApplicable,
            },
            usage: None,
            result_payload: Some(result_payload),
            duration_ms: 1,
            outcome: Outcome::Success,
            rejection_reason: None,
        }
    }

    /// Regression guard for the citation stall: model output that still carries legacy
    /// `citations` (top-level, under `step`, or nested in a reply) must not trigger any
    /// history read or reference-table rebuild on the awaited LlmCompleted path, and must
    /// not attach citation data to the completion event or assistant transcript.
    #[tokio::test]
    async fn llm_completed_with_legacy_citation_shapes_does_no_history_work() {
        let shapes = [
            json!({"message": "hello", "citations": ["#5"]}),
            json!({"step": {"op": "Send", "input": {}, "citations": ["#12", "@1"]}, "citations": ["@1"]}),
            json!({"step": {"op": "ReadOnlyFinish", "reply": {"parts": [{"type": "text", "text": "hi"}], "citations": ["@2"]}}}),
            json!({"message": "hello"}),
        ];
        for shape in shapes {
            let writer = Arc::new(RecordingWriter::default());
            let subscriber = ProvenanceEffectSubscriber::new(writer.clone());
            subscriber
                .on_effect(&llm_completed(shape.clone()))
                .await
                .unwrap();

            assert_eq!(
                writer
                    .history_reads
                    .load(std::sync::atomic::Ordering::SeqCst),
                0,
                "LlmCompleted must not read conversation history for {shape}"
            );
            let events = writer.events.lock().unwrap();
            let completed = events
                .iter()
                .find_map(|e| match e.data() {
                    ProvEventData::LlmCallCompleted {
                        citation_integrity,
                        citations,
                        resolved_citations,
                        ..
                    } => Some((
                        citation_integrity.clone(),
                        citations.clone(),
                        resolved_citations.clone(),
                    )),
                    _ => None,
                })
                .expect("completion event written before the awaited subscriber returns");
            assert!(completed.0.is_none(), "no integrity for {shape}");
            assert!(completed.1.is_empty(), "no citations for {shape}");
            assert!(completed.2.is_empty(), "no resolved citations for {shape}");
        }
    }

    #[tokio::test]
    async fn llm_completed_writes_assistant_transcript_without_citations() {
        let writer = Arc::new(RecordingWriter::default());
        let subscriber = ProvenanceEffectSubscriber::new(writer.clone());
        subscriber
            .on_effect(&llm_completed(
                json!({"message": "hello", "citations": ["#1"]}),
            ))
            .await
            .unwrap();
        let events = writer.events.lock().unwrap();
        let completed_at = events
            .iter()
            .position(|e| matches!(e.data(), ProvEventData::LlmCallCompleted { .. }))
            .expect("completion event");
        let transcript_at = events
            .iter()
            .position(|e| matches!(e.data(), ProvEventData::MessageSent { .. }))
            .expect("assistant transcript message");
        assert!(
            completed_at < transcript_at,
            "completion precedes transcript"
        );
        match events[transcript_at].data() {
            ProvEventData::MessageSent { citations, .. } => assert!(citations.is_empty()),
            _ => unreachable!(),
        }
    }
}
