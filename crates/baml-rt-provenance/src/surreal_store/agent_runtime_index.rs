// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Agent runtime package → instance index (registry + in-process cache).

use std::collections::HashMap;

use baml_rt_core::ids::{AgentId, TaskId};
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{check_and_take_zero, map_surreal_error},
};
use crate::{
    error::Result,
    id_semantics::{AgentNodeId, AgentNodeInput},
    metamodel::{GraphQuery, labels},
    store::TaskAgentResolution,
    surreal_tables::{
        TBL_AGENT_ARTIFACT_REGISTRY, TBL_AGENT_IDENTITY_REGISTRY, TBL_EDGE, TBL_NODE,
    },
    types::ProvAgentId,
};

/// Exact operation -> boot -> artifact version resolution for ops rows.
#[derive(Debug, Clone, Default)]
pub(super) struct AgentVersionIndex {
    by_task_id: HashMap<String, String>,
    by_boot_event_id: HashMap<String, String>,
    by_activity_id: HashMap<String, String>,
}

impl AgentVersionIndex {
    pub(super) fn version_for_row(&self, row: &Value) -> Option<&str> {
        row.get("a2a_boot_event_id")
            .and_then(Value::as_str)
            .and_then(|boot| self.by_boot_event_id.get(boot))
            .or_else(|| {
                row.get("task_id")
                    .and_then(Value::as_str)
                    .and_then(|task| self.by_task_id.get(task))
            })
            .or_else(|| {
                row.get("activity_id")
                    .and_then(Value::as_str)
                    .and_then(|activity| self.by_activity_id.get(activity))
            })
            .map(String::as_str)
    }
}

fn prop_str<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get("props")
        .and_then(Value::as_object)
        .and_then(|props| props.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// Outcome of checking whether `agent_package` adds information when `task_id`
/// is already set (task head pointer implies executing agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskAgentPackageCheck {
    /// Task agent matches package — omit redundant agent edge filter.
    OmitAgentFilter,
    /// Task agent belongs to a different package — empty result.
    MismatchEmpty,
    /// No task scope or agent could not be validated — apply package edge filter.
    ApplyPackageFilter,
}

pub(super) fn task_agent_package_check(
    task_id: Option<&TaskId>,
    agent_package: Option<&str>,
    resolution: Option<&TaskAgentResolution>,
    index: &AgentRuntimeIndex,
) -> TaskAgentPackageCheck {
    let (Some(_task_id), Some(pkg)) = (task_id, agent_package) else {
        return TaskAgentPackageCheck::ApplyPackageFilter;
    };
    match resolution {
        Some(TaskAgentResolution::Resolved(agent_id)) => {
            match index.identity_by_agent_id.get(agent_id.as_str()) {
                Some(task_pkg) if task_pkg == pkg => TaskAgentPackageCheck::OmitAgentFilter,
                Some(_) => TaskAgentPackageCheck::MismatchEmpty,
                None => TaskAgentPackageCheck::OmitAgentFilter,
            }
        }
        Some(TaskAgentResolution::NotLinked | TaskAgentResolution::TimedOut) | None => {
            TaskAgentPackageCheck::OmitAgentFilter
        }
    }
}

/// Agent runtime rows indexed once per request (package filter uses instance node ids).
#[derive(Debug, Clone, Default)]
pub(super) struct AgentRuntimeIndex {
    pub identity_by_agent_id: HashMap<String, String>,
    pub instance_node_ids_by_package: HashMap<String, Vec<String>>,
}

impl SurrealProvenanceStore {
    pub(super) fn invalidate_agent_runtime_index_cache(&self) {
        if let Ok(mut guard) = self.agent_runtime_index_cache.write() {
            *guard = None;
        }
    }

    pub(super) async fn load_agent_runtime_index(&self) -> Result<AgentRuntimeIndex> {
        if let Ok(guard) = self.agent_runtime_index_cache.read()
            && let Some(index) = guard.as_ref()
        {
            return Ok(index.clone());
        }

        let index = self.load_agent_runtime_index_uncached().await?;
        if let Ok(mut guard) = self.agent_runtime_index_cache.write() {
            *guard = Some(index.clone());
        }
        Ok(index)
    }

    async fn load_agent_runtime_index_uncached(&self) -> Result<AgentRuntimeIndex> {
        let registry_index = self.load_agent_runtime_index_from_registry().await?;
        if !registry_index.identity_by_agent_id.is_empty()
            || !registry_index.instance_node_ids_by_package.is_empty()
        {
            return Ok(registry_index);
        }
        self.scan_agent_runtime_index_from_graph().await
    }

    pub(super) async fn load_agent_runtime_index_from_registry(&self) -> Result<AgentRuntimeIndex> {
        let sql = format!(
            "SELECT instance_node_id, agent_package, agent_id \
             FROM {TBL_AGENT_IDENTITY_REGISTRY}"
        );
        let response = self.db.query(&sql).await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(index_from_registry_rows(&rows))
    }

    async fn scan_agent_runtime_index_from_graph(&self) -> Result<AgentRuntimeIndex> {
        let (sql, binds) = GraphQuery::<labels::AgentRuntimeInstance, _>::new()
            .all()
            .into_surreal();
        let rows = self.execute_typed_node_query(&sql, &binds).await?;
        let boot_complete_rows: Vec<Value> = rows
            .iter()
            .filter(|row| {
                row.get("props")
                    .and_then(Value::as_object)
                    .and_then(|props| props.get("a2a_archive_path").and_then(Value::as_str))
                    .is_some_and(|s| !s.is_empty())
            })
            .cloned()
            .collect();
        let index = index_from_graph_rows(&boot_complete_rows);
        for row in &boot_complete_rows {
            let Some(props) = row.get("props").and_then(Value::as_object) else {
                continue;
            };
            let Some(instance_node_id) = row
                .get("node_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(agent_id) = props
                .get("a2a_agent_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let agent_package = normalize_agent_field(
                props.get("a2a_agent_type").and_then(Value::as_str),
                "unknown",
            );
            let identity_digest = normalize_agent_field(
                props.get("identity_digest").and_then(Value::as_str),
                "unknown",
            );
            self.upsert_agent_package_registry_row(
                instance_node_id,
                &agent_package,
                agent_id,
                &identity_digest,
            )
            .await?;
        }
        Ok(index)
    }

    pub(super) async fn upsert_agent_package_registry_on_boot(
        &self,
        agent_id: &AgentId,
        agent_package: &str,
        identity_digest: &str,
    ) -> Result<()> {
        let instance_node_id =
            ProvAgentId::derived::<AgentNodeId>(AgentNodeInput { agent_id }).into_string();
        self.upsert_agent_package_registry_row(
            &instance_node_id,
            agent_package,
            agent_id.as_str(),
            identity_digest,
        )
        .await?;
        self.invalidate_agent_runtime_index_cache();
        Ok(())
    }

    async fn upsert_agent_package_registry_row(
        &self,
        instance_node_id: &str,
        agent_package: &str,
        agent_id: &str,
        identity_digest: &str,
    ) -> Result<()> {
        let select = format!(
            "SELECT instance_node_id, agent_package, agent_id, identity_digest \
             FROM {TBL_AGENT_IDENTITY_REGISTRY} WHERE agent_id = $agent_id LIMIT 1"
        );
        let response = self
            .db
            .query(&select)
            .bind(("agent_id", agent_id.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        if let Some(existing) = rows.first() {
            let exact = existing.get("instance_node_id").and_then(Value::as_str)
                == Some(instance_node_id)
                && existing.get("agent_package").and_then(Value::as_str) == Some(agent_package)
                && existing.get("identity_digest").and_then(Value::as_str) == Some(identity_digest);
            return if exact {
                Ok(())
            } else {
                Err(crate::error::ProvenanceError::InvalidEvent {
                    activity_anchor: "agent_registration".to_string(),
                    reason: format!("immutable Agent registration mismatch for {agent_id}"),
                })
            };
        }
        let sql = format!(
            "CREATE {TBL_AGENT_IDENTITY_REGISTRY} CONTENT {{
               instance_node_id: $instance_node_id,
               agent_package: $agent_package,
               agent_id: $agent_id,
               identity_digest: $identity_digest
             }}"
        );
        self.db
            .query(&sql)
            .bind(("instance_node_id", instance_node_id.to_string()))
            .bind(("agent_package", agent_package.to_string()))
            .bind(("agent_id", agent_id.to_string()))
            .bind(("identity_digest", identity_digest.to_string()))
            .await
            .map_err(map_surreal_error)?
            .check()
            .map_err(map_surreal_error)?;
        Ok(())
    }

    pub(super) async fn register_agent_artifact(
        &self,
        artifact_hash: &str,
        deployment_content_hash: &str,
        repository_version: &str,
    ) -> Result<()> {
        use crate::surreal_tables::TBL_AGENT_ARTIFACT_REGISTRY;
        let select = format!(
            "SELECT deployment_content_hash, repository_version FROM {TBL_AGENT_ARTIFACT_REGISTRY} \
             WHERE artifact_hash = $artifact_hash LIMIT 1"
        );
        let response = self
            .db
            .query(&select)
            .bind(("artifact_hash", artifact_hash.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        if let Some(existing) = rows.first() {
            let exact = existing
                .get("deployment_content_hash")
                .and_then(Value::as_str)
                == Some(deployment_content_hash)
                && existing.get("repository_version").and_then(Value::as_str)
                    == Some(repository_version);
            return if exact {
                Ok(())
            } else {
                Err(crate::error::ProvenanceError::InvalidEvent {
                    activity_anchor: "agent_artifact_registration".to_string(),
                    reason: format!(
                        "immutable AgentArtifact registration mismatch for {artifact_hash}"
                    ),
                })
            };
        }
        let insert = format!(
            "CREATE {TBL_AGENT_ARTIFACT_REGISTRY} CONTENT {{
                artifact_hash: $artifact_hash,
                deployment_content_hash: $deployment_content_hash,
                repository_version: $repository_version
            }}"
        );
        self.db
            .query(&insert)
            .bind(("artifact_hash", artifact_hash.to_string()))
            .bind((
                "deployment_content_hash",
                deployment_content_hash.to_string(),
            ))
            .bind(("repository_version", repository_version.to_string()))
            .await
            .map_err(map_surreal_error)?
            .check()
            .map_err(map_surreal_error)?;
        Ok(())
    }

    pub(super) async fn load_agent_version_index(&self) -> Result<AgentVersionIndex> {
        let mut index = AgentVersionIndex::default();

        let artifact_sql =
            format!("SELECT artifact_hash, repository_version FROM {TBL_AGENT_ARTIFACT_REGISTRY}");
        let artifact_response = self
            .db
            .query(artifact_sql)
            .await
            .map_err(map_surreal_error)?;
        let artifact_rows: Vec<Value> = check_and_take_zero(artifact_response, map_surreal_error)?;
        let version_by_artifact: HashMap<String, String> = artifact_rows
            .iter()
            .filter_map(|row| {
                Some((
                    row.get("artifact_hash")?.as_str()?.to_string(),
                    row.get("repository_version")?.as_str()?.to_string(),
                ))
            })
            .collect();

        let boot_response = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_NODE} WHERE label = 'AgentBoot'"
            ))
            .await
            .map_err(map_surreal_error)?;
        let boot_rows: Vec<Value> = check_and_take_zero(boot_response, map_surreal_error)?;
        let mut version_by_boot_node: HashMap<String, String> = HashMap::new();
        for row in &boot_rows {
            let Some(node_id) = row.get("node_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(anchor) = prop_str(row, "a2a_activity_anchor") else {
                continue;
            };
            let Some(version) = prop_str(row, "artifact_hash")
                .and_then(|artifact| version_by_artifact.get(artifact))
            else {
                continue;
            };
            version_by_boot_node.insert(node_id.to_string(), version.clone());
            index
                .by_boot_event_id
                .insert(anchor.to_string(), version.clone());
        }

        let execution_response = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_NODE} WHERE label = 'A2ATaskExecution'"
            ))
            .await
            .map_err(map_surreal_error)?;
        let execution_rows: Vec<Value> =
            check_and_take_zero(execution_response, map_surreal_error)?;
        let task_by_execution: HashMap<String, String> = execution_rows
            .iter()
            .filter_map(|row| {
                Some((
                    row.get("node_id")?.as_str()?.to_string(),
                    prop_str(row, "a2a_task_id")?.to_string(),
                ))
            })
            .collect();

        let task_boot_response = self
            .db
            .query(format!(
                "SELECT from_id, to_id FROM {TBL_EDGE} WHERE rel_type = 'EXECUTED_UNDER_BOOT'"
            ))
            .await
            .map_err(map_surreal_error)?;
        let task_boot_rows: Vec<Value> =
            check_and_take_zero(task_boot_response, map_surreal_error)?;
        for row in &task_boot_rows {
            let Some(from_id) = row.get("from_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(to_id) = row.get("to_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(version) = version_by_boot_node.get(to_id) else {
                continue;
            };
            index
                .by_activity_id
                .insert(from_id.to_string(), version.clone());
            if let Some(task_id) = task_by_execution.get(from_id) {
                index.by_task_id.insert(task_id.clone(), version.clone());
            }
        }

        let stop_response = self
            .db
            .query(format!(
                "SELECT * FROM {TBL_NODE} WHERE label = 'AgentStop'"
            ))
            .await
            .map_err(map_surreal_error)?;
        let stop_rows: Vec<Value> = check_and_take_zero(stop_response, map_surreal_error)?;
        for row in &stop_rows {
            let Some(node_id) = row.get("node_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(boot) = prop_str(row, "a2a_boot_event_id") else {
                continue;
            };
            if let Some(version) = index.by_boot_event_id.get(boot) {
                index
                    .by_activity_id
                    .insert(node_id.to_string(), version.clone());
            }
        }

        Ok(index)
    }

    pub(super) async fn execute_typed_node_query(
        &self,
        sql: &str,
        binds: &Value,
    ) -> Result<Vec<Value>> {
        let mut q = self.db.query(sql);
        if let Some(obj) = binds.as_object() {
            for (k, v) in obj {
                q = q.bind((k.clone(), v.clone()));
            }
        }
        let response = q.await.map_err(map_surreal_error)?;
        check_and_take_zero(response, map_surreal_error)
    }
}

fn normalize_agent_field(raw: Option<&str>, fallback: &str) -> String {
    raw.map(str::trim)
        .filter(|s| !s.is_empty() && *s != "null")
        .unwrap_or(fallback)
        .to_string()
}

fn index_from_graph_rows(rows: &[Value]) -> AgentRuntimeIndex {
    let mut identity_by_agent_id: HashMap<String, String> = HashMap::new();
    let mut instance_node_ids_by_package: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let Some(props) = row.get("props").and_then(Value::as_object) else {
            continue;
        };
        let Some(node_id) = row
            .get("node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(agent_id) = props
            .get("a2a_agent_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let agent_id = agent_id.to_string();
        let agent_package = normalize_agent_field(
            props.get("a2a_agent_type").and_then(Value::as_str),
            "unknown",
        );
        identity_by_agent_id.insert(agent_id, agent_package.clone());
        instance_node_ids_by_package
            .entry(agent_package)
            .or_default()
            .push(node_id.to_string());
    }
    AgentRuntimeIndex {
        identity_by_agent_id,
        instance_node_ids_by_package,
    }
}

fn index_from_registry_rows(rows: &[Value]) -> AgentRuntimeIndex {
    let mut identity_by_agent_id: HashMap<String, String> = HashMap::new();
    let mut instance_node_ids_by_package: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let Some(instance_node_id) = row
            .get("instance_node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let agent_package =
            normalize_agent_field(row.get("agent_package").and_then(Value::as_str), "unknown");
        let agent_id = normalize_agent_field(row.get("agent_id").and_then(Value::as_str), "");
        if !agent_id.is_empty() {
            identity_by_agent_id.insert(agent_id.clone(), agent_package.clone());
        }
        instance_node_ids_by_package
            .entry(agent_package)
            .or_default()
            .push(instance_node_id.to_string());
    }
    AgentRuntimeIndex {
        identity_by_agent_id,
        instance_node_ids_by_package,
    }
}

pub(super) fn normalize_agent_field_for_ops(raw: Option<&str>, fallback: &str) -> String {
    normalize_agent_field(raw, fallback)
}

#[cfg(test)]
mod task_agent_package_tests {
    use std::collections::HashMap;

    use baml_rt_core::ids::{AgentId, ExternalId, TaskId};
    use baml_rt_id::UuidId;

    use super::*;
    use crate::store::TaskAgentResolution;

    const AGENT_UUID: &str = "11111111-2222-3333-4444-555555555555";

    fn task_id() -> TaskId {
        TaskId::from_external(ExternalId::new("dispatch-unit-test"))
    }

    fn agent_id() -> AgentId {
        AgentId::from_uuid(UuidId::parse_str(AGENT_UUID).expect("valid uuid"))
    }

    fn index_with_package(package: &str) -> AgentRuntimeIndex {
        let agent_id = agent_id().as_str().to_string();
        let mut identity_by_agent_id = HashMap::new();
        identity_by_agent_id.insert(agent_id, package.to_string());
        let mut instance_node_ids_by_package = HashMap::new();
        instance_node_ids_by_package.insert(
            package.to_string(),
            vec![format!("agent_instance:{AGENT_UUID}")],
        );
        AgentRuntimeIndex {
            identity_by_agent_id,
            instance_node_ids_by_package,
        }
    }

    #[test]
    fn omit_filter_when_task_agent_matches_package() {
        let index = index_with_package("slack-agent");
        let resolution = TaskAgentResolution::Resolved(agent_id());
        assert_eq!(
            task_agent_package_check(
                Some(&task_id()),
                Some("slack-agent"),
                Some(&resolution),
                &index,
            ),
            TaskAgentPackageCheck::OmitAgentFilter,
        );
    }

    #[test]
    fn empty_on_package_mismatch() {
        let index = index_with_package("slack-agent");
        let resolution = TaskAgentResolution::Resolved(agent_id());
        assert_eq!(
            task_agent_package_check(
                Some(&task_id()),
                Some("clickup-agent"),
                Some(&resolution),
                &index,
            ),
            TaskAgentPackageCheck::MismatchEmpty,
        );
    }

    #[test]
    fn apply_filter_without_task_scope() {
        let index = index_with_package("slack-agent");
        assert_eq!(
            task_agent_package_check(None, Some("slack-agent"), None, &index),
            TaskAgentPackageCheck::ApplyPackageFilter,
        );
    }
}
