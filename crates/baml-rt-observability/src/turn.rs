// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Per-context turn ordinals for telemetry labels.
//!
//! A **turn** is one user-originated request. Internal agent-to-agent delegations
//! share the initiating turn, so coordinator and child metrics can be grouped under
//! the same ordinal while still being separable by `agent_package`.
//!
//! The counter is process-local and keyed by `context_id`. It is intentionally a
//! small integer (`1`, `2`, …) so it stays low-cardinality in Prometheus. `0` means
//! no turn has begun for the context yet.

use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
};

use baml_rt_core::ids::ContextId;

static TURNS: OnceLock<RwLock<HashMap<ContextId, u64>>> = OnceLock::new();

fn turns() -> &'static RwLock<HashMap<ContextId, u64>> {
    TURNS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Record a new user-originated turn for `context_id` and return its 1-based ordinal.
pub fn begin_turn(context_id: &ContextId) -> u64 {
    let mut guard = turns().write().unwrap_or_else(|e| e.into_inner());
    let counter = guard.entry(context_id.clone()).or_insert(0);
    *counter = counter.saturating_add(1);
    *counter
}

/// Current 1-based turn ordinal for `context_id`, or `0` when no turn has begun.
pub fn current_turn(context_id: &ContextId) -> u64 {
    turns()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(context_id)
        .copied()
        .unwrap_or(0)
}

/// Current turn ordinal rendered as a low-cardinality metric label.
pub fn current_turn_label(context_id: &ContextId) -> String {
    current_turn(context_id).to_string()
}

#[cfg(test)]
mod tests {
    use baml_rt_core::ids::ContextId;

    use super::{begin_turn, current_turn};

    #[test]
    fn turn_counter_is_per_context_and_monotonic() {
        let a = ContextId::new(1_000, 1);
        let b = ContextId::new(1_000, 2);
        assert_eq!(current_turn(&a), 0);
        assert_eq!(begin_turn(&a), 1);
        assert_eq!(begin_turn(&a), 2);
        assert_eq!(current_turn(&a), 2);
        assert_eq!(current_turn(&b), 0);
        assert_eq!(begin_turn(&b), 1);
        assert_eq!(current_turn(&a), 2);
    }
}
