// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-License-Identifier: Apache-2.0

use baml_rt_core::ids::{AgentId, ContextId, TaskId, UuidId};
use baml_rt_provenance::{
    AuditDecision, GateDecisionOutcome, GateDecisionWrite, GateFlowOrigin, GatePrincipal,
    ProvEvent, ProvenanceWriter,
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
            .decide_gate(
                &context_id,
                GateDecisionWrite {
                    gate_id: format!("gate-{index}"),
                    task_id: task_id(index),
                    agent_id: agent,
                    outcome: GateDecisionOutcome::Allow,
                    principal: GatePrincipal::Ingress {
                        ingress_key: "grafana:alert-1".to_string(),
                    },
                    reason: "trusted ingress".to_string(),
                },
            )
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

fn decision(index: usize, agent_id: AgentId, outcome: GateDecisionOutcome) -> GateDecisionWrite {
    GateDecisionWrite {
        gate_id: format!("gate-{index}"),
        task_id: task_id(index),
        agent_id,
        outcome,
        principal: match outcome {
            GateDecisionOutcome::Allow => GatePrincipal::Channel {
                channel: "C123".to_string(),
                actor: "U123".to_string(),
            },
            GateDecisionOutcome::Deny => GatePrincipal::Runtime {
                reason: "expiry".to_string(),
            },
        },
        reason: "test".to_string(),
    }
}

#[tokio::test]
async fn a_late_allow_cannot_replace_a_deny() {
    let store = build_isolated_store().await;
    let context_id = ContextId::new(702, 1);
    let agent = agent_id();

    let first = store
        .decide_gate(
            &context_id,
            decision(1, agent.clone(), GateDecisionOutcome::Deny),
        )
        .await
        .expect("deny");
    assert_eq!(first.outcome, GateDecisionOutcome::Deny);
    let second = store
        .decide_gate(&context_id, decision(1, agent, GateDecisionOutcome::Allow))
        .await
        .expect("late allow");
    assert_eq!(
        second.outcome,
        GateDecisionOutcome::Deny,
        "the late writer must get the decision that stands"
    );
    let stored = store
        .gate_ledger_decision(&context_id, "gate-1")
        .await
        .expect("read decision")
        .expect("decision");
    assert_eq!(stored.outcome, GateDecisionOutcome::Deny);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_decisions_all_see_one_outcome() {
    let store = build_isolated_store().await;
    let context_id = ContextId::new(702, 2);
    let agent = agent_id();
    let attempts = (0..8).map(|attempt| {
        let store = store.clone();
        let context_id = context_id.clone();
        let outcome = if attempt % 2 == 0 {
            GateDecisionOutcome::Allow
        } else {
            GateDecisionOutcome::Deny
        };
        let write = decision(2, agent.clone(), outcome);
        tokio::spawn(async move { store.decide_gate(&context_id, write).await })
    });
    let mut outcomes = Vec::new();
    for attempt in attempts {
        outcomes.push(attempt.await.expect("task").expect("decide").outcome);
    }
    let stored = store
        .gate_ledger_decision(&context_id, "gate-2")
        .await
        .expect("read decision")
        .expect("decision")
        .outcome;
    assert!(
        outcomes.iter().all(|outcome| *outcome == stored),
        "every writer must observe the stored outcome: {outcomes:?} vs {stored:?}"
    );
}

#[tokio::test]
async fn malformed_expired_requests_are_quarantined_and_do_not_block_the_sweep() {
    let store = build_isolated_store().await;
    let context_id = ContextId::new(703, 1);
    for index in 0..101 {
        store
            .db()
            .query(
                "CREATE prov_node CONTENT { node_id: $node_id, label: 'AuditDecision', props: { \
                   audit_decision_kind: 'gate_requested', gate_id: $gate_id, \
                   a2a_context_id: $context_id } }",
            )
            .bind(("node_id", format!("malformed-gate-{index}")))
            .bind(("gate_id", format!("gate-malformed-{index}")))
            .bind(("context_id", context_id.as_str().to_string()))
            .await
            .expect("insert malformed request")
            .check()
            .expect("malformed request write");
    }
    assert!(
        store
            .gate_ledger_request(&context_id, "gate-malformed-0")
            .await
            .is_err(),
        "a request without an expiry is corrupt ledger data, not an immediate expiry"
    );
    let pending_agent = agent_id();
    store
        .add_event(requested(&context_id, 900, pending_agent))
        .await
        .expect("valid request");

    // The first sweep quarantines the first 100 malformed rows; the next ones reach the
    // last malformed row and the valid gate.
    for _ in 0..3 {
        store.expire_pending_gate_requests(2).await.expect("sweep");
    }
    let quarantined = store
        .gate_ledger_decision(&context_id, "gate-malformed-0")
        .await
        .expect("read quarantined")
        .expect("quarantine decision");
    assert_eq!(quarantined.outcome, GateDecisionOutcome::Deny);
    let expired = store
        .gate_ledger_decision(&context_id, "gate-900")
        .await
        .expect("read expired")
        .expect("valid gate must still expire");
    assert_eq!(expired.outcome, GateDecisionOutcome::Deny);
    assert_eq!(
        store.expire_pending_gate_requests(2).await.expect("sweep"),
        0,
        "nothing left to sweep"
    );
}
