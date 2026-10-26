// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Operational lease fencing and ownership-audit coverage.

use baml_rt_core::ids::{ActivityAnchorId, AgentId, ContextId, ExternalId, TaskId, UuidId};
use baml_rt_provenance::{
    ContextAgentLeaseRequest, LeaseAcquisition, LeaseTransition, ProvEvent, ProvenanceError,
    ProvenanceWriter,
};
use serde_json::Value;
use test_support::testing::provenance_fixtures::build_isolated_store;

const AGENT_UUID: &str = "00000000-0000-0000-0000-0000000000a7";

#[tokio::test]
async fn takeover_bumps_token_and_rejects_stale_fenced_write() {
    let store = build_isolated_store().await;
    let context_id = ContextId::from("ctx-lease-fencing");
    let agent_id = AgentId::from_uuid(UuidId::parse_str(AGENT_UUID).unwrap());
    let boot_a = ActivityAnchorId::from("prov-lease-a");
    let boot_b = ActivityAnchorId::from("prov-lease-b");
    let boot_c = ActivityAnchorId::from("prov-lease-c");

    let first = store
        .acquire_context_agent_lease(
            ContextAgentLeaseRequest::new(
                context_id.clone(),
                agent_id.clone(),
                boot_a.clone(),
                "runner-a",
                10_000,
            )
            .with_endpoint("http://runner-a:8080")
            .with_stale_after_ms(1_000),
        )
        .await
        .expect("first acquire");
    let LeaseAcquisition::Acquired {
        lease: first_lease,
        transition,
    } = first
    else {
        panic!("first acquisition must acquire the pair");
    };
    assert_eq!(transition, LeaseTransition::Created);
    assert_eq!(first_lease.fencing_token, 1);
    let stale_fence = first_lease.fence();

    // Before the heartbeat goes stale, another boot is refused without a write.
    let refused = store
        .acquire_context_agent_lease(
            ContextAgentLeaseRequest::new(
                context_id.clone(),
                agent_id.clone(),
                boot_c.clone(),
                "runner-c",
                10_500,
            )
            .with_stale_after_ms(1_000),
        )
        .await
        .expect("live holder refusal");
    assert!(matches!(refused, LeaseAcquisition::HeldByOther { .. }));

    // A stale heartbeat lets the second boot take over; the fence must advance.
    let second = store
        .acquire_context_agent_lease(
            ContextAgentLeaseRequest::new(
                context_id.clone(),
                agent_id.clone(),
                boot_b.clone(),
                "runner-b",
                20_000,
            )
            .with_stale_after_ms(1_000),
        )
        .await
        .expect("takeover");
    let LeaseAcquisition::Acquired {
        lease: second_lease,
        transition,
    } = second
    else {
        panic!("stale lease must be taken over");
    };
    assert_eq!(transition, LeaseTransition::TakenOver);
    assert_eq!(second_lease.boot_event_id, boot_b);
    assert_eq!(second_lease.fencing_token, first_lease.fencing_token + 1);

    let stale_event = ProvEvent::task_exists(
        context_id.clone(),
        TaskId::from_external(ExternalId::new("task-stale-write")),
    );
    let error = store
        .add_event_fenced(stale_event, stale_fence)
        .await
        .expect_err("pre-takeover fence must be rejected");
    assert!(
        matches!(error, ProvenanceError::StaleContextAgentLease { .. }),
        "unexpected stale-write error: {error:?}"
    );

    let current_event = ProvEvent::task_exists(
        context_id.clone(),
        TaskId::from_external(ExternalId::new("task-current-write")),
    );
    store
        .add_event_fenced(current_event, second_lease.fence())
        .await
        .expect("current fence writes");

    let mut response = store
        .db()
        .query(
            "SELECT * FROM prov_node \
             WHERE label = 'AuditDecision' ORDER BY props.a2a_event_order",
        )
        .await
        .expect("audit rows query");
    let rows: Vec<Value> = response.take(0).expect("audit rows");
    let kinds: Vec<&str> = rows
        .iter()
        .filter_map(|row| {
            row.get("props")
                .and_then(|props| props.get("audit_decision_kind"))
                .and_then(Value::as_str)
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "ownership_acquired",
            "pair_served_elsewhere",
            "ownership_taken_over",
        ]
    );
}
