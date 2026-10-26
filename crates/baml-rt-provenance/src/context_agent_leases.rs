// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Operational `(context_id, agent_id)` execution-ownership leases.
//!
//! A lease answers one question: which boot may execute turns for one
//! context-agent pair right now? The row is mutable operational state, not a
//! provenance node. It never appears in graph export, context history, or an
//! agent prompt.
//!
//! Semantic decisions taken while maintaining the lease (acquisition, takeover,
//! refusal) are appended to provenance as [`AuditDecision`] rows. A heartbeat,
//! refresh, or token touch is not a decision and is never audited.
//!
//! ## Fencing
//!
//! `fencing_token` is a per-pair counter. It increases on every ownership
//! change, including release and takeover. A write scoped to the pair carries
//! the token it observed. The token check runs in the same transaction as the
//! event write, so a paused old holder cannot resume after a takeover and land
//! rows in a transcript another boot now owns.

use baml_rt_core::ids::{ActivityAnchorId, AgentId, ContextId, UuidId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::{ProvenanceError, Result},
    events::AuditDecision,
    store::ProvenanceWriter,
    surreal_store::{SurrealProvenanceStore, map_surreal_error},
    surreal_tables::TBL_CONTEXT_AGENT_LEASES,
};

/// Default staleness window for a lease heartbeat.
///
/// The owning boot renews well inside this window. A boot that stops heartbeating
/// leaves its lease takable after this interval.
pub const DEFAULT_CONTEXT_AGENT_LEASE_STALE_AFTER_MS: u64 = 30_000;

/// The fence a writer must present for one context-agent pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextAgentFence {
    pub context_id: ContextId,
    pub agent_id: AgentId,
    pub boot_event_id: ActivityAnchorId,
    pub fencing_token: u64,
}

impl ContextAgentFence {
    pub fn new(
        context_id: ContextId,
        agent_id: AgentId,
        boot_event_id: ActivityAnchorId,
        fencing_token: u64,
    ) -> Self {
        Self {
            context_id,
            agent_id,
            boot_event_id,
            fencing_token,
        }
    }
}

/// Current operational owner of one context-agent pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextAgentLease {
    pub context_id: ContextId,
    pub agent_id: AgentId,
    pub boot_event_id: ActivityAnchorId,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub fencing_token: u64,
    pub heartbeat_ms: u64,
}

impl ContextAgentLease {
    #[must_use]
    pub fn fence(&self) -> ContextAgentFence {
        ContextAgentFence::new(
            self.context_id.clone(),
            self.agent_id.clone(),
            self.boot_event_id.clone(),
            self.fencing_token,
        )
    }
}

/// Request to acquire or keep ownership of one context-agent pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextAgentLeaseRequest {
    pub context_id: ContextId,
    pub agent_id: AgentId,
    pub boot_event_id: ActivityAnchorId,
    pub runner_id: String,
    pub endpoint: Option<String>,
    /// Observation time for this attempt. Injected so expiry is testable.
    pub now_ms: u64,
    /// A lease whose last heartbeat is at or before this instant is stale.
    pub stale_before_ms: u64,
}

impl ContextAgentLeaseRequest {
    #[must_use]
    pub fn new(
        context_id: ContextId,
        agent_id: AgentId,
        boot_event_id: ActivityAnchorId,
        runner_id: impl Into<String>,
        now_ms: u64,
    ) -> Self {
        let stale_before_ms = now_ms.saturating_sub(DEFAULT_CONTEXT_AGENT_LEASE_STALE_AFTER_MS);
        Self {
            context_id,
            agent_id,
            boot_event_id,
            runner_id: runner_id.into(),
            endpoint: None,
            now_ms,
            stale_before_ms,
        }
    }

    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    #[must_use]
    pub fn with_stale_after_ms(mut self, stale_after_ms: u64) -> Self {
        self.stale_before_ms = self.now_ms.saturating_sub(stale_after_ms);
        self
    }
}

/// What an acquisition call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseTransition {
    /// No row existed for the pair.
    Created,
    /// The requesting boot already held the lease; its heartbeat moved.
    Refreshed,
    /// Another boot's lease was released or stale, so the token advanced.
    TakenOver,
}

/// Result of [`SurrealProvenanceStore::acquire_context_agent_lease`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseAcquisition {
    /// The requesting boot now owns the pair.
    Acquired {
        lease: ContextAgentLease,
        transition: LeaseTransition,
    },
    /// A different boot still owns the pair. The caller must not execute.
    HeldByOther { lease: ContextAgentLease },
}

/// Stored row, including released tombstones.
///
/// A released row keeps `fencing_token` so the next acquisition advances past
/// every token an earlier holder could still be carrying.
#[derive(Debug, Clone)]
struct StoredLease {
    context_id: ContextId,
    agent_id: AgentId,
    boot_event_id: Option<ActivityAnchorId>,
    runner_id: String,
    endpoint: Option<String>,
    fencing_token: u64,
    heartbeat_ms: u64,
    released: bool,
}

impl StoredLease {
    fn into_live(self) -> Option<ContextAgentLease> {
        if self.released {
            return None;
        }
        let boot_event_id = self.boot_event_id?;
        Some(ContextAgentLease {
            context_id: self.context_id,
            agent_id: self.agent_id,
            boot_event_id,
            runner_id: self.runner_id,
            endpoint: self.endpoint,
            fencing_token: self.fencing_token,
            heartbeat_ms: self.heartbeat_ms,
        })
    }
}

const ACQUIRE_LEASE_SQL: &str = r#"
BEGIN;
LET $current = SELECT * FROM context_agent_leases
    WHERE context_id = $ctx AND agent_id = $agent LIMIT 1;
IF array::len($current) = 0 {
    CREATE context_agent_leases CONTENT {
        context_id: $ctx,
        agent_id: $agent,
        boot_event_id: $boot,
        runner_id: $runner,
        endpoint: $endpoint,
        fencing_token: 1,
        heartbeat_ms: $now,
        released: false
    };
} ELSE IF $current[0].released = false AND $current[0].boot_event_id = $boot {
    UPDATE context_agent_leases SET
        runner_id = $runner,
        endpoint = $endpoint,
        heartbeat_ms = $now
        WHERE context_id = $ctx AND agent_id = $agent;
} ELSE IF $current[0].released = true OR $current[0].heartbeat_ms <= $stale_before {
    UPDATE context_agent_leases SET
        boot_event_id = $boot,
        runner_id = $runner,
        endpoint = $endpoint,
        fencing_token = $current[0].fencing_token + 1,
        heartbeat_ms = $now,
        released = false
        WHERE context_id = $ctx AND agent_id = $agent;
} ELSE {
    SELECT * FROM context_agent_leases WHERE false;
};
COMMIT;
"#;

const RENEW_LEASE_SQL: &str = r#"
UPDATE context_agent_leases SET heartbeat_ms = $now
WHERE context_id = $ctx
  AND agent_id = $agent
  AND boot_event_id = $boot
  AND fencing_token = $token
  AND released = false;
"#;

const RELEASE_LEASE_SQL: &str = r#"
UPDATE context_agent_leases SET
    boot_event_id = NONE,
    fencing_token = fencing_token + 1,
    heartbeat_ms = $now,
    released = true
WHERE context_id = $ctx
  AND agent_id = $agent
  AND boot_event_id = $boot
  AND fencing_token = $token
  AND released = false;
"#;

fn parse_agent_id(raw: &str) -> Result<AgentId> {
    UuidId::parse_str(raw)
        .map(AgentId::from_uuid)
        .map_err(|error| ProvenanceError::InvalidEvent {
            activity_anchor: "context_agent_lease".to_string(),
            reason: format!("invalid agent_id {raw:?}: {error}"),
        })
}

fn parse_lease_row(row: &Value, context_id: &ContextId) -> Result<StoredLease> {
    let agent_id = row
        .get("agent_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: "context_agent_lease".to_string(),
            reason: "lease row missing agent_id".to_string(),
        })
        .and_then(parse_agent_id)?;
    let boot_event_id = row
        .get("boot_event_id")
        .and_then(Value::as_str)
        .map(ActivityAnchorId::from);
    let runner_id = row
        .get("runner_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let endpoint = row
        .get("endpoint")
        .and_then(Value::as_str)
        .map(str::to_string);
    let fencing_token = row
        .get("fencing_token")
        .and_then(Value::as_u64)
        .ok_or_else(|| ProvenanceError::InvalidEvent {
            activity_anchor: "context_agent_lease".to_string(),
            reason: "lease row missing fencing_token".to_string(),
        })?;
    let heartbeat_ms = row
        .get("heartbeat_ms")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let released = row
        .get("released")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(StoredLease {
        context_id: context_id.clone(),
        agent_id,
        boot_event_id,
        runner_id,
        endpoint,
        fencing_token,
        heartbeat_ms,
        released,
    })
}

impl SurrealProvenanceStore {
    async fn read_stored_context_agent_lease(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
    ) -> Result<Option<StoredLease>> {
        let sql = format!(
            "SELECT * FROM {TBL_CONTEXT_AGENT_LEASES} \
             WHERE context_id = $ctx AND agent_id = $agent LIMIT 1"
        );
        let response = self
            .db()
            .query(&sql)
            .bind(("ctx", context_id.as_str().to_string()))
            .bind(("agent", agent_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> =
            crate::surreal_store::check_and_take_zero(response, map_surreal_error)?;
        rows.first()
            .map(|row| parse_lease_row(row, context_id))
            .transpose()
    }

    /// Narrow operational read for routing. A released lease reads as absent.
    pub async fn read_context_agent_lease(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
    ) -> Result<Option<ContextAgentLease>> {
        let Some(stored) = self
            .read_stored_context_agent_lease(context_id, agent_id)
            .await?
        else {
            return Ok(None);
        };
        Ok(stored.into_live())
    }

    /// True when `fence` still names the current owner and token.
    pub async fn context_agent_fence_is_current(&self, fence: &ContextAgentFence) -> Result<bool> {
        let current = self
            .read_context_agent_lease(&fence.context_id, &fence.agent_id)
            .await?;
        Ok(current.is_some_and(|lease| {
            lease.boot_event_id == fence.boot_event_id && lease.fencing_token == fence.fencing_token
        }))
    }

    /// Fail with the typed stale-fence error when this writer no longer owns
    /// the pair. Fenced transactions check this after commit, so a takeover
    /// that won the race turns the event write into a typed rejection.
    pub(crate) async fn ensure_context_agent_fence(&self, fence: &ContextAgentFence) -> Result<()> {
        if self.context_agent_fence_is_current(fence).await? {
            return Ok(());
        }
        Err(ProvenanceError::StaleContextAgentLease {
            context_id: fence.context_id.as_str().to_string(),
            agent_id: fence.agent_id.as_str().to_string(),
            boot_event_id: fence.boot_event_id.as_str().to_string(),
            fencing_token: fence.fencing_token,
        })
    }

    /// Acquire, refresh, or take over the pair lease.
    ///
    /// A live lease held by another boot returns [`LeaseAcquisition::HeldByOther`]
    /// without mutating anything. A released or stale lease is taken over and its
    /// fencing token advances.
    pub async fn acquire_context_agent_lease(
        &self,
        request: ContextAgentLeaseRequest,
    ) -> Result<LeaseAcquisition> {
        if request.runner_id.trim().is_empty() {
            return Err(ProvenanceError::InvalidEvent {
                activity_anchor: "context_agent_lease".to_string(),
                reason: "lease runner_id must be non-empty".to_string(),
            });
        }
        let before = self
            .read_stored_context_agent_lease(&request.context_id, &request.agent_id)
            .await?;

        self.run_lease_write(
            ACQUIRE_LEASE_SQL,
            vec![
                (
                    "ctx",
                    Value::String(request.context_id.as_str().to_string()),
                ),
                (
                    "agent",
                    Value::String(request.agent_id.as_str().to_string()),
                ),
                (
                    "boot",
                    Value::String(request.boot_event_id.as_str().to_string()),
                ),
                ("runner", Value::String(request.runner_id.clone())),
                (
                    "endpoint",
                    request
                        .endpoint
                        .clone()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                ("now", Value::from(request.now_ms)),
                ("stale_before", Value::from(request.stale_before_ms)),
            ],
        )
        .await?;

        let after = self
            .read_stored_context_agent_lease(&request.context_id, &request.agent_id)
            .await?
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: "context_agent_lease".to_string(),
                reason: "lease acquisition committed no row".to_string(),
            })?;
        let holds_pair =
            !after.released && after.boot_event_id.as_ref() == Some(&request.boot_event_id);

        if !holds_pair {
            let owner_boot_event_id =
                after
                    .boot_event_id
                    .clone()
                    .ok_or_else(|| ProvenanceError::InvalidEvent {
                        activity_anchor: "context_agent_lease".to_string(),
                        reason: "live lease row has no boot_event_id".to_string(),
                    })?;
            let lease = after
                .clone()
                .into_live()
                .ok_or_else(|| ProvenanceError::InvalidEvent {
                    activity_anchor: "context_agent_lease".to_string(),
                    reason: "live lease row could not be decoded".to_string(),
                })?;
            self.record_audit_decision(
                &request.context_id,
                AuditDecision::PairServedElsewhere {
                    agent_id: request.agent_id.clone(),
                    owner_boot_event_id,
                    requesting_boot_event_id: request.boot_event_id.clone(),
                },
            )
            .await?;
            return Ok(LeaseAcquisition::HeldByOther { lease });
        }

        let transition = match before.as_ref() {
            None => LeaseTransition::Created,
            Some(row)
                if !row.released && row.boot_event_id.as_ref() == Some(&request.boot_event_id) =>
            {
                LeaseTransition::Refreshed
            }
            Some(_) => LeaseTransition::TakenOver,
        };

        let lease = after
            .into_live()
            .ok_or_else(|| ProvenanceError::InvalidEvent {
                activity_anchor: "context_agent_lease".to_string(),
                reason: "acquired lease row could not be decoded".to_string(),
            })?;

        match transition {
            LeaseTransition::Created => {
                self.record_audit_decision(
                    &request.context_id,
                    AuditDecision::OwnershipAcquired {
                        agent_id: request.agent_id.clone(),
                        boot_event_id: request.boot_event_id.clone(),
                        runner_id: request.runner_id.clone(),
                    },
                )
                .await?;
            }
            LeaseTransition::TakenOver => {
                let from_boot_event_id = before
                    .as_ref()
                    .and_then(|row| row.boot_event_id.clone())
                    .ok_or_else(|| ProvenanceError::InvalidEvent {
                        activity_anchor: "context_agent_lease".to_string(),
                        reason: "takeover requires a previous boot".to_string(),
                    })?;
                let reason = if before.as_ref().is_some_and(|row| row.released) {
                    "lease_released"
                } else {
                    "lease_expired"
                };
                self.record_audit_decision(
                    &request.context_id,
                    AuditDecision::OwnershipTakenOver {
                        agent_id: request.agent_id.clone(),
                        from_boot_event_id,
                        to_boot_event_id: request.boot_event_id.clone(),
                        reason: reason.to_string(),
                    },
                )
                .await?;
            }
            LeaseTransition::Refreshed => {}
        }

        Ok(LeaseAcquisition::Acquired { lease, transition })
    }

    /// Refresh the heartbeat for the current holder. A stale token is a no-op.
    pub async fn renew_context_agent_lease(
        &self,
        fence: &ContextAgentFence,
        now_ms: u64,
    ) -> Result<Option<ContextAgentLease>> {
        self.run_lease_write(
            RENEW_LEASE_SQL,
            vec![
                ("ctx", Value::String(fence.context_id.as_str().to_string())),
                ("agent", Value::String(fence.agent_id.as_str().to_string())),
                (
                    "boot",
                    Value::String(fence.boot_event_id.as_str().to_string()),
                ),
                ("token", Value::from(fence.fencing_token)),
                ("now", Value::from(now_ms)),
            ],
        )
        .await?;
        let current = self
            .read_context_agent_lease(&fence.context_id, &fence.agent_id)
            .await?;
        Ok(current.filter(|lease| {
            lease.boot_event_id == fence.boot_event_id && lease.fencing_token == fence.fencing_token
        }))
    }

    /// Release the pair and advance the token so the old fence cannot write.
    ///
    /// Idempotent: a release from a boot that no longer holds the pair does
    /// nothing.
    pub async fn release_context_agent_lease(
        &self,
        fence: &ContextAgentFence,
        now_ms: u64,
    ) -> Result<()> {
        self.run_lease_write(
            RELEASE_LEASE_SQL,
            vec![
                ("ctx", Value::String(fence.context_id.as_str().to_string())),
                ("agent", Value::String(fence.agent_id.as_str().to_string())),
                (
                    "boot",
                    Value::String(fence.boot_event_id.as_str().to_string()),
                ),
                ("token", Value::from(fence.fencing_token)),
                ("now", Value::from(now_ms)),
            ],
        )
        .await
    }

    async fn record_audit_decision(
        &self,
        context_id: &ContextId,
        decision: AuditDecision,
    ) -> Result<()> {
        let event = crate::events::ProvEvent::audit_decision(context_id.clone(), decision);
        self.add_event(event).await
    }

    /// Execute one lease mutation transaction with the shared MVCC retry policy.
    ///
    /// Lease acquisition can race its first insert, so duplicate-record writes
    /// are retried like transaction conflicts: the retry then takes the update
    /// branch and converges on one row.
    async fn run_lease_write(&self, sql: &str, binds: Vec<(&str, Value)>) -> Result<()> {
        for attempt in 0..crate::surreal_store::WRITE_CONFLICT_MAX_ATTEMPTS {
            let mut query = self.db().query(sql);
            for (name, value) in &binds {
                query = query.bind(((*name).to_string(), value.clone()));
            }
            match query.await.and_then(surrealdb::IndexedResults::check) {
                Ok(_) => return Ok(()),
                Err(error)
                    if crate::surreal_store::is_transaction_conflict(&error)
                        || crate::surreal_store::is_duplicate_record_write(&error) =>
                {
                    if attempt + 1 >= crate::surreal_store::WRITE_CONFLICT_MAX_ATTEMPTS {
                        return Err(ProvenanceError::Contention {
                            details: error.message().to_string(),
                        });
                    }
                    tokio::time::sleep(crate::surreal_store::jittered_backoff(attempt)).await;
                }
                Err(error) => return Err(map_surreal_error(error)),
            }
        }
        unreachable!("lease retry loop returns on every iteration");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_and_lease_share_pair_identity() {
        let context_id = ContextId::from("ctx-lease");
        let agent_id =
            AgentId::from_uuid(UuidId::parse_str("00000000-0000-0000-0000-000000000001").unwrap());
        let lease = ContextAgentLease {
            context_id: context_id.clone(),
            agent_id: agent_id.clone(),
            boot_event_id: ActivityAnchorId::from("prov-1"),
            runner_id: "runner-a".to_string(),
            endpoint: None,
            fencing_token: 7,
            heartbeat_ms: 1000,
        };
        let fence = lease.fence();
        assert_eq!(fence.context_id, context_id);
        assert_eq!(fence.agent_id, agent_id);
        assert_eq!(fence.fencing_token, 7);
    }
}
