// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Rebuild a session [`RefTable`] from durable provenance (graph-backed refs).

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Instant,
};

use baml_rt_conversation::view::{ConversationItemContent, ProvenanceConversationContextItem};
use baml_rt_core::ids::{AgentId, ContextId};
use baml_rt_tools::{
    archive_read::ShortRef,
    archive_refs::{HistoryEntry, RefTable},
    prompt_projection::{PromptProjectionContent, PromptProjectionItem},
};

use super::SurrealProvenanceStore;
use crate::{
    error::Result,
    store::{ProvenanceContextReader, ProvenanceReadPrincipal},
    surreal_tables::TBL_ARCHIVE_BODY,
};

/// Load archive bodies for a context into `table`, scoped to an owner when `agent_id` is set.
///
/// `None` loads every body (operator hydration). A sibling body is never loaded into an agent's
/// execution ref table.
async fn hydrate_archive_bodies(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    agent_id: Option<&AgentId>,
    table: &RefTable,
    // `None` hydrates everything (operator). `Some(set)` hydrates only those `archive_local`
    // values, which for an agent is the set its prompt can actually reference.
    only_locals: Option<&std::collections::HashSet<u32>>,
) -> Result<()> {
    if let Some(locals) = only_locals
        && locals.is_empty()
    {
        return Ok(());
    }
    use serde_json::Value;

    use super::{
        archive_ref::entry_from_json,
        helpers::{check_and_take_zero, map_surreal_error},
    };

    let ctx = context_id.as_str();
    let locals_filter = if only_locals.is_some() {
        " AND archive_local IN $locals"
    } else {
        ""
    };
    let q = match agent_id {
        Some(_) => format!(
            "SELECT archive_prefix, archive_local, entry FROM {TBL_ARCHIVE_BODY} \
             WHERE context_id = $ctx AND agent_id = $aid{locals_filter}"
        ),
        None => format!(
            "SELECT archive_prefix, archive_local, entry FROM {TBL_ARCHIVE_BODY} \
             WHERE context_id = $ctx{locals_filter}"
        ),
    };
    let mut query = store.db().query(&q).bind(("ctx", ctx.to_string()));
    if let Some(agent_id) = agent_id {
        query = query.bind(("aid", agent_id.as_str().to_string()));
    }
    if let Some(locals) = only_locals {
        query = query.bind(("locals", locals.iter().copied().collect::<Vec<u32>>()));
    }
    let response = query.await.map_err(map_surreal_error)?;
    let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
    for row in rows {
        let prefix = row
            .get("archive_prefix")
            .and_then(|x| x.as_u64())
            .unwrap_or(1);
        let local = row
            .get("archive_local")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let prefix = u32::try_from(prefix).unwrap_or(1);
        let local = u32::try_from(local).unwrap_or(0);
        if local == 0 {
            continue;
        }
        let archive_ref = ShortRef::new_prefixed(prefix, local);
        let Some(entry_json) = row.get("entry") else {
            continue;
        };
        if let Ok(entry) = entry_from_json(entry_json) {
            table.insert_virtual_archive_ref(archive_ref, entry);
        }
    }
    Ok(())
}

/// Rebuild [`RefTable`] from Surreal history registry + archive bodies.
///
/// `known_text` maps `activity_anchor -> text` for anchors the caller already has
/// in hand (the prompt projection it just built).
///
/// - [`ProvenanceReadPrincipal::Agent`] hydrates only the reader's archive bodies and only the
///   `#N` rows whose anchors are in the reader's projection. Sibling bodies and sibling history
///   refs never enter the table.
/// - [`ProvenanceReadPrincipal::Operator`] hydrates every body and every `#N` row, falling back
///   to a full conversation read for anchors outside the projection cap.
pub async fn hydrate_ref_table(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    principal: &ProvenanceReadPrincipal,
    known_text: &HashMap<String, String>,
) -> Result<Arc<RefTable>> {
    let table = Arc::new(RefTable::new());
    // Agent hydration only needs the registry rows for anchors it can cite, and only the archive
    // bodies it can reference. Operator hydration stays complete by design: the observation routes
    // render from this path and must see every row and every body.
    let rows = match principal {
        ProvenanceReadPrincipal::Agent(_) => {
            let anchors: Vec<String> = known_text.keys().cloned().collect();
            let started = Instant::now();
            let rows = store
                .history_ref_list_for_anchors(context_id, &anchors)
                .await?;
            baml_rt_observability::metrics::record_context_projection_stage(
                "ref_history",
                started.elapsed(),
            );
            rows
        }
        ProvenanceReadPrincipal::Operator => {
            let started = Instant::now();
            let rows = store.history_ref_list_for_context(context_id).await?;
            baml_rt_observability::metrics::record_context_projection_stage(
                "ref_history",
                started.elapsed(),
            );
            rows
        }
    };

    let history_ref_count = u64::try_from(rows.len()).unwrap_or(u64::MAX);

    match principal {
        ProvenanceReadPrincipal::Operator => {
            // Only registry anchors absent from `known_text` — history beyond the projection cap —
            // force a conversation re-read. In the common case the read is skipped entirely.
            let needs_read = rows
                .iter()
                .any(|(anchor, _, _)| !known_text.contains_key(anchor.as_str()));

            let fallback_text: HashMap<String, String> = if needs_read {
                let items = store.conversation_context(context_id, None).await?;
                let mut map: HashMap<String, String> = HashMap::with_capacity(items.len());
                for item in &items {
                    if let Some(text) = conversation_item_history_text(item) {
                        map.entry(item.activity_anchor.as_str().to_owned())
                            .or_insert(text);
                    }
                }
                map
            } else {
                HashMap::new()
            };

            for (anchor, source, n) in rows {
                let text = known_text
                    .get(anchor.as_str())
                    .or_else(|| fallback_text.get(anchor.as_str()))
                    .cloned()
                    .unwrap_or_default();
                table.insert_virtual_history(n, HistoryEntry::new(anchor, source), text);
            }
            let started = Instant::now();
            hydrate_archive_bodies(store, context_id, None, table.as_ref(), None).await?;
            baml_rt_observability::metrics::record_context_projection_stage(
                "ref_archives",
                started.elapsed(),
            );
        }
        ProvenanceReadPrincipal::Agent(agent_id) => {
            // A prompt only shows the reader's window, so the reader can only cite `#N` values
            // whose anchors are in `known_text`. Sibling anchors are dropped, giving sparse `#N`
            // values without a schema change.
            for (anchor, source, n) in rows {
                if let Some(text) = known_text.get(anchor.as_str()) {
                    table.insert_virtual_history(
                        n,
                        HistoryEntry::new(anchor, source),
                        text.clone(),
                    );
                }
            }
            // Closure of the archive refs this prompt can reference: `@N` mentioned by the rows in
            // the window, plus every unconsumed Send receipt (the prompt advertises those bodies as
            // archived-but-not-visible, and the next hop may PageRead them).
            let mut locals: HashSet<u32> = archive_locals_in_texts(known_text.values());
            let pending_started = Instant::now();
            let pending = store
                .archive_pending_send_locals(context_id, agent_id)
                .await?;
            baml_rt_observability::metrics::record_context_projection_stage(
                "ref_pending_receipts",
                pending_started.elapsed(),
            );
            locals.extend(pending);
            let started = Instant::now();
            hydrate_archive_bodies(
                store,
                context_id,
                Some(agent_id),
                table.as_ref(),
                Some(&locals),
            )
            .await?;
            baml_rt_observability::metrics::record_context_projection_stage(
                "ref_archives",
                started.elapsed(),
            );
            baml_rt_observability::metrics::record_context_projection_volume(
                "archive_bodies",
                u64::try_from(locals.len()).unwrap_or(u64::MAX),
            );
        }
    }
    baml_rt_observability::metrics::record_context_projection_volume(
        "history_refs",
        history_ref_count,
    );
    Ok(table)
}

/// `@N` references mentioned by the text a prompt will actually carry.
fn archive_locals_in_texts<'a>(texts: impl Iterator<Item = &'a String>) -> HashSet<u32> {
    let mut out = HashSet::new();
    for text in texts {
        let bytes = text.as_bytes();
        let mut index = 0;
        while let Some(found) = text[index..].find('@') {
            let at = index + found;
            let rest = &text[at + 1..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty()
                && let Ok(local) = digits.parse::<u32>()
            {
                out.insert(local);
            }
            index = at + 1 + digits.len();
            if index >= bytes.len() {
                break;
            }
        }
    }
    out
}

fn conversation_item_history_text(item: &ProvenanceConversationContextItem) -> Option<String> {
    match &item.content {
        ConversationItemContent::Message { text, .. } => {
            let t = text.trim();
            if t.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        ConversationItemContent::ToolCall(tc) => Some(format!("{} {}", tc.tool_name, tc.args)),
        _ => None,
    }
}

/// DB-first ref table for prompt projection: sync `#N` registry, hydrate cache, overlay live text.
///
/// The `principal` scopes archive-body and history-ref hydration. Agent projections hydrate only
/// the reader's own rows; operator projections hydrate the full context.
pub async fn prepare_ref_table_for_projection(
    store: &SurrealProvenanceStore,
    context_id: &ContextId,
    principal: &ProvenanceReadPrincipal,
    projection_items: &[PromptProjectionItem],
    registry: &baml_rt_tools::tools::ToolRegistry,
) -> Result<Arc<RefTable>> {
    let mut history_entries = Vec::new();
    for item in projection_items {
        match &item.content {
            PromptProjectionContent::Message { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                history_entries.push((
                    item.activity_anchor.clone(),
                    "message".to_string(),
                    text.clone(),
                ));
            }
            PromptProjectionContent::ToolCall { tool_name, args } => {
                let mut desc =
                    registry.describe_invocation_with_hint(Some(tool_name.as_str()), args);
                if desc.trim().is_empty() {
                    desc = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
                }
                if desc.trim().is_empty() {
                    continue;
                }
                history_entries.push((item.activity_anchor.clone(), "tool_call".to_string(), desc));
            }
            PromptProjectionContent::Planning(plan) => {
                if plan.summary.trim().is_empty() {
                    continue;
                }
                let mut body = format!("[planning:{}] {}", plan.kind.as_wire_str(), plan.summary);
                if let Some(ref detail) = plan.detail
                    && !detail.trim().is_empty()
                {
                    body.push_str(&format!(" — {detail}"));
                }
                history_entries.push((item.activity_anchor.clone(), "plan".to_string(), body));
            }
            PromptProjectionContent::CompactionSummary { summary, .. } => {
                if summary.trim().is_empty() {
                    continue;
                }
                history_entries.push((
                    item.activity_anchor.clone(),
                    "compaction".to_string(),
                    summary.clone(),
                ));
            }
            _ => {}
        }
    }

    let sync_pairs: Vec<(String, String)> = history_entries
        .iter()
        .map(|(a, s, _)| (a.clone(), s.clone()))
        .collect();
    store
        .sync_history_refs_for_projection(context_id, &sync_pairs)
        .await?;

    // Text the caller already has for every in-projection anchor; lets
    // `hydrate_ref_table` skip the conversation re-read unless older (capped-out)
    // history needs hydrating. The overlay loop below re-asserts these anyway, so
    // last-wins on duplicate anchors is fine here.
    let known_text: HashMap<String, String> = history_entries
        .iter()
        .map(|(anchor, _source, content)| (anchor.as_str().to_owned(), content.clone()))
        .collect();

    let table = hydrate_ref_table(store, context_id, principal, &known_text).await?;
    for (anchor, source, content) in history_entries {
        let n = store
            .history_ref_ensure(context_id, anchor.as_str(), source.as_str())
            .await?;
        let entry = HistoryEntry::new(anchor, source);
        table.insert_virtual_history(n, entry, content);
    }
    Ok(table)
}
