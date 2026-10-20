// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Symbol shaping helpers for the schema graph.

/// Bootstrap symbols owned by the generated BAML prelude/runtime. Tool DTOs must never claim one.
const RESERVED_SYMBOLS: &[&str] = &[
    "JsonValue",
    "OpaqueJson",
    "StructuredReply",
    "ToolFailure",
    "string",
    "int",
    "float",
    "bool",
    "null",
    "map",
];

/// Whether `name` collides with a reserved generated BAML symbol.
pub(crate) fn is_reserved_symbol(name: &str) -> bool {
    RESERVED_SYMBOLS.contains(&name)
}

/// Whether `name` is a legal BAML identifier.
pub(crate) fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
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

/// Deterministic safe identifier derived from an arbitrary wire name.
pub(crate) fn sanitize_identifier(name: &str) -> String {
    let mut shaped: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if shaped.is_empty() {
        shaped.push_str("field");
    }
    if !shaped
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic())
    {
        shaped.insert_str(0, "field_");
    }
    shaped
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
