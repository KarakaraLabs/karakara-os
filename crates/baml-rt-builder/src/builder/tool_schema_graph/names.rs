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

/// Keywords rejected by the BAML compiler for class field names.
///
/// Keep this list aligned with `baml_core::validate::validation_pipeline::validations::reserved_names`.
const BAML_KEYWORDS: &[&str] = &[
    "std",
    "this",
    "yield",
    "emit",
    "async",
    "await",
    "image",
    "audio",
    "video",
    "pdf",
    "and",
    "and_eq",
    "any",
    "assert",
    "asserts",
    "bigint",
    "bool",
    "box",
    "break",
    "byte",
    "catch",
    "chan",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "class",
    "compl",
    "const",
    "consteval",
    "constexpr",
    "continue",
    "constructor",
    "crossinline",
    "debugger",
    "declare",
    "delegate",
    "defer",
    "do",
    "dynamic",
    "dyn",
    "elif",
    "else",
    "enum",
    "export",
    "extends",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "func",
    "function",
    "get",
    "global",
    "go",
    "goto",
    "implements",
    "import",
    "in",
    "inline",
    "instanceof",
    "int",
    "internal",
    "into",
    "join",
    "keyof",
    "lambda",
    "lazy",
    "let",
    "lock",
    "long",
    "macro",
    "module",
    "move",
    "mut",
    "namespace",
    "native",
    "new",
    "nil",
    "noexcept",
    "nonlocal",
    "not",
    "not_eq",
    "null",
    "nullptr",
    "or",
    "or_eq",
    "package",
    "params",
    "private",
    "protected",
    "protocol",
    "provides",
    "public",
    "pub",
    "readonly",
    "ref",
    "reflexpr",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "sbyte",
    "sealed",
    "select",
    "self",
    "Self",
    "set",
    "short",
    "sizeof",
    "some",
    "stackalloc",
    "static",
    "static_assert",
    "static_cast",
    "strictfp",
    "string",
    "struct",
    "subscript",
    "super",
    "switch",
    "template",
    "thread_local",
    "throw",
    "throws",
    "trait",
    "transient",
    "transitive",
    "true",
    "try",
    "typedef",
    "typealias",
    "typeof",
    "u16",
    "u32",
    "u64",
    "uint",
    "ulong",
    "union",
    "unsafe",
    "unsized",
    "unsigned",
    "use",
    "using",
    "var",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
    "with",
    "xor",
    "xor_eq",
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

/// Whether `name` is rejected by the BAML compiler when used as a class field.
pub(crate) fn is_baml_keyword(name: &str) -> bool {
    BAML_KEYWORDS.contains(&name)
}

/// Deterministic BAML field identifier for an arbitrary wire property name.
pub(crate) fn field_identifier(wire_name: &str) -> String {
    let shaped = if is_valid_identifier(wire_name) {
        wire_name.to_string()
    } else {
        sanitize_identifier(wire_name)
    };
    if is_baml_keyword(&shaped) {
        format!("field_{shaped}")
    } else {
        shaped
    }
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
