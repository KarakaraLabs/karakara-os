// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Defaults for [`ProvenanceQueryApi::query_conversation_context`]-style APIs.
//!
//! Call sites that do not pass an explicit `limit` (for example
//! `Arc<dyn ProvenanceContextReader>::conversation_context` with `None`) may still apply a
//! default cap at a higher layer (e.g. A2A prompt building). The numeric policy lives here,
//! not in transport.
//!
//! **Write path:** every material `SessionStep` / tool row should be recorded in the graph; the
//! read path must not drop or merge rows to “fix” redundant emits.

/// Default maximum number of [`baml_rt_conversation::ProvenanceConversationContextItem`] rows to
/// fetch when assembling LLM-visible `conversation_history` (A2A `ProjectingConversationContextProvider`
/// and similar). Truncation at this cap is recorded with a debug log in the store reader.
pub const DEFAULT_LLM_CONTEXT_ITEM_CAP: usize = 40;

/// Explicit upper bound for the step-executor intra-turn delta read.
///
/// The delta must see the *complete* agent view rather than the prompt window: a sliding cap can
/// evict a visible row between snapshots and break identity-based deltas. This is a practical
/// bridge until [`crate::store::AgentHistoryRead`] replaces the numeric limit with explicit
/// window / after-cursor / replay modes.
pub const STEP_EXECUTOR_DELTA_ROW_CAP: usize = 4096;
