// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for the tool schema graph review fixes: strict additional-properties
//! handling, symbol collision policy, wire-name aliasing, and source-located diagnostics.

use std::collections::HashMap;

use baml_rt_builder::builder::tool_schema_graph::{
    SchemaDeclarationKind, SchemaOwner, SchemaTypeRef, ToolSchemaGraph, ToolSchemaRole,
    baml::render_tool_schema_baml,
};
use baml_rt_tools::tools::ToolName;
use baml_tools_system::metadata::system_callback_metadata;
use serde_json::json;

fn lower(schemas: &HashMap<String, serde_json::Value>) -> Result<ToolSchemaGraph, String> {
    let names: HashMap<String, String> = schemas.keys().map(|k| (k.clone(), k.clone())).collect();
    ToolSchemaGraph::from_root_schemas(schemas, &names).map_err(|e| e.to_string())
}

#[test]
fn typed_additional_properties_on_fixed_object_is_rejected() {
    let schemas = HashMap::from([(
        "Mixed".to_string(),
        json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "additionalProperties": { "type": "string" }
        }),
    )]);
    let err = lower(&schemas).expect_err("typed additionalProperties must be rejected");
    assert!(
        err.contains("typed `additionalProperties`"),
        "expected typed additionalProperties diagnostic, got: {err}"
    );
    assert!(
        err.contains("OpaqueJson"),
        "expected OpaqueJson guidance, got: {err}"
    );
    assert!(
        err.contains("/additionalProperties"),
        "expected exact additionalProperties pointer, got: {err}"
    );
}

#[test]
fn nullable_type_arrays_keep_the_null_arm() {
    let schemas = HashMap::from([(
        "Nullable".to_string(),
        json!({
            "type": "object",
            "properties": { "value": { "type": ["string", "null"] } },
            "required": ["value"],
            "additionalProperties": false
        }),
    )]);
    let graph = lower(&schemas).expect("lower nullable type array");
    let declaration = graph.declarations[graph.symbols.get("Nullable").expect("Nullable id")]
        .kind
        .clone();
    let SchemaDeclarationKind::Object(object) = declaration else {
        panic!("Nullable must be an object");
    };
    assert_eq!(
        object.fields[0].ty,
        SchemaTypeRef::Union(vec![SchemaTypeRef::String, SchemaTypeRef::Null])
    );
    let baml = render_tool_schema_baml(&graph).expect("render nullable type array");
    assert!(
        baml.contains("value string | null"),
        "rendered BAML:\n{baml}"
    );
}

#[test]
fn boolean_schema_policy_matrix() {
    let true_root = HashMap::from([("TrueRoot".to_string(), json!(true))]);
    let graph = lower(&true_root).expect("true root schema lowers to opaque JSON");
    let baml = render_tool_schema_baml(&graph).expect("render true root schema");
    assert!(baml.contains("type TrueRoot = OpaqueJson"));

    let true_nested = HashMap::from([(
        "TrueNested".to_string(),
        json!({
            "type": "object",
            "properties": { "anything": true },
            "required": ["anything"],
            "additionalProperties": false
        }),
    )]);
    let graph = lower(&true_nested).expect("true nested schema lowers to opaque JSON");
    let baml = render_tool_schema_baml(&graph).expect("render true nested schema");
    assert!(baml.contains("anything OpaqueJson"));

    let false_root = HashMap::from([("FalseRoot".to_string(), json!(false))]);
    let err = lower(&false_root).expect_err("false root schema is unsupported");
    assert!(err.contains("`false` boolean schema"), "got: {err}");
    assert!(err.contains("<root schema>"), "got: {err}");

    let mut tool = system_callback_metadata();
    tool.input_schema = json!({
        "type": "object",
        "properties": { "impossible": false }
    });
    let err = ToolSchemaGraph::from_tools(&[tool])
        .expect_err("false nested schema is unsupported")
        .to_string();
    assert!(err.contains("`false` boolean schema"), "got: {err}");
    assert!(
        err.contains("tool system/callback input/properties/impossible"),
        "got: {err}"
    );
}

#[test]
fn graph_retains_all_declaration_owners_after_deduplication() {
    let first = system_callback_metadata();
    let mut second = first.clone();
    second.name = ToolName::parse("review/callback").expect("tool name");

    let graph = ToolSchemaGraph::from_tools(&[first, second]).expect("lower equivalent tools");
    let contract = &graph.tools["system/callback"];
    let SchemaTypeRef::Named(input_id) = &contract.input else {
        panic!("tool input must be a named declaration");
    };
    let owners = &graph.declarations[input_id].owners;
    assert!(owners.contains(&SchemaOwner::PublicRoot {
        tool_name: "system/callback".to_string(),
        role: ToolSchemaRole::Input,
    }));
    assert!(owners.contains(&SchemaOwner::PublicRoot {
        tool_name: "review/callback".to_string(),
        role: ToolSchemaRole::Input,
    }));
}

#[test]
fn conflicting_public_root_symbols_are_rejected() {
    let schemas = HashMap::from([
        (
            "A".to_string(),
            json!({ "type": "object", "properties": { "x": { "type": "string" } } }),
        ),
        (
            "B".to_string(),
            json!({ "type": "object", "properties": { "y": { "type": "integer" } } }),
        ),
    ]);
    let names = HashMap::from([
        ("A".to_string(), "Same".to_string()),
        ("B".to_string(), "Same".to_string()),
    ]);
    let err = ToolSchemaGraph::from_root_schemas(&schemas, &names)
        .expect_err("two roots mapped to one symbol must conflict")
        .to_string();
    assert!(err.contains("conflicting public roots"), "got: {err}");
}

#[test]
fn conflicting_tool_roots_report_both_owners() {
    let first = system_callback_metadata();
    let mut second = first.clone();
    second.name = ToolName::parse("review/callback").expect("tool name");
    second.input_schema = json!({
        "type": "object",
        "properties": { "different": { "type": "integer" } },
        "required": ["different"],
        "additionalProperties": false
    });

    let err = ToolSchemaGraph::from_tools(&[first, second])
        .expect_err("different public roots with one symbol must fail")
        .to_string();
    assert!(err.contains("tool system/callback input"), "got: {err}");
    assert!(err.contains("tool review/callback input"), "got: {err}");
}

#[test]
fn reserved_bootstrap_symbols_are_rejected() {
    let schemas = HashMap::from([("StructuredReply".to_string(), json!({ "type": "string" }))]);
    let err = lower(&schemas).expect_err("reserved symbol must be rejected");
    assert!(err.contains("reserved"), "got: {err}");
}

#[test]
fn invalid_and_primitive_public_symbols_are_rejected() {
    for symbol in ["not valid", "string"] {
        let schemas = HashMap::from([("Root".to_string(), json!({ "type": "string" }))]);
        let names = HashMap::from([("Root".to_string(), symbol.to_string())]);
        let err = ToolSchemaGraph::from_root_schemas(&schemas, &names)
            .expect_err("invalid public symbol")
            .to_string();
        assert!(err.contains("public root"), "got: {err}");
    }
}

#[test]
fn invalid_wire_names_are_aliased_without_identifier_collisions() {
    let schemas = HashMap::from([(
        "Weird".to_string(),
        json!({
            "type": "object",
            "properties": {
                "1st value": { "type": "string" },
                "a b": { "type": "string" },
                "a?b": { "type": "integer" }
            },
            "required": ["1st value", "a b", "a?b"],
            "additionalProperties": false
        }),
    )]);
    let graph = lower(&schemas).expect("lower");
    let baml = render_tool_schema_baml(&graph).expect("render");
    assert!(baml.contains("field_1st_value string @alias(\"1st value\")"));
    assert!(baml.contains("a_b string @alias(\"a b\")"));
    assert!(baml.contains("a_b_2 int @alias(\"a?b\")"));
    assert!(!baml.contains("1st value string"), "rendered BAML:\n{baml}");
}

#[test]
fn local_refs_use_the_owning_document_and_decode_json_pointer_tokens() {
    let schemas = HashMap::from([
        (
            "A".to_string(),
            json!({
                "type": "object",
                "properties": { "value": { "$ref": "#/$defs/Thing" } },
                "$defs": {
                    "Thing": {
                        "type": "object",
                        "properties": { "a": { "type": "string" } },
                        "additionalProperties": false
                    }
                }
            }),
        ),
        (
            "B".to_string(),
            json!({
                "type": "object",
                "properties": { "value": { "$ref": "#/$defs/Thing" } },
                "$defs": {
                    "Thing": {
                        "type": "object",
                        "properties": { "b": { "type": "integer" } },
                        "additionalProperties": false
                    }
                }
            }),
        ),
        (
            "Escaped".to_string(),
            json!({
                "type": "object",
                "properties": { "value": { "$ref": "#/$defs/Foo~1Bar~0Baz" } },
                "$defs": {
                    "Foo/Bar~Baz": {
                        "type": "object",
                        "properties": { "ok": { "type": "boolean" } },
                        "additionalProperties": false
                    }
                }
            }),
        ),
    ]);
    let graph = lower(&schemas).expect("resolve document-scoped pointers");
    let baml = render_tool_schema_baml(&graph).expect("render refs");
    assert!(
        baml.contains("class Thing {\n  a string?"),
        "rendered BAML:\n{baml}"
    );
    assert!(
        baml.contains("class ThingDefsThing {\n  b int?"),
        "rendered BAML:\n{baml}"
    );
    assert!(baml.contains("value Thing?"), "rendered BAML:\n{baml}");
    assert!(
        baml.contains("value ThingDefsThing?"),
        "rendered BAML:\n{baml}"
    );
    assert!(
        baml.contains("value Foo_Bar_Baz?"),
        "rendered BAML:\n{baml}"
    );
}

#[test]
fn local_refs_distinguish_the_same_final_key_under_different_paths() {
    let schemas = HashMap::from([(
        "NestedPaths".to_string(),
        json!({
            "type": "object",
            "properties": {
                "left": { "$ref": "#/$defs/GroupA/$defs/Item" },
                "right": { "$ref": "#/$defs/GroupB/$defs/Item" }
            },
            "$defs": {
                "GroupA": {
                    "type": "object",
                    "properties": {},
                    "$defs": {
                        "Item": {
                            "type": "object",
                            "properties": { "left": { "type": "string" } },
                            "additionalProperties": false
                        }
                    }
                },
                "GroupB": {
                    "type": "object",
                    "properties": {},
                    "$defs": {
                        "Item": {
                            "type": "object",
                            "properties": { "right": { "type": "integer" } },
                            "additionalProperties": false
                        }
                    }
                }
            }
        }),
    )]);
    let graph = lower(&schemas).expect("resolve nested local definitions");
    let baml = render_tool_schema_baml(&graph).expect("render nested local definitions");
    assert!(baml.contains("left Item?"), "rendered BAML:\n{baml}");
    assert!(
        baml.contains("right ItemDefsGroupBDefsItem?"),
        "rendered BAML:\n{baml}"
    );
}

#[test]
fn unresolved_refs_report_their_tool_role_and_pointer() {
    let mut tool = system_callback_metadata();
    tool.input_schema = json!({
        "type": "object",
        "properties": { "bad": { "$ref": "#/$defs/Missing" } }
    });

    let err = ToolSchemaGraph::from_tools(&[tool])
        .expect_err("missing local reference")
        .to_string();
    assert!(
        err.contains("unresolved local JSON Schema reference `#/$defs/Missing` at tool system/callback input/properties/bad"),
        "got: {err}"
    );
}

#[test]
fn unsupported_nested_schema_reports_its_tool_role_and_pointer() {
    let mut tool = system_callback_metadata();
    tool.input_schema = json!({
        "type": "object",
        "properties": {
            "good": { "type": "string" },
            "bad": { "allOf": [{ "type": "string" }] }
        }
    });

    let err = ToolSchemaGraph::from_tools(&[tool])
        .expect_err("allOf must be unsupported")
        .to_string();
    assert!(
        err.contains("tool system/callback input/properties/bad"),
        "expected tool, role, and nested pointer in diagnostic, got: {err}"
    );
}

#[test]
fn equivalent_public_roots_with_nested_local_definitions_deduplicate() {
    let schema = json!({
        "type": "object",
        "properties": { "parent": { "$ref": "#/$defs/Parent" } },
        "required": ["parent"],
        "$defs": {
            "Parent": {
                "type": "object",
                "properties": { "child": { "$ref": "#/$defs/Child" } },
                "required": ["child"],
                "additionalProperties": false
            },
            "Child": { "type": "string", "enum": ["ok"] }
        }
    });
    let schemas = HashMap::from([("A".to_string(), schema.clone()), ("B".to_string(), schema)]);
    let names = HashMap::from([
        ("A".to_string(), "SharedRoot".to_string()),
        ("B".to_string(), "SharedRoot".to_string()),
    ]);

    let graph = ToolSchemaGraph::from_root_schemas(&schemas, &names)
        .expect("equivalent roots with nested local definitions must deduplicate");
    let root_id = *graph.symbols.get("SharedRoot").expect("shared root symbol");
    let root = &graph.declarations[&root_id];
    assert_eq!(root.owners.len(), 2, "both roots must retain ownership");
    assert!(root.owners.contains(&SchemaOwner::StandaloneRoot {
        schema_name: "A".to_string(),
    }));
    assert!(root.owners.contains(&SchemaOwner::StandaloneRoot {
        schema_name: "B".to_string(),
    }));
}

#[test]
fn equivalent_public_roots_with_recursive_local_definitions_deduplicate() {
    let schema = json!({
        "type": "object",
        "properties": { "node": { "$ref": "#/$defs/Node" } },
        "required": ["node"],
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {
                    "next": { "anyOf": [{ "$ref": "#/$defs/Node" }, { "type": "null" }] }
                },
                "additionalProperties": false
            }
        }
    });
    let schemas = HashMap::from([("A".to_string(), schema.clone()), ("B".to_string(), schema)]);
    let names = HashMap::from([
        ("A".to_string(), "SharedRoot".to_string()),
        ("B".to_string(), "SharedRoot".to_string()),
    ]);

    let graph = ToolSchemaGraph::from_root_schemas(&schemas, &names)
        .expect("equivalent recursive roots must deduplicate");
    let root_id = *graph.symbols.get("SharedRoot").expect("shared root symbol");
    assert_eq!(graph.declarations[&root_id].owners.len(), 2);
}

#[test]
fn named_definitions_prefer_a_valid_schema_title() {
    let schemas = HashMap::from([(
        "Root".to_string(),
        json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/thing" } },
            "required": ["value"],
            "$defs": {
                "thing": { "title": "Widget", "type": "string", "enum": ["ok"] }
            }
        }),
    )]);
    let graph = lower(&schemas).expect("lower titled definition");
    assert!(
        graph.symbols.contains_key("Widget"),
        "symbols: {:?}",
        graph.symbols
    );
    assert!(
        !graph.symbols.contains_key("thing"),
        "symbols: {:?}",
        graph.symbols
    );
    let rendered = render_tool_schema_baml(&graph).expect("render titled definition");
    assert!(
        rendered.contains("enum Widget"),
        "rendered BAML:\n{rendered}"
    );
}

#[test]
fn explicit_definition_type_names_override_titles() {
    let schemas = HashMap::from([(
        "Root".to_string(),
        json!({
            "type": "object",
            "properties": { "value": { "$ref": "#/$defs/thing" } },
            "required": ["value"],
            "$defs": {
                "thing": { "title": "Widget", "type": "string", "enum": ["ok"] }
            }
        }),
    )]);
    let names = HashMap::from([
        ("Root".to_string(), "Root".to_string()),
        ("thing".to_string(), "Override".to_string()),
    ]);
    let graph = ToolSchemaGraph::from_root_schemas(&schemas, &names)
        .expect("lower explicitly named definition");
    assert!(graph.symbols.contains_key("Override"));
    assert!(!graph.symbols.contains_key("Widget"));
}
