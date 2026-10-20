// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Table-driven contract evidence for schema support, diagnostics, collisions, and open input.

use std::collections::HashMap;

use baml_rt_builder::builder::tool_schema_graph::{
    OpenInputContract, OpenInputRequirement, ToolSchemaGraph, baml::render_tool_schema_baml,
};
use baml_rt_tools::{OPAQUE_JSON_BAML_TYPE, OPAQUE_JSON_SCHEMA_MARKER_KEY, tools::ToolName};
use baml_tools_system::metadata::system_callback_metadata;
use serde_json::{Value, json};

fn lower(schema: Value) -> Result<ToolSchemaGraph, String> {
    let schemas = HashMap::from([("MatrixRoot".to_string(), schema)]);
    let names = HashMap::from([("MatrixRoot".to_string(), "MatrixRoot".to_string())]);
    ToolSchemaGraph::from_root_schemas(&schemas, &names).map_err(|error| error.to_string())
}

enum SupportExpectation {
    Renders(&'static [&'static str]),
    Rejects(&'static [&'static str]),
}

#[test]
fn schema_support_and_diagnostic_matrix() {
    let mut opaque = serde_json::Map::new();
    opaque.insert(
        OPAQUE_JSON_SCHEMA_MARKER_KEY.to_string(),
        Value::String(OPAQUE_JSON_BAML_TYPE.to_string()),
    );

    let cases = vec![
        (
            "primitives, requiredness, and nullable type arrays",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "count": { "type": "integer" },
                    "ratio": { "type": "number" },
                    "enabled": { "type": "boolean" },
                    "nullable": { "type": ["string", "null"] }
                },
                "required": ["text", "count", "ratio", "enabled", "nullable"],
                "additionalProperties": false
            }),
            SupportExpectation::Renders(&[
                "text string",
                "count int",
                "ratio float",
                "enabled bool",
                "nullable string | null",
            ]),
        ),
        (
            "arrays, typed maps, and enums",
            json!({
                "type": "object",
                "properties": {
                    "items": { "type": "array", "items": { "type": "string" } },
                    "labels": {
                        "type": "object",
                        "additionalProperties": { "type": "integer" }
                    },
                    "mode": { "type": "string", "enum": ["fast", "slow"] }
                },
                "required": ["items", "labels", "mode"]
            }),
            SupportExpectation::Renders(&[
                "items string[]",
                "labels map<string, int>",
                "enum MatrixRootMode",
            ]),
        ),
        (
            "oneOf vec-or-one and anyOf union",
            json!({
                "type": "object",
                "properties": {
                    "names": {
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" } }
                        ]
                    },
                    "value": {
                        "anyOf": [
                            { "type": "string" },
                            { "type": "integer" }
                        ]
                    }
                },
                "required": ["names", "value"]
            }),
            SupportExpectation::Renders(&["names (string | string[])", "value string | int"]),
        ),
        (
            "local definitions and references",
            json!({
                "type": "object",
                "properties": { "child": { "$ref": "#/$defs/Child" } },
                "required": ["child"],
                "$defs": {
                    "Child": {
                        "type": "object",
                        "properties": { "name": { "type": "string" } },
                        "required": ["name"]
                    }
                }
            }),
            SupportExpectation::Renders(&["class Child", "child Child"]),
        ),
        (
            "explicit opaque JSON marker",
            json!({
                "type": "object",
                "properties": { "payload": Value::Object(opaque) },
                "required": ["payload"]
            }),
            SupportExpectation::Renders(&["payload OpaqueJson"]),
        ),
        (
            "nested const literals",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "text" },
                    "count": { "const": 3 },
                    "enabled": { "const": true }
                },
                "required": ["kind", "count", "enabled"],
                "additionalProperties": false
            }),
            SupportExpectation::Renders(&["kind \"text\"", "count 3", "enabled true"]),
        ),
        (
            "const arms inside a union",
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "oneOf": [
                            { "const": "a" },
                            { "const": 1 },
                            { "const": true }
                        ]
                    },
                    "any_value": {
                        "anyOf": [
                            { "const": "b" },
                            { "const": 2 }
                        ]
                    }
                },
                "required": ["value", "any_value"],
                "additionalProperties": false
            }),
            SupportExpectation::Renders(&["value \"a\" | 1 | true", "any_value \"b\" | 2"]),
        ),
        (
            "enum without an explicit type",
            json!({
                "type": "object",
                "properties": { "mode": { "enum": ["fast", "slow"] } },
                "required": ["mode"],
                "additionalProperties": false
            }),
            SupportExpectation::Renders(&["enum MatrixRootMode", "mode MatrixRootMode"]),
        ),
        (
            "integer enum with an explicit type",
            json!({
                "type": "object",
                "properties": { "level": { "type": "integer", "enum": [1, 2] } },
                "required": ["level"],
                "additionalProperties": false
            }),
            SupportExpectation::Renders(&[
                "enum MatrixRootLevel",
                "level MatrixRootLevel",
                "@alias(\"1\")",
            ]),
        ),
        (
            "object const is unsupported",
            json!({
                "type": "object",
                "properties": { "value": { "const": { "nested": true } } }
            }),
            SupportExpectation::Rejects(&["`const` must be", "/properties/value"]),
        ),
        (
            "true boolean schema",
            json!(true),
            SupportExpectation::Renders(&["type MatrixRoot = OpaqueJson"]),
        ),
        (
            "empty root schema",
            json!({}),
            SupportExpectation::Renders(&["type MatrixRoot = OpaqueJson"]),
        ),
        (
            "false boolean schema",
            json!({
                "type": "object",
                "properties": { "value": false }
            }),
            SupportExpectation::Rejects(&["`false` boolean schema", "/properties/value"]),
        ),
        (
            "allOf",
            json!({
                "type": "object",
                "properties": { "value": { "allOf": [{ "type": "string" }] } }
            }),
            SupportExpectation::Rejects(&[
                "missing a supported type discriminator",
                "/properties/value",
            ]),
        ),
        (
            "external reference",
            json!({
                "type": "object",
                "properties": { "value": { "$ref": "https://example.test/schema.json" } }
            }),
            SupportExpectation::Rejects(&[
                "unsupported external JSON Schema reference",
                "/properties/value",
            ]),
        ),
        (
            "array without items",
            json!({
                "type": "object",
                "properties": { "value": { "type": "array" } }
            }),
            SupportExpectation::Rejects(&["array schema is missing `items`", "/properties/value"]),
        ),
        (
            "fixed fields with typed additional properties",
            json!({
                "type": "object",
                "properties": { "fixed": { "type": "string" } },
                "additionalProperties": { "type": "string" }
            }),
            SupportExpectation::Rejects(&["typed `additionalProperties`", "/additionalProperties"]),
        ),
    ];

    for (label, schema, expectation) in cases {
        match expectation {
            SupportExpectation::Renders(fragments) => {
                let graph = lower(schema).unwrap_or_else(|error| panic!("{label}: {error}"));
                let rendered = render_tool_schema_baml(&graph)
                    .unwrap_or_else(|error| panic!("{label}: {error}"));
                for fragment in fragments {
                    assert!(
                        rendered.contains(fragment),
                        "{label}: expected `{fragment}` in:\n{rendered}"
                    );
                }
            }
            SupportExpectation::Rejects(fragments) => {
                let error = lower(schema).expect_err(label);
                for fragment in fragments {
                    assert!(
                        error.contains(fragment),
                        "{label}: expected `{fragment}` in diagnostic: {error}"
                    );
                }
            }
        }
    }
}

enum CollisionExpectation {
    Symbols(&'static [&'static str]),
    Error(&'static str),
}

#[test]
fn schema_symbol_collision_matrix() {
    let cases = vec![
        (
            "identical public roots deduplicate",
            HashMap::from([
                ("A".to_string(), json!({ "type": "string", "enum": ["x"] })),
                ("B".to_string(), json!({ "type": "string", "enum": ["x"] })),
            ]),
            HashMap::from([
                ("A".to_string(), "SharedRoot".to_string()),
                ("B".to_string(), "SharedRoot".to_string()),
            ]),
            CollisionExpectation::Symbols(&["SharedRoot"]),
        ),
        (
            "conflicting public roots fail",
            HashMap::from([
                ("A".to_string(), json!({ "type": "string", "enum": ["x"] })),
                ("B".to_string(), json!({ "type": "string", "enum": ["y"] })),
            ]),
            HashMap::from([
                ("A".to_string(), "SharedRoot".to_string()),
                ("B".to_string(), "SharedRoot".to_string()),
            ]),
            CollisionExpectation::Error("conflicting public roots named `SharedRoot`"),
        ),
        (
            "identical definitions deduplicate",
            HashMap::from([
                (
                    "A".to_string(),
                    json!({
                        "type": "object",
                        "properties": { "value": { "$ref": "#/$defs/Shared" } },
                        "$defs": { "Shared": { "type": "string", "enum": ["same"] } }
                    }),
                ),
                (
                    "B".to_string(),
                    json!({
                        "type": "object",
                        "properties": { "value": { "$ref": "#/$defs/Shared" } },
                        "$defs": { "Shared": { "type": "string", "enum": ["same"] } }
                    }),
                ),
            ]),
            HashMap::new(),
            CollisionExpectation::Symbols(&["A", "B", "Shared"]),
        ),
        (
            "conflicting definitions qualify by stable path",
            HashMap::from([
                (
                    "A".to_string(),
                    json!({
                        "type": "object",
                        "properties": { "value": { "$ref": "#/$defs/Thing" } },
                        "$defs": { "Thing": { "type": "string", "enum": ["a"] } }
                    }),
                ),
                (
                    "B".to_string(),
                    json!({
                        "type": "object",
                        "properties": { "value": { "$ref": "#/$defs/Thing" } },
                        "$defs": { "Thing": { "type": "string", "enum": ["b"] } }
                    }),
                ),
            ]),
            HashMap::new(),
            CollisionExpectation::Symbols(&["A", "B", "Thing", "ThingDefsThing"]),
        ),
        (
            "nested declaration cannot rename a public root",
            HashMap::from([
                (
                    "Public".to_string(),
                    json!({ "type": "string", "enum": ["root"] }),
                ),
                (
                    "Container".to_string(),
                    json!({
                        "type": "object",
                        "properties": { "value": { "$ref": "#/$defs/Thing" } },
                        "$defs": { "Thing": { "type": "string", "enum": ["nested"] } }
                    }),
                ),
            ]),
            HashMap::from([("Public".to_string(), "Thing".to_string())]),
            CollisionExpectation::Symbols(&["Container", "Thing", "ThingDefsThing"]),
        ),
        (
            "reserved public root fails",
            HashMap::from([(
                "Root".to_string(),
                json!({ "type": "string", "enum": ["x"] }),
            )]),
            HashMap::from([("Root".to_string(), "StructuredReply".to_string())]),
            CollisionExpectation::Error("is reserved"),
        ),
    ];

    for (label, schemas, type_names, expectation) in cases {
        let result = ToolSchemaGraph::from_root_schemas(&schemas, &type_names);
        match expectation {
            CollisionExpectation::Symbols(expected) => {
                let graph = result.unwrap_or_else(|error| panic!("{label}: {error}"));
                let symbols: Vec<&str> = graph.symbols.keys().map(String::as_str).collect();
                assert_eq!(symbols, expected, "{label}");
            }
            CollisionExpectation::Error(fragment) => {
                let error = result.expect_err(label).to_string();
                assert!(
                    error.contains(fragment),
                    "{label}: expected `{fragment}` in diagnostic: {error}"
                );
            }
        }
    }

    let duplicate = system_callback_metadata();
    let error = ToolSchemaGraph::from_tools(&[duplicate.clone(), duplicate])
        .expect_err("duplicate tool names must fail before lowering")
        .to_string();
    assert!(
        error.contains("duplicate resolved tool metadata for `system/callback`"),
        "duplicate tool diagnostic: {error}"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedOpenInput {
    Absent,
    Optional,
    Required,
}

#[test]
fn open_input_classification_matrix() {
    let cases = [
        (
            "unit convention",
            "()",
            json!({}),
            ExpectedOpenInput::Absent,
        ),
        (
            "non-unit empty schema",
            "OptionalOpenInput",
            json!({}),
            ExpectedOpenInput::Optional,
        ),
        (
            "object without required fields",
            "OptionalObjectOpenInput",
            json!({
                "type": "object",
                "properties": { "workspace": { "type": "string" } }
            }),
            ExpectedOpenInput::Optional,
        ),
        (
            "object with a required field",
            "RequiredOpenInput",
            json!({
                "type": "object",
                "properties": { "workspace": { "type": "string" } },
                "required": ["workspace"]
            }),
            ExpectedOpenInput::Required,
        ),
    ];

    for (index, (label, type_name, schema, expected)) in cases.into_iter().enumerate() {
        let mut tool = system_callback_metadata();
        tool.name = ToolName::parse(&format!("matrix/case{index}")).expect("tool name");
        tool.open_input_type.name = type_name.to_string();
        tool.open_input_schema = schema;

        let graph = ToolSchemaGraph::from_tools(&[tool]).unwrap_or_else(|error| {
            panic!("{label}: graph construction failed: {error}");
        });
        let contract = &graph.tools[&format!("matrix/case{index}")];
        let actual = match contract.open_input {
            OpenInputContract::Absent => ExpectedOpenInput::Absent,
            OpenInputContract::Present {
                requirement: OpenInputRequirement::Optional,
                ..
            } => ExpectedOpenInput::Optional,
            OpenInputContract::Present {
                requirement: OpenInputRequirement::Required,
                ..
            } => ExpectedOpenInput::Required,
        };
        assert_eq!(actual, expected, "{label}");
    }
}
