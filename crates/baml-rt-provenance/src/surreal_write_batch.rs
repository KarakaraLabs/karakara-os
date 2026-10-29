// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `BEGIN…COMMIT` transactions for one provenance event (graph + blob pointers + payloads).
//!
//! ## Reference records vs event-owned records
//!
//! Every event writes both kinds:
//!
//! - **Event-owned records** carry the event's `a2a_activity_anchor` and have ids
//!   unique to that event (a `ToolCall` activity, its args, its transcript-index
//!   row, its payloads). They only ever belong to the event that created them.
//! - **Reference records** carry no anchor: the `Context` node, the `Task` entity,
//!   the `TaskExecution` activity, message placeholders, and stable `Agent` /
//!   `AgentArtifact` registration. Every event in a context touches them again.
//!
//! Same-context fanout runs several tasks at once, so branches re-upsert the same
//! reference records from concurrent transactions. SurrealDB resolves that with
//! MVCC `Transaction conflict`, and once the retry budget runs out the event is
//! dropped. Reference records therefore commit in their own plan, under the
//! store's shared-write lane ([`SurrealProvenanceStore::run_shared_event_write`]),
//! while event-owned records keep the plain per-event transaction.
//!
//! Reference writes are also coalesced in process: a reference record whose exact
//! content this store already committed is skipped instead of re-written. The
//! skip is a no-op by construction (the row already holds those values), and it
//! keeps the lane work proportional to what actually changed.
//!
//! The split costs whole-event atomicity: a reference plan can commit and the
//! event-owned plan then fail. What remains is the placeholder set a valid event
//! would have written anyway (Context, Task, TaskExecution, message placeholder),
//! so a retried event completes it and a dropped event leaves no event-owned rows.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use baml_rt_vocabulary::vocabulary::a2a;
use dashmap::DashMap;
use serde_json::Value;

use crate::{
    context_agent_leases::ContextAgentFence,
    graph_model::GraphNodeLabel,
    id_semantics::{context_entity_id_string, task_entity_id_string_raw},
    normalizer::{A2aRelationType, HeadPointerRepoint, NormalizedProv},
    payload_record::PayloadRecord,
    payload_storage,
    prov_write_semantics::{
        semantic_associated_with_label, semantic_derived_from_label, semantic_generated_by_label,
        semantic_used_label,
    },
    surreal_sql::{edge_props_object, storage_safe_props_sorted_keys},
    surreal_tables::{TBL_BLOB, TBL_CONTEXT_TRANSCRIPT_INDEX, TBL_EDGE, TBL_NODE, TBL_PAYLOAD},
    types::ProvNodeRef,
    vocabulary::semantic_labels,
};

/// Last reference-record write this store committed in this process.
///
/// Key is the record identity (`node:<id>`, `edge:<from>|<rel>|<to>`,
/// `head:<from>|<rel>`, `index:<context>|<node>`, `blob:<hash>`); value is a
/// fingerprint of the content written. Only reference records are recorded, so
/// the map is bounded by the reference graph (contexts, tasks, messages, agents),
/// not by event count.
pub(crate) type WriteCoalesceCache = DashMap<String, u64>;

/// One named bind for `Query::bind`.
#[derive(Clone)]
pub(crate) struct TxBind {
    pub name: String,
    pub value: Value,
}

pub(crate) struct EventWritePlan {
    pub sql: String,
    pub binds: Vec<TxBind>,
    pub statement_count: usize,
    /// True for the plan holding reference records. It runs under the shared-write
    /// lane so concurrent branches of one context cannot race the same rows.
    pub shared: bool,
    /// Reference records this plan writes; recorded only if the transaction commits.
    coalesced: Vec<(String, u64)>,
}

/// Opening statements of every fenced transaction.
///
/// The event body only runs when the presented fence still matches the live
/// lease. A stale writer therefore commits an empty transaction rather than
/// aborting it: SurrealDB reports a transaction abort back to the client
/// without the `THROW` payload, so the writer checks the pair again after the
/// commit and returns the typed stale-fence error itself.
const FENCE_GUARD_SQL: &str = "LET $lease_token = SELECT VALUE fencing_token \
     FROM context_agent_leases \
     WHERE context_id = $fence_ctx \
       AND agent_id = $fence_agent \
       AND boot_event_id = $fence_boot \
       AND fencing_token = $fence_token \
       AND released = false \
     LIMIT 1;";

impl EventWritePlan {
    /// Prepend the lease-fence guard and its binds to this transaction.
    pub(crate) fn with_fence_guard(mut self, fence: &ContextAgentFence) -> Self {
        if self.sql.is_empty() {
            return self;
        }
        let body = self
            .sql
            .strip_prefix("BEGIN;\n")
            .and_then(|sql| sql.strip_suffix("\nCOMMIT;"))
            .map(str::to_string)
            .unwrap_or_else(|| self.sql.clone());
        self.sql = format!(
            "BEGIN;\n{FENCE_GUARD_SQL}\nIF array::len($lease_token) = 1 {{\n{body};\n}};\nCOMMIT;"
        );
        self.statement_count += 2;
        // A stale fence commits an empty transaction. Reference records from
        // that plan did not land, so they must not enter the coalescing cache.
        self.coalesced.clear();
        self.binds.push(TxBind {
            name: "fence_ctx".to_string(),
            value: Value::String(fence.context_id.as_str().to_string()),
        });
        self.binds.push(TxBind {
            name: "fence_agent".to_string(),
            value: Value::String(fence.agent_id.as_str().to_string()),
        });
        self.binds.push(TxBind {
            name: "fence_boot".to_string(),
            value: Value::String(fence.boot_event_id.as_str().to_string()),
        });
        self.binds.push(TxBind {
            name: "fence_token".to_string(),
            value: Value::from(fence.fencing_token),
        });
        self
    }
}

/// Executable transaction plan for [`SurrealProvenanceStore::run_event_write_plan`].
pub(crate) trait ExecutableSurrealPlan {
    /// Split into SQL text, binds, and the coalescing entries to record once the
    /// transaction commits.
    fn into_sql_binds_and_coalesced(self) -> (String, Vec<TxBind>, Vec<(String, u64)>);
}

impl ExecutableSurrealPlan for EventWritePlan {
    fn into_sql_binds_and_coalesced(self) -> (String, Vec<TxBind>, Vec<(String, u64)>) {
        (self.sql, self.binds, self.coalesced)
    }
}

/// Statements belonging to one transaction.
///
/// `coalesce` is `Some` only for the reference fragment, which both skips writes
/// this process already committed and records the ones it emits.
struct WriteFragment {
    stmts: Vec<String>,
    binds: Vec<TxBind>,
    coalesced: Vec<(String, u64)>,
    coalesce: Option<Arc<WriteCoalesceCache>>,
    ni: usize,
    ei: usize,
    di: usize,
    ti: usize,
}

impl WriteFragment {
    fn reference(coalesce: &Arc<WriteCoalesceCache>) -> Self {
        Self {
            stmts: Vec::new(),
            binds: Vec::new(),
            coalesced: Vec::new(),
            coalesce: Some(Arc::clone(coalesce)),
            ni: 0,
            ei: 0,
            di: 0,
            ti: 0,
        }
    }

    fn event_owned() -> Self {
        Self {
            stmts: Vec::new(),
            binds: Vec::new(),
            coalesced: Vec::new(),
            coalesce: None,
            ni: 0,
            ei: 0,
            di: 0,
            ti: 0,
        }
    }

    fn is_shared(&self) -> bool {
        self.coalesce.is_some()
    }

    fn is_empty(&self) -> bool {
        self.stmts.is_empty()
    }

    /// True when this process already committed this exact reference record.
    fn skip(&self, key: &str, fingerprint: u64) -> bool {
        self.coalesce.as_ref().is_some_and(|cache| {
            cache
                .get(key)
                .is_some_and(|committed| *committed == fingerprint)
        })
    }

    fn record(&mut self, key: String, fingerprint: u64) {
        if self.is_shared() {
            self.coalesced.push((key, fingerprint));
        }
    }

    fn merge(mut a: Self, mut b: Self) -> Self {
        debug_assert_eq!(
            a.is_shared(),
            b.is_shared(),
            "reference and event-owned statements cannot share one transaction"
        );
        a.stmts.append(&mut b.stmts);
        a.binds.append(&mut b.binds);
        a.coalesced.append(&mut b.coalesced);
        a
    }

    fn into_event_plan(self) -> EventWritePlan {
        let statement_count = self.stmts.len();
        let shared = self.is_shared();
        let sql = if self.stmts.is_empty() {
            String::new()
        } else {
            format!("BEGIN;\n{};\nCOMMIT;", self.stmts.join(";\n"))
        };
        EventWritePlan {
            sql,
            binds: self.binds,
            statement_count,
            shared,
            coalesced: self.coalesced,
        }
    }
}

/// Content fingerprint for coalescing decisions. `parts` must be built from
/// ordered data (sorted props, fixed field order); hashing a `HashMap` directly
/// would differ between two maps with equal content.
fn write_fingerprint(parts: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

fn merged_txn_estimates(graph: &WriteFragment, data: &WriteFragment) -> (usize, usize) {
    let bind_count = graph.binds.len() + data.binds.len();
    let n = graph.stmts.len() + data.stmts.len();
    let sql_bytes = if n == 0 {
        0
    } else {
        const PREFIX: usize = 8;
        const SUFFIX: usize = 10;
        let stmts_sum: usize = graph
            .stmts
            .iter()
            .chain(data.stmts.iter())
            .map(|s| s.len())
            .sum();
        let sep = if n <= 1 { 0 } else { (n - 1) * 2 };
        PREFIX + stmts_sum + sep + SUFFIX
    };
    (bind_count, sql_bytes)
}

/// Records with the event's activity anchor were created by this event and have
/// ids unique to it. Everything else is a reference record that later events in
/// the same context touch again.
fn is_reference_record(props: &HashMap<String, Value>) -> bool {
    !props.contains_key(a2a::ACTIVITY_ANCHOR)
}

fn label_from_prov_type(prov_type: Option<&str>, default: &str) -> String {
    prov_type
        .map(|t| {
            t.rsplit_once(':')
                .map(|(_, suffix)| suffix)
                .unwrap_or(t)
                .to_string()
        })
        .unwrap_or_else(|| default.to_string())
}

fn label_for_prov_node_ref<'a>(
    node_ref: &ProvNodeRef,
    entity_labels: &'a HashMap<String, String>,
    activity_labels: &'a HashMap<String, String>,
    agent_labels: &'a HashMap<String, String>,
) -> &'a str {
    match node_ref {
        ProvNodeRef::Entity(eid) => entity_labels
            .get(eid.as_str())
            .map(|s| s.as_str())
            .unwrap_or("ProvEntity"),
        ProvNodeRef::Activity(aid) => activity_labels
            .get(aid.as_str())
            .map(|s| s.as_str())
            .unwrap_or("ProvActivity"),
        ProvNodeRef::Agent(agid) => agent_labels
            .get(agid.as_str())
            .map(|s| s.as_str())
            .unwrap_or("ProvAgent"),
    }
}

fn push_scoped_to_context_edge(
    frag: &mut WriteFragment,
    from_id: &str,
    from_label: &str,
    ctx_node_id: &str,
) {
    push_edge_upsert(
        frag,
        EdgeUpsertSpec {
            from_id,
            from_label,
            rel_type: crate::vocabulary::context_scope::SCOPED_TO,
            to_id: ctx_node_id,
            to_label: "Context",
        },
        &HashMap::new(),
    );
}

/// Resolve `LlmCall` / `ToolCall` activity `node_id` for this anchor without a post-write SELECT.
pub(crate) fn call_activity_id_from_normalized(
    normalized: &NormalizedProv,
    anchor: &str,
) -> Option<String> {
    for (id, activity) in normalized.document.activities() {
        let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
        if label != "LlmCall" && label != "ToolCall" {
            continue;
        }
        if let Some(Value::String(a)) = activity.attributes.get(a2a::ACTIVITY_ANCHOR)
            && a == anchor
        {
            return Some(id.as_str().to_string());
        }
    }
    None
}

fn push_bind(binds: &mut Vec<TxBind>, name: impl Into<String>, value: Value) {
    binds.push(TxBind {
        name: name.into(),
        value,
    });
}

/// From/to ids and labels plus `rel_type` for one `prov_edge` upsert.
struct EdgeUpsertSpec<'a> {
    from_id: &'a str,
    from_label: &'a str,
    rel_type: &'a str,
    to_id: &'a str,
    to_label: &'a str,
}

fn push_node_upsert(
    frag: &mut WriteFragment,
    node_id: &str,
    label: &str,
    props: &HashMap<String, Value>,
) {
    let safe_rows = storage_safe_props_sorted_keys(props);
    let key = format!("node:{node_id}");
    let fingerprint = write_fingerprint(&[label, &format!("{safe_rows:?}")]);
    if frag.skip(&key, fingerprint) {
        return;
    }
    let i = frag.ni;
    frag.ni += 1;
    let id_k = format!("n{i}_id");
    let lab_k = format!("n{i}_lab");
    push_bind(&mut frag.binds, &id_k, Value::String(node_id.to_string()));
    push_bind(&mut frag.binds, &lab_k, Value::String(label.to_string()));
    let mut sets = vec![format!("node_id = ${id_k}"), format!("label = ${lab_k}")];
    for (j, (k, v)) in safe_rows.iter().enumerate() {
        let pk = format!("n{i}_p{j}");
        push_bind(&mut frag.binds, &pk, v.clone());
        sets.push(format!("props.{k} = ${pk}"));
    }
    frag.stmts.push(format!(
        "UPSERT {TBL_NODE} SET {} WHERE node_id = ${id_k}",
        sets.join(", ")
    ));
    frag.record(key, fingerprint);
}

fn push_edge_upsert(
    frag: &mut WriteFragment,
    spec: EdgeUpsertSpec<'_>,
    props: &HashMap<String, Value>,
) {
    let EdgeUpsertSpec {
        from_id,
        from_label,
        rel_type,
        to_id,
        to_label,
    } = spec;
    let key = format!("edge:{from_id}|{rel_type}|{to_id}");
    let fingerprint = write_fingerprint(&[from_label, to_label, &edge_props_debug(props)]);
    if frag.skip(&key, fingerprint) {
        return;
    }
    let i = frag.ei;
    frag.ei += 1;
    let fk = format!("e{i}_fid");
    let flk = format!("e{i}_flab");
    let tk = format!("e{i}_tid");
    let tlk = format!("e{i}_tlab");
    let rk = format!("e{i}_rel");
    let pk = format!("e{i}_pr");
    push_bind(&mut frag.binds, &fk, Value::String(from_id.to_string()));
    push_bind(&mut frag.binds, &flk, Value::String(from_label.to_string()));
    push_bind(&mut frag.binds, &tk, Value::String(to_id.to_string()));
    push_bind(&mut frag.binds, &tlk, Value::String(to_label.to_string()));
    push_bind(&mut frag.binds, &rk, Value::String(rel_type.to_string()));
    push_bind(&mut frag.binds, &pk, edge_props_object(props));
    frag.stmts.push(format!(
        "UPSERT {TBL_EDGE} SET from_id = ${fk}, from_label = ${flk}, \
         to_id = ${tk}, to_label = ${tlk}, rel_type = ${rk}, props = ${pk} \
         WHERE from_id = ${fk} AND rel_type = ${rk} AND to_id = ${tk}"
    ));
    frag.record(key, fingerprint);
}

/// Deterministic rendering of edge props for fingerprinting. `HashMap` iteration
/// order is not stable across instances, so keys are sorted first.
fn edge_props_debug(props: &HashMap<String, Value>) -> String {
    let mut rows: Vec<(&String, &Value)> = props.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::new();
    for (key, value) in rows {
        out.push_str(key);
        out.push('=');
        out.push_str(&value.to_string());
        out.push(';');
    }
    out
}

/// Emit a `DELETE prov_edge WHERE from_id = ? AND rel_type = ?`
/// statement. Used by [`push_head_pointer_repoint`] to clear any prior
/// head-pointer edge from `from_id` before inserting the new one. The two
/// statements run inside the same `BEGIN..COMMIT` transaction as the rest
/// of the event's graph writes, so a concurrent reader cannot observe a
/// gap with zero edges or a window with two competing edges.
fn push_edge_delete_by_from_rel(frag: &mut WriteFragment, from_id: &str, rel_type: &str) {
    let i = frag.di;
    frag.di += 1;
    let fk = format!("hd{i}_fid");
    let rk = format!("hd{i}_rel");
    push_bind(&mut frag.binds, &fk, Value::String(from_id.to_string()));
    push_bind(&mut frag.binds, &rk, Value::String(rel_type.to_string()));
    frag.stmts.push(format!(
        "DELETE {TBL_EDGE} WHERE from_id = ${fk} AND rel_type = ${rk}"
    ));
}

/// Emit the DELETE-then-UPSERT pair that re-points one head-pointer edge
/// (`WAS_LAST_*`) to a new head
/// inside the current event transaction. Cardinality (exactly one
/// `(from_id, rel_type)` row per head-pointer per Task) is enforced
/// procedurally by the DELETE preceding the UPSERT inside `BEGIN..COMMIT`;
/// SurrealDB v3 does not yet support partial / WHERE-filtered UNIQUE
/// indexes, so an index-level backstop on `(rel_type, from_id)` is not
/// available without breaking the existing chain edges that legitimately
/// fan out from the same `from_id`.
fn push_head_pointer_repoint(frag: &mut WriteFragment, repoint: &HeadPointerRepoint) {
    let rel_type = repoint.rel.as_rel_str();
    let key = format!("head:{}|{rel_type}", repoint.from_id);
    let fingerprint = write_fingerprint(&[&repoint.to_id]);
    if frag.skip(&key, fingerprint) {
        return;
    }
    push_edge_delete_by_from_rel(frag, &repoint.from_id, rel_type);
    push_edge_upsert(
        frag,
        EdgeUpsertSpec {
            from_id: &repoint.from_id,
            from_label: &repoint.from_label,
            rel_type,
            to_id: &repoint.to_id,
            to_label: &repoint.to_label,
        },
        &HashMap::new(),
    );
    frag.record(key, fingerprint);
}

struct TranscriptIndexUpsert<'a> {
    context_id: &'a str,
    node_id: &'a str,
    label: &'a str,
    event_order: u64,
    task_entity_id: Option<&'a str>,
    agent_node_id: Option<&'a str>,
    projection_visible: bool,
}

fn push_transcript_index_upsert(frag: &mut WriteFragment, row: TranscriptIndexUpsert<'_>) {
    let TranscriptIndexUpsert {
        context_id,
        node_id,
        label,
        event_order,
        task_entity_id,
        agent_node_id,
        projection_visible,
    } = row;
    let key = format!("index:{context_id}|{node_id}");
    let fingerprint = write_fingerprint(&[
        label,
        &event_order.to_string(),
        task_entity_id.unwrap_or(""),
        agent_node_id.unwrap_or(""),
        if projection_visible {
            "visible"
        } else {
            "hidden"
        },
    ]);
    if frag.skip(&key, fingerprint) {
        return;
    }
    let i = frag.ti;
    frag.ti += 1;
    let ck = format!("ti{i}_ctx");
    let nk = format!("ti{i}_nid");
    let lk = format!("ti{i}_lab");
    let ok = format!("ti{i}_ord");
    let tk = format!("ti{i}_task");
    let ak = format!("ti{i}_agent");
    let vk = format!("ti{i}_visible");
    push_bind(&mut frag.binds, &ck, Value::String(context_id.to_string()));
    push_bind(&mut frag.binds, &nk, Value::String(node_id.to_string()));
    push_bind(&mut frag.binds, &lk, Value::String(label.to_string()));
    push_bind(&mut frag.binds, &ok, Value::Number(event_order.into()));
    push_bind(
        &mut frag.binds,
        &tk,
        task_entity_id
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null),
    );
    push_bind(
        &mut frag.binds,
        &ak,
        agent_node_id
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null),
    );
    push_bind(&mut frag.binds, &vk, Value::Bool(projection_visible));
    frag.stmts.push(format!(
        "UPSERT {TBL_CONTEXT_TRANSCRIPT_INDEX} SET \
         context_id = ${ck}, node_id = ${nk}, label = ${lk}, event_order = ${ok}, \
         task_entity_id = ${tk}, agent_node_id = ${ak}, projection_visible = ${vk}, \
         agent_scope_indexed = true \
         WHERE context_id = ${ck} AND node_id = ${nk}"
    ));
    frag.record(key, fingerprint);
}

fn push_blob_upsert(frag: &mut WriteFragment, hash: &str, body: &str) {
    let i = frag.ni;
    frag.ni += 1;
    let hk = format!("b{i}_h");
    let bk = format!("b{i}_body");
    push_bind(&mut frag.binds, &hk, Value::String(hash.to_string()));
    push_bind(&mut frag.binds, &bk, Value::String(body.to_string()));
    frag.stmts.push(format!(
        "UPSERT {TBL_BLOB} SET content_hash = ${hk}, body = ${bk} WHERE content_hash = ${hk}"
    ));
}

fn push_payload_upsert(frag: &mut WriteFragment, p: &PayloadRecord) {
    let i = frag.ni;
    frag.ni += 1;
    let id_k = format!("p{i}_id");
    let aa_k = format!("p{i}_aa");
    let act_k = format!("p{i}_act");
    let kind_k = format!("p{i}_kind");
    let pj_k = format!("p{i}_pj");
    let ch_k = format!("p{i}_ch");
    let sk_k = format!("p{i}_sk");
    let fk_k = format!("p{i}_fk");
    let st_k = format!("p{i}_st");
    push_bind(&mut frag.binds, &id_k, Value::String(p.payload_id.clone()));
    push_bind(
        &mut frag.binds,
        &aa_k,
        Value::String(p.activity_anchor_id.clone()),
    );
    push_bind(
        &mut frag.binds,
        &act_k,
        match &p.activity_id {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        },
    );
    push_bind(
        &mut frag.binds,
        &kind_k,
        Value::String(p.payload_kind.clone()),
    );
    push_bind(
        &mut frag.binds,
        &pj_k,
        Value::String(p.payload_json.clone()),
    );
    push_bind(
        &mut frag.binds,
        &ch_k,
        match &p.content_hash {
            Some(h) => Value::String(h.clone()),
            None => Value::Null,
        },
    );
    push_bind(
        &mut frag.binds,
        &sk_k,
        Value::String(p.storage_kind.as_str().to_string()),
    );
    push_bind(
        &mut frag.binds,
        &fk_k,
        match &p.file_key {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        },
    );
    push_bind(&mut frag.binds, &st_k, Value::String(p.search_text.clone()));
    frag.stmts.push(format!(
        "UPSERT {TBL_PAYLOAD} SET payload_id = ${id_k}, activity_anchor_id = ${aa_k}, \
         activity_id = ${act_k}, payload_kind = ${kind_k}, payload_json = ${pj_k}, \
         content_hash = ${ch_k}, storage_kind = ${sk_k}, file_key = ${fk_k}, search_text = ${st_k} \
         WHERE payload_id = ${id_k}"
    ));
}

struct LabelMaps {
    entity_labels: HashMap<String, String>,
    activity_labels: HashMap<String, String>,
    agent_labels: HashMap<String, String>,
}

fn build_label_maps(normalized: &NormalizedProv) -> LabelMaps {
    let mut entity_labels = HashMap::new();
    for (id, entity) in normalized.document.entities() {
        let label = label_from_prov_type(entity.prov_type.as_deref(), "ProvEntity");
        entity_labels.insert(id.as_str().to_string(), label);
    }
    let mut activity_labels = HashMap::new();
    for (id, activity) in normalized.document.activities() {
        let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
        activity_labels.insert(id.as_str().to_string(), label);
    }
    let mut agent_labels = HashMap::new();
    for (id, agent) in normalized.document.agents() {
        let label = label_from_prov_type(agent.prov_type.as_deref(), "ProvAgent");
        agent_labels.insert(id.as_str().to_string(), label);
    }
    for (id, label) in &normalized.agent_labels {
        agent_labels
            .entry(id.clone())
            .or_insert_with(|| label.clone());
    }
    LabelMaps {
        entity_labels,
        activity_labels,
        agent_labels,
    }
}

/// Build the reference fragment and the event-owned graph fragment.
///
/// Edges join the reference fragment only when both endpoints are reference
/// records: an edge from an event-owned activity is unique to this event even if
/// it points at a shared agent.
fn build_graph_fragment(
    normalized: &NormalizedProv,
    context_id: Option<&str>,
    transcript_agent_node_id: Option<&str>,
    coalesce: &Arc<WriteCoalesceCache>,
) -> (WriteFragment, WriteFragment) {
    let LabelMaps {
        entity_labels,
        activity_labels,
        agent_labels,
    } = build_label_maps(normalized);

    let mut reference = WriteFragment::reference(coalesce);
    let mut owned = WriteFragment::event_owned();
    // Agents are registered once by `AgentBooted`; every transcript event only
    // references them, including the runner identity. Seeding this from the label
    // map keeps `WAS_ASSOCIATED_WITH` edges to agents in the reference plan even
    // when the event does not carry the agent node itself.
    let mut reference_ids: HashSet<String> = normalized.agent_labels.keys().cloned().collect();

    for (id, entity) in normalized.document.entities() {
        let label = entity_labels
            .get(id.as_str())
            .map(String::as_str)
            .unwrap_or("ProvEntity");
        let mut props = entity.attributes.clone();
        if let Some(ref pt) = entity.prov_type {
            props.insert("prov_type".to_string(), Value::String(pt.clone()));
        }
        let target = if is_reference_record(&props) {
            reference_ids.insert(id.as_str().to_string());
            &mut reference
        } else {
            &mut owned
        };
        push_node_upsert(target, id.as_str(), label, &props);
    }

    for (id, activity) in normalized.document.activities() {
        let label = activity_labels
            .get(id.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let mut props = activity.attributes.clone();
        if let Some(ref pt) = activity.prov_type {
            props.insert("prov_type".to_string(), Value::String(pt.clone()));
        }
        if let Some(start) = activity.start_time_ms {
            props.insert("prov_startTime".to_string(), Value::from(start));
        }
        if let Some(end) = activity.end_time_ms {
            props.insert("prov_endTime".to_string(), Value::from(end));
        }
        let target = if is_reference_record(&props) {
            reference_ids.insert(id.as_str().to_string());
            &mut reference
        } else {
            &mut owned
        };
        push_node_upsert(target, id.as_str(), label, &props);
    }

    for (id, agent) in normalized.document.agents() {
        let label = agent_labels
            .get(id.as_str())
            .map(String::as_str)
            .unwrap_or("ProvAgent");
        let mut props = agent.attributes.clone();
        if let Some(ref pt) = agent.prov_type {
            props.insert("prov_type".to_string(), Value::String(pt.clone()));
        }
        reference_ids.insert(id.as_str().to_string());
        push_node_upsert(&mut reference, id.as_str(), label, &props);
    }

    for (_, used) in normalized.document.used() {
        let mut edge_props: HashMap<String, Value> = HashMap::new();
        if let Some(ref role) = used.role {
            edge_props.insert("prov_role".to_string(), Value::String(role.clone()));
        }
        let activity_label = activity_labels
            .get(used.activity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let entity_label = entity_labels
            .get(used.entity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvEntity");
        let rel_type = semantic_used_label(activity_label, used.role.as_deref());
        let target = if reference_ids.contains(used.activity.as_str())
            && reference_ids.contains(used.entity.as_str())
        {
            &mut reference
        } else {
            &mut owned
        };
        push_edge_upsert(
            target,
            EdgeUpsertSpec {
                from_id: used.activity.as_str(),
                from_label: activity_label,
                rel_type,
                to_id: used.entity.as_str(),
                to_label: entity_label,
            },
            &edge_props,
        );
    }

    for (_, generated) in normalized.document.was_generated_by() {
        let edge_props: HashMap<String, Value> = HashMap::new();
        let entity_id = generated.entity.id();
        let entity_label = label_for_prov_node_ref(
            &generated.entity,
            &entity_labels,
            &activity_labels,
            &agent_labels,
        );
        let activity_label = activity_labels
            .get(generated.activity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let rel_type = semantic_generated_by_label(entity_label, activity_label);
        let target = if reference_ids.contains(entity_id)
            && reference_ids.contains(generated.activity.as_str())
        {
            &mut reference
        } else {
            &mut owned
        };
        push_edge_upsert(
            target,
            EdgeUpsertSpec {
                from_id: entity_id,
                from_label: entity_label,
                rel_type,
                to_id: generated.activity.as_str(),
                to_label: activity_label,
            },
            &edge_props,
        );
    }

    for (_, generation) in normalized.document.qualified_generation() {
        let edge_props: HashMap<String, Value> = HashMap::new();
        let entity_id = generation.entity.id();
        let entity_label = label_for_prov_node_ref(
            &generation.entity,
            &entity_labels,
            &activity_labels,
            &agent_labels,
        );
        let activity_label = activity_labels
            .get(generation.activity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let target = if reference_ids.contains(entity_id)
            && reference_ids.contains(generation.activity.as_str())
        {
            &mut reference
        } else {
            &mut owned
        };
        push_edge_upsert(
            target,
            EdgeUpsertSpec {
                from_id: entity_id,
                from_label: entity_label,
                rel_type: crate::vocabulary::prov_relations::QUALIFIED_GENERATION,
                to_id: generation.activity.as_str(),
                to_label: activity_label,
            },
            &edge_props,
        );
    }

    for (_, assoc) in normalized.document.was_associated_with() {
        let mut edge_props: HashMap<String, Value> = HashMap::new();
        if let Some(ref role) = assoc.role {
            edge_props.insert("prov_role".to_string(), Value::String(role.clone()));
        }
        let activity_label = activity_labels
            .get(assoc.activity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let agent_label = agent_labels
            .get(assoc.agent.as_str())
            .map(String::as_str)
            .unwrap_or("ProvAgent");
        let rel_type = semantic_associated_with_label(assoc.role.as_deref());
        let target = if reference_ids.contains(assoc.activity.as_str())
            && reference_ids.contains(assoc.agent.as_str())
        {
            &mut reference
        } else {
            &mut owned
        };
        push_edge_upsert(
            target,
            EdgeUpsertSpec {
                from_id: assoc.activity.as_str(),
                from_label: activity_label,
                rel_type,
                to_id: assoc.agent.as_str(),
                to_label: agent_label,
            },
            &edge_props,
        );
    }

    for (_, derived) in normalized.document.was_derived_from() {
        let mut edge_props: HashMap<String, Value> = HashMap::new();
        if let Some(ref pt) = derived.prov_type {
            edge_props.insert("prov_type".to_string(), Value::String(pt.clone()));
        }
        let generated_label = entity_labels
            .get(derived.generated_entity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvEntity");
        let used_label = entity_labels
            .get(derived.used_entity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvEntity");
        let rel_type = semantic_derived_from_label(derived.prov_type.as_deref());
        let target = if reference_ids.contains(derived.generated_entity.as_str())
            && reference_ids.contains(derived.used_entity.as_str())
        {
            &mut reference
        } else {
            &mut owned
        };
        push_edge_upsert(
            target,
            EdgeUpsertSpec {
                from_id: derived.generated_entity.as_str(),
                from_label: generated_label,
                rel_type,
                to_id: derived.used_entity.as_str(),
                to_label: used_label,
            },
            &edge_props,
        );
    }

    for relation in &normalized.derived_relations {
        let mut edge_props: HashMap<String, Value> = HashMap::new();
        for (k, v) in &relation.attributes {
            edge_props.insert(k.clone(), v.clone());
        }
        let (from_label, to_label, rel_type) = match relation.relation {
            A2aRelationType::IntentReplacedBy => (
                GraphNodeLabel::Intent.as_str(),
                GraphNodeLabel::Intent.as_str(),
                semantic_labels::WAS_REPLACED_BY,
            ),
            A2aRelationType::IntentRefinedBy => (
                GraphNodeLabel::Intent.as_str(),
                GraphNodeLabel::Intent.as_str(),
                semantic_labels::WAS_REFINED_BY,
            ),
            A2aRelationType::PlanReplacedBy => (
                GraphNodeLabel::Plan.as_str(),
                GraphNodeLabel::Plan.as_str(),
                semantic_labels::WAS_REPLACED_BY,
            ),
            A2aRelationType::PlanRefinedBy => (
                GraphNodeLabel::Plan.as_str(),
                GraphNodeLabel::Plan.as_str(),
                semantic_labels::WAS_REFINED_BY,
            ),
            A2aRelationType::InformedByToolInvocation => (
                GraphNodeLabel::SessionStep.as_str(),
                GraphNodeLabel::ToolCall.as_str(),
                semantic_labels::WAS_INFORMED_BY,
            ),
            A2aRelationType::CitedSource => {
                let from_label = label_for_prov_node_ref(
                    &relation.from,
                    &entity_labels,
                    &activity_labels,
                    &agent_labels,
                );
                let to_label = label_for_prov_node_ref(
                    &relation.to,
                    &entity_labels,
                    &activity_labels,
                    &agent_labels,
                );
                (from_label, to_label, semantic_labels::CITED)
            }
            A2aRelationType::HasIntent => (
                GraphNodeLabel::Task.as_str(),
                GraphNodeLabel::Intent.as_str(),
                semantic_labels::HAS_INTENT,
            ),
            A2aRelationType::HasPlan => (
                GraphNodeLabel::Task.as_str(),
                GraphNodeLabel::Plan.as_str(),
                semantic_labels::HAS_PLAN,
            ),
            A2aRelationType::CallbackDispatchScheduledFrom => (
                GraphNodeLabel::Task.as_str(),
                GraphNodeLabel::Task.as_str(),
                semantic_labels::WAS_SCHEDULED_FROM,
            ),
            // All remaining derived relations use dynamic label resolution.
            // Every variant is handled — no silent skipping.
            // All remaining derived relations: resolve labels dynamically,
            // use the canonical a2a_relations string as rel_type.
            A2aRelationType::TaskHasMessage
            | A2aRelationType::TaskHasSessionStep
            | A2aRelationType::TaskHasArtifact
            | A2aRelationType::TaskCall
            | A2aRelationType::TaskStatusTransition
            | A2aRelationType::MessageCall
            | A2aRelationType::InformedByObservation
            | A2aRelationType::LifecycleStopInformedByBoot
            | A2aRelationType::HostDispatchTarget
            | A2aRelationType::ArtifactRealizesAgent
            | A2aRelationType::TaskExecutionExecutedUnderBoot
            | A2aRelationType::DelegationParent
            | A2aRelationType::DelegationChild
            | A2aRelationType::DelegationCaller
            | A2aRelationType::DelegationTargetAgent => {
                let from_label = label_for_prov_node_ref(
                    &relation.from,
                    &entity_labels,
                    &activity_labels,
                    &agent_labels,
                );
                let to_label = label_for_prov_node_ref(
                    &relation.to,
                    &entity_labels,
                    &activity_labels,
                    &agent_labels,
                );
                (from_label, to_label, relation.relation.as_str())
            }
        };
        let target = if reference_ids.contains(relation.from.id())
            && reference_ids.contains(relation.to.id())
        {
            &mut reference
        } else {
            &mut owned
        };
        push_edge_upsert(
            target,
            EdgeUpsertSpec {
                from_id: relation.from.id(),
                from_label,
                rel_type,
                to_id: relation.to.id(),
                to_label,
            },
            &edge_props,
        );
    }

    // Head-pointer re-points (`WAS_LAST_TRANSITIONED_TO`,
    // `WAS_LAST_EXECUTED_BY`): emit DELETE-then-UPSERT inside the same
    // transaction so the cardinality-one invariant holds without an
    // index backstop. They hang off the Task entity, which is a reference
    // record, so they belong to the reference plan.
    for repoint in &normalized.head_pointer_repoints {
        push_head_pointer_repoint(&mut reference, repoint);
    }

    if let Some(ctx_id) = context_id {
        let ctx_node_id = context_entity_id_string(ctx_id);
        reference_ids.insert(ctx_node_id.clone());
        push_node_upsert(&mut reference, &ctx_node_id, "Context", &HashMap::new());

        let ctx_id_str = ctx_id;
        for (id, entity) in normalized.document.entities() {
            let label = entity_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvEntity");
            let index_row = (matches!(label, "Message" | "ToolCall" | "SessionStep")
                && entity.attributes.contains_key(a2a::EVENT_ORDER))
            .then(|| {
                (
                    entity
                        .attributes
                        .get(a2a::EVENT_ORDER)
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    entity
                        .attributes
                        .get(a2a::TASK_ID)
                        .and_then(Value::as_str)
                        .map(task_entity_id_string_raw),
                    label != "ToolCall"
                        || entity
                            .attributes
                            .get(a2a::ACTIVITY_OUTCOME)
                            .and_then(Value::as_str)
                            .is_some_and(|outcome| matches!(outcome, "Success" | "Failed")),
                )
            });
            let target = if reference_ids.contains(id.as_str()) {
                &mut reference
            } else {
                &mut owned
            };
            push_scoped_to_context_edge(target, id.as_str(), label, &ctx_node_id);
            // A placeholder row for a reference node shares the node's lane.
            if let Some((event_order, task_entity_id, projection_visible)) = index_row {
                push_transcript_index_upsert(
                    target,
                    TranscriptIndexUpsert {
                        context_id: ctx_id_str,
                        node_id: id.as_str(),
                        label,
                        event_order,
                        task_entity_id: task_entity_id.as_deref(),
                        agent_node_id: transcript_agent_node_id,
                        projection_visible,
                    },
                );
            }
        }
        for (id, activity) in normalized.document.activities() {
            let label = activity_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvActivity");
            let index_row = (matches!(label, "ToolCall" | "SessionStep")
                && activity.attributes.contains_key(a2a::EVENT_ORDER))
            .then(|| {
                (
                    activity
                        .attributes
                        .get(a2a::EVENT_ORDER)
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    activity
                        .attributes
                        .get(a2a::TASK_ID)
                        .and_then(Value::as_str)
                        .map(task_entity_id_string_raw),
                    label != "ToolCall"
                        || activity
                            .attributes
                            .get(a2a::ACTIVITY_OUTCOME)
                            .and_then(Value::as_str)
                            .is_some_and(|outcome| matches!(outcome, "Success" | "Failed")),
                )
            });
            let target = if reference_ids.contains(id.as_str()) {
                &mut reference
            } else {
                &mut owned
            };
            push_scoped_to_context_edge(target, id.as_str(), label, &ctx_node_id);
            if let Some((event_order, task_entity_id, projection_visible)) = index_row {
                push_transcript_index_upsert(
                    target,
                    TranscriptIndexUpsert {
                        context_id: ctx_id_str,
                        node_id: id.as_str(),
                        label,
                        event_order,
                        task_entity_id: task_entity_id.as_deref(),
                        agent_node_id: transcript_agent_node_id,
                        projection_visible,
                    },
                );
            }
        }
        for (id, _) in normalized.document.agents() {
            let label = agent_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvAgent");
            push_scoped_to_context_edge(&mut reference, id.as_str(), label, &ctx_node_id);
        }
    }

    (reference, owned)
}

fn build_blob_payload_fragment(
    payloads: &[PayloadRecord],
    blob_bodies: &[(String, String)],
) -> WriteFragment {
    let mut frag = WriteFragment::event_owned();
    for (hash, body) in blob_bodies {
        push_blob_upsert(&mut frag, hash, body);
    }
    for p in payloads {
        push_payload_upsert(&mut frag, p);
    }
    frag
}

/// Transactions for one event: the reference plan first (so event-owned edges can
/// point at it), then the event-owned graph and payloads, merged when under the
/// bind/SQL size limits.
pub(crate) fn build_event_write_plans(
    normalized: &NormalizedProv,
    context_id: Option<&str>,
    transcript_agent_node_id: Option<&str>,
    payloads: &[PayloadRecord],
    blob_bodies: &[(String, String)],
    coalesce: &Arc<WriteCoalesceCache>,
) -> Vec<EventWritePlan> {
    let (reference, graph) =
        build_graph_fragment(normalized, context_id, transcript_agent_node_id, coalesce);
    let data = build_blob_payload_fragment(payloads, blob_bodies);
    let (bind_count, sql_bytes) = merged_txn_estimates(&graph, &data);

    let mut out = Vec::new();
    if !reference.is_empty() {
        out.push(reference.into_event_plan());
    }
    if payload_storage::txn_should_split(bind_count, sql_bytes) {
        let pg = graph.into_event_plan();
        if !pg.sql.is_empty() {
            out.push(pg);
        }
        let pd = data.into_event_plan();
        if !pd.sql.is_empty() {
            out.push(pd);
        }
    } else {
        let plan = WriteFragment::merge(graph, data).into_event_plan();
        if !plan.sql.is_empty() {
            out.push(plan);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use baml_rt_core::ids::{ContextId, ExternalId, MessageId};
    use serde_json::json;

    use super::*;
    use crate::{
        document::ProvDocument,
        normalizer::{NormalizedProv, normalize_event},
        payload_record::StorageKind,
        types::{Entity, ProvEntityId},
    };

    fn empty_coalesce() -> Arc<WriteCoalesceCache> {
        Arc::new(DashMap::new())
    }

    #[test]
    fn write_plan_wraps_statements_in_transaction() {
        let event = crate::events::ProvEvent::tool_call_started_global(
            ContextId::new(1, 1),
            MessageId::from_external(ExternalId::new("m-wrap")),
            "tool".to_string(),
            None,
            json!({}),
            json!({
                "message_id": "m-wrap",
                "agent_id": "00000000-0000-0000-0000-000000000010",
            }),
            None,
        );
        let normalized = normalize_event(&event).expect("normalize");
        let plans = build_event_write_plans(&normalized, None, None, &[], &[], &empty_coalesce());
        assert!(!plans.is_empty(), "expected at least one txn");
        for plan in &plans {
            assert!(plan.sql.starts_with("BEGIN;\n"), "{}", plan.sql);
            assert!(plan.sql.ends_with(";\nCOMMIT;"), "{}", plan.sql);
            assert!(plan.statement_count > 0);
        }
        let flags: Vec<bool> = plans.iter().map(|plan| plan.shared).collect();
        let mut sorted = flags.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(
            flags, sorted,
            "reference records must commit before event-owned records: {flags:?}"
        );
    }

    #[test]
    fn write_plan_splits_when_bind_threshold_exceeded() {
        let mut doc = ProvDocument::new();
        for i in 0..2000 {
            let id = ProvEntityId::test_only(format!("stress_entity_{i}"));
            let mut attributes = HashMap::new();
            // Event-owned record: unique anchor per entity.
            attributes.insert(
                a2a::ACTIVITY_ANCHOR.to_string(),
                Value::String(format!("anchor-{i}")),
            );
            doc.insert_entity(
                id,
                Entity {
                    prov_type: None,
                    attributes,
                },
            );
        }
        let normalized = NormalizedProv {
            document: doc,
            derived_relations: vec![],
            agent_labels: HashMap::new(),
            head_pointer_repoints: vec![],
        };
        let payloads: Vec<PayloadRecord> = (0..11)
            .map(|i| PayloadRecord {
                payload_id: format!("stress_payload_{i}"),
                activity_anchor_id: "anchor".into(),
                activity_id: Some("activity".into()),
                payload_kind: "tool_result".into(),
                payload_json: "{}".into(),
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text: "x".into(),
            })
            .collect();
        let plans =
            build_event_write_plans(&normalized, None, None, &payloads, &[], &empty_coalesce());
        assert_eq!(
            plans.len(),
            2,
            "expected graph txn + payload txn when bind budget exceeded"
        );
    }

    #[test]
    fn repeated_reference_write_is_coalesced() {
        let coalesce = empty_coalesce();
        let event = crate::events::ProvEvent::tool_call_started_global(
            ContextId::new(3, 1),
            MessageId::from_external(ExternalId::new("m-coalesce")),
            "tool".to_string(),
            None,
            json!({}),
            json!({
                "message_id": "m-coalesce",
                "agent_id": "00000000-0000-0000-0000-000000000011",
            }),
            None,
        );
        let normalized = normalize_event(&event).expect("normalize");
        let first = build_event_write_plans(&normalized, None, None, &[], &[], &coalesce);
        for (key, fingerprint) in first.iter().flat_map(|plan| plan.coalesced.iter()) {
            coalesce.insert(key.clone(), *fingerprint);
        }
        let second = build_event_write_plans(&normalized, None, None, &[], &[], &coalesce);
        let second_shared = second.iter().find(|plan| plan.shared);
        assert!(
            second_shared.is_none(),
            "identical reference writes must not repeat: {:?}",
            second_shared.map(|plan| plan.sql.clone())
        );
    }
}
