// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeSet, HashSet},
    fmt, fs,
    path::Path,
};

use baml_rt_tools::{
    SessionPlanFunctionsMap, ToolCapability, UnifiedStepExecutorFunctionsMap,
    tools::ToolFunctionMetadata,
    ts_gen::{
        generated_prelude_typescript_symbols, render_runtime_typescript, runtime_typescript_symbols,
    },
};
use genco::{fmt::Error as GencoFmtError, lang::js, prelude::*};
use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::{
    error::{BamlBuilderError, Result, write_line},
    ir_to_ts::{
        collect_ir_type_declarations_with_symbols, collect_type_decl_deps,
        type_to_ts_expr_with_symbols,
    },
    tool_schema_graph::{
        OpenInputRequirement, ToolSchemaGraph,
        typescript::{ToolTypescriptSurface, TsSymbolMap, render_tool_typescript_surface},
    },
    ts_declarations::TsDeclarationRegistry,
};

/// Wrapper so genco fmt errors can be used as [`std::error::Error`] source.
/// genco's `fmt::Error` does not implement `Error`; this preserves the chain.
#[derive(Debug)]
struct GencoRenderError(GencoFmtError);

impl fmt::Display for GencoRenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for GencoRenderError {}

pub fn load_manifest_tools(baml_src: &Path) -> Result<Vec<String>> {
    let agent_dir = baml_src.parent().ok_or_else(|| {
        BamlBuilderError::InvalidArgument("baml_src has no parent directory".to_string())
    })?;
    let manifest_path = agent_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&manifest_path)?;
    let manifest_json: serde_json::Value =
        serde_json::from_str(&content).map_err(BamlBuilderError::Json)?;
    let tools = manifest_json
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Ok(tools)
}

/// Generate TypeScript declarations for BAML runtime: typed function signatures, graph tool DTOs,
/// the tool catalog, and the imperative session API.
pub fn render_ts_declarations(
    ir_signature: &IRSignature,
    tool_metadata: &[ToolFunctionMetadata],
    tool_schema_graph: &ToolSchemaGraph,
    session_plan_functions: &SessionPlanFunctionsMap,
    unified_step_executors: &UnifiedStepExecutorFunctionsMap,
) -> Result<String> {
    let header = "/**
 * BAML runtime TypeScript declarations.
 * Auto-generated from BAML runtime IR — do not edit manually.
 * Use these types and function declarations in your agent code (e.g. index.ts).
 */";
    let mut tokens: js::Tokens = quote!($(header));
    tokens.line();

    validate_tool_metadata(tool_metadata, tool_schema_graph)?;
    let surface = render_tool_typescript_surface(tool_schema_graph)?;

    let mut all_type_deps: HashSet<String> = HashSet::new();
    let mut func_decls: Vec<(String, String, String)> = Vec::new();
    for (name, func_sig) in &ir_signature.functions {
        let args_frag = build_args_frag(func_sig, &surface.symbols)?;
        let return_frag = type_to_ts_expr_with_symbols(func_sig.output.as_ref(), &surface.symbols)?;
        all_type_deps.extend(collect_type_decl_deps(&args_frag));
        all_type_deps.extend(collect_type_decl_deps(&return_frag));
        func_decls.push((name.clone(), args_frag.expr, return_frag.expr));
    }
    func_decls.sort_by(|a, b| a.0.cmp(&b.0));

    let mut registry = TsDeclarationRegistry::new();
    registry.reserve_runtime_symbols(
        runtime_typescript_symbols(),
        generated_prelude_typescript_symbols(),
    )?;
    for declaration in surface.declarations.iter().cloned() {
        registry.insert(declaration)?;
    }
    for declaration in
        collect_ir_type_declarations_with_symbols(ir_signature, &all_type_deps, &surface.symbols)?
    {
        registry.insert(declaration)?;
    }

    let type_decls_comment =
        "/** Types for BAML function arguments and return values (classes, enums, aliases). */";
    quote_in!(tokens => $(type_decls_comment));
    tokens.line();
    let type_decls = registry.render()?;
    quote_in!(tokens => $(type_decls));
    tokens.line();

    let global_comment = "/** BAML functions: call these from your agent (e.g. await MyFunction(args)). Declared in global scope so they are visible when this file is used as a module. */";
    quote_in!(tokens => $(global_comment));
    tokens.line();
    let global_open = "declare global {";
    let global_close = "}";
    quote_in!(tokens => $(global_open));
    tokens.line();
    for (name, args_type, return_type) in func_decls {
        quote_in!(tokens =>   declare function $(name)(args: $(args_type) & { __baml_invocation_token?: string }): Promise<$(return_type)>;);
        tokens.line();
    }
    quote_in!(tokens => $(global_close));
    tokens.line();

    let tool_comment =
        "/** Runtime interaction API: A2A task FSM (message-first, typestate rails). */";
    quote_in!(tokens => $(tool_comment));
    tokens.line();
    let tool_ts = render_runtime_typescript()?;
    for line in tool_ts.lines() {
        quote_in!(tokens => $(line));
        tokens.push();
    }
    tokens.line();

    if !surface.tools.is_empty() {
        let catalog_comment =
            "/** Tool catalog and imperative tool-session API (schema-graph derived). */";
        quote_in!(tokens => $(catalog_comment));
        tokens.line();
        let catalog_ts = render_tool_catalog(&surface)?;
        quote_in!(tokens => $(catalog_ts.trim_end()));
        tokens.line();
    }

    if !session_plan_functions.is_empty() {
        let session_runner_comment = "/** Generated Step Executor bindings (function -> typed step-executor args/result). */";
        quote_in!(tokens => $(session_runner_comment));
        tokens.line();

        // Stable output for snapshots.
        let mut step_executor_names: Vec<String> = session_plan_functions.keys().cloned().collect();
        for k in unified_step_executors.keys() {
            if !step_executor_names.iter().any(|e| e == k) {
                step_executor_names.push(k.clone());
            }
        }
        step_executor_names.sort();
        let step_executor_union = step_executor_names
            .iter()
            .map(|name| format!("\"{}\"", name))
            .collect::<Vec<_>>()
            .join(" | ");
        quote_in!(tokens => export type StepExecutorFunctionName = $(step_executor_union););
        tokens.line();
        quote_in!(
            tokens =>
            export interface SessionContext {
                contract_version: "session_context_v5";
                session_open: boolean;
                status: "awaiting_open" | "just_opened" | "result_ready" | "done";
                last_step_op?: "open" | "send" | "duplicate_send" | "read" | "finish" | "abort";
                last_step_status?: "open" | "done" | "duplicate" | "finished" | "aborted";
                last_archive_ref?: string;
                last_output_header?: string;
                last_completion?: string;
                last_receipt?: { archive_ref: string; tool_name: string; action_identity: string; line_count: number; byte_count: number; size_display: string; completion: string; search_hints?: string[] };
                archives?: Array<{ archive_ref: string; header: string }>;
            }
        );
        tokens.line();
        let history_ctx_op_comment = "/** Last archive read op for this hop (`StepExecutorStateInput.history_context`); distinct from tool-session `SessionStepOp` in Rust provenance. */";
        quote_in!(tokens => $(history_ctx_op_comment));
        tokens.line();
        quote_in!(
            tokens =>
            export type HistoryContextSessionOp = "SearchRead" | "PageRead";
        );
        tokens.line();
        quote_in!(
            tokens =>
            export type HistoryContextStatus = "done" | "streaming" | "suspended" | "error";
        );
        tokens.line();
        quote_in!(
            tokens =>
            export interface HistoryContext {
                hop: number;
                op: HistoryContextSessionOp;
                status: HistoryContextStatus;
                truncated: boolean;
                cursor: string | null;
                payload: Record<string, unknown> | null;
            }
        );
        tokens.line();
        quote_in!(
            tokens =>
            export interface StepExecutorStateInput {
                session_context?: SessionContext | null;
                history_context?: HistoryContext | null;
            }
        );
        tokens.line();
        quote_in!(
            tokens =>
            export interface StepExecutorRunOptions {
                max_steps?: number;
            }
        );
        tokens.line();
        quote_in!(
            tokens =>
            export interface StepPlanRecovery {
                code: string;
                disposition: ErrorDisposition;
                mistake: string;
                invariant: string;
                fix_steps?: string[];
            }
        );
        tokens.line();
        quote_in!(
            tokens =>
            export type StepExecutorRunEnvelope<R = unknown> = { outcome: "completed", last: R, steps: R[], session_context: SessionContext, selected_tool: string | null } | { outcome: "agent_correctable", recovery: StepPlanRecovery } | { outcome: "fatal", message: string, code?: string | null };
        );
        tokens.line();
        let step_run_result_comment = "/**\n * Result of runGeneratedStepExecutor: discriminated envelope (`outcome`).\n * On `completed`, fields match the former flat telemetry shape.\n * `agent_correctable` carries structured recovery — not a thrown JS error.\n * User-facing replies are still SessionResult.message from the chat handler.\n */";
        quote_in!(tokens => $(step_run_result_comment));
        tokens.line();
        quote_in!(
            tokens =>
            export type StepExecutorRunResult<R = unknown> = StepExecutorRunEnvelope<R>;
        );
        tokens.line();
        let map_open = "export interface StepExecutorFunctionMap {";
        quote_in!(tokens => $(map_open));
        tokens.push();
        for name in &step_executor_names {
            let line = format!(
                "  {}: {{ args: Parameters<typeof {}>[0] & StepExecutorStateInput; result: Awaited<ReturnType<typeof {}>>; }};",
                name, name, name
            );
            quote_in!(tokens => $(line));
            tokens.push();
        }
        let map_close = "}";
        quote_in!(tokens => $(map_close));
        tokens.push();
        tokens.line();
        let global_open = "declare global {";
        quote_in!(tokens => $(global_open));
        tokens.push();
        let step_fn_l1 = "  function runGeneratedStepExecutor<F extends StepExecutorFunctionName>(";
        quote_in!(tokens => $(step_fn_l1));
        tokens.push();
        let step_fn_l2 = "    stepExecutor: F,";
        quote_in!(tokens => $(step_fn_l2));
        tokens.push();
        let step_fn_l3 =
            "    args: Omit<StepExecutorFunctionMap[F][\"args\"], keyof StepExecutorStateInput>,";
        quote_in!(tokens => $(step_fn_l3));
        tokens.push();
        let step_fn_l4 = "    options?: StepExecutorRunOptions";
        quote_in!(tokens => $(step_fn_l4));
        tokens.push();
        let step_fn_l5 =
            "  ): Promise<StepExecutorRunEnvelope<StepExecutorFunctionMap[F][\"result\"]>>;";
        quote_in!(tokens => $(step_fn_l5));
        tokens.push();
        let global_close = "}";
        quote_in!(tokens => $(global_close));
        tokens.push();
        tokens.line();
    }

    tokens
        .to_file_string()
        .map_err(|e| BamlBuilderError::InvalidArgumentWithSource {
            message: "TypeScript render error".into(),
            source: Box::new(GencoRenderError(e)),
        })
}

/// Ensure the resolved metadata and the retained graph describe the same tool allowlist.
fn validate_tool_metadata(
    tool_metadata: &[ToolFunctionMetadata],
    graph: &ToolSchemaGraph,
) -> Result<()> {
    let metadata_names: BTreeSet<String> = tool_metadata
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let graph_names: BTreeSet<&str> = graph.tools.keys().map(String::as_str).collect();
    if metadata_names.len() != graph_names.len()
        || metadata_names
            .iter()
            .any(|name| !graph_names.contains(name.as_str()))
    {
        let metadata_display = metadata_names.into_iter().collect::<Vec<_>>().join(", ");
        let graph_display = graph_names.into_iter().collect::<Vec<_>>().join(", ");
        return Err(BamlBuilderError::InvalidArgument(format!(
            "tool metadata and tool schema graph disagree: metadata [{metadata_display}], graph [{graph_display}]"
        )));
    }
    Ok(())
}

/// Render the typed tool catalog and the imperative session API.
fn render_tool_catalog(surface: &ToolTypescriptSurface) -> Result<String> {
    let mut out = String::new();
    if surface.tools.is_empty() {
        return Ok(out);
    }

    let names: Vec<String> = surface.tools.keys().map(|name| quote_ts(name)).collect();
    write_line(
        &mut out,
        &format!("export type ToolName =\n  | {};", names.join("\n  | ")),
    )?;
    write_line(&mut out, "")?;

    render_tool_map(
        &mut out,
        "ToolInputMap",
        surface
            .tools
            .values()
            .map(|contract| (contract.tool_name.as_str(), contract.input.as_str())),
    )?;
    render_tool_map(
        &mut out,
        "ToolOutputMap",
        surface
            .tools
            .values()
            .map(|contract| (contract.tool_name.as_str(), contract.output.as_str())),
    )?;
    render_tool_map(
        &mut out,
        "ToolOpenInputMap",
        surface.tools.values().map(|contract| {
            (
                contract.tool_name.as_str(),
                contract
                    .open_input
                    .as_deref()
                    .unwrap_or("Record<string, never>"),
            )
        }),
    )?;

    write_line(
        &mut out,
        "export type ToolInput<T extends ToolName> = ToolInputMap[T];",
    )?;
    write_line(
        &mut out,
        "export type ToolOutput<T extends ToolName> = ToolOutputMap[T];",
    )?;
    write_line(
        &mut out,
        "export type ToolOpenInput<T extends ToolName> = ToolOpenInputMap[T];",
    )?;
    write_line(&mut out, "")?;

    let required: Vec<String> = surface
        .tools
        .values()
        .filter(|contract| {
            matches!(
                contract.open_requirement,
                Some(OpenInputRequirement::Required)
            )
        })
        .map(|contract| quote_ts(&contract.tool_name))
        .collect();
    if required.is_empty() {
        write_line(&mut out, "export type ToolWithRequiredOpenInput = never;")?;
    } else {
        write_line(
            &mut out,
            &format!(
                "export type ToolWithRequiredOpenInput =\n  | {};",
                required.join("\n  | ")
            ),
        )?;
    }
    write_line(&mut out, "")?;

    for line in [
        "export type ToolOpenArgs<T extends ToolName> =",
        "  T extends ToolWithRequiredOpenInput",
        "    ? [openInput: ToolOpenInput<T>]",
        "    : [openInput?: ToolOpenInput<T>];",
        "",
    ] {
        write_line(&mut out, line)?;
    }

    // One-shot author surface: only manifest-allowed `ToolCapability::OneShot`
    // tools are reachable through `invokeTool`.
    let one_shot_names: Vec<String> = surface
        .tools
        .values()
        .filter(|contract| contract.capability == ToolCapability::OneShot)
        .map(|contract| quote_ts(&contract.tool_name))
        .collect();
    if one_shot_names.is_empty() {
        write_line(&mut out, "export type OneShotToolName = never;")?;
    } else {
        write_line(
            &mut out,
            &format!(
                "export type OneShotToolName =\n  | {};",
                one_shot_names.join("\n  | ")
            ),
        )?;
    }
    write_line(&mut out, "")?;
    for line in [
        "export type ToolOutcome<O> =",
        "  | { status: \"done\"; result: O }",
        "  | { status: \"error\"; error: ToolFailure };",
        "",
        "declare global {",
        "  function invokeTool<T extends OneShotToolName>(",
        "    toolName: T,",
        "    input: ToolInput<T>",
        "  ): Promise<ToolOutcome<ToolOutput<T>>>;",
        "}",
        "",
    ] {
        write_line(&mut out, line)?;
    }

    out.push_str(TOOL_SESSION_TYPES);
    out.push('\n');
    Ok(out)
}

fn render_tool_map<'a>(
    out: &mut String,
    name: &str,
    entries: impl Iterator<Item = (&'a str, &'a str)>,
) -> Result<()> {
    write_line(out, &format!("export interface {name} {{"))?;
    for (tool_name, ty) in entries {
        write_line(out, &format!("  {}: {ty};", quote_ts(tool_name)))?;
    }
    write_line(out, "}")?;
    write_line(out, "")?;
    Ok(())
}

fn quote_ts(value: &str) -> String {
    serde_json::to_string(value).expect("JSON string serialization cannot fail")
}

const TOOL_SESSION_TYPES: &str = r#"export type ToolSessionPhase =
  | "Open"
  | "Send"
  | "Continue"
  | "Finish"
  | "Abort";

export type ToolReadInput<I> =
  I extends readonly unknown[]
    ? Record<string, never>
    : I extends object
      ? Partial<I>
      : Record<string, never>;

export type ToolStep<O> =
  | { status: "streaming"; output: O }
  | { status: "suspended"; output: O }
  | { status: "done"; output: O | null }
  | { status: "error"; error: ToolFailure };

export interface ToolSession<I, O> {
  sessionId: string;
  phase(): ToolSessionPhase;
  send(input: I): Promise<void>;
  continue(readInput?: ToolReadInput<I>): Promise<ToolStep<O>>;
  finish(): Promise<void>;
  abort(reason?: string): Promise<void>;
}

declare global {
  function openToolSession<T extends ToolName>(
    toolName: T,
    ...args: ToolOpenArgs<T>
  ): Promise<ToolSession<ToolInput<T>, ToolOutput<T>>>;
}"#;

/// Build an object type for function args: { name1: Type1; name2: Type2; ... }.
fn build_args_frag(
    func_sig: &internal_baml_core::ir::ir_hasher::FunctionSignature,
    symbols: &TsSymbolMap,
) -> Result<crate::builder::ir_to_ts::TsTypeFrag> {
    use crate::builder::ir_to_ts::TsTypeFrag;
    if func_sig.inputs.is_empty() {
        return Ok(TsTypeFrag {
            expr: "Record<string, never>".to_string(),
            deps: vec![],
        });
    }
    let mut parts = Vec::with_capacity(func_sig.inputs.len());
    let mut deps = Vec::new();
    for (arg_name, arg_ty) in func_sig.inputs.iter() {
        let frag = type_to_ts_expr_with_symbols(arg_ty, symbols)?;
        deps.extend(frag.deps);
        parts.push(format!("{}: {}", arg_name, frag.expr));
    }
    let expr = format!("{{ {} }}", parts.join("; "));
    Ok(TsTypeFrag { expr, deps })
}
