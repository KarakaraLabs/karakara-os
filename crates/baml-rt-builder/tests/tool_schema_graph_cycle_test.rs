// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Cycle behavior for the canonical tool schema graph.

use std::collections::HashMap;

use baml_rt_builder::builder::tool_schema_graph::{ToolSchemaGraph, baml::render_tool_schema_baml};
use baml_tools_system::metadata::system_callback_metadata;
use serde_json::{Value, json};

fn lower(schema: Value) -> Result<ToolSchemaGraph, String> {
    let schemas = HashMap::from([("Root".to_string(), schema)]);
    let names = HashMap::from([("Root".to_string(), "Root".to_string())]);
    ToolSchemaGraph::from_root_schemas(&schemas, &names).map_err(|error| error.to_string())
}

fn render(schema: Value) -> Result<String, String> {
    let graph = lower(schema)?;
    render_tool_schema_baml(&graph).map_err(|error| error.to_string())
}

fn compile(baml: &str) {
    let files = HashMap::from([("cycle.baml".to_string(), baml.to_string())]);
    baml_runtime::BamlRuntime::from_file_content(
        "test",
        &files,
        HashMap::<String, String>::new(),
        internal_baml_core::feature_flags::FeatureFlags::default(),
    )
    .unwrap_or_else(|error| panic!("rendered BAML must compile: {error}\n{baml}"));
}

#[test]
fn alias_cycles_are_rejected_with_symbols_and_sources() {
    // Primitive arms do not terminate an alias cycle in BAML; only lists and maps do.
    let error = render(json!({
        "type": "object",
        "properties": { "value": { "$ref": "#/$defs/A" } },
        "$defs": {
            "A": { "oneOf": [{ "$ref": "#/$defs/B" }, { "type": "string" }] },
            "B": { "oneOf": [{ "$ref": "#/$defs/A" }, { "type": "integer" }] }
        }
    }))
    .expect_err("alias cycle must fail before rendering");

    for fragment in [
        "unsupported recursive type aliases",
        "dependency cycle A -> B -> A",
        "participating symbols",
        "A (/$defs/A)",
        "B (/$defs/B)",
        "array or a map",
    ] {
        assert!(
            error.contains(fragment),
            "expected `{fragment}` in alias cycle diagnostic: {error}"
        );
    }
}

#[test]
fn required_class_cycles_are_rejected_with_symbols_and_sources() {
    let error = render(json!({
        "type": "object",
        "properties": { "node": { "$ref": "#/$defs/A" } },
        "$defs": {
            "A": {
                "type": "object",
                "properties": { "b": { "$ref": "#/$defs/B" } },
                "required": ["b"],
                "additionalProperties": false
            },
            "B": {
                "type": "object",
                "properties": { "a": { "$ref": "#/$defs/A" } },
                "required": ["a"],
                "additionalProperties": false
            }
        }
    }))
    .expect_err("required class cycle must fail before rendering");

    for fragment in [
        "unsupported recursive classes",
        "dependency cycle A -> B -> A",
        "participating symbols",
        "A (/$defs/A)",
        "B (/$defs/B)",
        "optional field",
    ] {
        assert!(
            error.contains(fragment),
            "expected `{fragment}` in class cycle diagnostic: {error}"
        );
    }
}

#[test]
fn required_class_cycles_are_rejected_through_aliases() {
    let error = render(json!({
        "type": "object",
        "properties": { "node": { "$ref": "#/$defs/A" } },
        "$defs": {
            "A": {
                "type": "object",
                "properties": { "edge": { "$ref": "#/$defs/AToB" } },
                "required": ["edge"],
                "additionalProperties": false
            },
            "AToB": { "oneOf": [{ "$ref": "#/$defs/B" }] },
            "B": {
                "type": "object",
                "properties": { "edge": { "$ref": "#/$defs/A" } },
                "required": ["edge"],
                "additionalProperties": false
            }
        }
    }))
    .expect_err("class cycle through aliases must fail before rendering");

    for fragment in [
        "unsupported recursive classes",
        "dependency cycle A -> B -> A",
        "A (/$defs/A)",
        "B (/$defs/B)",
    ] {
        assert!(
            error.contains(fragment),
            "expected `{fragment}` in aliased class cycle diagnostic: {error}"
        );
    }
}

#[test]
fn cycle_diagnostics_include_tool_role_and_pointer() {
    let mut tool = system_callback_metadata();
    tool.input_schema = json!({
        "type": "object",
        "properties": { "value": { "$ref": "#/$defs/A" } },
        "$defs": {
            "A": { "oneOf": [{ "$ref": "#/$defs/B" }, { "type": "string" }] },
            "B": { "oneOf": [{ "$ref": "#/$defs/A" }, { "type": "string" }] }
        }
    });

    let graph = ToolSchemaGraph::from_tools(&[tool]).expect("lower tool schema graph");
    let error = render_tool_schema_baml(&graph)
        .expect_err("alias cycle must fail before rendering")
        .to_string();

    assert!(
        error.contains("tool system/callback input/$defs/A"),
        "expected tool role and pointer for A in diagnostic: {error}"
    );
    assert!(
        error.contains("tool system/callback input/$defs/B"),
        "expected tool role and pointer for B in diagnostic: {error}"
    );
}

#[test]
fn optional_class_recursion_is_rendered_and_compiles() {
    let baml = render(json!({
        "type": "object",
        "properties": { "node": { "$ref": "#/$defs/Node" } },
        "$defs": {
            "Node": {
                "type": "object",
                "properties": { "next": { "$ref": "#/$defs/Node" } },
                "additionalProperties": false
            }
        }
    }))
    .expect("optional self-recursion is representable");

    assert!(baml.contains("next Node?"), "rendered BAML:\n{baml}");
    compile(&baml);
}

#[test]
fn array_class_recursion_is_rendered_and_compiles() {
    let baml = render(json!({
        "type": "object",
        "properties": { "node": { "$ref": "#/$defs/Node" } },
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {
                    "children": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/Node" }
                    }
                },
                "required": ["children"],
                "additionalProperties": false
            }
        }
    }))
    .expect("array self-recursion is representable");

    assert!(baml.contains("children Node[]"), "rendered BAML:\n{baml}");
    compile(&baml);
}

#[test]
fn union_with_a_primitive_arm_terminates_class_recursion() {
    let baml = render(json!({
        "type": "object",
        "properties": { "node": { "$ref": "#/$defs/Node" } },
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {
                    "value": {
                        "oneOf": [
                            { "$ref": "#/$defs/Node" },
                            { "type": "string" }
                        ]
                    }
                },
                "required": ["value"],
                "additionalProperties": false
            }
        }
    }))
    .expect("a union with a non-recursive arm is representable");

    assert!(
        baml.contains("value Node | string"),
        "rendered BAML:\n{baml}"
    );
    compile(&baml);
}

#[test]
fn alias_recursion_through_an_array_is_rendered_and_compiles() {
    let baml = render(json!({
        "type": "object",
        "properties": { "value": { "$ref": "#/$defs/A" } },
        "$defs": {
            "A": { "oneOf": [{ "$ref": "#/$defs/B" }, { "type": "string" }] },
            "B": {
                "oneOf": [
                    { "type": "array", "items": { "$ref": "#/$defs/A" } },
                    { "type": "string" }
                ]
            }
        }
    }))
    .expect("alias recursion through a list is representable");

    assert!(baml.contains("type A = "), "rendered BAML:\n{baml}");
    assert!(
        baml.contains("type B = A[] | string"),
        "rendered BAML:\n{baml}"
    );
    compile(&baml);
}
