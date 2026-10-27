// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! [`ProvenanceContextReader`] and [`ProvenanceQueryApi`].

use std::collections::HashMap;

use async_trait::async_trait;
use baml_rt_conversation::view::{
    AgentRef, ConversationItemContent, EventAttribution, EventKind, ProvenanceContextMessage,
    ProvenanceConversationContextItem, SessionStepContent, SessionStepOp, ToolCallContent,
    ToolOutcome, ToolResultContent, ToolSessionPhase, UserSpeakerKind, classify_user_speaker_kind,
    conversation_history_role_for_message,
};
use baml_rt_core::{
    Citation,
    ids::{ActivityAnchorId, AgentId, ContextId, MessageId, TaskId},
    is_history_infrastructure_notice,
};
use serde_json::{Map, Value};

use super::{
    SurrealProvenanceStore,
    conversation_context_pipeline::ConversationContextBatch,
    helpers::{
        check_and_take_zero, has_meaningful_result, is_empty_object,
        json_value_from_embedded_string, map_surreal_error, metadata_error,
        normalize_message_content, parse_json_object_field,
    },
    payload::{decode_payload_row, payload_id_for},
};
use crate::{
    error::Result,
    events::ToolSessionStepOpKind,
    host_ingress_transcript::operational_from_host_message,
    id_semantics::{
        context_entity_id_string, task_entity_id_string_raw, task_execution_activity_id_string,
    },
    payload_id::DEFAULT_SESSION_READ_LINE_LIMIT,
    payload_record::PayloadRecord,
    store::{
        ContextHistoryCursor, ContextHistoryPage, ProvenanceContextReader, ProvenanceQueryApi,
    },
    surreal_tables::{PAYLOAD_ROW_SELECT, TBL_EDGE, TBL_NODE, TBL_PAYLOAD},
    vocabulary::{a2a, a2a_relations, context_scope, semantic_labels, storage_safe},
};

/// Builds the node scan shared by bounded and unbounded conversation reads.
fn conversation_node_scan_sql(
    scoped_to: &str,
    filters: &[&str],
    ordering_and_limit: &str,
) -> String {
    let filters = filters
        .iter()
        .copied()
        .filter(|filter| !filter.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "SELECT node_id, label, props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
         WHERE node_id IN (\
           SELECT VALUE from_id FROM {TBL_EDGE} \
           WHERE to_id = $ctx_node_id AND rel_type = '{scoped_to}' \
             AND from_label IN ['Message', 'ToolCall', 'SessionStep']\
         ) \
         AND (label != 'ToolCall' OR props.a2a_activity_outcome IN ['Success', 'Failed']) \
         {filters} {ordering_and_limit}"
    )
}

/// True when this row is a successful `system/internal_a2a` payload whose
/// relayed message already exists in the agent's view.
fn is_elided_delegation_payload(
    row: &Value,
    proven: &std::collections::HashSet<(String, String)>,
) -> bool {
    if row.get("label").and_then(Value::as_str) != Some("ToolCall") {
        return false;
    }
    let props = row.get("props").unwrap_or(&Value::Null);
    if props
        .get(storage_safe::A2A_TOOL_NAME)
        .and_then(Value::as_str)
        != Some("system/internal_a2a")
    {
        return false;
    }
    // Keep failures: an unmatched delegation error must stay visible.
    if props
        .get(storage_safe::A2A_ACTIVITY_OUTCOME)
        .and_then(Value::as_str)
        != Some("Success")
    {
        return false;
    }
    let (Some(caller), Some(parent_task)) = (
        props
            .get(storage_safe::A2A_AGENT_ID)
            .and_then(Value::as_str),
        props.get(storage_safe::A2A_TASK_ID).and_then(Value::as_str),
    ) else {
        return false;
    };
    proven.contains(&(caller.to_string(), parent_task.to_string()))
}

fn stored_event_order(props: &Value) -> u64 {
    props
        .get("a2a_event_order")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn task_id_from_props(props: &Value) -> Option<TaskId> {
    props
        .get(storage_safe::A2A_TASK_ID)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| TaskId::from_external(baml_rt_core::ids::ExternalId::new(value)))
}

/// Stable actor identity for rows that carry their own `AgentId` property.
///
/// Message rows deliberately do not: their agent ownership is a two-hop
/// traversal (`Message <- A2AMessageProcessing -> Agent`), so their attribution
/// fills in once the agent-scoped read provides it.
fn agent_ref_from_props(props: &Value) -> Option<AgentRef> {
    let raw = props.get(storage_safe::A2A_AGENT_ID)?.as_str()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let agent_id = baml_rt_core::ids::UuidId::parse_str(raw).ok()?;
    Some(AgentRef {
        agent_id: AgentId::from_uuid(agent_id),
        name: props
            .get(storage_safe::A2A_AGENT_TYPE)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    })
}

fn host_ingress_source_identity(props: &Value) -> Option<String> {
    let kind = props
        .get(a2a::HOST_INGRESS_SOURCE_KIND)
        .and_then(Value::as_str)?
        .trim();
    let key = props
        .get(a2a::HOST_INGRESS_SOURCE_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match key {
        Some(key) => Some(format!("{kind}:{key}")),
        None if !kind.is_empty() => Some(kind.to_string()),
        None => None,
    }
}

fn is_session_bookkeeping_result(phase: &ToolSessionPhase, value: &Value) -> bool {
    if !phase.is_session_phase() {
        return false;
    }
    match value {
        Value::Object(map) if map.len() == 1 => matches!(
            map.get("status").and_then(Value::as_str),
            Some("sent" | "finished" | "aborted" | "opened")
        ),
        _ => false,
    }
}

fn resolve_user_speaker_kind_for_message(
    context_id: &ContextId,
    props: &Value,
    activity_anchor: &str,
    history_role: &str,
    graph_role: &str,
) -> Option<UserSpeakerKind> {
    if history_role != "user" {
        return None;
    }
    if let Some(raw) = props.get("a2a_user_speaker_kind").and_then(Value::as_str)
        && let Some(kind) = UserSpeakerKind::from_wire_str(raw)
    {
        return Some(kind);
    }
    let anchor = ActivityAnchorId::from(activity_anchor);
    classify_user_speaker_kind(context_id, &anchor, None, graph_role)
}

#[async_trait]
impl ProvenanceContextReader for SurrealProvenanceStore {
    async fn context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        let ctx_node_id = context_entity_id_string(context_id.as_str());
        // SurrealDB requires every `ORDER BY` field to appear in the projection.
        let query = format!(
            "SELECT node_id, props, props.a2a_event_order AS event_order FROM {TBL_NODE} \
             WHERE node_id IN (\
               SELECT VALUE from_id FROM {TBL_EDGE} \
               WHERE to_id = $ctx_node_id AND rel_type = '{scoped}' AND from_label = 'Message'\
             ) ORDER BY event_order ASC, node_id ASC",
            scoped = context_scope::SCOPED_TO,
        );
        let response = self
            .db
            .query(&query)
            .bind(("ctx_node_id", ctx_node_id))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;

        let mut messages: Vec<ProvenanceContextMessage> = Vec::new();
        for row in &rows {
            let props = match row.get("props") {
                Some(p) => p,
                None => continue,
            };
            let message_id = props
                .get("a2a_message_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let role = props
                .get("a2a_role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let content_raw = props.get("a2a_content").cloned().unwrap_or(Value::Null);
            let content_value = json_value_from_embedded_string(&content_raw);
            let content = normalize_message_content(&content_value);
            if content.trim().is_empty() || is_history_infrastructure_notice(&content) {
                continue;
            }
            messages.push(ProvenanceContextMessage {
                message_id: MessageId::from(message_id),
                timestamp_ms: stored_event_order(props),
                role: role.to_string(),
                content: vec![content],
            });
        }
        messages.retain(|m| !m.content.iter().all(|c| c.trim().is_empty()));
        messages.sort_by_key(|m| m.timestamp_ms);
        if let Some(n) = limit {
            if n == 0 {
                return Ok(Vec::new());
            }
            if messages.len() > n {
                let had = messages.len();
                tracing::debug!(
                    %context_id,
                    limit = n,
                    had,
                    "truncating context messages to last N (tail cap)"
                );
                messages = messages.split_off(messages.len() - n);
            }
        }
        Ok(messages)
    }

    async fn conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.conversation_context_filtered(context_id, limit, None, None, None, false, true, None)
            .await
    }

    async fn conversation_context_with_task(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.conversation_context_filtered(
            context_id, limit, task_id, None, None, false, true, None,
        )
        .await
    }
}

fn warn_conversation_context_row_skip(context_id: &ContextId, row: &Value, reason: &'static str) {
    tracing::warn!(
        target: "baml_rt_provenance::conversation_context",
        context_id = %context_id.as_str(),
        node_id = row.get("node_id").and_then(serde_json::Value::as_str).unwrap_or(""),
        label = row.get("label").and_then(serde_json::Value::as_str).unwrap_or(""),
        reason,
        "skipping graph row for conversation_context export"
    );
}

fn task_message_filter_sql() -> String {
    let tm = a2a_relations::TASK_MESSAGE;
    format!(
        "AND label = 'Message' AND node_id IN (\
           SELECT VALUE to_id FROM {TBL_EDGE} \
           WHERE from_id = $task_entity_id AND rel_type = '{tm}'\
         )"
    )
}

fn task_tool_call_filter_sql() -> String {
    let tc = a2a_relations::TASK_CALL;
    format!(
        "AND label = 'ToolCall' AND node_id IN (\
           SELECT VALUE to_id FROM {TBL_EDGE} \
           WHERE from_id = $task_exec_id AND rel_type = '{tc}'\
         )"
    )
}

fn task_session_step_filter_sql() -> String {
    let ts = a2a_relations::TASK_SESSION_STEP;
    format!(
        "AND label = 'SessionStep' AND node_id IN (\
           SELECT VALUE to_id FROM {TBL_EDGE} \
           WHERE from_id = $task_entity_id AND rel_type = '{ts}'\
         )"
    )
}

fn sort_conversation_rows(rows: &mut [Value]) {
    rows.sort_by(|a, b| {
        let order_key = |row: &Value| {
            (
                row.get("event_order").and_then(Value::as_u64).unwrap_or(0),
                row.get("node_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        order_key(a).cmp(&order_key(b))
    });
}

struct ScopedConversationQuery<'a> {
    ctx_node_id: &'a str,
    scoped_to: &'a str,
    after_filter_sql: &'a str,
    label_task_filter_sql: &'a str,
    agent_filter_sql: &'a str,
    task_id: Option<&'a TaskId>,
    agent_instance_nodes: Option<&'a [String]>,
    after_event_order: Option<u64>,
}

impl SurrealProvenanceStore {
    /// Agent-facing conversation rows for exactly one reader agent.
    ///
    /// Reads the same actor-or-target view as [`Self::context_history_view`] and
    /// applies only that agent's own compaction head. A sibling's summary, or an
    /// operator/audit summary, can never satisfy this read.
    pub async fn conversation_context_for_agent_prompt(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        use crate::{
            conversation_context_query::DEFAULT_LLM_CONTEXT_ITEM_CAP,
            read::transcript::TranscriptProjectionProfile,
        };

        let items = self
            .conversation_context_for_agent(context_id, agent_id, task_id)
            .await?;
        let head = self
            .latest_compaction_head(context_id, Some(agent_id), task_id)
            .await?;
        let cap = limit.unwrap_or(DEFAULT_LLM_CONTEXT_ITEM_CAP).max(1);
        if head.is_some() {
            let compacted = crate::context_compaction::apply_compaction_profile(
                TranscriptProjectionProfile::AgentPromptCompacted,
                items,
                head.as_ref(),
            );
            // The prompt is the summary plus a bounded newest tail. `apply_compaction_profile`
            // returns summary + every post-coverage row (uncapped), so trim the tail to
            // `cap - 1` items rather than letting a deferred compaction leave the prompt unbounded.
            if compacted.len() <= cap {
                return Ok(compacted);
            }
            let tail_keep = cap - 1;
            let mut out = Vec::with_capacity(cap);
            out.push(compacted[0].clone()); // summary row is always kept
            let tail = &compacted[1..];
            out.extend(
                tail.iter()
                    .skip(tail.len().saturating_sub(tail_keep))
                    .cloned(),
            );
            return Ok(out);
        }
        if items.len() <= cap {
            return Ok(items);
        }
        let split_at = items.len() - cap;
        Ok(items.into_iter().skip(split_at).collect())
    }

    /// `ContextHistoryView(context_id, agent_id, window)` — the prompt-facing
    /// projection over one agent's participation in a context.
    ///
    /// Reads across every task in the context and keeps only events where
    /// `agent_id` is the actor or the target. The visibility predicate and the
    /// window are applied **in the query**, so a read costs this agent's own
    /// rows rather than the whole context: sibling rows and their tool payloads
    /// are never fetched, parsed, or hydrated.
    ///
    /// The window is the newest `window` visible rows by the durable ordering key
    /// `(event_order, node_id)`. `cursor` pages older-ward and is exclusive;
    /// `truncated` says that older visible rows exist outside the window.
    pub async fn context_history_view(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
        window: Option<usize>,
        cursor: Option<&ContextHistoryCursor>,
        task_id: Option<&TaskId>,
    ) -> Result<ContextHistoryPage> {
        use crate::conversation_context_query::DEFAULT_LLM_CONTEXT_ITEM_CAP;

        let window = window.unwrap_or(DEFAULT_LLM_CONTEXT_ITEM_CAP).max(1);
        // One extra row tells us whether anything older remains, without a second
        // count over the unfiltered context.
        let probe = window.saturating_add(1);
        let mut rows = self
            .fetch_agent_window_rows(context_id, agent_id, probe, cursor, task_id)
            .await?;
        // Rows arrive newest-first per label arm; keep the newest `probe` overall.
        sort_conversation_rows(&mut rows);
        let truncated = rows.len() > window;
        if truncated {
            let drop = rows.len() - window;
            rows.drain(0..drop);
        }
        let next_cursor = rows.first().and_then(|row| {
            let node_id = row.get("node_id").and_then(Value::as_str)?;
            let event_order = row
                .get("event_order")
                .or_else(|| row.get("props").and_then(|p| p.get("a2a_event_order")))
                .and_then(Value::as_u64)?;
            Some(ContextHistoryCursor::new(event_order, node_id))
        });
        if rows.is_empty() {
            return Ok(ContextHistoryPage::default());
        }
        // Elide a successful internal delegation payload only when typed lineage
        // proves the relayed message was persisted. Ambiguous or failed
        // delegations keep their payload: failure information must not vanish.
        let proven = self.persisted_delegations(context_id).await?;
        if !proven.is_empty() {
            rows.retain(|row| !is_elided_delegation_payload(row, &proven));
        }
        if rows.is_empty() {
            // The window was selected before dedup elided it. Keep the cursor and
            // the truncation flag so the reader can continue older-ward instead of
            // treating an elided-only window as the end of the history.
            return Ok(ContextHistoryPage {
                items: Vec::new(),
                cursor: next_cursor.map(|cursor| cursor.encode()),
                truncated,
            });
        }
        // Hydrate only the selected window: payloads, citations, and edge maps are
        // all keyed off these rows.
        let items = self
            .conversation_context_filtered(
                context_id,
                None,
                task_id,
                None,
                None,
                true,
                false,
                Some(rows),
            )
            .await?
            .into_iter()
            .filter(|item| item.is_visible_to_agent(agent_id))
            .collect();
        Ok(ContextHistoryPage {
            items,
            cursor: next_cursor.map(|cursor| cursor.encode()),
            truncated,
        })
    }

    /// Full, uncapped agent participation view for exactly one agent.
    ///
    /// This is the source for agent-prompt compaction (P4): it returns every
    /// actor-or-target row for `agent_id` across the context so `prepare_compaction`
    /// can select a covered prefix and a recent tail from the same view the prompt
    /// measures. It deliberately does not page or window — callers are compaction and
    /// budget measurement, not the bounded prompt window.
    pub async fn conversation_context_for_agent(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        // A large-but-finite cap keeps the DB from scanning unbounded while still
        // covering any realistic agent participation in a single context.
        const UNCAPPED_READ_CAP: usize = 1_000_000;
        let mut rows = self
            .fetch_agent_window_rows(context_id, agent_id, UNCAPPED_READ_CAP, None, task_id)
            .await?;
        sort_conversation_rows(&mut rows);
        let proven = self.persisted_delegations(context_id).await?;
        if !proven.is_empty() {
            rows.retain(|row| !is_elided_delegation_payload(row, &proven));
        }
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let items = self
            .conversation_context_filtered(
                context_id,
                None,
                task_id,
                None,
                None,
                true,
                false,
                Some(rows),
            )
            .await?
            .into_iter()
            .filter(|item| item.is_visible_to_agent(agent_id))
            .collect();
        Ok(items)
    }

    /// Bounded, agent-scoped node fetch: the storage half of
    /// [`Self::context_history_view`].
    ///
    /// Each label arm applies its own agent traversal, the exclusive cursor
    /// bound, a newest-first order, and the limit — so the DB never returns rows
    /// the reading agent cannot see.
    async fn fetch_agent_window_rows(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
        limit: usize,
        cursor: Option<&ContextHistoryCursor>,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<Value>> {
        use crate::metamodel::query::{
            conversation_call_agent_instances_clause, conversation_message_agent_instances_clause,
            conversation_session_step_agent_instances_clause,
        };

        let ctx_node_id = context_entity_id_string(context_id.as_str());
        let scoped_to = context_scope::SCOPED_TO;
        let agent_nodes = vec![crate::id_semantics::agent_node_id_string(agent_id)];
        let msg_filter = conversation_message_agent_instances_clause("agent_nodes");
        let tool_filter = conversation_call_agent_instances_clause("agent_nodes");
        let session_filter = conversation_session_step_agent_instances_clause("agent_nodes");
        let task_message = task_id
            .map(|_| task_message_filter_sql())
            .unwrap_or_default();
        let task_tool = task_id
            .map(|_| task_tool_call_filter_sql())
            .unwrap_or_default();
        let task_session = task_id
            .map(|_| task_session_step_filter_sql())
            .unwrap_or_default();
        let before_sql = match cursor {
            Some(_) => {
                "AND (props.a2a_event_order < $cursor_order \
                   OR (props.a2a_event_order = $cursor_order AND node_id < $cursor_node_id))"
            }
            None => "",
        };
        // Without a task filter the tool and session arms would run the same
        // `label IN ['ToolCall', 'SessionStep']` clause twice and return every
        // call twice. With a task filter the two arms are disjoint: each one is
        // restricted to the label its task edge belongs to.
        let no_task_filter = String::new();
        let mut arms: Vec<(&str, &str)> = vec![(task_message.as_str(), msg_filter.as_str())];
        if task_id.is_some() {
            arms.push((task_tool.as_str(), tool_filter.as_str()));
            arms.push((task_session.as_str(), session_filter.as_str()));
        } else {
            arms.push((no_task_filter.as_str(), tool_filter.as_str()));
            arms.push((no_task_filter.as_str(), session_filter.as_str()));
        }
        let mut rows: Vec<Value> = Vec::new();
        for (task_filter_sql, agent_filter_sql) in arms {
            let sql = conversation_node_scan_sql(
                scoped_to,
                &[task_filter_sql, agent_filter_sql, before_sql],
                &format!("ORDER BY event_order DESC, node_id DESC LIMIT {limit}"),
            );
            let mut q = self
                .db
                .query(&sql)
                .bind(("ctx_node_id", ctx_node_id.clone()))
                .bind(("agent_nodes", agent_nodes.clone()));
            if let Some(cursor) = cursor {
                q = q
                    .bind(("cursor_order", cursor.event_order))
                    .bind(("cursor_node_id", cursor.node_id.clone()));
            }
            if let Some(task_id) = task_id {
                q = q
                    .bind((
                        "task_entity_id",
                        task_entity_id_string_raw(task_id.as_str()),
                    ))
                    .bind((
                        "task_exec_id",
                        task_execution_activity_id_string(task_id.as_str()),
                    ));
            }
            let response = q.await.map_err(map_surreal_error)?;
            rows.extend(check_and_take_zero(response, map_surreal_error)?);
        }
        Ok(rows)
    }

    async fn compacted_conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
        agent_package: Option<&str>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        use crate::{
            conversation_context_query::DEFAULT_LLM_CONTEXT_ITEM_CAP,
            observation::{ObservationScope, TaskObservationScope, TemporalBound},
            read::{
                TranscriptEngine, TranscriptPageDirection, TranscriptPageRequest,
                TranscriptProjectionProfile,
            },
        };

        let scope = ObservationScope {
            context_id: context_id.clone(),
            task: match task_id {
                Some(id) => TaskObservationScope::Task(id.clone()),
                None => TaskObservationScope::ContextWide,
            },
            agent_package: agent_package.map(str::to_string),
            temporal: TemporalBound::All,
        };
        let page = TranscriptEngine::page(
            self,
            TranscriptPageRequest {
                scope,
                limit: limit.unwrap_or(DEFAULT_LLM_CONTEXT_ITEM_CAP),
                profile: TranscriptProjectionProfile::OperatorTimeline,
                direction: TranscriptPageDirection::Backward,
            },
        )
        .await?;
        // Operator/audit reads may carry a context-wide audit summary (agent_id = None),
        // never an agent-prompt summary.
        let head = self
            .latest_compaction_head(context_id, None, task_id)
            .await?;
        Ok(crate::context_compaction::apply_compaction_profile(
            TranscriptProjectionProfile::OperatorTimeline,
            page.items,
            head.as_ref(),
        ))
    }

    async fn fetch_scoped_conversation_nodes(
        &self,
        query: ScopedConversationQuery<'_>,
    ) -> Result<Vec<Value>> {
        let ScopedConversationQuery {
            ctx_node_id,
            scoped_to,
            after_filter_sql,
            label_task_filter_sql,
            agent_filter_sql,
            task_id,
            agent_instance_nodes,
            after_event_order,
        } = query;
        let sql = conversation_node_scan_sql(
            scoped_to,
            &[after_filter_sql, label_task_filter_sql, agent_filter_sql],
            "ORDER BY event_order ASC, node_id ASC",
        );
        let mut q = self.db.query(&sql);
        q = q.bind(("ctx_node_id", ctx_node_id.to_string()));
        if let Some(instances) = agent_instance_nodes {
            let value: Vec<Value> = instances.iter().cloned().map(Value::String).collect();
            q = q.bind(("agent_instance_nodes", Value::Array(value)));
        }
        if let Some(tid) = task_id {
            q = q.bind(("task_entity_id", task_entity_id_string_raw(tid.as_str())));
            q = q.bind((
                "task_exec_id",
                task_execution_activity_id_string(tid.as_str()),
            ));
        }
        if let Some(after) = after_event_order {
            q = q.bind(("after_event_order", after));
        }
        let response = q.await.map_err(map_surreal_error)?;
        check_and_take_zero(response, map_surreal_error)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn conversation_context_filtered(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
        agent_package: Option<&str>,
        after_event_order: Option<u64>,
        forward_limit: bool,
        enrich_extensions: bool,
        preloaded_rows: Option<Vec<Value>>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        let ctx_node_id = context_entity_id_string(context_id.as_str());
        let scoped_to = context_scope::SCOPED_TO;

        let after_filter_sql = match after_event_order {
            Some(_) => crate::observation::after_event_order_filter_sql(),
            None => "",
        };

        let agent_instance_nodes: Option<Vec<String>> = if let Some(pkg) = agent_package {
            let index = self.load_agent_runtime_index().await?;
            if let Some(task_id) = task_id {
                let resolution = self.get_task_agent_id(task_id).await?;
                match super::agent_runtime_index::task_agent_package_check(
                    Some(task_id),
                    Some(pkg),
                    Some(&resolution),
                    &index,
                ) {
                    super::agent_runtime_index::TaskAgentPackageCheck::OmitAgentFilter => None,
                    super::agent_runtime_index::TaskAgentPackageCheck::MismatchEmpty => {
                        return Ok(Vec::new());
                    }
                    super::agent_runtime_index::TaskAgentPackageCheck::ApplyPackageFilter => {
                        let instances = index
                            .instance_node_ids_by_package
                            .get(pkg)
                            .cloned()
                            .unwrap_or_default();
                        if instances.is_empty() {
                            return Ok(Vec::new());
                        }
                        Some(instances)
                    }
                }
            } else {
                let instances = index
                    .instance_node_ids_by_package
                    .get(pkg)
                    .cloned()
                    .unwrap_or_default();
                if instances.is_empty() {
                    return Ok(Vec::new());
                }
                Some(instances)
            }
        } else {
            None
        };
        let agent_instances_ref = agent_instance_nodes.as_deref();

        let msg_agent_filter = if agent_instances_ref.is_some() {
            crate::metamodel::query::conversation_message_agent_instances_clause(
                "agent_instance_nodes",
            )
        } else {
            String::new()
        };
        let tool_agent_filter = if agent_instances_ref.is_some() {
            crate::metamodel::query::conversation_call_agent_instances_clause(
                "agent_instance_nodes",
            )
        } else {
            String::new()
        };
        let session_agent_filter = if agent_instances_ref.is_some() {
            crate::metamodel::query::conversation_session_step_agent_instances_clause(
                "agent_instance_nodes",
            )
        } else {
            String::new()
        };

        let task_message = task_message_filter_sql();
        let task_tool = task_tool_call_filter_sql();
        let task_session = task_session_step_filter_sql();

        let rows_from_preload = preloaded_rows.is_some();
        let mut rows: Vec<Value> = preloaded_rows.unwrap_or_default();
        if !rows_from_preload && task_id.is_some() {
            let agent_msg = if agent_instances_ref.is_some() {
                msg_agent_filter.as_str()
            } else {
                ""
            };
            let agent_tool = if agent_instances_ref.is_some() {
                tool_agent_filter.as_str()
            } else {
                ""
            };
            let agent_session = if agent_instances_ref.is_some() {
                session_agent_filter.as_str()
            } else {
                ""
            };
            for (label_task_filter_sql, agent_filter_sql) in [
                (task_message.as_str(), agent_msg),
                (task_tool.as_str(), agent_tool),
                (task_session.as_str(), agent_session),
            ] {
                rows.extend(
                    self.fetch_scoped_conversation_nodes(ScopedConversationQuery {
                        ctx_node_id: &ctx_node_id,
                        scoped_to,
                        after_filter_sql,
                        label_task_filter_sql,
                        agent_filter_sql,
                        task_id,
                        agent_instance_nodes: agent_instances_ref,
                        after_event_order,
                    })
                    .await?,
                );
            }
        } else if !rows_from_preload && agent_instances_ref.is_some() {
            for agent_filter_sql in [
                msg_agent_filter.as_str(),
                tool_agent_filter.as_str(),
                session_agent_filter.as_str(),
            ] {
                rows.extend(
                    self.fetch_scoped_conversation_nodes(ScopedConversationQuery {
                        ctx_node_id: &ctx_node_id,
                        scoped_to,
                        after_filter_sql,
                        label_task_filter_sql: "",
                        agent_filter_sql,
                        task_id,
                        agent_instance_nodes: agent_instances_ref,
                        after_event_order,
                    })
                    .await?,
                );
            }
        } else if !rows_from_preload {
            rows = self
                .fetch_scoped_conversation_nodes(ScopedConversationQuery {
                    ctx_node_id: &ctx_node_id,
                    scoped_to,
                    after_filter_sql,
                    label_task_filter_sql: "",
                    agent_filter_sql: "",
                    task_id,
                    agent_instance_nodes: agent_instances_ref,
                    after_event_order,
                })
                .await?;
        }
        if rows_from_preload || !rows.is_empty() {
            sort_conversation_rows(&mut rows);
        }

        // Collect ToolCall node_ids, payload anchors, and Message node_ids for batch queries.
        let mut tool_call_node_ids: Vec<String> = Vec::new();
        let mut payload_ids: Vec<String> = Vec::new();
        let mut message_node_ids: Vec<String> = Vec::new();
        for row in &rows {
            let label = row.get("label").and_then(Value::as_str).unwrap_or_default();
            if label == "ToolCall" {
                if let Some(nid) = row
                    .get("node_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    tool_call_node_ids.push(nid.to_string());
                }
                if let Some(anchor) = row
                    .get("props")
                    .and_then(|p| p.get("a2a_activity_anchor"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    payload_ids.push(payload_id_for(anchor, "tool_call"));
                    payload_ids.push(payload_id_for(anchor, "tool_result"));
                }
            } else if label == "Message"
                && let Some(nid) = row
                    .get("node_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            {
                message_node_ids.push(nid.to_string());
            }
        }

        // Single batch payload fetch for all ToolCall payloads (tool_call + tool_result).
        let payload_map: HashMap<String, PayloadRecord> = if payload_ids.is_empty() {
            HashMap::new()
        } else {
            let in_list = payload_ids
                .iter()
                .map(|id| format!("\'{id}\'"))
                .collect::<Vec<_>>()
                .join(", ");
            let pq = format!(
                "SELECT {PAYLOAD_ROW_SELECT} FROM {TBL_PAYLOAD} WHERE payload_id IN [{in_list}]"
            );
            let prows: Vec<Value> = self.query_sql_rows(&pq).await?;
            let decoded: Vec<PayloadRecord> = prows
                .into_iter()
                .map(decode_payload_row)
                .collect::<Result<Vec<_>>>()?;
            let hydrated = self.hydrate_payload_records(decoded).await?;
            let mut map = HashMap::new();
            for rec in hydrated {
                map.insert(rec.payload_id.clone(), rec);
            }
            map
        };

        // Scoped WAS_USED_BY -> ToolArgs edge validation (only for our ToolCalls).
        let tool_call_edge_info: HashMap<String, (String, String)> =
            if tool_call_node_ids.is_empty() {
                HashMap::new()
            } else {
                let node_id_list = tool_call_node_ids
                    .iter()
                    .map(|id| format!("\'{id}\'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let eq = format!(
                    "SELECT from_id, props OMIT id FROM {TBL_EDGE} \
                     WHERE rel_type = '{}' AND from_label = 'ToolCall' AND to_label = 'ToolArgs' \
                       AND from_id IN [{node_id_list}]",
                    semantic_labels::WAS_USED_BY
                );
                let erows: Vec<Value> = self.query_sql_rows(&eq).await?;
                let mut info = HashMap::new();
                for edge in &erows {
                    let from_id = edge
                        .get("from_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let edge_props = edge.get("props").and_then(Value::as_object);
                    let prov_role = edge_props
                        .and_then(|p| p.get("prov_role"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let prov_type = edge_props
                        .and_then(|p| p.get("prov_type"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    info.insert(from_id.to_string(), (prov_role, prov_type));
                }
                info
            };

        // Batch-fetch CITED edges for all Message nodes to get their citation strings.
        // The CITED edges written by the normalizer carry a `raw` attribute with the
        // original citation string (#N, @N, …).
        let message_citations_map: HashMap<String, Vec<Citation>> = if message_node_ids.is_empty() {
            HashMap::new()
        } else {
            let node_id_list = message_node_ids
                .iter()
                .map(|id| format!("\'{id}\'"))
                .collect::<Vec<_>>()
                .join(", ");
            let cited_rel = semantic_labels::CITED;
            let cq = format!(
                "SELECT from_id, props.raw AS raw FROM {TBL_EDGE} \
                     WHERE rel_type = '{cited_rel}' AND from_label = 'Message' \
                       AND from_id IN [{node_id_list}]"
            );
            let crows: Vec<Value> = self.query_sql_rows(&cq).await?;
            let mut map: HashMap<String, Vec<Citation>> = HashMap::new();
            for edge in &crows {
                let from_id = edge
                    .get("from_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if from_id.is_empty() {
                    continue;
                }
                if let Some(raw) = edge
                    .get("raw")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    && let Ok(c) = Citation::try_new(raw)
                {
                    map.entry(from_id.to_string()).or_default().push(c);
                }
            }
            map
        };

        // Host dispatch outcome rows link to target AgentRuntimeInstance via graph edge.
        let dispatch_target_by_message: HashMap<String, (String, String)> = if message_node_ids
            .is_empty()
        {
            HashMap::new()
        } else {
            let node_id_list = message_node_ids
                .iter()
                .map(|id| format!("\'{id}\'"))
                .collect::<Vec<_>>()
                .join(", ");
            let rel = a2a_relations::HOST_DISPATCH_TARGET;
            let eq = format!(
                "SELECT from_id, to_id FROM {TBL_EDGE} \
                     WHERE rel_type = '{rel}' AND from_label = 'Message' AND to_label = 'AgentRuntimeInstance' \
                       AND from_id IN [{node_id_list}]"
            );
            let erows: Vec<Value> = self.query_sql_rows(&eq).await?;
            let mut target_ids: Vec<String> = Vec::new();
            let mut edge_map: HashMap<String, String> = HashMap::new();
            for edge in &erows {
                let from_id = edge
                    .get("from_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let to_id = edge
                    .get("to_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if from_id.is_empty() || to_id.is_empty() {
                    continue;
                }
                edge_map.insert(from_id.to_string(), to_id.to_string());
                target_ids.push(to_id.to_string());
            }
            if target_ids.is_empty() {
                HashMap::new()
            } else {
                target_ids.sort();
                target_ids.dedup();
                let target_list = target_ids
                    .iter()
                    .map(|id| format!("\'{id}\'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let nq = format!(
                    "SELECT node_id, props FROM {TBL_NODE} \
                         WHERE label = 'AgentRuntimeInstance' AND node_id IN [{target_list}]"
                );
                let nrows: Vec<Value> = self.query_sql_rows(&nq).await?;
                let mut agent_props: HashMap<String, (String, String)> = HashMap::new();
                for row in &nrows {
                    let node_id = row
                        .get("node_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let props = row.get("props").cloned().unwrap_or(Value::Null);
                    let package = props
                        .get("a2a_agent_type")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let instance = props
                        .get("a2a_host_ingress_target_instance")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let (Some(package), Some(instance)) = (package, instance) {
                        agent_props.insert(node_id.to_string(), (package, instance));
                    }
                }
                let mut out = HashMap::new();
                for (from_id, to_id) in edge_map {
                    if let Some(target) = agent_props.get(&to_id) {
                        out.insert(from_id, target.clone());
                    }
                }
                out
            }
        };

        // Agent ownership of a Message is a graph traversal, not a stored
        // `a2a:agent_id` property: `Message <- WAS_RECEIVED_BY|WAS_EMITTED_BY -
        // A2AMessageProcessing -WAS_EXECUTED_BY-> Agent`. Resolve it once so each
        // row can carry typed attribution instead of a guessed chat role.
        let message_ownership = self.message_agent_ownership(&message_node_ids).await?;

        // Process the unified result set, discriminating by label.
        let mut items: Vec<ProvenanceConversationContextItem> = Vec::new();

        for row in &rows {
            let label = row.get("label").and_then(Value::as_str).unwrap_or_default();
            let props = match row.get("props") {
                Some(p) => p,
                None => {
                    warn_conversation_context_row_skip(context_id, row, "missing_props");
                    continue;
                }
            };

            match label {
                "Message" => {
                    let node_id = row
                        .get("node_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let event_id = match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                        Some(id) if !id.is_empty() => id,
                        _ => {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "message_missing_activity_anchor",
                            );
                            continue;
                        }
                    };
                    let role = props
                        .get("a2a_role")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let content_raw = props.get("a2a_content").cloned().unwrap_or(Value::Null);
                    let content_value = json_value_from_embedded_string(&content_raw);
                    let text = normalize_message_content(&content_value);
                    let props_map = props.as_object().cloned().unwrap_or_default();
                    let is_host_ingress = role.eq_ignore_ascii_case("host")
                        || props_map.contains_key("a2a_host_ingress_kind");
                    if is_host_ingress {
                        if text.trim().is_empty()
                            && operational_from_host_message("", &props_map).is_none()
                        {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "host_ingress_empty",
                            );
                            continue;
                        }
                        let mut operational = operational_from_host_message(&text, &props_map)
                            .or_else(|| operational_from_host_message("", &props_map))
                            .unwrap_or_else(|| {
                                use baml_rt_conversation::operational::{
                                    OperationalEventContent, OperationalEventKind,
                                    OperationalEventSeverity,
                                };
                                OperationalEventContent {
                                    kind: OperationalEventKind::SourcePollRecorded,
                                    severity: OperationalEventSeverity::Info,
                                    summary: text.clone(),
                                    detail: None,
                                    agent_package: None,
                                    agent_instance_id: None,
                                    failure_class: None,
                                    failure_evidence: None,
                                    old_status: None,
                                    new_status: None,
                                }
                            });
                        if let Some((package, instance)) = dispatch_target_by_message.get(node_id) {
                            operational.agent_package = Some(package.clone());
                            operational.agent_instance_id = Some(instance.clone());
                        }
                        items.push(ProvenanceConversationContextItem {
                            attribution: Some(Box::new(EventAttribution {
                                event_kind: EventKind::IngressSignal,
                                context_id: context_id.clone(),
                                task_id: task_id_from_props(props),
                                actor: None,
                                target: None,
                                source_identity: host_ingress_source_identity(props),
                            })),
                            timestamp_ms: stored_event_order(props),
                            activity_anchor: ActivityAnchorId::from(event_id),
                            role: "host".to_string(),
                            content: ConversationItemContent::Operational(operational),
                            user_speaker_kind: None,
                        });
                        continue;
                    }
                    if text.trim().is_empty() {
                        warn_conversation_context_row_skip(context_id, row, "message_empty_text");
                        continue;
                    }
                    // Look up CITED edges for this Message node to get citations.
                    let citations = message_citations_map
                        .get(node_id)
                        .cloned()
                        .unwrap_or_default();
                    let history_role = conversation_history_role_for_message(role);
                    let user_speaker_kind = resolve_user_speaker_kind_for_message(
                        context_id,
                        props,
                        event_id,
                        history_role.as_str(),
                        role,
                    );
                    items.push(ProvenanceConversationContextItem {
                        attribution: Some(Box::new({
                            // A chat turn is an ingress signal or an emitted signal,
                            // resolved from the graph-owned agent rather than from the
                            // stored chat role or the direction-dependent edge.
                            let ownership = message_ownership.get(node_id);
                            let sent = ownership.is_some_and(|o| o.sent);
                            EventAttribution {
                                event_kind: if sent {
                                    EventKind::EmittedSignal
                                } else {
                                    EventKind::IngressSignal
                                },
                                context_id: context_id.clone(),
                                task_id: task_id_from_props(props),
                                actor: ownership.filter(|o| o.sent).map(|o| o.agent.clone()),
                                target: ownership.filter(|o| !o.sent).map(|o| o.agent.clone()),
                                source_identity: None,
                            }
                        })),
                        timestamp_ms: stored_event_order(props),
                        activity_anchor: ActivityAnchorId::from(event_id),
                        role: history_role,
                        content: ConversationItemContent::Message { text, citations },
                        user_speaker_kind,
                    });
                }
                "ToolCall" => {
                    let node_id = row
                        .get("node_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let event_id_str =
                        match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                            Some(id) if !id.is_empty() => id,
                            _ => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "tool_call_missing_activity_anchor",
                                );
                                continue;
                            }
                        };
                    let tool_name = match props.get("a2a_tool_name").and_then(Value::as_str) {
                        Some(name) if !name.is_empty() => name.to_string(),
                        _ => {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "tool_call_missing_tool_name",
                            );
                            continue;
                        }
                    };

                    // When present, enforce ToolCall→ToolArgs WAS_USED_BY topology. A missing edge does
                    // not invalidate the row — some writers attach arguments only via payloads or node
                    // metadata without a separate ToolArgs vertex.
                    if let Some((prov_role, prov_type)) = tool_call_edge_info.get(node_id) {
                        let role_ok = prov_role.is_empty() || prov_role == "a2a:args";
                        let type_ok = prov_type.is_empty() || prov_type == "a2a:ToolArgs";
                        if !role_ok || !type_ok {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "tool_call_invalid_tool_args_edge",
                            );
                            continue;
                        }
                    }

                    let metadata: Value = props
                        .get("a2a_metadata")
                        .and_then(parse_json_object_field)
                        .unwrap_or(Value::Object(Map::new()));
                    let metadata_args = metadata
                        .get("args")
                        .cloned()
                        .unwrap_or(Value::Object(Map::new()));

                    let tool_call_payload =
                        payload_map.get(&payload_id_for(event_id_str, "tool_call"));
                    let tool_result_payload =
                        payload_map.get(&payload_id_for(event_id_str, "tool_result"));

                    let (args, phase) = if let Some(payload) = tool_call_payload {
                        let parsed: Value =
                            serde_json::from_str(&payload.payload_json).unwrap_or(Value::Null);
                        let args = parsed
                            .get("args")
                            .cloned()
                            .unwrap_or_else(|| metadata_args.clone());
                        let phase_label = parsed
                            .get("phase")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_default();
                        (
                            args,
                            ToolSessionPhase::from_metadata(
                                &serde_json::json!({ "phase": phase_label }),
                            ),
                        )
                    } else {
                        (metadata_args, ToolSessionPhase::from_metadata(&metadata))
                    };

                    let (result, error) = if let Some(payload) = tool_result_payload {
                        let parsed: Value =
                            serde_json::from_str(&payload.payload_json).unwrap_or(Value::Null);
                        let result = parsed
                            .get("result")
                            .cloned()
                            .unwrap_or_else(|| parsed.clone());
                        let error = parsed.get("error").cloned();
                        (result, error)
                    } else {
                        let result = metadata
                            .get("result")
                            .cloned()
                            .unwrap_or(Value::Object(Map::new()));
                        let error = metadata_error(&metadata);
                        (result, error)
                    };

                    // Non-session (execute/unknown) invocations always surface so the UI can render
                    // tool cards even when args/results are empty. Session FSM phases are narrated
                    // by `SessionStep` rows in the graph; only session **Send** with non-empty step
                    // args is mirrored as ToolCall for `describe_invocation` (Open/Finish/etc. stay
                    // out of the raw tool-call pair).
                    let include_call = if phase.is_session_phase() {
                        matches!(phase, ToolSessionPhase::Send) && !is_empty_object(&args)
                    } else {
                        true
                    };

                    let tool_event_order = stored_event_order(props);

                    if include_call {
                        let tool_actor = agent_ref_from_props(props);
                        items.push(ProvenanceConversationContextItem {
                            attribution: Some(Box::new(EventAttribution {
                                event_kind: EventKind::ToolOperation,
                                context_id: context_id.clone(),
                                task_id: task_id_from_props(props),
                                actor: tool_actor.clone(),
                                target: None,
                                source_identity: None,
                            })),
                            timestamp_ms: tool_event_order,
                            activity_anchor: ActivityAnchorId::from(event_id_str),
                            role: "tool".to_string(),
                            content: ConversationItemContent::ToolCall(ToolCallContent {
                                tool_name: tool_name.clone(),
                                args,
                                fsm_phase: phase.clone(),
                            }),
                            user_speaker_kind: None,
                        });

                        let outcome = if let Some(error) = error {
                            ToolOutcome::Error(error)
                        } else if has_meaningful_result(&result)
                            && !is_session_bookkeeping_result(&phase, &result)
                        {
                            ToolOutcome::Result(result)
                        } else {
                            ToolOutcome::StatusOnly
                        };
                        items.push(ProvenanceConversationContextItem {
                            attribution: Some(Box::new(EventAttribution {
                                event_kind: EventKind::ToolOperation,
                                context_id: context_id.clone(),
                                task_id: task_id_from_props(props),
                                actor: tool_actor,
                                target: None,
                                source_identity: None,
                            })),
                            timestamp_ms: tool_event_order,
                            activity_anchor: ActivityAnchorId::from(event_id_str),
                            role: "tool".to_string(),
                            content: ConversationItemContent::ToolResult(ToolResultContent {
                                tool_name,
                                fsm_phase: phase,
                                outcome,
                            }),
                            user_speaker_kind: None,
                        });
                    }
                }
                "SessionStep" => {
                    let event_id = match props.get("a2a_activity_anchor").and_then(Value::as_str) {
                        Some(id) if !id.is_empty() => id.to_string(),
                        _ => {
                            warn_conversation_context_row_skip(
                                context_id,
                                row,
                                "session_step_missing_activity_anchor",
                            );
                            continue;
                        }
                    };
                    let tool_name = props
                        .get("a2a_tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let op_kind_raw = props
                        .get("op_kind")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let Some(op_kind) = ToolSessionStepOpKind::parse_graph(op_kind_raw) else {
                        warn_conversation_context_row_skip(
                            context_id,
                            row,
                            "session_step_unknown_op_kind",
                        );
                        continue;
                    };
                    let header = props
                        .get("header")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    let archive_ref = props
                        .get("archive_ref")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    let grep = props
                        .get("grep")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);

                    let op = match op_kind {
                        ToolSessionStepOpKind::Open => SessionStepOp::Open,
                        ToolSessionStepOpKind::SendDone => match (archive_ref, header) {
                            (Some(r), Some(hdr)) => {
                                let informed_by = props
                                    .get("informed_by_tool_activity_anchor")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                SessionStepOp::SendDone {
                                    archive_ref: r,
                                    header: hdr,
                                    informed_by,
                                }
                            }
                            _ => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "session_step_send_done_incomplete",
                                );
                                continue;
                            }
                        },
                        ToolSessionStepOpKind::SearchRead => match archive_ref {
                            Some(r) => {
                                let offset =
                                    props.get("offset").and_then(Value::as_u64).unwrap_or(0)
                                        as usize;
                                let limit = props
                                    .get("limit")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(DEFAULT_SESSION_READ_LINE_LIMIT as u64)
                                    as usize;
                                let Some(grep_pat) = grep else {
                                    warn_conversation_context_row_skip(
                                        context_id,
                                        row,
                                        "session_step_search_read_missing_grep",
                                    );
                                    continue;
                                };
                                SessionStepOp::SearchRead {
                                    archive_ref: r,
                                    grep: grep_pat,
                                    offset,
                                    limit,
                                }
                            }
                            None => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "session_step_search_read_missing_archive_ref",
                                );
                                continue;
                            }
                        },
                        ToolSessionStepOpKind::PageRead => match archive_ref {
                            Some(r) => {
                                let offset =
                                    props.get("offset").and_then(Value::as_u64).unwrap_or(0)
                                        as usize;
                                let limit = props
                                    .get("limit")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(DEFAULT_SESSION_READ_LINE_LIMIT as u64)
                                    as usize;
                                SessionStepOp::PageRead {
                                    archive_ref: r,
                                    offset,
                                    limit,
                                }
                            }
                            None => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "session_step_page_read_missing_archive_ref",
                                );
                                continue;
                            }
                        },
                        // Legacy graphs from before SearchRead/PageRead split.
                        ToolSessionStepOpKind::Read => match archive_ref {
                            Some(r) => {
                                if let Some(g) = grep.filter(|s| !s.is_empty()) {
                                    SessionStepOp::SearchRead {
                                        archive_ref: r,
                                        grep: g,
                                        offset: 0,
                                        limit: DEFAULT_SESSION_READ_LINE_LIMIT,
                                    }
                                } else {
                                    SessionStepOp::PageRead {
                                        archive_ref: r,
                                        offset: 0,
                                        limit: DEFAULT_SESSION_READ_LINE_LIMIT,
                                    }
                                }
                            }
                            None => {
                                warn_conversation_context_row_skip(
                                    context_id,
                                    row,
                                    "session_step_legacy_read_missing_archive_ref",
                                );
                                continue;
                            }
                        },
                    };

                    items.push(ProvenanceConversationContextItem {
                        attribution: Some(Box::new(EventAttribution {
                            event_kind: EventKind::ToolOperation,
                            context_id: context_id.clone(),
                            task_id: task_id_from_props(props),
                            actor: agent_ref_from_props(props),
                            target: None,
                            source_identity: None,
                        })),
                        timestamp_ms: stored_event_order(props),
                        activity_anchor: ActivityAnchorId::from(event_id.as_str()),
                        role: "tool".to_string(),
                        content: ConversationItemContent::SessionStep(SessionStepContent {
                            tool_name,
                            op,
                            send_done_replay_payload: None,
                            read_replay_payload: None,
                        }),
                        user_speaker_kind: None,
                    });
                }
                _ => {
                    if !label.is_empty() {
                        warn_conversation_context_row_skip(
                            context_id,
                            row,
                            "unsupported_or_unknown_label",
                        );
                    }
                    continue;
                }
            }
        }

        let mut items = ConversationContextBatch::from_graph_rows(items)
            .hydrate(self)
            .await?
            .canonicalize_suppress_covered_tool_rows()
            .into_items();

        let existing_anchors: std::collections::HashSet<String> = items
            .iter()
            .map(|i| i.activity_anchor.as_str().to_string())
            .collect();
        if enrich_extensions {
            let after_order = after_event_order.map(crate::observation::EventOrder);
            let mut supplement = self
                .load_transcript_extension_items(
                    context_id,
                    task_id,
                    after_order,
                    &existing_anchors,
                )
                .await?;
            items.append(&mut supplement);
        }

        items.sort_by(crate::observation::cmp_transcript_items);
        if let Some(n) = limit {
            if n == 0 {
                return Ok(Vec::new());
            }
            if items.len() > n {
                let had = items.len();
                tracing::debug!(
                    %context_id,
                    limit = n,
                    had,
                    forward = forward_limit,
                    "truncating conversation context to limit (after sort)"
                );
                if forward_limit {
                    items.truncate(n);
                } else {
                    items = items.split_off(items.len() - n);
                }
            }
        }
        Ok(items)
    }

    /// Delegations whose relayed message was persisted, keyed by
    /// `(caller AgentId, parent TaskId)`.
    ///
    /// Lineage is the only trusted source for this: text, target name, timestamp
    /// proximity, and parent task alone are explicitly not sufficient.
    async fn persisted_delegations(
        &self,
        context_id: &ContextId,
    ) -> Result<std::collections::HashSet<(String, String)>> {
        let ctx = context_id.as_str();
        // Stored property keys are the storage-safe underscore form; a
        // colon-bearing key never matches a row written by the normalizer.
        let q = format!(
            "SELECT node_id, props FROM {TBL_NODE} WHERE label = 'Delegation' \
             AND props.{} = $ctx",
            storage_safe::A2A_CONTEXT_ID
        );
        let rows: Vec<Value> = self
            .db
            .query(&q)
            .bind(("ctx", ctx.to_string()))
            .await
            .map_err(map_surreal_error)
            .and_then(|response| check_and_take_zero(response, map_surreal_error))?;
        if rows.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let mut by_child_task: Vec<(String, (String, String))> = Vec::new();
        for row in &rows {
            let props = row.get("props").cloned().unwrap_or(Value::Null);
            let child_task = props.get(storage_safe::A2A_TASK_ID).and_then(Value::as_str);
            let caller = props
                .get(storage_safe::A2A_AGENT_ID)
                .and_then(Value::as_str);
            let parent = props
                .get(storage_safe::A2A_DELEGATION_PARENT_TASK_ID)
                .and_then(Value::as_str);
            let (Some(child_task), Some(caller), Some(parent)) = (child_task, caller, parent)
            else {
                continue;
            };
            by_child_task.push((
                task_entity_id_string_raw(child_task),
                (caller.to_string(), parent.to_string()),
            ));
        }
        if by_child_task.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let task_list = by_child_task
            .iter()
            .map(|(entity, _)| format!("'{entity}'"))
            .collect::<Vec<_>>()
            .join(", ");
        // The delegated task has a persisted inbound message => the relay landed.
        let msg_q = format!(
            "SELECT from_id FROM {TBL_EDGE} WHERE rel_type = '{tm}' AND from_id IN [{task_list}]",
            tm = a2a_relations::TASK_MESSAGE
        );
        let msg_rows: Vec<Value> = self.query_sql_rows(&msg_q).await?;
        let with_message: std::collections::HashSet<&str> = msg_rows
            .iter()
            .filter_map(|row| row.get("from_id").and_then(Value::as_str))
            .collect();
        Ok(by_child_task
            .into_iter()
            .filter(|(entity, _)| with_message.contains(entity.as_str()))
            .map(|(_, key)| key)
            .collect())
    }

    /// Resolve each Message node to its owning stable agent.
    ///
    /// Ownership is the two-hop traversal the normalizer documents:
    /// `(m:Message) <-[:WAS_RECEIVED_BY|:WAS_EMITTED_BY]- (p:A2AMessageProcessing)
    ///  -[:WAS_EXECUTED_BY]-> (a:Agent)`.
    async fn message_agent_ownership(
        &self,
        message_node_ids: &[String],
    ) -> Result<HashMap<String, MessageAgentOwnership>> {
        if message_node_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let id_list = message_node_ids
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let received = semantic_labels::WAS_RECEIVED_BY;
        let emitted = semantic_labels::WAS_EMITTED_BY;
        // Received edges point at the Message, emitted edges point away from it.
        let edge_q = format!(
            "SELECT from_id, to_id, rel_type FROM {TBL_EDGE} \
             WHERE (rel_type = '{received}' AND to_label = 'Message' AND to_id IN [{id_list}]) \
                OR (rel_type = '{emitted}' AND from_label = 'Message' AND from_id IN [{id_list}])"
        );
        let edge_rows: Vec<Value> = self.query_sql_rows(&edge_q).await?;
        if edge_rows.is_empty() {
            return Ok(HashMap::new());
        }
        // message node -> (processing node, sent?)
        let mut by_processing: HashMap<String, (String, bool)> = HashMap::new();
        for row in &edge_rows {
            let from_id = row
                .get("from_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let to_id = row.get("to_id").and_then(Value::as_str).unwrap_or_default();
            let rel = row
                .get("rel_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if from_id.is_empty() || to_id.is_empty() {
                continue;
            }
            let (processing, message, sent) = if rel == emitted {
                (to_id, from_id, true)
            } else {
                (from_id, to_id, false)
            };
            by_processing.insert(processing.to_string(), (message.to_string(), sent));
        }
        if by_processing.is_empty() {
            return Ok(HashMap::new());
        }
        let processing_list = by_processing
            .keys()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let executed = semantic_labels::WAS_EXECUTED_BY;
        let executed_q = format!(
            "SELECT from_id, to_id FROM {TBL_EDGE} \
             WHERE rel_type = '{executed}' AND from_label = 'A2AMessageProcessing' \
               AND from_id IN [{processing_list}]"
        );
        let executed_rows: Vec<Value> = self.query_sql_rows(&executed_q).await?;
        // processing node -> agent node
        let mut agent_by_processing: HashMap<String, String> = HashMap::new();
        for row in &executed_rows {
            let from_id = row
                .get("from_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let to_id = row.get("to_id").and_then(Value::as_str).unwrap_or_default();
            if from_id.is_empty() || to_id.is_empty() {
                continue;
            }
            agent_by_processing.insert(from_id.to_string(), to_id.to_string());
        }
        if agent_by_processing.is_empty() {
            return Ok(HashMap::new());
        }
        let agent_list = {
            let mut ids: Vec<&str> = agent_by_processing.values().map(String::as_str).collect();
            ids.sort_unstable();
            ids.dedup();
            ids.iter()
                .map(|id| format!("'{id}'"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let agent_q = format!(
            "SELECT node_id, props FROM {TBL_NODE} WHERE label = 'Agent' AND node_id IN [{agent_list}]"
        );
        let agent_rows: Vec<Value> = self.query_sql_rows(&agent_q).await?;
        let mut agent_refs: HashMap<String, AgentRef> = HashMap::new();
        for row in &agent_rows {
            let node_id = row
                .get("node_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let props = row.get("props").cloned().unwrap_or(Value::Null);
            let Some(agent_id) = agent_id_from_agent_node(&props, node_id) else {
                continue;
            };
            let name = props
                .get(storage_safe::A2A_AGENT_TYPE)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            agent_refs.insert(node_id.to_string(), AgentRef { agent_id, name });
        }
        let mut out = HashMap::new();
        for (processing, (message, sent)) in by_processing {
            let Some(agent_node) = agent_by_processing.get(&processing) else {
                continue;
            };
            let Some(agent_ref) = agent_refs.get(agent_node) else {
                continue;
            };
            out.insert(
                message,
                MessageAgentOwnership {
                    agent: agent_ref.clone(),
                    sent,
                },
            );
        }
        Ok(out)
    }
}

/// Owning agent of one Message node, plus the direction it was carried in.
struct MessageAgentOwnership {
    agent: AgentRef,
    sent: bool,
}

fn agent_id_from_agent_node(props: &Value, node_id: &str) -> Option<AgentId> {
    let raw = props
        .get(storage_safe::A2A_AGENT_ID)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| node_id.strip_prefix("agent:").map(str::to_string))?;
    let uuid = baml_rt_core::ids::UuidId::parse_str(&raw).ok()?;
    Some(AgentId::from_uuid(uuid))
}

#[async_trait]
impl ProvenanceQueryApi for SurrealProvenanceStore {
    async fn query_context_messages(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
    ) -> Result<Vec<ProvenanceContextMessage>> {
        ProvenanceContextReader::context_messages(self, context_id, limit).await
    }

    async fn query_conversation_context(
        &self,
        context_id: &ContextId,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
        agent_package: Option<&str>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        self.compacted_conversation_context(context_id, limit, task_id, agent_package)
            .await
    }

    async fn query_conversation_context_after(
        &self,
        context_id: &ContextId,
        after_event_order: u64,
        limit: Option<usize>,
        task_id: Option<&TaskId>,
        agent_package: Option<&str>,
    ) -> Result<Vec<ProvenanceConversationContextItem>> {
        use crate::{
            conversation_context_query::DEFAULT_LLM_CONTEXT_ITEM_CAP,
            observation::{EventOrder, ObservationScope, TaskObservationScope, TemporalBound},
            read::{
                TranscriptEngine, TranscriptPageDirection, TranscriptPageRequest,
                TranscriptProjectionProfile,
            },
        };

        let scope = ObservationScope {
            context_id: context_id.clone(),
            task: match task_id {
                Some(id) => TaskObservationScope::Task(id.clone()),
                None => TaskObservationScope::ContextWide,
            },
            agent_package: agent_package.map(str::to_string),
            temporal: TemporalBound::After(EventOrder(after_event_order)),
        };
        let page = TranscriptEngine::page(
            self,
            TranscriptPageRequest {
                scope,
                limit: limit.unwrap_or(DEFAULT_LLM_CONTEXT_ITEM_CAP),
                profile: TranscriptProjectionProfile::AgentPromptIndex,
                direction: TranscriptPageDirection::Forward,
            },
        )
        .await?;
        Ok(page.items)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::sort_conversation_rows;

    #[test]
    fn sort_conversation_rows_orders_by_event_order_then_node_id() {
        let mut rows = vec![
            json!({ "event_order": 2, "node_id": "node:b", "props": {} }),
            json!({ "event_order": 1, "node_id": "node:z", "props": {} }),
            json!({ "event_order": 2, "node_id": "node:a", "props": {} }),
            json!({ "event_order": 1, "node_id": "node:a", "props": {} }),
        ];
        sort_conversation_rows(&mut rows);
        let keys: Vec<(u64, String)> = rows
            .iter()
            .map(|row| {
                (
                    row.get("event_order")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    row.get("node_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (1, "node:a".to_string()),
                (1, "node:z".to_string()),
                (2, "node:a".to_string()),
                (2, "node:b".to_string()),
            ],
            "rows must sort by (event_order, node_id) ascending"
        );
    }
}
