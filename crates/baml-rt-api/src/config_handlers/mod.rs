// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Config and secret-request HTTP handlers.
//!
//! Config is keyed by bundle name; tools in a bundle share the same config.

pub mod karakara;

pub mod bundles;
pub mod common;
pub mod llm_budgets;
pub mod secrets;
