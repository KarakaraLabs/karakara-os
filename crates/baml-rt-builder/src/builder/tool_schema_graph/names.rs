// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Symbol shaping helpers for the schema graph.
//!
//! Public-root validation, reserved-symbol rejection, and nested collision qualification are
//! added in a later task when the production pipeline switches to the graph. This task only needs
//! the wire-name shaping shared by the historical renderer and the graph renderer.

/// Whether `name` is a legal BAML identifier (ASCII alpha or `_` start, then alphanumeric / `_` / `-`).
pub(crate) fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Stable BAML identifier for an enum variant wire value.
///
/// Wire literals are the source of truth; the identifier is an output concern. Prefer the
/// PascalCased wire value; fall back to `Variant{index}` for symbolic wire values that cannot be
/// a BAML identifier (the historical renderer emitted those raw, producing invalid BAML).
pub(crate) fn enum_variant_identifier(wire: &str, index: usize) -> String {
    let pascal = to_pascal_case(wire);
    if is_valid_identifier(&pascal) {
        pascal
    } else {
        format!("Variant{index}")
    }
}

/// PascalCase a wire identifier by splitting on `_` and `-`.
///
/// Matches the historical renderer so enum variant names stay stable across the migration.
pub(crate) fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for ch in s.chars() {
        if ch == '_' || ch == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(ch.to_uppercase().next().unwrap_or(ch));
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }

    result
}
