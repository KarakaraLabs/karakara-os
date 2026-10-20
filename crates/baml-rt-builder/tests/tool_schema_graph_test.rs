// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Boundary and parity evidence for the canonical tool schema graph.

use std::{collections::HashMap, ops::Deref};

use baml_rt_builder::builder::{
    baml_gen::render_baml_tool_interfaces_from_metadata,
    tool_schema_graph::{
        OpenInputContract, SchemaDeclarationKind, SchemaTypeRef, ToolSchemaGraph,
        baml::render_tool_schema_baml,
    },
};
use baml_rt_tools::{
    OpaqueJson, ToolBackend, ToolOrigin, tool_catalog::resolve_manifest_tools,
    tool_schema::json_schema_value, tools::ToolName,
};
use baml_rt_tools_claude::tools::ClaudeUserContentBlockDto;
use serde_json::json;

// ---------------------------------------------------------------------------
// Graph boundary: normalized projection + rendered BAML.
// ---------------------------------------------------------------------------

#[test]
fn tool_schema_graph_representative_projection_snapshot() {
    let claude_content_block = json_schema_value::<ClaudeUserContentBlockDto>();
    let opaque_json = json_schema_value::<OpaqueJson>();
    let representative_schema = json!({
        "type": "object",
        "properties": {
            "request": { "$ref": "#/$defs/NestedRequest" },
            "required_nullable": { "type": ["string", "null"] },
            "optional_note": { "type": "string" },
            "labels": { "type": "object", "additionalProperties": { "type": "string" } },
            "addresses": { "type": "array", "items": { "$ref": "#/$defs/Address" } },
            "content": {
                "oneOf": [
                    claude_content_block.clone(),
                    { "type": "array", "items": claude_content_block }
                ]
            },
            "opaque_payload": opaque_json
        },
        "required": ["request", "required_nullable", "labels", "addresses", "opaque_payload"],
        "$defs": {
            "Address": {
                "type": "object",
                "properties": {
                    "city": { "type": "string" },
                    "lines": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["city", "lines"]
            },
            "NestedRequest": {
                "type": "object",
                "properties": {
                    "address": { "$ref": "#/$defs/Address" },
                    "kind": { "$ref": "#/$defs/RequestKind" }
                },
                "required": ["address", "kind"]
            },
            "RequestKind": { "type": "string", "enum": ["create", "update"] }
        }
    });

    let schemas = HashMap::from([("RepresentativeToolInput".to_string(), representative_schema)]);
    let type_names = HashMap::from([(
        "RepresentativeToolInput".to_string(),
        "RepresentativeToolInput".to_string(),
    )]);
    let graph = ToolSchemaGraph::from_root_schemas(&schemas, &type_names)
        .expect("representative schema should lower");
    let rendered = render_tool_schema_baml(&graph).expect("render BAML");

    insta::assert_snapshot!(
        "tool_schema_graph_representative_projection",
        format!(
            "== graph projection ==\n{}\n== rendered BAML ==\n{}",
            project_graph(&graph),
            rendered.trim_end()
        )
    );
}

// ---------------------------------------------------------------------------
// Static / external parity: origin and backend must not change the graph.
// ---------------------------------------------------------------------------

#[test]
fn tool_schema_graph_static_external_parity() {
    let static_tool = resolve_manifest_tools(&["support/calculate".to_string()])
        .expect("resolve calculator")
        .into_iter()
        .next()
        .expect("calculator present");

    let static_graph =
        ToolSchemaGraph::from_tools(std::slice::from_ref(&static_tool)).expect("lower static");

    let mut external_tool = static_tool.clone();
    external_tool.name = ToolName::parse("external/calculate").expect("parse external name");
    external_tool.class_name = "ExternalCalculate".to_string();
    external_tool.origin = ToolOrigin::Guest;
    external_tool.backend = ToolBackend::External;
    external_tool.baml_decl = None;

    let external_graph = ToolSchemaGraph::from_tools(&[external_tool]).expect("lower external");

    let static_render = render_tool_schema_baml(&static_graph).expect("render static");
    let external_render = render_tool_schema_baml(&external_graph).expect("render external");

    assert_eq!(
        static_render, external_render,
        "equivalent schemas must render identically regardless of tool origin"
    );

    let static_shapes = declaration_shapes(&static_graph);
    let external_shapes = declaration_shapes(&external_graph);
    assert_eq!(
        static_shapes, external_shapes,
        "equivalent schemas must produce equivalent normalized declarations"
    );
    assert!(
        !static_shapes.is_empty(),
        "expected non-empty declaration set"
    );
}

// ---------------------------------------------------------------------------
// Production renderer: resolved metadata plus one graph, never `baml_decl`.
// ---------------------------------------------------------------------------

#[test]
fn production_tool_interfaces_use_the_graph_not_static_baml_declarations() {
    let mut tool = resolve_manifest_tools(&["support/calculate".to_string()])
        .expect("resolve calculator")
        .into_iter()
        .next()
        .expect("calculator present");
    tool.baml_decl = Some("class MustNotAppearInGeneratedBaml {}".to_string());

    let graph = ToolSchemaGraph::from_tools(std::slice::from_ref(&tool)).expect("lower graph");
    let generated = render_baml_tool_interfaces_from_metadata(&[tool], &graph)
        .expect("render production tool interfaces");

    assert!(
        generated.contains("class CalculatorInput"),
        "graph-generated BAML:\n{generated}"
    );
    assert!(
        generated.contains("input CalculatorInput"),
        "graph-generated BAML:\n{generated}"
    );
    assert!(
        !generated.contains("MustNotAppearInGeneratedBaml"),
        "production rendering must ignore compatibility baml_decl metadata"
    );
}

// ---------------------------------------------------------------------------
// Dual-render: compile `baml_decl` and graph-rendered BAML, compare wire-level IR.
// ---------------------------------------------------------------------------

#[test]
fn tool_schema_graph_dual_render_static_tool_wire_parity() {
    let tool = resolve_manifest_tools(&["support/calculate".to_string()])
        .expect("resolve calculator")
        .into_iter()
        .next()
        .expect("calculator present");

    let baml_decl = tool
        .baml_decl
        .clone()
        .expect("calculator ships a pre-rendered baml_decl");

    let graph = ToolSchemaGraph::from_tools(&[tool]).expect("lower calculator");
    let graph_baml = render_tool_schema_baml(&graph).expect("render graph BAML");

    let existing_runtime = compile_dto("existing.baml", &baml_decl);
    let graph_runtime = compile_dto("graph.baml", &graph_baml);

    let existing_sig = dto_wire_signature(&existing_runtime);
    let graph_sig = dto_wire_signature(&graph_runtime);

    assert_eq!(
        existing_sig, graph_sig,
        "graph renderer must reproduce the wire-level DTO contract of the static tool"
    );

    // The schema path drops `#[baml(alias)]` metadata (the enum identifiers differ), but the wire
    // values must survive. Assert the graph render emits the symbolic wire aliases.
    assert!(
        graph_baml.contains("@alias(\"+\")") && graph_baml.contains("@alias(\"/\")"),
        "symbolic enum wire values must survive the schema path:\n{graph_baml}"
    );
    // Field descriptions authored via `#[baml(description)]` flow into the schema and back out.
    assert!(
        graph_baml.contains("Left-hand integer operand."),
        "field descriptions must survive the schema path:\n{graph_baml}"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn compile_dto(name: &str, baml: &str) -> baml_runtime::BamlRuntime {
    let files = HashMap::from([(name.to_string(), baml.to_string())]);
    baml_runtime::BamlRuntime::from_file_content(
        "test",
        &files,
        HashMap::<String, String>::new(),
        internal_baml_core::feature_flags::FeatureFlags::default(),
    )
    .expect("compile DTO declarations")
}

fn type_str(ty: &baml_types::TypeIR) -> String {
    use baml_types::ir_type::{TypeGeneric, UnionTypeViewGeneric};
    match ty {
        TypeGeneric::Primitive(tv, _) => tv.basename().to_string(),
        TypeGeneric::Class { name, .. }
        | TypeGeneric::Enum { name, .. }
        | TypeGeneric::RecursiveTypeAlias { name, .. } => name.clone(),
        TypeGeneric::Union(u, _) => match u.view() {
            UnionTypeViewGeneric::Optional(inner) => format!("{}?", type_str(inner)),
            UnionTypeViewGeneric::OneOf(variants)
            | UnionTypeViewGeneric::OneOfOptional(variants) => {
                let parts: Vec<String> = variants.iter().map(|v| type_str(v)).collect();
                format!("({})", parts.join(" | "))
            }
            _ => "string".to_string(),
        },
        TypeGeneric::List(item, _) => format!("{}[]", type_str(item)),
        TypeGeneric::Literal(lv, _) => {
            use baml_types::LiteralValue;
            match lv {
                LiteralValue::String(s) => format!("\"{s}\""),
                LiteralValue::Int(i) => i.to_string(),
                LiteralValue::Bool(b) => b.to_string(),
            }
        }
        _ => "string".to_string(),
    }
}

/// Wire-level projection of a compiled BAML IR: class field wire names/types, enum wire values,
/// and type-alias shapes. Identifiers are resolved to wire spellings so cosmetic naming deltas
/// (enum variant identifiers) do not hide structural parity.
fn dto_wire_signature(runtime: &baml_runtime::BamlRuntime) -> Vec<String> {
    let ir = runtime.ir.deref();
    let mut lines = Vec::new();

    for class in &ir.classes {
        let mut fields: Vec<String> = class
            .elem
            .static_fields
            .iter()
            .map(|f| {
                let wire_name = f
                    .attributes
                    .alias()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| f.elem.name.clone());
                format!("{wire_name}:{}", type_str(&f.elem.r#type.elem))
            })
            .collect();
        fields.sort();
        lines.push(format!("class {}[{}]", class.elem.name, fields.join(";")));
    }

    for en in &ir.enums {
        let mut values: Vec<String> = en
            .elem
            .values
            .iter()
            .map(|(v, _)| {
                v.attributes
                    .alias()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| v.elem.0.clone())
            })
            .collect();
        values.sort();
        lines.push(format!("enum {}[{}]", en.elem.name, values.join(";")));
    }

    for alias in &ir.type_aliases {
        lines.push(format!(
            "alias {}={}",
            alias.elem.name,
            type_str(&alias.elem.r#type.elem)
        ));
    }

    lines.sort();
    lines
}

fn declaration_shapes(graph: &ToolSchemaGraph) -> Vec<(String, String)> {
    let mut shapes: Vec<(String, String)> = graph
        .symbols
        .iter()
        .map(|(symbol, id)| (symbol.clone(), graph.declarations[id].fingerprint.clone()))
        .collect();
    shapes.sort();
    shapes
}

fn project_graph(graph: &ToolSchemaGraph) -> String {
    let mut decls: Vec<_> = graph.declarations.values().collect();
    decls.sort_by_key(|d| d.symbol.clone());

    let mut out = String::new();
    for decl in decls {
        out.push_str(&format!(
            "{} ({}) fp={}\n",
            decl.symbol,
            decl.id.as_u32(),
            decl.fingerprint
        ));
        match &decl.kind {
            SchemaDeclarationKind::Object(obj) => {
                for field in &obj.fields {
                    out.push_str(&format!(
                        "  field {}: {} required={}\n",
                        field.wire_name,
                        ty_disp(graph, &field.ty),
                        field.required
                    ));
                }
            }
            SchemaDeclarationKind::Enum(e) => {
                for variant in &e.variants {
                    out.push_str(&format!(
                        "  variant {} = {:?}\n",
                        variant.preferred_identifier, variant.wire_value
                    ));
                }
            }
            SchemaDeclarationKind::Alias(ty) => {
                out.push_str(&format!("  alias {}\n", ty_disp(graph, ty)));
            }
        }
        if !decl.dependencies.is_empty() {
            let deps: Vec<String> = decl
                .dependencies
                .iter()
                .map(|id| graph.declarations[id].symbol.clone())
                .collect();
            out.push_str(&format!("  deps [{}]\n", deps.join(", ")));
        }
        out.push('\n');
    }

    // Per-tool contracts, when present.
    for (name, contract) in &graph.tools {
        out.push_str(&format!("tool {name} (class {})\n", contract.class_name));
        match &contract.open_input {
            OpenInputContract::Absent => out.push_str("  open_input: <absent>\n"),
            OpenInputContract::Present { ty, requirement } => {
                out.push_str(&format!(
                    "  open_input: {} ({requirement:?})\n",
                    ty_disp(graph, ty)
                ));
            }
        }
        out.push_str(&format!("  input: {}\n", ty_disp(graph, &contract.input)));
        out.push_str(&format!("  output: {}\n", ty_disp(graph, &contract.output)));
    }

    out
}

fn ty_disp(graph: &ToolSchemaGraph, ty: &SchemaTypeRef) -> String {
    match ty {
        SchemaTypeRef::String => "string".to_string(),
        SchemaTypeRef::Integer => "int".to_string(),
        SchemaTypeRef::Number => "float".to_string(),
        SchemaTypeRef::Boolean => "bool".to_string(),
        SchemaTypeRef::Null => "null".to_string(),
        SchemaTypeRef::JsonValue => "json".to_string(),
        SchemaTypeRef::Literal(l) => format!("literal({l:?})"),
        SchemaTypeRef::Array(inner) => format!("{}[]", ty_disp(graph, inner)),
        SchemaTypeRef::Map(inner) => format!("map<string, {}>", ty_disp(graph, inner)),
        SchemaTypeRef::Named(id) => graph
            .declarations
            .get(id)
            .map(|d| d.symbol.clone())
            .unwrap_or_else(|| format!("<unresolved {}>", id.as_u32())),
        SchemaTypeRef::Union(arms) => arms
            .iter()
            .map(|a| ty_disp(graph, a))
            .collect::<Vec<_>>()
            .join(" | "),
        SchemaTypeRef::VecOrOne(inner) => {
            format!("({} | {}[])", ty_disp(graph, inner), ty_disp(graph, inner))
        }
    }
}
