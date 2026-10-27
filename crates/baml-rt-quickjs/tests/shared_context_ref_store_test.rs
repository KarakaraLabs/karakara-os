// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Two runtime managers sharing one [`baml_rt_tools::SharedContextRefStore`] resolve the same
//! agent-scoped `@N` table for a provenance `context_id`, and never share a sibling agent's table.

use baml_rt_quickjs::BamlRuntimeManager;
use baml_rt_tools::{
    SharedContextRefStore,
    archive_read::render_to_lines,
    archive_refs::{self, ArchiveEntry},
};
use serde_json::json;

#[test]
fn shared_store_same_agent_shares_table_and_siblings_do_not() {
    let store = SharedContextRefStore::new();
    let a = BamlRuntimeManager::builder()
        .with_shared_context_ref_store(store.clone())
        .build()
        .expect("manager a");
    let b = BamlRuntimeManager::builder()
        .with_shared_context_ref_store(store.clone())
        .build()
        .expect("manager b");

    let ctx = "shared-provenance-context";
    let ta = archive_refs::get_or_create_ref_table(a.archive_ref_tables().as_ref(), ctx, "agent-a");
    let tb = archive_refs::get_or_create_ref_table(b.archive_ref_tables().as_ref(), ctx, "agent-a");
    assert!(
        std::sync::Arc::ptr_eq(&ta, &tb),
        "both managers must share the same RefTable Arc for one (context_id, agent_id)"
    );

    let sibling =
        archive_refs::get_or_create_ref_table(a.archive_ref_tables().as_ref(), ctx, "agent-b");
    assert!(
        !std::sync::Arc::ptr_eq(&ta, &sibling),
        "sibling agents must never share a ref table"
    );

    let content = render_to_lines(&json!([{"name": "alice"}]));
    let entry = ArchiveEntry::new(
        content,
        "support/crm".into(),
        Some("listed 1 account".into()),
        String::new(),
        "tool_result".into(),
    );
    let short = ta.insert(entry);
    assert!(
        tb.get(short).is_some(),
        "insert on one manager's path must be visible on the other for the same agent"
    );
    assert!(
        sibling.get(short).is_none(),
        "a sibling agent's table must not see another agent's archive"
    );
}
