// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Contract evidence for the generated tool catalog and imperative session API.
//!
//! One authoritative test covers the OpenInput matrix, typed external/MCP DTO retention, opaque
//! schema lowering, and the runtime-accurate session handle.

use std::collections::HashMap;

use baml_rt_builder::builder::{
    baml_signature_gen::extract_baml_signatures, tool_schema_graph::ToolSchemaGraph,
    ts_gen::render_ts_declarations,
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
fn tool_typescript_catalog_contract() {
    let tools = vec![
        tool(
            "matrix/absent",
            "MatrixAbsent",
            "MatrixAbsentInput",
            "MatrixAbsentOutput",
            "()",
            json!({}),
            object_schema(),
            object_schema(),
            ToolOrigin::Host,
            ToolBackend::Static,
        ),
        tool(
            "matrix/optional",
            "MatrixOptional",
            "MatrixOptionalInput",
            "MatrixOptionalOutput",
            "MatrixOptionalOpenInput",
            json!({
                "type": "object",
                "properties": { "workspace": { "type": "string" } }
            }),
            object_schema(),
            object_schema(),
            ToolOrigin::Host,
            ToolBackend::Static,
        ),
        tool(
            "matrix/required",
            "MatrixRequired",
            "MatrixRequiredInput",
            "MatrixRequiredOutput",
            "MatrixRequiredOpenInput",
            json!({
                "type": "object",
                "properties": { "workspace": { "type": "string" } },
                "required": ["workspace"]
            }),
            object_schema(),
            object_schema(),
            ToolOrigin::Host,
            ToolBackend::Static,
        ),
        // Typed imported tools keep graph DTOs instead of degrading to `unknown`.
        tool(
            "external/typed",
            "ExternalTyped",
            "ExternalTypedInput",
            "ExternalTypedOutput",
            "()",
            json!({}),
            object_schema(),
            object_schema(),
            ToolOrigin::Guest,
            ToolBackend::External,
        ),
        tool(
            "mcp/typed",
            "McpTyped",
            "McpTypedInput",
            "McpTypedOutput",
            "()",
            json!({}),
            object_schema(),
            object_schema(),
            ToolOrigin::Guest,
            ToolBackend::Mcp,
        ),
        // An explicitly opaque MCP schema lowers to JsonValue, not an untyped DTO.
        tool(
            "mcp/opaque",
            "McpOpaque",
            "McpOpaqueInput",
            "McpOpaqueOutput",
            "()",
            json!({}),
            json_schema_value::<OpaqueJson>(),
            object_schema(),
            ToolOrigin::Guest,
            ToolBackend::Mcp,
        ),
    ];

    let graph = ToolSchemaGraph::from_tools(&tools).expect("lower synthetic tool schemas");
    let ir = compile_ir(
        r#"
class UnusedContractProbe {
  id string
}
"#,
    );
    let rendered = render_ts_declarations(
        &ir,
        &tools,
        &graph,
        &Default::default(),
        &Default::default(),
    )
    .expect("render TypeScript declarations");

    // Reviewed contract snapshot: catalog maps, OpenInput requiredness, and session handle.
    let catalog = rendered
        .split("/** Tool catalog and imperative tool-session API (schema-graph derived). */")
        .nth(1)
        .expect("tool catalog section");
    insta::assert_snapshot!("tool_typescript_catalog_contract", catalog.trim());

    // Manifest allowlist is complete and sorted.
    let tool_name_section = rendered
        .split("export type ToolName =")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("ToolName union");
    let mut expected_names: Vec<String> = tools.iter().map(|tool| tool.name.to_string()).collect();
    expected_names.sort_unstable();
    let mut actual_names: Vec<String> = Vec::new();
    for line in tool_name_section.lines() {
        if let Some(name) = line.trim().strip_prefix("| ") {
            actual_names.push(name.trim_end_matches(';').trim_matches('"').to_string());
        }
    }
    assert_eq!(actual_names, expected_names, "ToolName union");

    // Absent, optional, and required OpenInput remain distinct contracts.
    for fragment in [
        "\"matrix/absent\": Record<string, never>;",
        "\"matrix/optional\": MatrixOptionalOpenInput;",
        "\"matrix/required\": MatrixRequiredOpenInput;",
    ] {
        assert!(
            rendered.contains(fragment),
            "missing OpenInput map row `{fragment}`"
        );
    }
    let required_section = rendered
        .split("export type ToolWithRequiredOpenInput =")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("required OpenInput union");
    assert!(required_section.contains("\"matrix/required\""));
    assert!(!required_section.contains("\"matrix/optional\""));
    assert!(!required_section.contains("\"matrix/absent\""));
    assert!(
        rendered.contains("? [openInput: ToolOpenInput<T>]")
            && rendered.contains(": [openInput?: ToolOpenInput<T>];"),
        "conditional OpenInput args"
    );

    // Typed imported tools keep concrete DTOs; opaque schemas use JsonValue.
    for symbol in [
        "ExternalTypedInput",
        "McpTypedInput",
        "McpTypedOutput",
        "MatrixRequiredInput",
    ] {
        assert!(
            rendered.contains(&format!("export interface {symbol} {{")),
            "missing graph DTO `{symbol}`"
        );
    }
    assert!(
        rendered.contains("export type McpOpaqueInput = JsonValue;"),
        "opaque MCP input must lower to JsonValue"
    );

    // Session contract matches `register_tool_session_wrapper` and `tool_step_to_value`.
    for phase in [
        "\"Open\"",
        "\"Send\"",
        "\"Continue\"",
        "\"Finish\"",
        "\"Abort\"",
    ] {
        assert!(rendered.contains(phase), "missing session phase {phase}");
    }
    for fragment in [
        "sessionId: string;",
        "phase(): ToolSessionPhase;",
        "send(input: I): Promise<void>;",
        "continue(readInput?: ToolReadInput<I>): Promise<ToolStep<O>>;",
        "finish(): Promise<void>;",
        "abort(reason?: string): Promise<void>;",
        "{ status: \"streaming\"; output: O }",
        "{ status: \"suspended\"; output: O }",
        "{ status: \"done\"; output: O | null }",
        "{ status: \"error\"; error: ToolFailure }",
    ] {
        assert!(
            rendered.contains(fragment),
            "missing session member `{fragment}`"
        );
    }
    assert!(
        rendered.contains("function openToolSession<T extends ToolName>("),
        "generic global openToolSession"
    );
    assert_eq!(
        rendered.matches("function openToolSession").count(),
        1,
        "one global opener"
    );

    // One shared failure envelope with the seven runtime fields.
    assert_eq!(
        rendered.matches("export type ErrorDisposition =").count(),
        1,
        "ErrorDisposition is owned by the runtime block only"
    );
    for fragment in [
        "disposition: ErrorDisposition;",
        "code: string;",
        "hint: string | null;",
        "retry_after_ms: number | null;",
    ] {
        assert!(
            rendered.contains(fragment),
            "missing ToolFailure field `{fragment}`"
        );
    }

    // No phantom per-tool opener declarations.
    for prefix in ["openMatrix", "openExternal", "openMcp"] {
        assert!(
            !rendered.contains(prefix),
            "generated a phantom opener `{prefix}`"
        );
    }
}

fn object_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"],
        "additionalProperties": false
    })
}

#[allow(clippy::too_many_arguments)]
fn tool(
    name: &str,
    class_name: &str,
    input_type: &str,
    output_type: &str,
    open_input_type: &str,
    open_input_schema: Value,
    input_schema: Value,
    output_schema: Value,
    origin: ToolOrigin,
    backend: ToolBackend,
) -> ToolFunctionMetadata {
    let mut tool = system_callback_metadata();
    tool.name = ToolName::parse(name).expect("valid tool name");
    tool.class_name = class_name.to_string();
    tool.open_input_type.name = open_input_type.to_string();
    tool.open_input_schema = open_input_schema;
    tool.input_type.name = input_type.to_string();
    tool.input_schema = input_schema;
    tool.output_type.name = output_type.to_string();
    tool.output_schema = output_schema;
    tool.origin = origin;
    tool.backend = backend;
    tool.baml_decl = None;
    tool.extra_ts_decls.clear();
    tool
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
