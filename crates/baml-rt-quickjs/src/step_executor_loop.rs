// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Typed step executor loop.
//!
//! Drives the multi-hop BAML function invocation loop entirely in Rust.
//! Replaces the JS `runGeneratedStepExecutor` shim — all FSM state, polymorphic
//! narrowing, and transition validation live here
//! with no invalid state representation.
//!
//! ## Deployment invariant
//!
//! Each hop **requires** the generated per-phase BAML functions (e.g. `ExecuteStep__entry`,
//! `ExecuteStep__active__…`, …) to exist in the agent IR. An older tarball or snapshot built without the
//! current builder fails **deterministically** on the first hop with a clear error — there is
//! no fallback to the base polymorphic function.
//!
//! BAML `ctx.tags['session_step_stable_prefix']` is set on every hop (see
//! `baml_rt_tools::session_ctx_tags` and `BamlRuntimeManager::invoke_function_with_intra`).
//!
//! ## Provenance
//!
//! [`StepExecutorResult`] is **execution telemetry** (per-hop JSON, `last`, selected tool).
//! It must not be treated as the canonical user-visible chat reply: that role belongs to
//! `SessionResult.message` returned from the agent handler (e.g. structured synthesis).
//! Hosts record the surfaced reply from the chat completion path, not a duplicate scraped
//! from step envelopes.

use std::sync::{Arc, Mutex};

use baml_rt_core::{BamlFunctionId, BamlPromptName, BamlRtError, Result, VariantPhase, context};
use baml_rt_tools::{
    ToolName, ToolSlug, archive_refs::ArchiveReceipt,
    phase_step_json::unwrap_session_plan_step_shape_for_phase_output,
    prompt_projection::ProjectedConversationRow,
};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::baml::{
    BamlRuntimeManager, append_step_intra_deltas, read_provider_conversation_rows_after_hop,
};

/// FSM status extracted from a tool session plan execution result.
/// Streaming/Suspended/Sent are now invisible to the LLM — Send blocks until Done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Open,
    Done,
    Finished,
    Aborted,
    /// Legacy statuses emitted during internal polling; mapped to Done at the FSM boundary.
    Sent,
    Streaming,
    Suspended,
    /// Runtime suppressed a duplicate Send and pointed at existing archive.
    Duplicate,
}

impl StepStatus {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "done" => Some(Self::Done),
            "finished" => Some(Self::Finished),
            "aborted" => Some(Self::Aborted),
            // Legacy / internal statuses — kept for graceful handling
            "sent" => Some(Self::Sent),
            "streaming" => Some(Self::Streaming),
            "suspended" => Some(Self::Suspended),
            // Recoverable tool/archive failures returned as JSON (not thrown) — treat like Done
            // for strict graph / FSM so the LLM can emit a corrective next hop.
            "error" => Some(Self::Done),
            "duplicate" => Some(Self::Duplicate),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Finished => "finished",
            Self::Aborted => "aborted",
            Self::Sent => "done",      // normalise legacy → done
            Self::Streaming => "done", // normalise legacy → done
            Self::Suspended => "done", // normalise legacy → done
            Self::Duplicate => "duplicate",
        }
    }
}

/// Why did the session terminate?
#[derive(Debug, Clone, Copy)]
enum TerminalReason {
    Finished,
    Aborted,
    MissingStatus,
}

/// Tool identity bound after the Open phase.
#[derive(Debug, Clone)]
struct ToolBinding {
    name: ToolName,
    slug: ToolSlug,
}

/// Step executor FSM phase. Impossible states are unrepresentable:
/// - `Entry` has no host tool session bound (archive reuse, read-only finish, or Open)
/// - `Active` always carries tool identity; `has_done` is true after at least one Send completed with Done
/// - `Terminal` cannot transition further
#[derive(Debug, Clone)]
enum Phase {
    Entry,
    Active {
        tool: ToolBinding,
        has_done: bool,
    },
    /// The model must acknowledge a completed Send before another Send is legal.
    Consume {
        tool: ToolBinding,
        return_to: ConsumeReturn,
    },
    #[expect(
        dead_code,
        reason = "Terminal outcome reserved for the step-loop termination path"
    )]
    Terminal(TerminalReason),
}

impl Phase {
    fn is_session_open(&self) -> bool {
        matches!(
            self,
            Self::Active { .. }
                | Self::Consume {
                    return_to: ConsumeReturn::Active,
                    ..
                }
        )
    }

    fn selected_tool(&self) -> Option<&ToolName> {
        match self {
            Self::Active { tool, .. } | Self::Consume { tool, .. } => Some(&tool.name),
            _ => None,
        }
    }

    #[expect(
        dead_code,
        reason = "tool_slug accessor reserved for step-loop diagnostics"
    )]
    fn tool_slug(&self) -> Option<&ToolSlug> {
        match self {
            Self::Active { tool, .. } | Self::Consume { tool, .. } => Some(&tool.slug),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ConsumeReturn {
    Entry,
    Active,
}

/// Coarse FSM status injected into step-executor BAML args.
#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SessionStatus {
    AwaitingOpen,
    JustOpened,
    ResultReady,
    Done,
}

/// Compact metadata about the previous step, injected into the next BAML hop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LastStepContext {
    op: Option<&'static str>,
    status: Option<&'static str>,
    archive_ref: Option<String>,
    output_header: Option<String>,
    completion: Option<String>,
    receipt: Option<ArchiveReceipt>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
struct ArchiveLedgerEntry {
    archive_ref: String,
    header: String,
}

/// JSON shape for `session_context` injected into step-executor BAML args (matches prelude `SessionContext`).
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
struct SessionContextWire {
    contract_version: &'static str,
    session_open: bool,
    status: SessionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_step_op: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_step_status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_archive_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_output_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_completion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_receipt: Option<ArchiveReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    archives: Vec<ArchiveLedgerEntry>,
}

/// Stable id for the `session_context` object shape injected into step-executor BAML args;
/// must match generated prelude `SessionContext.contract_version` in agent packages.
const SESSION_CONTEXT_CONTRACT_VERSION: &str = "session_context_v5";
const SESSION_CONTEXT_ARCHIVE_LEDGER_CAP: usize = 8;
const SESSION_CONTEXT_ARCHIVE_HEADER_CAP: usize = 240;

fn session_status_for_phase(phase: &Phase) -> SessionStatus {
    match phase {
        Phase::Entry => SessionStatus::AwaitingOpen,
        Phase::Active {
            has_done: false, ..
        } => SessionStatus::JustOpened,
        Phase::Consume { .. } => SessionStatus::ResultReady,
        Phase::Active { has_done: true, .. } | Phase::Terminal(_) => SessionStatus::Done,
    }
}

/// Build the `session_context` JSON injected into BAML function args.
///
/// FSM facts only — which operation is legal is expressed by the **per-phase**
/// BAML function's narrowed return type (`ExecuteStep__entry`, `ExecuteStep__active__…`, …).
fn strip_archive_header_for_ledger(header: &str) -> String {
    let without_ref = header
        .split_once(" · ")
        .map(|(_, rest)| rest)
        .unwrap_or(header);
    let without_size = without_ref
        .rsplit_once(" · ")
        .and_then(|(prefix, tail)| {
            let looks_like_size =
                tail.ends_with('B') || tail.ends_with("KB") || tail.ends_with("MB");
            looks_like_size.then_some(prefix)
        })
        .and_then(|prefix| {
            prefix
                .rsplit_once(" · ")
                .map(|(prefix, _line_count)| prefix)
        })
        .unwrap_or(without_ref);
    if without_size.chars().count() > SESSION_CONTEXT_ARCHIVE_HEADER_CAP {
        let truncated: String = without_size
            .chars()
            .take(SESSION_CONTEXT_ARCHIVE_HEADER_CAP)
            .collect();
        format!("{truncated}…")
    } else {
        without_size.to_string()
    }
}

fn status_for_context(phase: &Phase) -> SessionStatus {
    session_status_for_phase(phase)
}

fn build_session_context(
    phase: &Phase,
    last_step: &LastStepContext,
    archives: &[ArchiveLedgerEntry],
) -> Value {
    let wire = SessionContextWire {
        contract_version: SESSION_CONTEXT_CONTRACT_VERSION,
        session_open: phase.is_session_open(),
        status: status_for_context(phase),
        last_step_op: last_step.op,
        last_step_status: last_step.status,
        last_archive_ref: last_step.archive_ref.clone(),
        last_output_header: last_step.output_header.clone(),
        last_completion: last_step.completion.clone(),
        last_receipt: last_step.receipt.clone(),
        archives: archives.to_vec(),
    };
    serde_json::to_value(wire).unwrap_or(Value::Null)
}

fn receipt_from_result(result: &Value) -> Option<ArchiveReceipt> {
    result
        .get("receipt")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn step_op(result: &Value) -> Option<&str> {
    result.get("op").and_then(Value::as_str).or_else(|| {
        result
            .get("step")
            .and_then(|step| step.get("op"))
            .and_then(Value::as_str)
    })
}

// Archive reads reach this loop in two shapes. Direct plan values retain `op`, while
// dispatched reads replace it with the rendered output and root-level pagination fields.
fn is_archive_read_result(result: &Value) -> bool {
    matches!(step_op(result), Some("SearchRead" | "PageRead")) || result.get("has_more").is_some()
}

/// True when an archive read returns exactly the output a prior read already produced.
///
/// Repeated successful reads add no information and let a model spend the whole step
/// budget re-reading the same `@N`. Treating that as terminal consumes the receipt
/// instead of surfacing `max_steps_exhausted`.
fn is_repeated_archive_read_output(result: &Value, steps: &[Value]) -> bool {
    if !is_archive_read_result(result) {
        return false;
    }
    let Some(current_output) = result
        .get("output")
        .and_then(Value::as_str)
        .filter(|output| !output.is_empty())
    else {
        return false;
    };
    steps.iter().any(|step| {
        is_archive_read_result(step)
            && step.get("output").and_then(Value::as_str) == Some(current_output)
    })
}

fn infer_last_step_context(
    phase_before_hop: &Phase,
    status: StepStatus,
    result: &Value,
) -> LastStepContext {
    let op = match phase_before_hop {
        Phase::Entry => {
            if status == StepStatus::Open {
                Some("open")
            } else if status == StepStatus::Duplicate {
                Some("duplicate_send")
            } else if status == StepStatus::Done {
                if result.get("has_more").is_some() {
                    Some("read")
                } else if result.get("archive_ref").is_some() {
                    Some("send")
                } else {
                    None
                }
            } else {
                None
            }
        }
        Phase::Active {
            has_done: false, ..
        } => {
            if status == StepStatus::Duplicate {
                Some("duplicate_send")
            } else if result.get("archive_ref").is_some() {
                Some("send")
            } else if result.get("has_more").is_some() {
                Some("read")
            } else {
                None
            }
        }
        Phase::Active { has_done: true, .. } => match status {
            StepStatus::Finished => Some("finish"),
            StepStatus::Aborted => Some("abort"),
            StepStatus::Duplicate => Some("duplicate_send"),
            _ if result.get("archive_ref").is_some() => Some("send"),
            _ if result.get("has_more").is_some() => Some("read"),
            _ => None,
        },
        Phase::Consume { .. } => match status {
            StepStatus::Finished => Some("finish"),
            StepStatus::Aborted => Some("abort"),
            _ if result.get("has_more").is_some() => Some("read"),
            _ => None,
        },
        Phase::Terminal(_) => None,
    };

    LastStepContext {
        op,
        status: Some(status.as_str()),
        archive_ref: result
            .get("archive_ref")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        output_header: result
            .get("output")
            .and_then(Value::as_str)
            .map(|s| s.lines().next().unwrap_or(s).to_string()),
        completion: result
            .get("result")
            .and_then(|r| r.get("completion"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        receipt: receipt_from_result(result),
    }
}

/// Extract the status string from a tool execution result.
/// Handles `{ "status": "..." }` and `{ "output": { "status": "..." } }`.
fn update_archive_ledger(archives: &mut Vec<ArchiveLedgerEntry>, step: &LastStepContext) {
    let (Some(archive_ref), Some(header)) =
        (step.archive_ref.as_ref(), step.output_header.as_ref())
    else {
        return;
    };
    if archives
        .iter()
        .any(|entry| entry.archive_ref == *archive_ref)
    {
        return;
    }
    archives.push(ArchiveLedgerEntry {
        archive_ref: archive_ref.clone(),
        header: strip_archive_header_for_ledger(header),
    });
    if archives.len() > SESSION_CONTEXT_ARCHIVE_LEDGER_CAP {
        let drop_count = archives.len() - SESSION_CONTEXT_ARCHIVE_LEDGER_CAP;
        tracing::debug!(
            drop_count,
            keep_count = SESSION_CONTEXT_ARCHIVE_LEDGER_CAP,
            "session_context archive ledger truncated"
        );
        archives.drain(0..drop_count);
    }
}

fn extract_status(result: &Value) -> Option<StepStatus> {
    let s = result.get("status").and_then(Value::as_str).or_else(|| {
        result
            .get("output")
            .and_then(|o| o.get("status"))
            .and_then(Value::as_str)
    })?;
    StepStatus::parse(s)
}

fn result_is_read_only_finish(result: &Value) -> bool {
    matches!(
        result.get("op").and_then(Value::as_str),
        Some("ReadOnlyFinish")
    ) || matches!(
        result
            .get("step")
            .and_then(|s| s.get("op"))
            .and_then(Value::as_str),
        Some("ReadOnlyFinish")
    )
}

/// Unified-primary `__entry` hops return structured variants without tool-session `status`.
fn unified_entry_status_fallback(result: &Value) -> Option<StepStatus> {
    let v = result
        .get("output")
        .filter(|x| !x.is_null())
        .unwrap_or(result);
    if v.get("action").and_then(Value::as_str) == Some("AskUser") {
        return Some(StepStatus::Done);
    }
    if v.get("goal").is_some() && v.get("nodes").is_some() {
        return Some(StepStatus::Finished);
    }
    None
}

/// Result of a completed step executor loop (FSM hops only — not the chat-layer user reply).
#[derive(Debug, serde::Serialize)]
pub struct StepExecutorResult {
    pub last: Value,
    pub steps: Vec<Value>,
    pub session_context: Value,
    pub selected_tool: Option<ToolName>,
}

/// Select the per-phase BAML function identity for polymorphic narrowing.
fn phase_function_id(base: &BamlPromptName, phase: &Phase) -> BamlFunctionId {
    match phase {
        Phase::Entry => BamlFunctionId::variant(base.clone(), VariantPhase::Entry),
        Phase::Active { tool, .. } => BamlFunctionId::variant(
            base.clone(),
            VariantPhase::Active {
                tool_slug: tool.slug.to_string(),
            },
        ),
        Phase::Consume { tool, .. } => BamlFunctionId::variant(
            base.clone(),
            VariantPhase::Consume {
                tool_slug: tool.slug.to_string(),
            },
        ),
        Phase::Terminal(_) => BamlFunctionId::base(base.as_str()),
    }
}

/// Extract tool_name from a step executor result (Open step output).
fn tool_binding_from_name(raw: &str) -> Option<ToolBinding> {
    match ToolName::parse(raw) {
        Ok(name) => {
            let slug = name.slug();
            Some(ToolBinding { name, slug })
        }
        Err(e) => {
            tracing::warn!(raw_tool_name = raw, error = %e, "step_executor_loop: invalid tool name");
            None
        }
    }
}

fn tool_binding_from_receipt(receipt: &ArchiveReceipt) -> Option<ToolBinding> {
    tool_binding_from_name(&receipt.tool_name)
}

fn extract_tool_binding(result: &Value) -> Option<ToolBinding> {
    fn binding_from_tool_name_str(raw: &str) -> Option<ToolBinding> {
        tool_binding_from_name(raw)
    }

    fn extract_tool_binding_inner(result: &Value) -> Option<ToolBinding> {
        let raw = result
            .get("tool_name")
            .and_then(Value::as_str)
            .or_else(|| {
                result
                    .get("step")
                    .and_then(|s| s.get("tool_name"))
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                result
                    .get("output")
                    .and_then(|o| o.get("tool_name"))
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                result
                    .get("output")
                    .and_then(|o| o.get("step"))
                    .and_then(|s| s.get("tool_name"))
                    .and_then(Value::as_str)
            })?;
        binding_from_tool_name_str(raw)
    }

    extract_tool_binding_inner(result).or_else(|| {
        // LLM sometimes returns `SystemInternal_a2aSessionPlan` shape (`step` + `citations`) at
        // the root without a top-level `op`; normalize to a flat step for tool_name extraction.
        let Value::Object(map) = result else {
            return None;
        };
        if map.contains_key("op") || !map.contains_key("step") {
            return None;
        }
        let unwrapped = unwrap_session_plan_step_shape_for_phase_output(result.clone());
        extract_tool_binding_inner(&unwrapped)
    })
}

/// Run the step executor loop entirely in Rust.
///
/// Locks the manager per-hop (not for the entire loop) so other host helpers
/// (`__execution_session_invoke`, `__baml_invoke`) can interleave. Each hop:
/// lock -> `invoke_function_with_intra` -> unlock -> advance FSM.
///
/// The loop holds a local `Vec` of **graph** line values not yet merged into the provider read
/// used for the next hop’s tags (see [`BamlRuntimeManager::invoke_function_with_intra`]). When `step_intra_supplement_mirror` is
/// `Some(s)`, `s` is set before each `invoke` to the supplement used for that hop
/// (tests: share with an interceptor that calls
/// [`BamlRuntimeManager::merged_conversation_history_lines_json`] with the same slice).
pub async fn run_step_executor_loop(
    manager: &Arc<RwLock<BamlRuntimeManager>>,
    scope: &context::RuntimeScope,
    function_name: &str,
    base_args: Value,
    max_steps: usize,
    step_intra_supplement_mirror: Option<Arc<Mutex<Vec<Value>>>>,
) -> Result<StepExecutorResult> {
    let base = BamlPromptName::new(function_name);
    let mut phase = Phase::Entry;
    let mut steps: Vec<Value> = Vec::new();
    let mut last = Value::Null;
    let mut step_intra_supplement: Vec<Value> = Vec::new();
    let mut last_step_context = LastStepContext::default();
    let mut archive_ledger: Vec<ArchiveLedgerEntry> = Vec::new();
    // Carries the prior hop's terminal projected rows so we do not re-read the graph
    // at the start of the next hop. A fresh read can race async provenance / projection
    // and disagree with the previous hop's snapshot.
    let mut prev_hop_graph_rows: Option<Vec<ProjectedConversationRow>> = None;

    for hop_idx in 0..max_steps {
        if matches!(phase, Phase::Terminal(_)) {
            break;
        }

        let function_id = phase_function_id(&base, &phase);
        let candidate = function_id.full_name();

        // Phase function MUST exist — absence means the agent package was built without
        // phase function generation (codegen bug or stale tarball). Fail hard so the
        // error surfaces immediately rather than silently using the full-union base
        // function, which would let the LLM emit any op regardless of FSM state.
        {
            let guard = manager.read().await;
            if guard.get_function_signature(&candidate).is_none() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "step executor: phase function '{candidate}' not found in agent schema. \
                     Rebuild the agent package to regenerate phase functions.",
                )));
            }
        }
        let current_function = candidate.clone();

        let session_context = build_session_context(&phase, &last_step_context, &archive_ledger);

        let mut args = match base_args.as_object() {
            Some(obj) => Value::Object(obj.clone()),
            None => Value::Object(serde_json::Map::new()),
        };
        if let Some(obj) = args.as_object_mut() {
            obj.insert("session_context".to_string(), session_context);
        }

        tracing::debug!(
            hop = hop_idx,
            prompt = %function_id.prompt_name(),
            function = %current_function,
            phase = ?phase,
            "step_executor_loop: starting hop"
        );

        let p_before = if let Some(rows) = prev_hop_graph_rows.take() {
            rows
        } else {
            let guard = manager.read().await;
            guard
                .read_provider_conversation_rows(scope, &current_function)
                .await?
        };

        if let Some(m) = step_intra_supplement_mirror.as_ref() {
            *m.lock()
                .expect("step_intra_supplement mirror mutex poisoned") =
                step_intra_supplement.clone();
        }

        let result = {
            let guard = manager.read().await;
            guard
                .invoke_function_with_intra(scope, &current_function, args, &step_intra_supplement)
                .await
        };

        match &result {
            Ok(v) => tracing::debug!(hop = hop_idx, result = %v, "step_executor_loop: hop ok"),
            Err(e) => tracing::error!(hop = hop_idx, error = %e, "step_executor_loop: hop error"),
        }

        let result = result?;

        let read_only_finish = result_is_read_only_finish(&result);
        let status = match extract_status(&result) {
            Some(s) => s,
            None => {
                if read_only_finish {
                    // Passthrough LLM shape without tool-session status — drive graph growth like Done.
                    StepStatus::Done
                } else if matches!(phase, Phase::Entry) {
                    match unified_entry_status_fallback(&result) {
                        Some(s) => s,
                        None => {
                            phase = Phase::Terminal(TerminalReason::MissingStatus);
                            break;
                        }
                    }
                } else if matches!(phase, Phase::Consume { .. }) {
                    // Root synthesis variants in a consume union are terminal responses rather
                    // than tool-session envelopes, so they intentionally carry no status.
                    StepStatus::Finished
                } else {
                    phase = Phase::Terminal(TerminalReason::MissingStatus);
                    break;
                }
            }
        };

        let consumed_receipt_ref = if matches!(phase, Phase::Consume { .. })
            && (read_only_finish || matches!(status, StepStatus::Finished | StepStatus::Aborted))
        {
            last_step_context.receipt.as_ref().and_then(|receipt| {
                baml_rt_tools::archive_read::ShortRef::parse(&receipt.archive_ref)
            })
        } else {
            None
        };

        let p_after =
            read_provider_conversation_rows_after_hop(manager, scope, &current_function).await?;
        append_step_intra_deltas(&mut step_intra_supplement, &p_before, &p_after);

        // Content-loss signal: a visible row left the agent view between snapshots.
        // With the uncapped identity-based read this is rare (delegation dedup or an
        // explicit removal); log it instead of failing the hop.
        {
            use std::collections::HashSet;
            let after_anchors: HashSet<&str> = p_after
                .iter()
                .map(|row| row.activity_anchor.as_str())
                .collect();
            let evicted: Vec<&str> = p_before
                .iter()
                .map(|row| row.activity_anchor.as_str())
                .filter(|anchor| !after_anchors.contains(anchor))
                .collect();
            if !evicted.is_empty() {
                tracing::warn!(
                    hop = hop_idx,
                    function = %current_function,
                    evicted_anchors = ?evicted,
                    "step_executor_loop: visible row evicted from the agent view between snapshots"
                );
            }
        }
        prev_hop_graph_rows = Some(p_after);

        if let Some(archive_ref) = consumed_receipt_ref {
            let guard = manager.read().await;
            guard
                .mark_archive_send_consumed(scope.context_id(), archive_ref)
                .await?;
        }

        // Capture the last result with meaningful output (Done has output,
        // Finished/Open/Sent are status-only). This ensures `last` contains
        // the tool data, not a bare `{"status":"finished"}`.
        let has_content = result.get("output").is_some()
            || result.get("message").is_some()
            || result.get("reply").is_some();
        if matches!(
            status,
            StepStatus::Finished | StepStatus::Aborted | StepStatus::Open
        ) || has_content
            || last.is_null()
        {
            last = result.clone();
        }
        steps.push(result.clone());
        let mut next_step_context = if read_only_finish {
            LastStepContext {
                op: Some("readonly_finish"),
                status: Some("finished"),
                archive_ref: None,
                output_header: None,
                completion: None,
                receipt: None,
            }
        } else {
            infer_last_step_context(&phase, status, &result)
        };
        if next_step_context.op == Some("read") {
            if next_step_context.archive_ref.is_none() {
                next_step_context.archive_ref = last_step_context.archive_ref.clone();
            }
            if next_step_context.completion.is_none() {
                next_step_context.completion = last_step_context.completion.clone();
            }
        }

        if status == StepStatus::Done
            && is_repeated_archive_read_output(&result, &steps[..steps.len() - 1])
        {
            tracing::warn!(
                hop = hop_idx,
                function = %current_function,
                "step_executor_loop: repeated archive read returned no new output; terminating with last result"
            );
            last_step_context = next_step_context;
            phase = Phase::Terminal(TerminalReason::Finished);
            break;
        }

        update_archive_ledger(&mut archive_ledger, &next_step_context);
        last_step_context = next_step_context;

        // Advance FSM based on current phase + status.
        phase = if read_only_finish {
            Phase::Terminal(TerminalReason::Finished)
        } else {
            match phase {
                Phase::Entry => {
                    if matches!(status, StepStatus::Done | StepStatus::Duplicate) {
                        if matches!(last_step_context.op, Some("send") | Some("duplicate_send")) {
                            // A direct Send has no open session to bind, but its receipt is the
                            // authoritative producing-tool binding. A completed Send without one
                            // would reopen Entry and make the old repeat-Send loop possible.
                            let receipt = last_step_context.receipt.as_ref().ok_or_else(|| {
                                BamlRtError::InvalidArgument(format!(
                                    "step executor contract violation ({current_function}): \
                                     completed Send did not provide an archive receipt"
                                ))
                            })?;
                            let tool = tool_binding_from_receipt(receipt).ok_or_else(|| {
                                BamlRtError::InvalidArgument(format!(
                                    "step executor contract violation ({current_function}): \
                                     completed Send receipt has invalid tool name {:?}",
                                    receipt.tool_name
                                ))
                            })?;
                            Phase::Consume {
                                tool,
                                return_to: ConsumeReturn::Entry,
                            }
                        } else {
                            // Entry archive reads are already legal and do not create a receipt.
                            Phase::Entry
                        }
                    } else if status == StepStatus::Finished {
                        Phase::Terminal(TerminalReason::Finished)
                    } else if status == StepStatus::Aborted {
                        Phase::Terminal(TerminalReason::Aborted)
                    } else if status != StepStatus::Open {
                        return Err(BamlRtError::InvalidArgument(format!(
                            "step executor contract violation ({current_function}): \
                             expected ENTRY hop to yield status 'open', 'done', or unified terminal, got '{}'",
                            status.as_str()
                        )));
                    } else {
                        let tool = extract_tool_binding(&result);
                        match tool {
                            Some(binding) => Phase::Active {
                                tool: binding,
                                has_done: false,
                            },
                            None => {
                                return Err(BamlRtError::InvalidArgument(format!(
                                    "step executor ({current_function}): Open hop yielded no tool_name"
                                )));
                            }
                        }
                    }
                }
                Phase::Active { tool, has_done } => match status {
                    StepStatus::Finished => {
                        if !has_done {
                            return Err(BamlRtError::InvalidArgument(format!(
                                "step executor contract violation ({current_function}): \
                                 Finish is invalid before any Send completed with status done — emit Send first"
                            )));
                        }
                        Phase::Terminal(TerminalReason::Finished)
                    }
                    StepStatus::Aborted => Phase::Terminal(TerminalReason::Aborted),
                    StepStatus::Open => {
                        return Err(BamlRtError::InvalidArgument(format!(
                            "step executor contract violation ({current_function}): \
                             unexpected nested Open while session is active"
                        )));
                    }
                    StepStatus::Done
                    | StepStatus::Sent
                    | StepStatus::Streaming
                    | StepStatus::Suspended
                    | StepStatus::Duplicate => {
                        let inferred = infer_last_step_context(
                            &Phase::Active {
                                tool: tool.clone(),
                                has_done,
                            },
                            status,
                            &result,
                        );
                        let send_completed =
                            matches!(inferred.op, Some("send") | Some("duplicate_send"));
                        if send_completed {
                            Phase::Consume {
                                tool,
                                return_to: ConsumeReturn::Active,
                            }
                        } else {
                            Phase::Active { tool, has_done }
                        }
                    }
                },
                Phase::Consume { tool, return_to } => match status {
                    StepStatus::Finished => Phase::Terminal(TerminalReason::Finished),
                    StepStatus::Aborted => Phase::Terminal(TerminalReason::Aborted),
                    StepStatus::Done
                    | StepStatus::Sent
                    | StepStatus::Streaming
                    | StepStatus::Suspended
                    | StepStatus::Duplicate => match return_to {
                        ConsumeReturn::Entry => Phase::Entry,
                        ConsumeReturn::Active => Phase::Active {
                            tool,
                            has_done: true,
                        },
                    },
                    StepStatus::Open => {
                        return Err(BamlRtError::InvalidArgument(format!(
                            "step executor contract violation ({current_function}): \
                             Open is invalid while consuming an archived result"
                        )));
                    }
                },
                Phase::Terminal(_) => break,
            }
        };

        if matches!(phase, Phase::Terminal(_)) {
            break;
        }
    }

    if matches!(
        phase,
        Phase::Entry | Phase::Active { .. } | Phase::Consume { .. }
    ) && steps.len() >= max_steps
    {
        return Err(BamlRtError::from(
            baml_rt_core::StepPlanRecovery::max_steps_exhausted(
                format!("{phase:?}"),
                last_step_context.op.unwrap_or("unknown"),
                last_step_context.archive_ref.as_deref(),
                steps.len(),
            ),
        ));
    }

    let session_context = build_session_context(&phase, &last_step_context, &archive_ledger);
    let selected_tool = phase.selected_tool().cloned();

    Ok(StepExecutorResult {
        last,
        steps,
        session_context,
        selected_tool,
    })
}

#[cfg(test)]
mod session_context_wire_tests {
    use baml_rt_tools::{ToolName, archive_refs::ArchiveReceipt};

    use super::{
        ConsumeReturn, LastStepContext, Phase, SESSION_CONTEXT_CONTRACT_VERSION,
        SessionContextWire, SessionStatus, ToolBinding, build_session_context,
        update_archive_ledger,
    };

    #[test]
    fn session_context_wire_json_stable() {
        let v = serde_json::to_value(SessionContextWire {
            contract_version: SESSION_CONTEXT_CONTRACT_VERSION,
            session_open: true,
            status: SessionStatus::Done,
            last_step_op: Some("send"),
            last_step_status: Some("done"),
            last_archive_ref: Some("@1".to_string()),
            last_output_header: Some("@1 · \"tool: Send\" · 1L · 10B".to_string()),
            last_completion: Some("DONE".to_string()),
            last_receipt: None,
            archives: vec![super::ArchiveLedgerEntry {
                archive_ref: "@1".to_string(),
                header: "tool: Send".to_string(),
            }],
        })
        .expect("serialize session_context");
        assert_eq!(v["contract_version"], "session_context_v5");
        assert_eq!(v["session_open"], true);
        assert_eq!(v["status"], "done");
        assert_eq!(v["last_step_op"], "send");
        assert_eq!(v["last_completion"], "DONE");
        assert_eq!(v["archives"][0]["archive_ref"], "@1");
    }

    #[test]
    fn session_context_reports_result_ready_and_archive_ledger() {
        let last = LastStepContext {
            op: Some("send"),
            status: Some("done"),
            archive_ref: Some("@7".to_string()),
            output_header: Some(
                "@7 · mcp/grafana/query_prometheus:Send(expr=\"up\") · 7L · 2.8KB".to_string(),
            ),
            completion: None,
            receipt: Some(ArchiveReceipt {
                archive_ref: "@7".to_string(),
                tool_name: "mcp/grafana/query_prometheus".to_string(),
                action_identity: "Send(expr=\"up\")".to_string(),
                line_count: 7,
                byte_count: 2_800,
                size_display: "2.7KB".to_string(),
                completion: "done".to_string(),
                search_hints: vec!["id".to_string(), "status".to_string()],
            }),
        };
        let mut archives = Vec::new();
        update_archive_ledger(&mut archives, &last);
        let name = ToolName::parse("mcp/grafana/query_prometheus").expect("valid tool");
        let phase = Phase::Consume {
            tool: ToolBinding {
                slug: name.slug(),
                name,
            },
            return_to: ConsumeReturn::Entry,
        };
        let v = build_session_context(&phase, &last, &archives);
        assert_eq!(v["contract_version"], "session_context_v5");
        assert_eq!(v["session_open"], false);
        assert_eq!(v["status"], "result_ready");
        assert_eq!(v["last_receipt"]["archive_ref"], "@7");
        assert_eq!(v["last_receipt"]["completion"], "done");
        assert_eq!(
            v["last_receipt"]["search_hints"],
            serde_json::json!(["id", "status"])
        );
        assert_eq!(v["archives"][0]["archive_ref"], "@7");
        assert_eq!(
            v["archives"][0]["header"],
            "mcp/grafana/query_prometheus:Send(expr=\"up\")"
        );
    }

    #[test]
    fn repeated_archive_read_output_is_terminal() {
        use super::is_repeated_archive_read_output;

        // Dispatched reads no longer retain the plan's `op`; they expose their archive-read
        // identity through pagination fields. This is the shape returned during a real turn.
        let prior = serde_json::json!({
            "status": "done",
            "has_more": false,
            "next_offset": 2,
            "output": "@1 · discover_agents ...\n  - name: clickup-agent",
        });
        let repeat = serde_json::json!({
            "status": "done",
            "has_more": false,
            "next_offset": 2,
            "output": "@1 · discover_agents ...\n  - name: clickup-agent",
        });
        let advanced = serde_json::json!({
            "status": "done",
            "has_more": false,
            "next_offset": 3,
            "output": "@1 · discover_agents ...\n  - name: notion-agent",
        });
        let send = serde_json::json!({
            "op": "Send",
            "input": {"query": "clickup"},
            "status": "done",
            "output": "@1 · discover_agents ...\n  - name: clickup-agent",
        });

        assert!(is_repeated_archive_read_output(
            &repeat,
            std::slice::from_ref(&prior)
        ));
        assert!(!is_repeated_archive_read_output(
            &advanced,
            std::slice::from_ref(&prior)
        ));
        assert!(!is_repeated_archive_read_output(
            &send,
            std::slice::from_ref(&prior)
        ));
    }
}
