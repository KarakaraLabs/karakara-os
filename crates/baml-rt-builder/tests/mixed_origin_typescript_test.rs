// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Authority vertical slice: one agent with a static tool, an external tool, a typed MCP tool,
//! an opaque MCP tool, and BAML signatures that overlap graph-owned DTOs.
//!
//! The slice renders the full declaration file, type-checks the fixture with `tsc`, and asserts
//! every graph-owned symbol appears exactly once.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use baml_rt_builder::builder::{
    baml_signature_gen::extract_baml_signatures, tool_schema_graph::ToolSchemaGraph,
    ts_gen::render_ts_declarations, write_canonical_tsconfig,
};
use baml_rt_tools::{
    OpaqueJson, ToolBackend, ToolOrigin,
    tool_schema::json_schema_value,
    tools::{ToolFunctionMetadata, ToolName},
};
use baml_tools_system::metadata::system_callback_metadata;
use internal_baml_core::feature_flags::FeatureFlags;
use serde_json::{Value, json};

#[test]
fn mixed_origin_typescript_authority() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_path_buf();
    let fixture = workspace
        .join("tests")
        .join("fixtures")
        .join("mixed-origin-tools");

    let temp = tempfile::TempDir::new().expect("temp agent dir");
    copy_dir_all(&fixture, temp.path()).expect("copy mixed-origin fixture");

    let static_tool = system_callback_metadata();
    let external = external_tool(
        "external/echo",
        "ExternalEchoInput",
        "ExternalEchoOutput",
        json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"],
            "additionalProperties": false
        }),
        ToolBackend::External,
    );
    let mcp_typed = external_tool(
        "mcp/typed",
        "McpTypedInput",
        "McpTypedOutput",
        json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"],
            "additionalProperties": false
        }),
        ToolBackend::Mcp,
    );
    let mcp_opaque = external_tool(
        "mcp/opaque",
        "McpOpaqueInput",
        "McpOpaqueOutput",
        json_schema_value::<OpaqueJson>(),
        ToolBackend::Mcp,
    );

    let tools = vec![static_tool, external, mcp_typed, mcp_opaque];
    let graph = ToolSchemaGraph::from_tools(&tools).expect("lower mixed-origin schemas");

    let runtime = baml_runtime::BamlRuntime::from_directory(
        &temp.path().join("baml_src"),
        HashMap::<String, String>::new(),
        FeatureFlags::default(),
    )
    .expect("compile mixed-origin BAML");
    let ir = extract_baml_signatures(&runtime).expect("extract mixed-origin IR");
    assert!(
        ir.functions.contains_key("EchoExternal") && ir.functions.contains_key("EchoMcp"),
        "fixture BAML functions must reach the IR"
    );
    assert!(
        ir.classes.contains_key("ExternalEchoInput") && ir.classes.contains_key("McpTypedInput"),
        "fixture BAML classes must overlap graph-owned DTOs"
    );

    let rendered = render_ts_declarations(
        &ir,
        &tools,
        &graph,
        &Default::default(),
        &Default::default(),
    )
    .expect("render mixed-origin declarations");
    fs::write(temp.path().join("src").join("baml-runtime.d.ts"), &rendered)
        .expect("write baml-runtime.d.ts");
    write_canonical_tsconfig(temp.path()).expect("write tsconfig");

    // Every allowlisted origin appears once.
    for tool in [
        "system/callback",
        "external/echo",
        "mcp/typed",
        "mcp/opaque",
    ] {
        assert_eq!(
            rendered.matches(&format!("\"{tool}\": ")).count(),
            3,
            "`{tool}` must appear in the input, output, and open-input maps"
        );
    }
    assert_eq!(rendered.matches("function openToolSession").count(), 1);

    // Graph DTOs survive for typed external and typed MCP tools; opaque schemas use JsonValue.
    for symbol in ["ExternalEchoInput", "McpTypedInput", "McpTypedOutput"] {
        assert_eq!(
            declaration_count(&rendered, symbol),
            1,
            "graph DTO `{symbol}` must be emitted exactly once"
        );
    }
    assert!(rendered.contains("export type McpOpaqueInput = JsonValue;"));
    assert!(rendered.contains("\"external/echo\": ExternalEchoInput;"));
    assert!(rendered.contains("\"mcp/typed\": McpTypedInput;"));

    // Graph-owned symbols shadow the IR copies generated from the fixture BAML.
    for symbol in ["ExternalEchoInput", "McpTypedInput"] {
        assert_eq!(
            rendered
                .matches(&format!("export interface {symbol} "))
                .count(),
            1,
            "IR copy of graph DTO `{symbol}` must be suppressed"
        );
    }

    // Compatibility metadata is never a declaration source.
    for forbidden in ["MustNotAppear", "extra_ts_decls"] {
        assert!(
            !rendered.contains(forbidden),
            "generated declarations must not consume `{forbidden}`"
        );
    }

    // Authored BAML that shadows a non-prelude runtime symbol is a hard conflict, not a silent
    // IR drop that retypes the function with the unrelated runtime declaration.
    let authored_ir = compile_ir_source(
        r##"
client AuthoredClient {
  provider openai-generic
  options {
    model "gpt-4o-mini"
    base_url "https://example.invalid"
    api_key "test"
  }
}

class Message {
  text string
}

function UseMessage(message: Message) -> string {
  client AuthoredClient
  prompt #"{{ message }}"#
}
"##,
    );
    let error = render_ts_declarations(
        &authored_ir,
        &tools,
        &graph,
        &Default::default(),
        &Default::default(),
    )
    .expect_err("authored BAML class must not shadow runtime Message")
    .to_string();
    assert!(
        error.contains("conflicting TypeScript declaration `Message`"),
        "diagnostic: {error}"
    );
    assert!(error.contains("runtime block"), "diagnostic: {error}");
    assert!(error.contains("BAML IR"), "diagnostic: {error}");

    run_tsc(temp.path());
}

fn compile_ir_source(baml: &str) -> internal_baml_core::ir::ir_hasher::IRSignature {
    let files = HashMap::from([("authored.baml".to_string(), baml.to_string())]);
    let runtime = baml_runtime::BamlRuntime::from_file_content(
        "test",
        &files,
        HashMap::<String, String>::new(),
        FeatureFlags::default(),
    )
    .expect("compile authored BAML");
    extract_baml_signatures(&runtime).expect("extract authored IR")
}

fn external_tool(
    name: &str,
    input_type: &str,
    output_type: &str,
    input_schema: Value,
    backend: ToolBackend,
) -> ToolFunctionMetadata {
    let mut tool = system_callback_metadata();
    tool.name = ToolName::parse(name).expect("valid mixed-origin tool name");
    tool.class_name = format!("MixedOrigin{}", input_type.trim_end_matches("Input"));
    tool.open_input_type.name = "()".to_string();
    tool.open_input_schema = json!({});
    tool.input_type.name = input_type.to_string();
    tool.input_type.ts_decl = Some("export type MustNotAppearFromTsDecl = never;".to_string());
    tool.input_schema = input_schema;
    tool.output_type.name = output_type.to_string();
    tool.output_type.ts_decl =
        Some("export type MustNotAppearFromOutputTsDecl = never;".to_string());
    tool.output_schema = json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"],
        "additionalProperties": false
    });
    tool.origin = ToolOrigin::Guest;
    tool.backend = backend;
    tool.baml_decl = Some("class MustNotAppearFromBamlDecl {}".to_string());
    tool.extra_ts_decls = vec!["export type MustNotAppearFromExtraTsDecl = never;".to_string()];
    tool
}

fn declaration_count(rendered: &str, symbol: &str) -> usize {
    let interface_prefix = format!("export interface {symbol} ");
    let type_prefix = format!("export type {symbol} =");
    rendered
        .lines()
        .filter(|line| line.starts_with(&interface_prefix) || line.starts_with(&type_prefix))
        .count()
}

fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn run_tsc(project: &Path) {
    let attempts: [(&str, &[&str]); 2] = [
        ("tsc", &[]),
        ("npx", &["--yes", "--package", "typescript@5", "tsc"]),
    ];
    let mut unavailable = Vec::new();
    for (program, base_args) in attempts {
        let output = Command::new(program)
            .args(base_args)
            .arg("--project")
            .arg(project.join("tsconfig.json"))
            .arg("--noEmit")
            .current_dir(project)
            .output();
        match output {
            Ok(output) if output.status.success() => return,
            Ok(output) => panic!(
                "tsc rejected the mixed-origin fixture:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => unavailable.push(format!("{program}: {error}")),
        }
    }
    panic!(
        "no TypeScript compiler available ({})",
        unavailable.join("; ")
    );
}
