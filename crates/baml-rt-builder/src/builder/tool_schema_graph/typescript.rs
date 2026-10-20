// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Render a [`ToolSchemaGraph`] into TypeScript tool DTO declarations.
//!
//! This renderer consumes graph nodes only. It never reads raw JSON Schema, `ToolTypeSpec.ts_decl`,
//! or `extra_ts_decls`, so static, external, MCP, sandbox, and WASM tools with equivalent schemas
//! produce equivalent declarations.

use std::collections::BTreeMap;

use super::{
    AdditionalProperties, OpenInputContract, OpenInputRequirement, SchemaDeclaration,
    SchemaDeclarationKind, SchemaEnum, SchemaLiteral, SchemaObject, SchemaTypeRef,
    ToolSchemaContract, ToolSchemaGraph,
};
use crate::builder::{
    error::{BamlBuilderError, Result, write_line},
    ts_declarations::{TsDeclaration, TsDeclarationOrigin},
};

/// Every TypeScript declaration and per-tool contract rendered from one graph.
#[derive(Debug, Clone)]
pub struct ToolTypescriptSurface {
    /// One declaration per graph symbol, sorted by emitted symbol.
    pub declarations: Vec<TsDeclaration>,
    /// Per-tool root expressions, keyed by qualified tool name in lexical order.
    pub tools: BTreeMap<String, ToolTypescriptContract>,
}

/// One tool's root TypeScript expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTypescriptContract {
    pub tool_name: String,
    /// `None` means the tool has no Open input (unit), not that the schema is opaque.
    pub open_input: Option<String>,
    /// Requiredness for a present Open input. `None` when [`Self::open_input`] is `None`.
    pub open_requirement: Option<OpenInputRequirement>,
    pub input: String,
    pub output: String,
}

/// Render every declaration and tool contract in `graph`.
///
/// Declarations are sorted by emitted symbol, which is sufficient because TypeScript does not
/// require dependency order. The output is therefore byte-stable regardless of graph ids or tool
/// origin.
pub fn render_tool_typescript_surface(graph: &ToolSchemaGraph) -> Result<ToolTypescriptSurface> {
    let mut ordered: Vec<&SchemaDeclaration> = graph.declarations.values().collect();
    ordered.sort_by(|left, right| left.symbol.cmp(&right.symbol));

    let mut declarations = Vec::with_capacity(ordered.len());
    for declaration in ordered {
        declarations.push(TsDeclaration {
            symbol: declaration.symbol.clone(),
            body: render_declaration(graph, declaration)?,
            dependencies: declaration
                .dependencies
                .iter()
                .map(|id| {
                    graph
                        .declarations
                        .get(id)
                        .map(|dependency| dependency.symbol.clone())
                        .unwrap_or_else(|| format!("<unresolved:{}>", id.as_u32()))
                })
                .collect(),
            origin: TsDeclarationOrigin::ToolSchema {
                owners: declaration.owners.clone(),
                source: declaration.source.clone(),
            },
        });
    }

    let tools = graph
        .tools
        .iter()
        .map(|(name, contract)| (name.clone(), render_tool_contract(graph, contract)))
        .collect();

    Ok(ToolTypescriptSurface {
        declarations,
        tools,
    })
}

fn render_declaration(graph: &ToolSchemaGraph, declaration: &SchemaDeclaration) -> Result<String> {
    match &declaration.kind {
        SchemaDeclarationKind::Object(object) => render_interface(graph, declaration, object),
        SchemaDeclarationKind::Enum(enumeration) => {
            Ok(render_enum(&declaration.symbol, enumeration))
        }
        SchemaDeclarationKind::Alias(ty) => Ok(format!(
            "export type {} = {};",
            declaration.symbol,
            render_schema_type_ref(graph, ty)
        )),
    }
}

fn render_tool_contract(
    graph: &ToolSchemaGraph,
    contract: &ToolSchemaContract,
) -> ToolTypescriptContract {
    let (open_input, open_requirement) = match &contract.open_input {
        OpenInputContract::Absent => (None, None),
        OpenInputContract::Present { ty, requirement } => {
            (Some(render_schema_type_ref(graph, ty)), Some(*requirement))
        }
    };
    ToolTypescriptContract {
        tool_name: contract.tool_name.clone(),
        open_input,
        open_requirement,
        input: render_schema_type_ref(graph, &contract.input),
        output: render_schema_type_ref(graph, &contract.output),
    }
}

fn render_interface(
    graph: &ToolSchemaGraph,
    declaration: &SchemaDeclaration,
    object: &SchemaObject,
) -> Result<String> {
    let mut body = String::new();
    // Graph field order is the canonical wire projection order, so do not sort fields.
    for field in &object.fields {
        let optional = if field.required { "" } else { "?" };
        write_line(
            &mut body,
            &format!(
                "  {}{optional}: {};",
                render_property_key(&field.wire_name),
                render_schema_type_ref(graph, &field.ty)
            ),
        )?;
    }
    match &object.additional_properties {
        AdditionalProperties::AllowedAny => {
            write_line(&mut body, "  [key: string]: JsonValue | undefined;")?;
        }
        AdditionalProperties::Forbidden => {}
        AdditionalProperties::Typed(_) => {
            return Err(BamlBuilderError::InvalidArgument(format!(
                "interface `{}` at {} mixes fixed fields with typed additional properties, which generated TypeScript cannot represent",
                declaration.symbol, declaration.source
            )));
        }
    }
    Ok(format!(
        "export interface {} {{\n{body}}}",
        declaration.symbol
    ))
}

fn render_enum(symbol: &str, enumeration: &SchemaEnum) -> String {
    let variants: Vec<String> = enumeration
        .variants
        .iter()
        .map(|variant| render_literal(&variant.wire_value))
        .collect();
    let union = if variants.is_empty() {
        "never".to_string()
    } else {
        variants.join(" | ")
    };
    format!("export type {symbol} = {union};")
}

/// Render a normalized graph type expression in TypeScript syntax.
pub fn render_schema_type_ref(graph: &ToolSchemaGraph, ty: &SchemaTypeRef) -> String {
    match ty {
        SchemaTypeRef::String => "string".to_string(),
        SchemaTypeRef::Integer | SchemaTypeRef::Number => "number".to_string(),
        SchemaTypeRef::Boolean => "boolean".to_string(),
        SchemaTypeRef::Null => "null".to_string(),
        SchemaTypeRef::JsonValue => "JsonValue".to_string(),
        SchemaTypeRef::Literal(literal) => render_literal(literal),
        SchemaTypeRef::Array(inner) => render_array_suffix(graph, inner),
        SchemaTypeRef::Map(inner) => {
            format!("Record<string, {}>", render_schema_type_ref(graph, inner))
        }
        SchemaTypeRef::Named(id) => graph
            .declarations
            .get(id)
            .map(|declaration| declaration.symbol.clone())
            .unwrap_or_else(|| format!("<unresolved:{}>", id.as_u32())),
        SchemaTypeRef::Union(arms) => arms
            .iter()
            .map(|arm| render_schema_type_ref(graph, arm))
            .collect::<Vec<_>>()
            .join(" | "),
        SchemaTypeRef::VecOrOne(inner) => {
            let scalar = render_schema_type_ref(graph, inner);
            let list = render_array_suffix(graph, inner);
            format!("{scalar} | {list}")
        }
    }
}

/// Render an array of `inner`, parenthesizing a union element so `[]` does not bind to only its
/// last arm (`A | B[]` parses as `A | (B[])`). `VecOrOne` also renders as a union.
fn render_array_suffix(graph: &ToolSchemaGraph, inner: &SchemaTypeRef) -> String {
    let rendered = render_schema_type_ref(graph, inner);
    if is_union_like(inner) {
        format!("({rendered})[]")
    } else {
        format!("{rendered}[]")
    }
}

fn is_union_like(ty: &SchemaTypeRef) -> bool {
    matches!(ty, SchemaTypeRef::Union(_) | SchemaTypeRef::VecOrOne(_))
}

fn render_literal(literal: &SchemaLiteral) -> String {
    match literal {
        SchemaLiteral::String(value) => escape_json_string(value),
        SchemaLiteral::Integer(value) => value.to_string(),
        SchemaLiteral::Number(value) => value.clone(),
        SchemaLiteral::Boolean(value) => value.to_string(),
        SchemaLiteral::Null => "null".to_string(),
    }
}

/// Quote a wire property name only when it is not a legal TypeScript identifier.
fn render_property_key(wire_name: &str) -> String {
    if is_typescript_identifier(wire_name) {
        wire_name.to_string()
    } else {
        escape_json_string(wire_name)
    }
}

fn is_typescript_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

fn escape_json_string(value: &str) -> String {
    serde_json::to_string(value).expect("JSON string serialization cannot fail")
}
