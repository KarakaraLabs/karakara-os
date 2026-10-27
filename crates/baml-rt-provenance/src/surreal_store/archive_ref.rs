// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Surreal-backed session archive refs (`@N` / `@prefix/local`) for multi-runtime consistency.
//!
//! SurrealDB 3.x uses [`type::record`](https://surrealdb.com/docs/surrealql/functions/type#record)
//! for computed record IDs (`type::thing` was removed).

use std::sync::Arc;

use baml_rt_core::ids::{AgentId, ContextId};
use baml_rt_tools::{
    archive_read::{RecordSpan, RenderedContent, ShortRef},
    archive_refs::ArchiveEntry,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    SurrealProvenanceStore,
    helpers::{check_and_take_zero, map_surreal_error},
};
use crate::{
    error::{ProvenanceError, Result},
    store::ProvenanceReadPrincipal,
    surreal_tables::{TBL_ARCHIVE_BODY, TBL_ARCHIVE_LOCAL_COUNTER, TBL_ARCHIVE_PREFIX_REGISTRY},
};

fn hex_id(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update(b"|");
    }
    format!("{:x}", h.finalize())
}

fn entry_to_json(entry: &ArchiveEntry) -> Value {
    let lines: Vec<String> = entry.content.lines().map(str::to_string).collect();
    json!({
        "tool_name": entry.tool_name,
        "summary": entry.summary,
        "action_identity": entry.action_identity,
        "activity_anchor": entry.activity_anchor,
        "source": entry.source,
        "completion": entry.completion,
        "search_hints": entry.search_hints,
        // Duplicate suppression is durable: a restarted runtime must not reissue a Send whose
        // result was archived immediately before shutdown.
        "send_signature": entry.send_signature,
        "pending_send": entry.pending_send,
        "rendered_lines": lines,
        "record_spans": entry.content.record_spans(),
    })
}

pub(super) fn entry_from_json(v: &Value) -> Result<ArchiveEntry> {
    let tool_name = v
        .get("tool_name")
        .and_then(|x| x.as_str())
        .ok_or_else(|| ProvenanceError::CorruptArchiveEntry {
            reason: "missing tool_name".into(),
        })?
        .to_string();
    // `summary` is optional now: new rows with `action_identity` typically omit it.
    let summary = v
        .get("summary")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());
    let action_identity = v
        .get("action_identity")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    let activity_anchor = v
        .get("activity_anchor")
        .and_then(|x| x.as_str())
        .ok_or_else(|| ProvenanceError::CorruptArchiveEntry {
            reason: "missing activity_anchor".into(),
        })?
        .to_string();
    let source = v
        .get("source")
        .and_then(|s| s.as_str())
        .unwrap_or("tool_result")
        .to_string();
    let completion = v
        .get("completion")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let search_hints = v
        .get("search_hints")
        .and_then(|x| x.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let send_signature = v
        .get("send_signature")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let pending_send = v
        .get("pending_send")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let lines: Vec<String> = v
        .get("rendered_lines")
        .and_then(|x| x.as_array())
        .ok_or_else(|| ProvenanceError::CorruptArchiveEntry {
            reason: "missing or invalid rendered_lines array".into(),
        })?
        .iter()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
    let record_spans: Vec<RecordSpan> = match v.get("record_spans") {
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            ProvenanceError::CorruptArchiveEntry {
                reason: format!("invalid record_spans: {error}"),
            }
        })?,
        None => Vec::new(),
    };
    let content = RenderedContent::try_from_lines_with_record_spans(lines, record_spans).map_err(
        |reason| ProvenanceError::CorruptArchiveEntry {
            reason: format!("invalid record_spans: {reason}"),
        },
    )?;
    Ok(
        ArchiveEntry::new(content, tool_name, summary, activity_anchor, source)
            .with_action_identity(action_identity)
            .with_completion(completion)
            .with_search_hints(search_hints)
            .with_send_signature(send_signature)
            .with_pending_send(pending_send),
    )
}

impl SurrealProvenanceStore {
    /// Existing composite ref when `activity_anchor` already has a body row (idempotent allocate).
    async fn archive_get_ref_for_activity_anchor(
        &self,
        context_id: &ContextId,
        activity_anchor: &str,
    ) -> Result<Option<ShortRef>> {
        let ctx = context_id.as_str();
        let q = format!(
            "SELECT archive_prefix, archive_local FROM {TBL_ARCHIVE_BODY} \
             WHERE context_id = $ctx AND activity_anchor = $anchor LIMIT 1"
        );
        let response = self
            .db()
            .query(&q)
            .bind(("ctx", ctx.to_string()))
            .bind(("anchor", activity_anchor.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let p = row
            .get("archive_prefix")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| ProvenanceError::CorruptArchiveEntry {
                reason: "archive_body row missing archive_prefix".into(),
            })?;
        let l = row
            .get("archive_local")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| ProvenanceError::CorruptArchiveEntry {
                reason: "archive_body row missing archive_local".into(),
            })?;
        let prefix = u32::try_from(p).map_err(|_| ProvenanceError::InvalidEvent {
            activity_anchor: activity_anchor.into(),
            reason: format!("archive_prefix overflow: {p}"),
        })?;
        let local = u32::try_from(l).map_err(|_| ProvenanceError::InvalidEvent {
            activity_anchor: activity_anchor.into(),
            reason: format!("archive_local overflow: {l}"),
        })?;
        Ok(Some(ShortRef::new_prefixed(prefix, local)))
    }

    fn archive_prefix_cache_key(context_id: &ContextId, agent_id: &AgentId) -> (String, String) {
        (
            context_id.as_str().to_string(),
            agent_id.as_str().to_string(),
        )
    }

    /// Resolve or mint stable `archive_prefix` for `(context_id, agent_id)`.
    ///
    /// `archive_prefix` is unique per `context_id` across agents; the first agent in a context
    /// receives `1`, then `2`, …
    pub async fn archive_ensure_prefix(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
    ) -> Result<u32> {
        let ctx = context_id.as_str();
        let aid = agent_id.as_str();
        let cache_key = Self::archive_prefix_cache_key(context_id, agent_id);
        if let Some(hit) = self.archive_prefix_cache.get(&cache_key) {
            return Ok(*hit);
        }
        // Serialize allocation per context. Without this, N concurrent allocators all read the
        // same max prefix and collide on the same INSERT, burning write-conflict retries.
        let ser_key = ctx.to_string();
        let mx = self
            .archive_prefix_serializers
            .entry(ser_key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _serial = mx.lock().await;
        // Another task may have allocated this agent's prefix while we waited on the lock.
        if let Some(hit) = self.archive_prefix_cache.get(&cache_key) {
            return Ok(*hit);
        }
        let sel = format!(
            "SELECT archive_prefix FROM {TBL_ARCHIVE_PREFIX_REGISTRY} \
             WHERE context_id = $ctx AND agent_id = $aid LIMIT 1"
        );
        let response = self
            .db()
            .query(&sel)
            .bind(("ctx", ctx.to_string()))
            .bind(("aid", aid.to_string()))
            .await
            .map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        if let Some(p) = rows
            .first()
            .and_then(|row| row.get("archive_prefix"))
            .and_then(|x| x.as_u64())
        {
            let p = u32::try_from(p).map_err(|_| ProvenanceError::InvalidEvent {
                activity_anchor: "archive_prefix".into(),
                reason: format!("archive_prefix overflow: {p}"),
            })?;
            self.archive_prefix_cache.insert(cache_key, p);
            return Ok(p);
        }

        for attempt in 0..super::WRITE_CONFLICT_MAX_ATTEMPTS {
            // SurrealDB's `math::max` accepts an array, not a scalar aggregate argument.
            // Ordering the rows also keeps this portable across the supported Surreal versions.
            let max_q = format!(
                "SELECT archive_prefix FROM {TBL_ARCHIVE_PREFIX_REGISTRY} \
                 WHERE context_id = $ctx ORDER BY archive_prefix DESC LIMIT 1"
            );
            let max_resp = self
                .db()
                .query(&max_q)
                .bind(("ctx", ctx.to_string()))
                .await
                .map_err(map_surreal_error)?;
            let max_rows: Vec<Value> = check_and_take_zero(max_resp, map_surreal_error)?;
            let next_p = match max_rows
                .first()
                .and_then(|r| r.get("archive_prefix"))
                .and_then(Value::as_u64)
            {
                None => 1u32,
                Some(m) => {
                    let max_p = u32::try_from(m).map_err(|_| ProvenanceError::InvalidEvent {
                        activity_anchor: "archive_prefix".into(),
                        reason: format!("math::max(archive_prefix) out of u32 range: {m}"),
                    })?;
                    max_p
                        .checked_add(1)
                        .ok_or_else(|| ProvenanceError::InvalidEvent {
                            activity_anchor: "archive_prefix".into(),
                            reason: format!(
                                "archive_prefix assignment would overflow u32 (max was {max_p})"
                            ),
                        })?
                }
            };

            let ins = format!(
                "INSERT INTO {TBL_ARCHIVE_PREFIX_REGISTRY} {{ \
                    context_id: $ctx, agent_id: $aid, archive_prefix: $pfx \
                }} RETURN archive_prefix;"
            );
            let ins_res = self
                .db()
                .query(&ins)
                .bind(("ctx", ctx.to_string()))
                .bind(("aid", aid.to_string()))
                .bind(("pfx", next_p))
                .await;

            match ins_res {
                Ok(r) => {
                    match check_and_take_zero(r, map_surreal_error) {
                        Ok(out) => {
                            if let Some(p) = out
                                .first()
                                .and_then(|row| row.get("archive_prefix"))
                                .and_then(|x| x.as_u64())
                            {
                                let p = u32::try_from(p).map_err(|_| {
                                    ProvenanceError::InvalidEvent {
                                        activity_anchor: "archive_prefix".into(),
                                        reason: format!("archive_prefix overflow: {p}"),
                                    }
                                })?;
                                self.archive_prefix_cache.insert(cache_key.clone(), p);
                                return Ok(p);
                            }
                        }
                        Err(_) => { /* duplicate / conflict — retry */ }
                    }
                    // Re-read after failed insert (another writer won the same p, or we raced).
                    let reread = self
                        .db()
                        .query(&sel)
                        .bind(("ctx", ctx.to_string()))
                        .bind(("aid", aid.to_string()))
                        .await
                        .map_err(map_surreal_error)?;
                    let reread_rows: Vec<Value> = check_and_take_zero(reread, map_surreal_error)?;
                    if let Some(p) = reread_rows
                        .first()
                        .and_then(|row| row.get("archive_prefix"))
                        .and_then(|x| x.as_u64())
                    {
                        let p = u32::try_from(p).map_err(|_| ProvenanceError::InvalidEvent {
                            activity_anchor: "archive_prefix".into(),
                            reason: format!("archive_prefix overflow: {p}"),
                        })?;
                        self.archive_prefix_cache.insert(cache_key.clone(), p);
                        return Ok(p);
                    }
                }
                Err(e) if super::is_transaction_conflict(&e) => {
                    if attempt + 1 >= super::WRITE_CONFLICT_MAX_ATTEMPTS {
                        return Err(ProvenanceError::Contention {
                            details: e.message().to_string(),
                        });
                    }
                    tokio::time::sleep(super::jittered_backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(map_surreal_error(e)),
            }

            if attempt + 1 >= super::WRITE_CONFLICT_MAX_ATTEMPTS {
                break;
            }
            tokio::time::sleep(super::jittered_backoff(attempt)).await;
        }

        Err(ProvenanceError::Contention {
            details: "archive_ensure_prefix: exhausted retries".into(),
        })
    }

    /// Next monotonic `archive_local` for `(context_id, archive_prefix)` (multi-runtime safe).
    pub async fn archive_next_local(&self, context_id: &ContextId, prefix: u32) -> Result<u32> {
        let ctx = context_id.as_str();
        let ser_key = format!("{ctx}\x1f{prefix}");
        let mx = self
            .archive_local_serializers
            .entry(ser_key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _serial = mx.lock().await;

        let rid = hex_id(&[ctx, &prefix.to_string(), "alc"]);

        for attempt in 0..super::WRITE_CONFLICT_MAX_ATTEMPTS {
            let upd = format!(
                "UPDATE type::record('{TBL_ARCHIVE_LOCAL_COUNTER}', $rid) \
                 SET next_local += 1 RETURN next_local;"
            );
            let mut uresp = self.db().query(&upd).bind(("rid", rid.clone())).await;

            match uresp {
                Ok(ref mut r) => {
                    let rows: std::result::Result<Vec<Value>, _> = r.take(0);
                    if let Ok(rs) = rows
                        && let Some(n) = rs
                            .first()
                            .and_then(|v| v.get("next_local"))
                            .and_then(|x| x.as_u64())
                    {
                        return u32::try_from(n).map_err(|_| ProvenanceError::InvalidEvent {
                            activity_anchor: "archive_local".into(),
                            reason: format!("next_local overflow: {n}"),
                        });
                    }
                }
                Err(e) if super::is_transaction_conflict(&e) => {
                    if attempt + 1 >= super::WRITE_CONFLICT_MAX_ATTEMPTS {
                        return Err(ProvenanceError::Contention {
                            details: e.message().to_string(),
                        });
                    }
                    tokio::time::sleep(super::jittered_backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(map_surreal_error(e)),
            }

            let create = format!(
                "CREATE type::record('{TBL_ARCHIVE_LOCAL_COUNTER}', $rid) CONTENT {{ \
                    context_id: $ctx, archive_prefix: $pfx, next_local: 1 \
                }} RETURN next_local;"
            );
            let cresp = self
                .db()
                .query(&create)
                .bind(("rid", rid.clone()))
                .bind(("ctx", ctx.to_string()))
                .bind(("pfx", prefix))
                .await;

            match cresp {
                Ok(cresp) => match check_and_take_zero(cresp, std::convert::identity) {
                    Ok(crs) => {
                        if let Some(n) = crs
                            .first()
                            .and_then(|v| v.get("next_local"))
                            .and_then(Value::as_u64)
                        {
                            return u32::try_from(n).map_err(|_| ProvenanceError::InvalidEvent {
                                activity_anchor: "archive_local".into(),
                                reason: format!("next_local overflow: {n}"),
                            });
                        }
                    }
                    Err(e)
                        if super::is_transaction_conflict(&e)
                            || super::is_duplicate_record_write(&e) =>
                    {
                        continue;
                    }
                    Err(e) => return Err(map_surreal_error(e)),
                },
                Err(e)
                    if super::is_transaction_conflict(&e)
                        || super::is_duplicate_record_write(&e) =>
                {
                    continue;
                }
                Err(e) => return Err(map_surreal_error(e)),
            }

            if attempt + 1 >= super::WRITE_CONFLICT_MAX_ATTEMPTS {
                break;
            }
            tokio::time::sleep(super::jittered_backoff(attempt)).await;
        }

        Err(ProvenanceError::Contention {
            details: "archive_next_local: exhausted retries".into(),
        })
    }

    /// Persist archive body.
    pub async fn archive_put_body(
        &self,
        context_id: &ContextId,
        archive_ref: ShortRef,
        agent_id: &AgentId,
        activity_anchor: &str,
        entry: &ArchiveEntry,
    ) -> Result<()> {
        let ctx = context_id.as_str();
        let body_id = hex_id(&[
            ctx,
            &archive_ref.prefix.to_string(),
            &archive_ref.local.to_string(),
        ]);
        let payload = entry_to_json(entry);
        let q = format!(
            "UPSERT type::record('{TBL_ARCHIVE_BODY}', $body_id) MERGE {{
                context_id: $ctx,
                agent_id: $aid,
                archive_prefix: $pfx,
                archive_local: $loc,
                activity_anchor: $anchor,
                entry: $entry
            }};"
        );
        let response = self
            .db()
            .query(&q)
            .bind(("body_id", body_id))
            .bind(("ctx", ctx.to_string()))
            .bind(("aid", agent_id.as_str().to_string()))
            .bind(("pfx", archive_ref.prefix))
            .bind(("loc", archive_ref.local))
            .bind(("anchor", activity_anchor.to_string()))
            .bind(("entry", payload))
            .await
            .map_err(map_surreal_error)?;
        let _: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(())
    }

    /// Persist receipt consumption so a restored runtime permits a later identical Send.
    ///
    /// Ownership-enforced: only the owning agent may clear its own pending Send guard.
    pub async fn archive_mark_send_consumed(
        &self,
        context_id: &ContextId,
        archive_ref: ShortRef,
        agent_id: &AgentId,
    ) -> Result<()> {
        let q = format!(
            "UPDATE {TBL_ARCHIVE_BODY} SET entry.pending_send = false \
             WHERE context_id = $ctx AND archive_prefix = $pfx AND archive_local = $loc \
             AND agent_id = $aid;"
        );
        let response = self
            .db()
            .query(&q)
            .bind(("ctx", context_id.as_str().to_string()))
            .bind(("pfx", archive_ref.prefix))
            .bind(("loc", archive_ref.local))
            .bind(("aid", agent_id.as_str().to_string()))
            .await
            .map_err(map_surreal_error)?;
        let _: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(())
    }

    /// Load archive body by composite ref, scoped to the reading principal.
    ///
    /// [`ProvenanceReadPrincipal::Agent`] only resolves bodies the reader owns;
    /// [`ProvenanceReadPrincipal::Operator`] resolves any body in the context.
    pub async fn archive_get_body(
        &self,
        context_id: &ContextId,
        archive_ref: ShortRef,
        principal: &ProvenanceReadPrincipal,
    ) -> Result<Option<ArchiveEntry>> {
        let ctx = context_id.as_str();
        let q = match principal {
            ProvenanceReadPrincipal::Agent(_) => {
                format!(
                    "SELECT entry FROM {TBL_ARCHIVE_BODY} \
                     WHERE context_id = $ctx AND archive_prefix = $pfx AND archive_local = $loc \
                     AND agent_id = $aid LIMIT 1"
                )
            }
            ProvenanceReadPrincipal::Operator => {
                format!(
                    "SELECT entry FROM {TBL_ARCHIVE_BODY} \
                     WHERE context_id = $ctx AND archive_prefix = $pfx AND archive_local = $loc LIMIT 1"
                )
            }
        };
        let mut query = self
            .db()
            .query(&q)
            .bind(("ctx", ctx.to_string()))
            .bind(("pfx", archive_ref.prefix))
            .bind(("loc", archive_ref.local));
        if let ProvenanceReadPrincipal::Agent(agent_id) = principal {
            query = query.bind(("aid", agent_id.as_str().to_string()));
        }
        let response = query.await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let row: &Value = row;
        let Some(v) = row.get("entry") else {
            return Err(ProvenanceError::CorruptArchiveEntry {
                reason: "archive_body row missing entry field".into(),
            });
        };
        Ok(Some(entry_from_json(v)?))
    }

    /// Allocate `(prefix, local)` and persist `entry` to Surreal; returns the composite ref.
    pub async fn archive_allocate_and_put(
        &self,
        context_id: &ContextId,
        agent_id: &AgentId,
        activity_anchor: &str,
        entry: ArchiveEntry,
    ) -> Result<ShortRef> {
        let anchor_key = format!("{}\x1f{}", context_id.as_str(), activity_anchor);
        let mx = self
            .archive_anchor_serializers
            .entry(anchor_key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _anchor_serial = mx.lock().await;

        if let Some(existing) = self
            .archive_get_ref_for_activity_anchor(context_id, activity_anchor)
            .await?
        {
            return Ok(existing);
        }

        let prefix = self.archive_ensure_prefix(context_id, agent_id).await?;
        let local = self.archive_next_local(context_id, prefix).await?;
        let archive_ref = ShortRef::new_prefixed(prefix, local);
        match self
            .archive_put_body(context_id, archive_ref, agent_id, activity_anchor, &entry)
            .await
        {
            Ok(()) => Ok(archive_ref),
            Err(e) if super::storage_err_is_duplicate_record_write(&e) => self
                .archive_get_ref_for_activity_anchor(context_id, activity_anchor)
                .await?
                .ok_or(e),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use baml_rt_tools::{archive_read::render_to_lines, archive_refs::ArchiveEntry};
    use serde_json::json;

    use super::{entry_from_json, entry_to_json};

    fn sample_entry() -> ArchiveEntry {
        ArchiveEntry::new(
            render_to_lines(&json!({
                "tasks": [
                    {"id": "one", "status": "in progress"},
                    {"id": "two", "status": "to do"}
                ],
                "message": "Found two tasks"
            })),
            "support/clickup".to_string(),
            None,
            "anchor".to_string(),
            "tool_result".to_string(),
        )
    }

    #[test]
    fn archive_json_round_trip_preserves_record_spans() {
        let entry = sample_entry();
        assert_eq!(entry.content.record_spans().len(), 2);

        let restored = entry_from_json(&entry_to_json(&entry)).expect("round trip");
        assert_eq!(
            restored.content.record_spans(),
            entry.content.record_spans()
        );
        assert_eq!(
            restored.content.lines().collect::<Vec<_>>(),
            entry.content.lines().collect::<Vec<_>>()
        );
    }

    #[test]
    fn legacy_archive_without_record_spans_remains_readable() {
        let entry = sample_entry();
        let mut value = entry_to_json(&entry);
        value
            .as_object_mut()
            .expect("archive entry object")
            .remove("record_spans");

        let restored = entry_from_json(&value).expect("legacy archive");
        assert!(restored.content.record_spans().is_empty());
    }
}
