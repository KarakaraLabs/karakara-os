// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Observability helpers (metrics, spans, tracing setup).

pub mod metrics;
pub mod otel_env;
pub mod runner_identity;
pub mod scope;
pub mod spans;
pub mod tracing_setup;
pub mod turn;

#[cfg(test)]
pub(crate) mod test_env;

pub use metrics::*;
pub use otel_env::*;
pub use runner_identity::{
    INGRESS_SERVICE_INSTANCE_ID_BAGGAGE_KEY, SERVICE_INSTANCE_ID_KEY, UNKNOWN_SERVICE_INSTANCE_ID,
    derive_service_instance_id, pod_identity, service_instance_id,
};
pub use scope::*;
pub use spans::*;
pub use tracing_setup::*;
pub use turn::{begin_turn, current_turn, current_turn_label};
