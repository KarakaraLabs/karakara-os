// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Observation event service: SSE subscribers for unified operator bundles.
//!
//! The host's provenance store emits one update for each context-scoped event it
//! commits, whichever agent or host component wrote it. This service only hands
//! out receivers, so it covers agents deployed after it was built and has no
//! lifetime of its own.

use std::sync::Arc;

use baml_rt_core::ObservationUpdate;
use baml_rt_provenance::SurrealProvenanceStore;
use tokio::sync::broadcast;

pub(crate) struct ObservationEventServiceImpl {
    store: Arc<SurrealProvenanceStore>,
}

impl ObservationEventServiceImpl {
    pub(crate) fn new(store: Arc<SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

impl baml_rt_api::ObservationEventService for ObservationEventServiceImpl {
    fn subscribe_updates(&self) -> broadcast::Receiver<ObservationUpdate> {
        self.store.subscribe_observations()
    }
}
