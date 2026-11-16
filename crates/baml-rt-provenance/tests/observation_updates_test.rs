// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! The store emits one operator observation update per committed event.
//!
//! Every writer (agents, host ingress, the task store, the runner) commits
//! through the store, so these tests write straight to it: what reaches the
//! operator stream depends only on what was committed, not on who wrote it.

use baml_rt_core::{
    ObservationUpdate,
    ids::{AgentId, ContextId, ExternalId, MessageId, TaskId, UuidId},
    observation::kinds,
};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceError, ProvenanceWriter, SurrealProvenanceStore,
    TaskAgentBinding, TaskAgentBindingSource,
};
use test_support::testing::provenance_fixtures::build_isolated_store;
use tokio::sync::broadcast::{Receiver, error::TryRecvError};
use uuid::Uuid;

fn fresh_agent() -> AgentId {
    AgentId::from_uuid(UuidId::new(Uuid::new_v4()))
}

async fn booted_agent(store: &SurrealProvenanceStore) -> AgentId {
    let agent = fresh_agent();
    store
        .add_event(ProvEvent::agent_booted(
            agent.clone(),
            AgentType::new("test_agent").expect("agent type"),
            "1.0.0".to_string(),
            "test@1.0.0".to_string(),
        ))
        .await
        .expect("agent booted");
    agent
}

/// The single update the last write produced; fails if there were none or more.
fn only_update(updates: &mut Receiver<ObservationUpdate>) -> ObservationUpdate {
    let update = updates.try_recv().expect("the write emits an update");
    assert_eq!(
        updates.try_recv(),
        Err(TryRecvError::Empty),
        "one write emits exactly one update"
    );
    update
}

fn assert_no_update(updates: &mut Receiver<ObservationUpdate>) {
    assert_eq!(updates.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn each_committed_event_emits_one_update_with_its_kinds() {
    let store = build_isolated_store().await;
    let mut updates = store.subscribe_observations();

    // A boot belongs to no context, so no operator view changes.
    let agent = booted_agent(&store).await;
    assert_no_update(&mut updates);

    let context = ContextId::from("ctx-observation-updates");
    let task = TaskId::from_external(ExternalId::new("task-observation-updates"));
    let transcript_and_ops = kinds::TRANSCRIPT | kinds::OPS;

    store
        .bind_task_executing_agent(
            TaskAgentBinding::new(
                context.clone(),
                task.clone(),
                agent.clone(),
                TaskAgentBindingSource::HostDispatchInvocation,
            )
            .expect("binding"),
        )
        .await
        .expect("bind task");
    // A binding commits TaskExists then TaskExecutionStarted.
    for _ in 0..2 {
        let update = updates.try_recv().expect("binding event update");
        assert_eq!(update.context_id, context.as_str());
        assert_eq!(update.task_id.as_deref(), Some(task.as_str()));
        assert_eq!(update.kinds, transcript_and_ops);
    }
    assert_no_update(&mut updates);

    store
        .add_event(ProvEvent::message_received_task(
            context.clone(),
            task.clone(),
            MessageId::from("observation-msg-in"),
            "ROLE_USER".to_string(),
            vec!["disk alert on node-7".to_string()],
            None,
            agent.clone(),
            1_780_000_000_000,
        ))
        .await
        .expect("message received");
    let update = only_update(&mut updates);
    assert_eq!(update.context_id, context.as_str());
    assert_eq!(update.task_id.as_deref(), Some(task.as_str()));
    assert_eq!(update.kinds, kinds::TRANSCRIPT);

    store
        .add_event(ProvEvent::message_sent_task(
            context.clone(),
            task.clone(),
            MessageId::from("observation-msg-out"),
            "ROLE_AGENT".to_string(),
            vec!["investigating".to_string()],
            None,
            agent.clone(),
            1_780_000_000_100,
            Vec::new(),
        ))
        .await
        .expect("message sent");
    assert_eq!(only_update(&mut updates).kinds, kinds::TRANSCRIPT);

    // Host ingress writes context-scoped events with no task.
    store
        .add_event(ProvEvent::host_source_poll_recorded(
            context.clone(),
            "slack".to_string(),
            "slack:C123".to_string(),
            "slack:1".to_string(),
            "host.source-records.v1".to_string(),
            0,
            None,
            vec![],
        ))
        .await
        .expect("source poll recorded");
    let update = only_update(&mut updates);
    assert_eq!(update.context_id, context.as_str());
    assert_eq!(update.task_id, None);
    assert_eq!(update.kinds, kinds::TRANSCRIPT);

    store
        .add_event(ProvEvent::task_status_changed(
            context.clone(),
            task.clone(),
            None,
            Some("working".to_string()),
        ))
        .await
        .expect("task status changed");
    assert_eq!(only_update(&mut updates).kinds, transcript_and_ops);
}

#[tokio::test]
async fn a_refused_write_emits_no_update() {
    let store = build_isolated_store().await;
    let owner = booted_agent(&store).await;
    let intruder = booted_agent(&store).await;
    let context = ContextId::from("ctx-observation-refused");
    let task = TaskId::from_external(ExternalId::new("task-observation-refused"));
    let binding = |agent: &AgentId| {
        TaskAgentBinding::new(
            context.clone(),
            task.clone(),
            agent.clone(),
            TaskAgentBindingSource::HostDispatchInvocation,
        )
        .expect("binding")
    };
    store
        .bind_task_executing_agent(binding(&owner))
        .await
        .expect("owner binds");

    let mut updates = store.subscribe_observations();
    let err = store
        .bind_task_executing_agent(binding(&intruder))
        .await
        .expect_err("another agent's binding is refused");
    assert!(
        matches!(err, ProvenanceError::TaskOwnedByAnotherAgent { .. }),
        "unexpected error: {err:?}"
    );
    assert_no_update(&mut updates);
}

#[tokio::test]
async fn every_subscriber_receives_every_update() {
    let store = build_isolated_store().await;
    let agent = booted_agent(&store).await;
    let mut early = store.subscribe_observations();
    let context = ContextId::from("ctx-observation-fanout");
    store
        .add_event(ProvEvent::host_source_poll_recorded(
            context.clone(),
            "slack".to_string(),
            "slack:C9".to_string(),
            "slack:1".to_string(),
            "host.source-records.v1".to_string(),
            0,
            None,
            vec![],
        ))
        .await
        .expect("first write");

    // A subscriber that joins later sees only what is committed after it joined.
    let mut late = store.subscribe_observations();
    store
        .add_event(ProvEvent::message_received_task(
            context.clone(),
            TaskId::from_external(ExternalId::new("task-observation-fanout")),
            MessageId::from("observation-fanout-msg"),
            "ROLE_USER".to_string(),
            vec!["second".to_string()],
            None,
            agent,
            1_780_000_000_200,
        ))
        .await
        .expect("second write");

    assert_eq!(early.try_recv().expect("first").task_id, None);
    assert!(early.try_recv().expect("second").task_id.is_some());
    assert_no_update(&mut early);
    assert!(only_update(&mut late).task_id.is_some());
}
