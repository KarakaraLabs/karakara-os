// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Step-executor hop graph deltas: **provenance-backed** `conversation_context` line changes
//! (strict prefix extension or set-delta) for composing merged lines that become
//! `ctx.tags['conversation_transcript']`, using a loop-local `Vec<serde_json::Value>` that augments
//! the provider when the graph
//! lags a hop. No second copy of history on [`BamlRuntimeState`].

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use baml_rt_core::{Result, context};
use baml_rt_provenance::DEFAULT_LLM_CONTEXT_ITEM_CAP;
use baml_rt_tools::prompt_projection::ProjectedConversationRow;
use baml_types::BamlValue;
use serde_json::Value;
use tokio::sync::RwLock;

use super::BamlRuntimeManager;

/// New projected rows for this hop, compared by durable `activity_anchor` identity.
///
/// A sliding prompt window cannot break this: it compares the *complete* (uncapped)
/// agent view before and after the hop and returns the projected lines of rows whose
/// anchor appeared. Empty is valid — FSM-only hops (`Open`/`SendDone`, empty-args
/// `Send`) produce no new visible line, and that is no longer an error.
fn hop_rows_from_provider_delta(
    before: &[ProjectedConversationRow],
    after: &[ProjectedConversationRow],
) -> Vec<Value> {
    let before_anchors: HashSet<&str> = before
        .iter()
        .map(|row| row.activity_anchor.as_str())
        .collect();
    after
        .iter()
        .filter(|row| !before_anchors.contains(row.activity_anchor.as_str()))
        .map(|row| row.line.clone())
        .collect()
}

/// Capped provider projection first, then the loop supplement **in order**, appending
/// only lines whose [`Value`] is not already in the provider slice.
///
/// The graph row is authoritative: a `Value` already on the provider side must
/// not reappear from the supplement. With the awaitable
/// `ProvenanceEffectSubscriber` persisting before `emit` returns, the provider
/// read after each hop already reflects LLM-backed projection — the merge is
/// belt-and-braces against transient projection lag, not load-bearing.
/// Then tail to [`DEFAULT_LLM_CONTEXT_ITEM_CAP`].
pub(crate) fn append_intra_lines_to_provider_then_cap(
    mut prov: Vec<Value>,
    extra: impl IntoIterator<Item = Value>,
) -> Vec<Value> {
    for line in extra {
        if !prov.contains(&line) {
            prov.push(line);
        }
    }
    if prov.len() > DEFAULT_LLM_CONTEXT_ITEM_CAP {
        let take = prov.len() - DEFAULT_LLM_CONTEXT_ITEM_CAP;
        return prov.split_off(take);
    }
    prov
}

/// Append new projected lines for this completed hop to the step-executor loop's local
/// supplement (used at the next `invoke` when merged with the provider).
pub(crate) fn append_step_intra_deltas(
    supplement: &mut Vec<Value>,
    before: &[ProjectedConversationRow],
    after: &[ProjectedConversationRow],
) {
    supplement.extend(hop_rows_from_provider_delta(before, after));
}

/// Reads the complete (uncapped) projected agent view after a hop for the identity-based
/// step-executor delta.
///
/// [`BusWithEffects`](baml_rt_core::bus::BusWithEffects) awaits
/// [`EffectSubscriberTier::Awaitable`](baml_rt_core::bus::EffectSubscriberTier::Awaitable)
/// subscribers — notably `ProvenanceEffectSubscriber`, which persists the LLM completion row
/// that conversation projection consumes — before
/// [`EffectEmitter::emit`](baml_rt_core::bus::EffectEmitter::emit) returns, so the provider read
/// after each [`invoke_function_with_intra`](BamlRuntimeManager::invoke_function_with_intra) sees
/// LLM-backed projection without wall-clock polling.
pub(crate) async fn read_provider_conversation_rows_after_hop(
    manager: &Arc<RwLock<BamlRuntimeManager>>,
    scope: &context::RuntimeScope,
    function_name: &str,
) -> Result<Vec<ProjectedConversationRow>> {
    let g = manager.read().await;
    g.read_provider_conversation_rows(scope, function_name)
        .await
}

impl BamlRuntimeManager {
    /// Provider conversation lines plus an explicit step-executor local supplement, as JSON
    /// line objects in the same order and cap policy as the merged transcript source lines
    /// (supplement rows that match a line already in the provider are **not** repeated).
    ///
    /// Pass an empty `step_intra_supplement` for provider-only (same as graph projection
    /// for normal invokes). `step_intra_supplement` is the loop-owned buffer from
    /// [`run_step_executor_loop`](crate::step_executor_loop::run_step_executor_loop).
    pub async fn merged_conversation_history_lines_json(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        step_intra_supplement: &[Value],
    ) -> Result<Value> {
        let Some(ref exec) = self.state.executor else {
            return Ok(Value::Array(vec![]));
        };
        let prov = exec
            .provider_conversation_history_lines_for_llm(scope, function_name)
            .await?;
        let merged =
            append_intra_lines_to_provider_then_cap(prov, step_intra_supplement.iter().cloned());
        Ok(Value::Array(merged))
    }

    /// BAML conversation tags (transcript): graph provider + optional loop-local supplement
    /// (no duplicate `Value` lines vs the provider slice; same merge as
    /// `append_intra_lines_to_provider_then_cap` in this module). The catalog injection comes
    /// from the shared [`Self::enrich_with_tool_schema_prelude`] helper, so plain and
    /// step-executor invocations produce byte-identical `tool_schema_prelude` tags.
    pub(in crate::baml) async fn build_conversation_context_tags_with_intra(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
        step_intra_supplement: &[Value],
    ) -> Result<Option<HashMap<String, BamlValue>>> {
        let Some(ref exec) = self.state.executor else {
            return Ok(None);
        };
        let prov = exec
            .provider_conversation_history_lines_for_llm(scope, function_name)
            .await?;
        let merged =
            append_intra_lines_to_provider_then_cap(prov, step_intra_supplement.iter().cloned());
        let mut tags = exec.tags_from_merged_conversation_lines(merged)?;
        self.enrich_with_tool_schema_prelude(&mut tags);
        Ok(tags)
    }

    /// Complete projected graph rows with durable identities for the identity-based
    /// step-executor delta.
    pub(crate) async fn read_provider_conversation_rows(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
    ) -> Result<Vec<ProjectedConversationRow>> {
        let Some(ref exec) = self.state.executor else {
            return Ok(vec![]);
        };
        exec.provider_conversation_history_rows_for_delta(scope, function_name)
            .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value as JsonValue, json};

    use super::{append_intra_lines_to_provider_then_cap, hop_rows_from_provider_delta};

    fn row(anchor: &str, order: u64, line: JsonValue) -> super::ProjectedConversationRow {
        super::ProjectedConversationRow {
            activity_anchor: anchor.to_string(),
            event_order: order,
            line,
        }
    }

    #[test]
    fn append_intra_preserves_order() {
        let a = json!({"role":"a"});
        let b = json!({"role":"b"});
        let m = append_intra_lines_to_provider_then_cap(vec![a.clone()], [b.clone()]);
        assert_eq!(m, vec![a, b]);
    }

    #[test]
    fn append_skips_extra_when_same_value_as_provider() {
        let line = json!({"role":"x","content":"1"});
        let m = append_intra_lines_to_provider_then_cap(vec![line.clone()], [line]);
        assert_eq!(
            m.len(),
            1,
            "supplement does not repeat graph rows the provider already shows"
        );
    }

    #[test]
    fn hop_rows_appends_new_anchor_lines_in_order() {
        let before = [row("a", 1, json!({"role":"user","content":"#1 hi"}))];
        let after = [
            row("a", 1, json!({"role":"user","content":"#1 hi"})),
            row("b", 2, json!({"role":"tool","content":"#2 call"})),
        ];
        let appended = hop_rows_from_provider_delta(&before, &after);
        assert_eq!(appended, vec![json!({"role":"tool","content":"#2 call"})]);
    }

    #[test]
    fn hop_rows_ignores_reordering_and_re_rendering() {
        // Same anchors, different order and rendered text: identity-based delta is empty.
        let before = [
            row("a", 1, json!({"role":"user","content":"#1 hi"})),
            row("b", 2, json!({"role":"tool","content":"#2 call"})),
        ];
        let after = [
            row(
                "b",
                2,
                json!({"role":"tool","content":"#2 call (re-rendered)"}),
            ),
            row("a", 1, json!({"role":"user","content":"#1 hi"})),
        ];
        assert!(hop_rows_from_provider_delta(&before, &after).is_empty());
    }

    #[test]
    fn hop_rows_suppressed_row_without_new_anchor_is_empty_not_err() {
        // A row disappears (delegation dedup) with no new anchor: empty delta, no error.
        let before = [
            row("a", 1, json!({"role":"user","content":"#1 relayed"})),
            row("b", 2, json!({"role":"tool","content":"#2 internal_a2a"})),
            row("c", 3, json!({"role":"assistant","content":"#3 ok"})),
        ];
        let after = [
            row("a", 1, json!({"role":"user","content":"#1 relayed"})),
            row("c", 3, json!({"role":"assistant","content":"#3 ok"})),
        ];
        assert!(hop_rows_from_provider_delta(&before, &after).is_empty());
    }
}
