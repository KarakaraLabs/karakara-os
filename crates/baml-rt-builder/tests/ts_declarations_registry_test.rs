// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Ownership and conflict evidence for the TypeScript declaration registry.
//!
//! One authoritative test covers the runtime inventory, the conflict matrix, IR dependency
//! retention, and the Notion regression that graph DTOs are emitted exactly once.

use std::collections::{BTreeSet, HashMap, HashSet};

#[cfg(feature = "notion")]
use baml_rt_builder::builder::tool_schema_graph::{
    ToolSchemaGraph, typescript::render_tool_typescript_surface,
};
use baml_rt_builder::builder::{
    baml_signature_gen::extract_baml_signatures,
    ir_to_ts::collect_ir_type_declarations,
    tool_schema_graph::{SchemaLocation, SchemaOwner, ToolSchemaRole},
    ts_declarations::{TsDeclaration, TsDeclarationOrigin, TsDeclarationRegistry},
};
#[cfg(feature = "notion")]
use baml_rt_tools::tool_catalog::resolve_manifest_tools;
use baml_rt_tools::{
    tools::ToolName,
    ts_gen::{render_runtime_typescript, runtime_typescript_symbols},
};
#[cfg(feature = "notion")]
use baml_tools_notion as _;
use internal_baml_core::feature_flags::FeatureFlags;

#[test]
fn ts_declaration_registry_ownership_and_conflicts() {
    assert_runtime_inventory_is_real();
    assert_runtime_versus_graph_conflict();
    assert_runtime_versus_ir_suppression();
    assert_identical_duplicates_dedupe();
    assert_graph_shadows_authored_ir();
    assert_ir_only_dependency_is_kept();

    #[cfg(feature = "notion")]
    assert_notion_graph_symbols_are_single_copy();
}

fn assert_runtime_inventory_is_real() {
    let runtime = render_runtime_typescript().expect("render runtime TypeScript");
    let inventory: BTreeSet<&str> = runtime_typescript_symbols().iter().copied().collect();
    assert!(!inventory.is_empty(), "runtime symbol inventory");

    for symbol in &inventory {
        assert!(
            runtime.contains(&format!("export type {symbol}"))
                || runtime.contains(&format!("export interface {symbol}")),
            "runtime block must declare reserved symbol `{symbol}`"
        );
    }

    // The old hard-coded bootstrap skip list is now a subset of the runtime inventory.
    for symbol in [
        "StructuredReply",
        "TextPart",
        "DataPart",
        "ReplyPart",
        "ReplyMediaType",
    ] {
        assert!(
            inventory.contains(symbol),
            "`{symbol}` must remain runtime-reserved"
        );
    }
}

fn assert_runtime_versus_graph_conflict() {
    let mut registry = new_runtime_registry();
    let error = registry
        .insert(declaration(
            "JsonValue",
            "export interface JsonValue { graph: true; }",
            graph_origin("JsonValue"),
        ))
        .expect_err("a graph root cannot shadow a runtime symbol")
        .to_string();

    assert!(
        error.contains("conflicting TypeScript declaration `JsonValue`"),
        "diagnostic: {error}"
    );
    for fragment in [
        "tool schema graph",
        "runtime block",
        "matrix/tool",
        "/properties/JsonValue",
    ] {
        assert!(error.contains(fragment), "diagnostic `{fragment}`: {error}");
    }
}

fn assert_runtime_versus_ir_suppression() {
    let mut registry = new_runtime_registry();
    registry
        .insert(declaration(
            "StructuredReply",
            "export interface StructuredReply { legacy_ir_copy: true; }",
            TsDeclarationOrigin::BamlIr,
        ))
        .expect("runtime owns its symbols");

    assert!(registry.get("StructuredReply").is_none());
    assert!(
        !registry
            .render()
            .expect("render")
            .contains("legacy_ir_copy")
    );
}

fn assert_identical_duplicates_dedupe() {
    let mut registry = TsDeclarationRegistry::new();
    let body = "export interface Repeated { value: string; }";
    registry
        .insert(declaration("Repeated", body, TsDeclarationOrigin::BamlIr))
        .expect("first copy");
    registry
        .insert(declaration("Repeated", body, TsDeclarationOrigin::BamlIr))
        .expect("identical duplicate");

    assert_eq!(registry.len(), 1);
    assert_eq!(
        declaration_count(&registry.render().expect("render"), "Repeated"),
        1
    );
}

fn assert_graph_shadows_authored_ir() {
    let mut registry = TsDeclarationRegistry::new();
    registry
        .insert(declaration(
            "SharedDto",
            "export interface SharedDto { from_graph: true; }",
            graph_origin("SharedDto"),
        ))
        .expect("graph declaration");
    registry
        .insert(declaration(
            "SharedDto",
            "export interface SharedDto { from_ir: true; }",
            TsDeclarationOrigin::BamlIr,
        ))
        .expect("graph owns the symbol");

    assert_eq!(registry.len(), 1);
    let rendered = registry.render().expect("render");
    assert!(rendered.contains("from_graph"));
    assert!(!rendered.contains("from_ir"));
}

fn assert_ir_only_dependency_is_kept() {
    let ir = compile_ir(
        r#"
class Leaf {
  id string
}

class Root {
  leaf Leaf
}
"#,
    );
    let declarations = collect_ir_type_declarations(&ir, &HashSet::from(["Root".to_string()]))
        .expect("collect IR declarations");
    let symbols: Vec<&str> = declarations
        .iter()
        .map(|declaration| declaration.symbol.as_str())
        .collect();
    assert_eq!(symbols, ["Leaf", "Root"], "transitive closure");

    let mut registry = TsDeclarationRegistry::new();
    for declaration in declarations {
        registry.insert(declaration).expect("insert IR declaration");
    }
    assert!(registry.get("Leaf").is_some(), "IR-only dependency kept");
}

#[cfg(feature = "notion")]
fn assert_notion_graph_symbols_are_single_copy() {
    let tool = resolve_manifest_tools(&["support/notion".to_string()])
        .expect("resolve Notion metadata")
        .into_iter()
        .next()
        .expect("Notion tool present");
    let graph =
        ToolSchemaGraph::from_tools(std::slice::from_ref(&tool)).expect("lower Notion graph");
    let surface = render_tool_typescript_surface(&graph).expect("render graph TypeScript");
    let ir = compile_ir(
        tool.baml_decl
            .as_deref()
            .expect("Notion ships a pre-rendered BAML declaration"),
    );
    let needed: HashSet<String> = graph.symbols.keys().cloned().collect();
    let ir_declarations =
        collect_ir_type_declarations(&ir, &needed).expect("collect Notion IR declarations");

    let mut registry = new_runtime_registry();
    for declaration in surface.declarations {
        registry
            .insert(declaration)
            .expect("insert graph declaration");
    }
    for declaration in ir_declarations {
        registry.insert(declaration).expect("insert IR declaration");
    }

    let rendered = registry.render().expect("render merged declarations");
    assert!(
        graph.symbols.contains_key("BlockRenderMode"),
        "Notion graph must own BlockRenderMode"
    );
    assert!(!graph.symbols.is_empty(), "Notion graph symbols");
    for symbol in graph.symbols.keys() {
        assert_eq!(
            declaration_count(&rendered, symbol),
            1,
            "graph DTO `{symbol}` must appear exactly once"
        );
    }
}

fn new_runtime_registry() -> TsDeclarationRegistry {
    let mut registry = TsDeclarationRegistry::new();
    registry
        .reserve_runtime_symbols(runtime_typescript_symbols())
        .expect("reserve runtime symbols");
    registry
}

fn declaration(symbol: &str, body: &str, origin: TsDeclarationOrigin) -> TsDeclaration {
    TsDeclaration {
        symbol: symbol.to_string(),
        body: body.to_string(),
        dependencies: BTreeSet::new(),
        origin,
    }
}

fn graph_origin(symbol: &str) -> TsDeclarationOrigin {
    let tool_name = "matrix/tool";
    TsDeclarationOrigin::ToolSchema {
        owners: BTreeSet::from([SchemaOwner::PublicRoot {
            tool_name: tool_name.to_string(),
            role: ToolSchemaRole::Input,
        }]),
        source: SchemaLocation {
            tool_name: Some(ToolName::parse(tool_name).expect("matrix tool name")),
            role: Some(ToolSchemaRole::Input),
            json_pointer: format!("/properties/{symbol}"),
        },
    }
}

fn compile_ir(baml: &str) -> internal_baml_core::ir::ir_hasher::IRSignature {
    let files = HashMap::from([("declarations.baml".to_string(), baml.to_string())]);
    let runtime = baml_runtime::BamlRuntime::from_file_content(
        "test",
        &files,
        HashMap::<String, String>::new(),
        FeatureFlags::default(),
    )
    .expect("compile BAML declarations");
    extract_baml_signatures(&runtime).expect("extract IR signatures")
}

fn declaration_count(rendered: &str, symbol: &str) -> usize {
    let interface_prefix = format!("export interface {symbol} ");
    let type_prefix = format!("export type {symbol} =");
    rendered
        .lines()
        .filter(|line| line.starts_with(&interface_prefix) || line.starts_with(&type_prefix))
        .count()
}
