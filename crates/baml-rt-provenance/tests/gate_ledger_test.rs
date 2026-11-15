// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-License-Identifier: Apache-2.0

use baml_rt_core::ids::{AgentId, ContextId, TaskId, UuidId};
use baml_rt_provenance::{
    AuditDecision, GateDecisionOutcome, GateFlowOrigin, GatePrincipal, ProvEvent, ProvenanceWriter,
};
use test_support::testing::provenance_fixtures::build_isolated_store;
use uuid::Uuid;

fn agent_id() -> AgentId {
    AgentId::from_uuid(UuidId::new(Uuid::new_v4()))
}

fn task_id(index: usize) -> TaskId {
    TaskId::from_external(baml_rt_id::ExternalId::new(format!("gate-task-{index}")))
}

fn requested(context_id: &ContextId, index: usize, agent_id: AgentId) -> ProvEvent {
    ProvEvent::audit_decision(
        context_id.clone(),
        AuditDecision::GateRequested {
            gate_id: format!("gate-{index}"),
            task_id: task_id(index),
            agent_id,
            tool_name: "support/delete".to_string(),
            channel: None,
            expires_at_ms: 1,
        },
    )
}

#[tokio::test]
async fn expiry_sweep_skips_closed_rows_beyond_the_batch_limit() {
    let store = build_isolated_store().await;
    let context_id = ContextId::new(701, 1);

    for index in 0..101 {
        let agent = agent_id();
        store
            .add_event(requested(&context_id, index, agent.clone()))
            .await
            .expect("gate request");
        store
            .add_event(ProvEvent::audit_decision(
                context_id.clone(),
                AuditDecision::GateDecided {
                    gate_id: format!("gate-{index}"),
                    task_id: task_id(index),
                    agent_id: agent,
                    outcome: GateDecisionOutcome::Allow,
                    principal: GatePrincipal::Ingress {
                        ingress_key: "grafana:alert-1".to_string(),
                    },
                    reason: "trusted ingress".to_string(),
                },
            ))
            .await
            .expect("gate decision");
    }

    let pending_agent = agent_id();
    store
        .add_event(requested(&context_id, 101, pending_agent))
        .await
        .expect("pending gate request");

    assert_eq!(
        store
            .expire_pending_gate_requests(2)
            .await
            .expect("expiry sweep"),
        1,
        "closed requests must not consume the 100-row sweep"
    );
    let decision = store
        .gate_ledger_decision(&context_id, "gate-101")
        .await
        .expect("read expired decision")
        .expect("expired gate decision");
    assert_eq!(decision.outcome, GateDecisionOutcome::Deny);
    assert!(matches!(decision.principal, GatePrincipal::Runtime { .. }));
}

#[tokio::test]
async fn gate_origin_comes_from_graph_linked_ingress() {
    let store = build_isolated_store().await;
    let context_id = ContextId::new(701, 2);
    store
        .add_event(ProvEvent::host_source_poll_recorded_with_channel(
            context_id.clone(),
            "slack".to_string(),
            "slack:C123".to_string(),
            "slack:1".to_string(),
            "host.source-records.v1".to_string(),
            1,
            None,
            Some("C123".to_string()),
            Vec::new(),
        ))
        .await
        .expect("host ingress");

    assert_eq!(
        store
            .gate_flow_origin(&context_id)
            .await
            .expect("read gate origin"),
        GateFlowOrigin::Channel {
            channel: "C123".to_string()
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exactly_one_concurrent_caller_claims_a_gate_confirmation() {
    let store = build_isolated_store().await;
    let attempts = (0..8).map(|_| {
        let store = store.clone();
        tokio::spawn(async move { store.claim_gate_confirmation("gate-concurrent").await })
    });
    let mut winners = 0;
    for attempt in attempts {
        if attempt.await.expect("claim task").expect("claim") {
            winners += 1;
        }
    }
    assert_eq!(winners, 1, "exactly one caller may post the confirmation");
    assert!(
        !store
            .claim_gate_confirmation("gate-concurrent")
            .await
            .expect("late claim"),
        "a later caller must not post a second confirmation"
    );
    assert!(
        store
            .claim_gate_confirmation("gate-other")
            .await
            .expect("independent claim"),
        "claims for different gates are independent"
    );
}
