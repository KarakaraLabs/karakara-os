// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON Schema generation trait for types derived with `#[derive(BamlType)]`.

use serde_json::Value;

/// Trait implemented by types that can emit an inline JSON Schema value.
///
/// This is automatically derived via `#[derive(BamlType)]` from the
/// `baml-derive` crate, alongside the `BamlType` and `TsType` implementations.
/// It drives the `open_input_schema` / `input_schema` / `output_schema` fields
/// in `ToolFunctionMetadata`, replacing the `schemars` dependency.
///
/// # JSON Schema output shapes
///
/// | Rust form | JSON Schema |
/// |---|---|
/// | Struct with named fields | `{"type":"object","title":…,"properties":{…},"required":[…]}` |
/// | Unit enum | `{"type":"string","title":…,"enum":["A","B",…]}` |
/// | Newtype union enum (`#[baml(union)]`) | `{"title":…,"anyOf":[{…},{…}]}` |
///
/// # Serde wire contract
///
/// The derived schema describes the JSON input wire shape, so it honors the supported
/// serde attributes:
///
/// - container `#[serde(rename_all = "...")]` and field `#[serde(rename = "...")]`
///   choose `properties` keys;
/// - `Option<T>` and `#[serde(default)]` fields are omitted from `required`;
/// - `#[serde(deny_unknown_fields)]` emits `"additionalProperties": false`;
/// - `#[serde(skip)]` / `#[serde(skip_deserializing)]` remove a field or unit-enum variant
///   from the input schema;
/// - unit enum wire values follow `#[serde(rename)]` / `#[serde(rename_all)]`.
///
/// `#[serde(skip_serializing)]` does not remove anything from this schema: the variant or
/// field is still accepted on input, which is the direction the generated document describes.
///
/// `#[serde(alias = "...")]` spellings stay accepted by serde but are not enumerated in
/// the schema. The schema lists the canonical wire spelling, which is also what generated
/// BAML emits, so the model never sees a spelling the constrained decoder will not produce.
///
/// `#[baml(skip)]` suppresses a field or variant from the schema, BAML, and TypeScript output
/// even when serde still accepts it. Use it to hide an internal value from the model-facing
/// surface; add `#[serde(skip)]` when the value should not be on the wire at all.
/// User-defined types appearing as field types must themselves implement
/// `JsonSchemaType` (guaranteed when they also derive `BamlType`).
///
/// # Unsupported serde forms
///
/// `#[serde(flatten)]`, internally or adjacently tagged enums, untagged non-union
/// enums, asymmetric renames, and custom deserializers without an explicit BAML wire
/// annotation fail the derive. Implement [`JsonSchemaType`] manually for those shapes.
pub trait JsonSchemaType: Send + Sync + 'static {
    /// Return an inline JSON Schema value for this type.
    ///
    /// For structs, this is a full `"type":"object"` schema with `properties`
    /// and `required`.  For enums and unions, it's a string-enum or `anyOf`
    /// schema respectively.
    fn json_schema_inline() -> Value;
}

/// `JsonSchemaType` for the Rust unit type `()`.
///
/// `()` maps to the empty schema `{}`, which accepts any JSON value.
impl JsonSchemaType for () {
    fn json_schema_inline() -> Value {
        serde_json::json!({})
    }
}

/// `JsonSchemaType` for `serde_json::Value`.
///
/// Maps to the empty schema `{}`, which accepts any JSON value — matching
/// the TypeScript `any` type.
impl JsonSchemaType for Value {
    fn json_schema_inline() -> Value {
        serde_json::json!({})
    }
}

/// Build a complete root JSON Schema for type `T`.
///
/// Wraps the inline schema from `T::json_schema_inline()` and adds the
/// `"$schema"` pointer so consumers receive a well-formed schema document.
pub fn root_json_schema<T: JsonSchemaType>() -> Value {
    let mut schema = T::json_schema_inline();
    if let Some(obj) = schema.as_object_mut() {
        obj.insert(
            "$schema".to_string(),
            Value::String("http://json-schema.org/draft-07/schema#".to_string()),
        );
    }
    schema
}
