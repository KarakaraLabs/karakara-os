// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Projects provenance conversation rows into wire JSON for BAML tags.

use std::sync::Arc;

use async_trait::async_trait;
use baml_rt_conversation::view::ProvenanceConversationContextItem;
use baml_rt_core::{Result, context};
use baml_rt_provenance::{
    CompactionRequest, DEFAULT_LLM_CONTEXT_ITEM_CAP, ProvenanceReadPrincipal,
    STEP_EXECUTOR_DELTA_ROW_CAP, estimate_compaction_prompt_bytes,
};
use baml_rt_quickjs::baml_execution::ConversationContextProvider;
use baml_rt_tools::{
    prompt_projection::{
        ProjectedConversationRow, PromptProjectionItem, project_prompt_context,
        project_prompt_context_with_identity,
    },
    tools::ToolRegistry,
};
use serde_json::Value;

use crate::a2a_store::{ConversationContextSource, ProvenanceWriterConversationSource};

type BoxedArchiveReader = Box<dyn Fn(&str, Option<&str>, usize, usize) -> Option<String>>;

struct PromptEvaluationContext {
    item_count: usize,
    prompt_bytes: u64,
}

/// Projects [`ProvenanceConversationContextItem`] rows into **wire** conversation JSON.
pub struct ProjectingConversationContextProvider {
    source: Arc<dyn ConversationContextSource>,
    tool_registry: Arc<ToolRegistry>,
    provenance_store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
    archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
    compaction_subscriber: Option<Arc<baml_rt_provenance::ContextCompactionSubscriber>>,
}

impl ProjectingConversationContextProvider {
    pub fn new(
        source: Arc<dyn ConversationContextSource>,
        tool_registry: Arc<ToolRegistry>,
        provenance_store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
        archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
        compaction_subscriber: Option<Arc<baml_rt_provenance::ContextCompactionSubscriber>>,
    ) -> Self {
        Self {
            source,
            tool_registry,
            provenance_store,
            archive_ref_tables,
            compaction_subscriber,
        }
    }

    pub fn from_provenance_writer(
        writer: Arc<dyn baml_rt_provenance::ProvenanceWriter>,
        tool_registry: Arc<ToolRegistry>,
        provenance_store: Option<Arc<baml_rt_provenance::SurrealProvenanceStore>>,
        archive_ref_tables: Option<Arc<baml_rt_tools::archive_refs::ContextRefTables>>,
        compaction_subscriber: Option<Arc<baml_rt_provenance::ContextCompactionSubscriber>>,
    ) -> Self {
        let source: Arc<dyn ConversationContextSource> =
            Arc::new(ProvenanceWriterConversationSource::new(writer));
        Self::new(
            source,
            tool_registry,
            provenance_store,
            archive_ref_tables,
            compaction_subscriber,
        )
    }

    async fn load_prompt_evaluation(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Option<PromptEvaluationContext>> {
        let Some(store) = self.provenance_store.as_ref() else {
            return Ok(None);
        };
        let context_id = scope.context_id();
        let agent_id = scope.agent_id().clone();
        // Budget is measured on the same materialization the prompt path renders: this
        // agent's compacted (summary + bounded tail) or windowed view. Measuring raw history
        // here would keep tripping the emergency trigger after a summary already shrank the
        // prompt (P4 review finding 2).
        let agent_items = store
            .conversation_context_for_agent_prompt(
                context_id,
                &agent_id,
                Some(DEFAULT_LLM_CONTEXT_ITEM_CAP),
                None,
            )
            .await
            .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
        let item_count = agent_items.len();
        let prompt_bytes = estimate_compaction_prompt_bytes(
            store,
            context_id,
            &ProvenanceReadPrincipal::Agent(agent_id),
            &agent_items,
            self.tool_registry.as_ref(),
        )
        .await
        .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
            source: Box::new(e),
        })?;
        Ok(Some(PromptEvaluationContext {
            item_count,
            prompt_bytes,
        }))
    }

    async fn project_conversation_to_json(
        &self,
        scope: &context::RuntimeScope,
        item_limit: Option<usize>,
    ) -> Result<Option<Value>> {
        let context_id = scope.context_id();
        let task_id = scope.task_id_opt();
        let agent_id = scope.agent_id().clone();
        // Agent prompts read this agent's participation across every task in the
        // context, so the store path is agent-scoped and deliberately not
        // task-filtered. A task filter is still correct for replay and inspection.
        //
        // When this agent has a compaction head, the prompt is the summary plus the
        // recent tail (P4.7); otherwise it is the bounded newest window.
        let (items, truncated) = if let Some(store) = self.provenance_store.as_ref() {
            let head = store
                .latest_compaction_head(context_id, Some(&agent_id), None)
                .await
                .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
                    source: Box::new(e),
                })?;
            if head.is_some() {
                let items = store
                    .conversation_context_for_agent_prompt(context_id, &agent_id, item_limit, None)
                    .await
                    .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
                        source: Box::new(e),
                    })?;
                (items, false)
            } else {
                let page = store
                    .context_history_view(context_id, &agent_id, item_limit, None, None)
                    .await
                    .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
                        source: Box::new(e),
                    })?;
                (page.items, page.truncated)
            }
        } else {
            (
                self.source
                    .conversation_context_with_task(context_id, item_limit, task_id)
                    .await?,
                false,
            )
        };
        tracing::debug!(
            context_id = %context_id,
            agent_id = %agent_id,
            task_id = ?task_id.map(|t| t.as_str()),
            item_count = items.len(),
            item_limit = ?item_limit,
            truncated,
            "project_conversation_to_json: context source returned items"
        );
        if items.is_empty() {
            return Ok(None);
        }

        let mut projection_items = items
            .into_iter()
            .filter_map(|item| {
                baml_rt_conversation::projection::agent_view_projection_item(item, &agent_id)
            })
            .collect::<Vec<_>>();
        if truncated {
            projection_items.push(baml_rt_conversation::projection::truncation_marker(
                item_limit.unwrap_or_default(),
            ));
        }
        if projection_items.is_empty() {
            return Ok(None);
        }

        let context_id_str = context_id.as_str().to_string();

        let ref_table_arc = if let Some(store) = self.provenance_store.as_ref() {
            let principal = ProvenanceReadPrincipal::Agent(agent_id.clone());
            let prepared = baml_rt_provenance::prepare_ref_table_for_projection(
                store,
                context_id,
                &principal,
                &projection_items,
                self.tool_registry.as_ref(),
            )
            .await
            .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;
            if let Some(tables) = self.archive_ref_tables.as_ref() {
                // Merge into the existing cached table rather than replacing it. A wholesale
                // replace can discard a pending Send receipt inserted by a concurrent same-agent
                // operation (the step executor's per-hop read lock lets operations interleave).
                let cached = baml_rt_tools::archive_refs::get_or_create_ref_table(
                    tables,
                    &context_id_str,
                    agent_id.as_str(),
                );
                cached.merge_hydrated_snapshot(&prepared);
                cached
            } else {
                prepared
            }
        } else if let Some(tables) = self.archive_ref_tables.as_deref() {
            baml_rt_tools::archive_refs::get_or_create_ref_table(
                tables,
                &context_id_str,
                agent_id.as_str(),
            )
        } else {
            Arc::new(baml_rt_tools::archive_refs::RefTable::new())
        };

        let reader: Option<BoxedArchiveReader> = self.archive_ref_tables.clone().map(|t| {
            let ctx = context_id_str.clone();
            let aid = agent_id.as_str().to_string();
            let boxed: BoxedArchiveReader =
                Box::new(move |archive_ref_str, grep_str, offset, limit| {
                    let ref_table = baml_rt_tools::archive_refs::get_ref_table(&t, &ctx, &aid)?;
                    baml_rt_tools::archive_read::format_session_read_from_vtable(
                        &ref_table,
                        archive_ref_str,
                        grep_str,
                        offset,
                        limit,
                    )
                });
            boxed
        });

        Ok(Some(project_prompt_context(
            projection_items,
            self.tool_registry.as_ref(),
            &ref_table_arc,
            reader.as_deref(),
        )))
    }

    /// Project the complete (uncapped) agent view into identity-carrying rows for the
    /// step-executor intra-turn delta.
    async fn project_conversation_rows_for_delta(
        &self,
        scope: &context::RuntimeScope,
    ) -> Result<Vec<ProjectedConversationRow>> {
        let context_id = scope.context_id();
        let agent_id = scope.agent_id().clone();
        let Some(store) = self.provenance_store.as_ref() else {
            return Ok(Vec::new());
        };
        let page = store
            .context_history_view(
                context_id,
                &agent_id,
                Some(STEP_EXECUTOR_DELTA_ROW_CAP),
                None,
                None,
            )
            .await
            .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
                source: Box::new(e),
            })?;

        if page.items.is_empty() {
            return Ok(Vec::new());
        }

        let projection_items = page
            .items
            .into_iter()
            .filter_map(|item| {
                baml_rt_conversation::projection::agent_view_projection_item(item, &agent_id)
            })
            .collect::<Vec<_>>();

        let context_id_str = context_id.as_str().to_string();
        let principal = ProvenanceReadPrincipal::Agent(agent_id.clone());
        let prepared = baml_rt_provenance::prepare_ref_table_for_projection(
            store,
            context_id,
            &principal,
            &projection_items,
            self.tool_registry.as_ref(),
        )
        .await
        .map_err(|e| baml_rt_core::BamlRtError::ProvenanceContextRead {
            source: Box::new(e),
        })?;
        let ref_table_arc = if let Some(tables) = self.archive_ref_tables.as_ref() {
            // Merge, don't replace: a wholesale publish can drop a pending Send receipt that a
            // concurrent same-agent operation inserted after this snapshot's DB read.
            let cached = baml_rt_tools::archive_refs::get_or_create_ref_table(
                tables,
                &context_id_str,
                agent_id.as_str(),
            );
            cached.merge_hydrated_snapshot(&prepared);
            cached
        } else {
            prepared
        };

        let reader: Option<BoxedArchiveReader> = self.archive_ref_tables.clone().map(|t| {
            let ctx = context_id_str.clone();
            let aid = agent_id.as_str().to_string();
            let boxed: BoxedArchiveReader =
                Box::new(move |archive_ref_str, grep_str, offset, limit| {
                    let ref_table = baml_rt_tools::archive_refs::get_ref_table(&t, &ctx, &aid)?;
                    baml_rt_tools::archive_read::format_session_read_from_vtable(
                        &ref_table,
                        archive_ref_str,
                        grep_str,
                        offset,
                        limit,
                    )
                });
            boxed
        });

        Ok(project_prompt_context_with_identity(
            projection_items,
            self.tool_registry.as_ref(),
            &ref_table_arc,
            reader.as_deref(),
        ))
    }
}

#[async_trait]
impl ConversationContextProvider for ProjectingConversationContextProvider {
    async fn conversation_history_json(
        &self,
        scope: &context::RuntimeScope,
        _function_name: &str,
    ) -> Result<Option<Value>> {
        self.project_conversation_to_json(scope, Some(DEFAULT_LLM_CONTEXT_ITEM_CAP))
            .await
    }

    async fn conversation_history_json_for_llm(
        &self,
        scope: &context::RuntimeScope,
        function_name: &str,
    ) -> Result<Option<Value>> {
        let evaluation = self.load_prompt_evaluation(scope).await?;
        let needs_emergency = evaluation.as_ref().is_some_and(|ctx| {
            self.compaction_subscriber.as_ref().is_some_and(|sub| {
                sub.pre_model_exceeds_emergency(ctx.prompt_bytes, ctx.item_count, function_name)
            })
        });
        if needs_emergency {
            if let (Some(subscriber), Some(ctx)) =
                (self.compaction_subscriber.as_ref(), evaluation.as_ref())
            {
                let request = CompactionRequest {
                    context_id: scope.context_id().clone(),
                    agent_id: scope.agent_id().clone(),
                };
                subscriber
                    .evaluate_pre_model_emergency(
                        &request,
                        ctx.prompt_bytes,
                        ctx.item_count,
                        function_name,
                    )
                    .await;
            }
            return self
                .project_conversation_to_json(scope, Some(DEFAULT_LLM_CONTEXT_ITEM_CAP))
                .await;
        }
        self.project_conversation_to_json(scope, Some(DEFAULT_LLM_CONTEXT_ITEM_CAP))
            .await
    }

    async fn conversation_history_json_for_intra_dedup(
        &self,
        scope: &context::RuntimeScope,
        _function_name: &str,
    ) -> Result<Option<Value>> {
        self.project_conversation_to_json(scope, None).await
    }

    async fn conversation_history_rows_for_delta(
        &self,
        scope: &context::RuntimeScope,
        _function_name: &str,
    ) -> Result<Vec<ProjectedConversationRow>> {
        self.project_conversation_rows_for_delta(scope).await
    }
}

/// Convert a provenance conversation item to a projection item.
pub fn to_projection_item(item: ProvenanceConversationContextItem) -> Option<PromptProjectionItem> {
    baml_rt_conversation::provenance_item_to_projection_item(item)
}
