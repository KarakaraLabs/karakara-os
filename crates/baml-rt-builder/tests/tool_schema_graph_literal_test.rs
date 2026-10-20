// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Literal support for the canonical tool schema graph.

use std::collections::HashMap;

use baml_rt_builder::builder::tool_schema_graph::{
    SchemaDeclarationKind, SchemaLiteral, SchemaTypeRef, ToolSchemaGraph,
    baml::render_tool_schema_baml,
};
use serde_json::{Value, json};

fn lower(schemas: HashMap<String, Value>) -> Result<ToolSchemaGraph, String> {
    let names = schemas
        .keys()
        .map(|name| (name.clone(), name.clone()))
        .collect();
    ToolSchemaGraph::from_root_schemas(&schemas, &names).map_err(|error| error.to_string())
}

fn lower_root(schema: Value) -> Result<ToolSchemaGraph, String> {
    lower(HashMap::from([("Root".to_string(), schema)]))
}

fn compile(baml: &str) {
    let files = HashMap::from([("literal.baml".to_string(), baml.to_string())]);
    baml_runtime::BamlRuntime::from_file_content(
        "test",
        &files,
        HashMap::<String, String>::new(),
        internal_baml_core::feature_flags::FeatureFlags::default(),
    )
    .unwrap_or_else(|error| panic!("rendered BAML must compile: {error}\n{baml}"));
}

#[test]
fn nested_const_fields_render_and_compile() {
    let graph = lower_root(json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "const": "text" },
            "count": { "const": 3 },
            "enabled": { "const": true },
            "nothing": { "const": null }
        },
        "required": ["kind", "count", "enabled", "nothing"],
        "additionalProperties": false
    }))
    .expect("nested const schemas lower to literals");

    let baml = render_tool_schema_baml(&graph).expect("render nested const literals");
    for fragment in ["kind \"text\"", "count 3", "enabled true", "nothing null"] {
        assert!(
            baml.contains(fragment),
            "expected `{fragment}` in rendered BAML:\n{baml}"
        );
    }
    compile(&baml);
}

#[test]
fn root_const_values_render_and_compile() {
    let graph = lower(HashMap::from([
        ("OnlyText".to_string(), json!({ "const": "fixed" })),
        ("OnlyCount".to_string(), json!({ "const": 7 })),
        ("OnlyFlag".to_string(), json!({ "const": false })),
        ("OnlyNull".to_string(), json!({ "const": null })),
    ]))
    .expect("root const schemas lower to literals");

    let baml = render_tool_schema_baml(&graph).expect("render root const literals");
    for fragment in [
        "type OnlyText = \"fixed\"",
        "type OnlyCount = 7",
        "type OnlyFlag = false",
        "type OnlyNull = null",
    ] {
        assert!(
            baml.contains(fragment),
            "expected `{fragment}` in rendered BAML:\n{baml}"
        );
    }
    compile(&baml);
}

#[test]
fn root_const_unions_accept_non_string_literals() {
    let graph = lower(HashMap::from([
        (
            "Numbers".to_string(),
            json!({
                "oneOf": [
                    { "const": 1, "description": "First number." },
                    { "const": 2 }
                ]
            }),
        ),
        (
            "Flags".to_string(),
            json!({ "oneOf": [{ "const": true }, { "const": false }] }),
        ),
        (
            "Mixed".to_string(),
            json!({ "oneOf": [{ "const": "a" }, { "const": 1 }] }),
        ),
    ]))
    .expect("root const unions lower");

    let baml = render_tool_schema_baml(&graph).expect("render root const unions");
    for fragment in [
        "enum Numbers",
        "@alias(\"1\")",
        "type Flags = true | false",
        "type Mixed = \"a\" | 1",
    ] {
        assert!(
            baml.contains(fragment),
            "expected `{fragment}` in rendered BAML:\n{baml}"
        );
    }
    compile(&baml);
}

#[test]
fn float_const_is_retained_in_graph_but_rejected_for_baml() {
    let graph = lower_root(json!({
        "type": "object",
        "properties": { "ratio": { "const": 1.5 } },
        "required": ["ratio"],
        "additionalProperties": false
    }))
    .expect("float const lowers into the graph");

    let root = &graph.declarations[&graph.symbols["Root"]];
    let SchemaDeclarationKind::Object(object) = &root.kind else {
        panic!("Root must be an object declaration");
    };
    assert_eq!(
        object.fields[0].ty,
        SchemaTypeRef::Literal(SchemaLiteral::Number("1.5".to_string()))
    );

    let error = render_tool_schema_baml(&graph)
        .expect_err("BAML has no float literal type")
        .to_string();
    for fragment in [
        "unsupported float literal `1.5`",
        "/properties/ratio",
        "OpaqueJson",
    ] {
        assert!(
            error.contains(fragment),
            "expected `{fragment}` in float literal diagnostic: {error}"
        );
    }
}
