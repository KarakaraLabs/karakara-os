// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Build errors for graph lowering and rendering.
//!
//! Messages intentionally preserve the wording of the historical `schema_to_baml` errors so the
//! compatibility path keeps its observable contract while gaining source locations.

use serde_json::Value;

use super::SchemaLocation;
use crate::builder::error::BamlBuilderError;

/// Compact, bounded schema snippet for diagnostics.
pub(crate) fn schema_snippet(schema: &Value) -> String {
    let compact =
        serde_json::to_string(schema).unwrap_or_else(|_| "<unserializable schema>".to_string());
    const MAX_LEN: usize = 200;
    if compact.len() <= MAX_LEN {
        compact
    } else {
        format!("{prefix}...", prefix = &compact[..MAX_LEN - 3])
    }
}

/// Unsupported JSON Schema construct with a source location and the historical guidance string.
pub(crate) fn unsupported_json_schema(
    detail: impl AsRef<str>,
    schema: &Value,
    location: &SchemaLocation,
) -> BamlBuilderError {
    let detail = detail.as_ref();
    let schema = schema_snippet(schema);
    BamlBuilderError::InvalidArgument(format!(
        "unsupported JSON Schema for generated BAML at {location}: {detail}; schema={schema}. Use baml_rt_tools::OpaqueJson for opaque JSON payloads."
    ))
}

/// A root/declaration schema had no recognizable discriminator (matches the historical
/// "unsupported schema format" path).
pub(crate) fn unsupported_named_schema(
    type_name: &str,
    location: &SchemaLocation,
) -> BamlBuilderError {
    BamlBuilderError::InvalidArgument(format!(
        "cannot generate BAML type for `{type_name}` at {location}: unsupported schema format"
    ))
}

/// External JSON Schema reference is out of scope for graph lowering.
pub(crate) fn external_reference(ref_path: &str, location: &SchemaLocation) -> BamlBuilderError {
    BamlBuilderError::InvalidArgument(format!(
        "unsupported external JSON Schema reference at {location}: `{ref_path}` resolves outside the current schema. Only local `#/$defs/...` and `#/definitions/...` references are supported."
    ))
}
