// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! One runtime store per host, shared by every agent the host runs.
//!
//! Agents on a runner already share one provenance graph. Live task updates
//! must be shared too: a `tasks.subscribe` handled by any agent has to receive
//! the frames the task's owner commits. [`HostRuntimeStore`] therefore owns the
//! single [`TaskSubgraphStore`] and, through it, the single
//! [`TaskUpdateBroadcaster`] for the host.
//!
//! It holds no per-agent state. Each agent reaches it through its own view,
//! which adds the agent's identity (see `A2aAgentBuilder::with_host_store`).

use std::sync::Arc;

use baml_rt_core::{BamlRtError, Result};
use baml_rt_provenance::{
    ProvenanceWriter, SurrealProvenanceStore, SurrealStoreBuilder, TaskGraphReader,
};

use crate::{
    task_subgraph_store::TaskSubgraphStore, task_update_broadcaster::TaskUpdateBroadcaster,
};

/// The host's provenance graph plus its single task store and broadcaster.
pub struct HostRuntimeStore {
    provenance: Arc<SurrealProvenanceStore>,
    task_store: Arc<TaskSubgraphStore>,
}

impl HostRuntimeStore {
    /// Wrap the host's provenance graph. Build one per host and hand the same
    /// `Arc` to every agent; a second instance over the same graph would have
    /// its own broadcaster and miss the other's live frames.
    pub fn new(provenance: Arc<SurrealProvenanceStore>) -> Arc<Self> {
        let reader: Arc<dyn TaskGraphReader> = provenance.clone();
        let writer: Arc<dyn ProvenanceWriter> = provenance.clone();
        Arc::new(Self {
            task_store: Arc::new(TaskSubgraphStore::new(reader, writer)),
            provenance,
        })
    }

    /// A host over a fresh, isolated in-memory graph (tests and local defaults).
    pub async fn in_memory() -> Result<Arc<Self>> {
        let provenance = SurrealStoreBuilder::in_memory_isolated()
            .build()
            .await
            .map_err(|source| BamlRtError::InvalidArgumentWithSource {
                message: "failed to open in-memory provenance store".into(),
                source: Box::new(source),
            })?;
        Ok(Self::new(provenance))
    }

    pub fn provenance(&self) -> &Arc<SurrealProvenanceStore> {
        &self.provenance
    }

    pub fn task_store(&self) -> &Arc<TaskSubgraphStore> {
        &self.task_store
    }

    pub fn broadcaster(&self) -> Arc<TaskUpdateBroadcaster> {
        self.task_store.broadcaster()
    }
}
