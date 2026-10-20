// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Render a [`ToolSchemaGraph`] into TypeScript tool DTO declarations.
//!
//! This renderer consumes graph nodes only. It never reads raw JSON Schema, `ToolTypeSpec.ts_decl`,
//! or `extra_ts_decls`, so static, external, MCP, sandbox, and WASM tools with equivalent schemas
//! produce equivalent declarations.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    AdditionalProperties, OpenInputContract, OpenInputRequirement, SchemaDeclaration,
    SchemaDeclarationKind, SchemaEnum, SchemaLiteral, SchemaObject, SchemaTypeId, SchemaTypeRef,
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
    /// Graph-name to TypeScript-name mapping shared with BAML IR declaration collection.
    pub symbols: TsSymbolMap,
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
///
/// Graph symbols are shaped into valid TypeScript type names first. The graph permits BAML
/// identifiers such as `Foo-Bar`, but TypeScript does not; those symbols are mapped deterministically
/// and every `Named` reference uses the same mapped name.
pub fn render_tool_typescript_surface(graph: &ToolSchemaGraph) -> Result<ToolTypescriptSurface> {
    let symbols = TsSymbolMap::from_graph(graph);
    let renderer = TypescriptRenderer::new(symbols.clone());
    let mut ordered: Vec<&SchemaDeclaration> = graph.declarations.values().collect();
    ordered.sort_by(|left, right| {
        renderer
            .symbol(left.id)
            .cmp(renderer.symbol(right.id))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut declarations = Vec::with_capacity(ordered.len());
    for declaration in ordered {
        declarations.push(TsDeclaration {
            symbol: renderer.symbol(declaration.id).to_string(),
            body: renderer.render_declaration(declaration)?,
            dependencies: declaration
                .dependencies
                .iter()
                .map(|id| renderer.symbol(*id).to_string())
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
        .map(|(name, contract)| (name.clone(), renderer.render_tool_contract(contract)))
        .collect();

    Ok(ToolTypescriptSurface {
        declarations,
        tools,
        symbols,
    })
}

/// Render a normalized graph type expression in TypeScript syntax.
///
/// This convenience entry point derives the same TypeScript-safe symbol map as
/// [`render_tool_typescript_surface`], so references match emitted declarations.
pub fn render_schema_type_ref(graph: &ToolSchemaGraph, ty: &SchemaTypeRef) -> String {
    TypescriptRenderer::new(TsSymbolMap::from_graph(graph)).render_type(ty)
}

/// Graph-name to TypeScript-name mapping shared by graph declarations, BAML IR declarations, and
/// function type expressions.
#[derive(Debug, Clone, Default)]
pub struct TsSymbolMap {
    by_id: BTreeMap<SchemaTypeId, String>,
    by_name: BTreeMap<String, String>,
}

impl TsSymbolMap {
    fn from_graph(graph: &ToolSchemaGraph) -> Self {
        let mut ordered: Vec<&SchemaDeclaration> = graph.declarations.values().collect();
        ordered.sort_by(|left, right| {
            left.symbol
                .cmp(&right.symbol)
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut by_id = BTreeMap::new();
        let mut by_name = BTreeMap::new();
        let mut used = BTreeSet::new();

        // Preserve graph symbols that are already legal TypeScript type names.
        for declaration in &ordered {
            if is_typescript_identifier(&declaration.symbol)
                && !is_typescript_reserved_word(&declaration.symbol)
            {
                used.insert(declaration.symbol.clone());
                by_id.insert(declaration.id, declaration.symbol.clone());
                by_name.insert(declaration.symbol.clone(), declaration.symbol.clone());
            }
        }

        // Derive stable names for the rest without taking a preserved name.
        for declaration in &ordered {
            if by_id.contains_key(&declaration.id) {
                continue;
            }
            let mut base = sanitize_typescript_identifier(&declaration.symbol);
            if is_typescript_reserved_word(&base) {
                base.push_str("_Type");
            }
            let mut candidate = base.clone();
            let mut suffix = 2;
            while used.contains(&candidate) {
                candidate = format!("{base}_{suffix}");
                suffix += 1;
            }
            used.insert(candidate.clone());
            by_id.insert(declaration.id, candidate.clone());
            by_name.insert(declaration.symbol.clone(), candidate);
        }

        Self { by_id, by_name }
    }

    /// Emitted TypeScript name for a graph declaration id.
    pub(crate) fn for_id(&self, id: SchemaTypeId) -> &str {
        self.by_id
            .get(&id)
            .map(String::as_str)
            .unwrap_or("<unresolved>")
    }

    /// TypeScript name for a BAML or graph symbol. Unknown names pass through unchanged.
    pub fn map_name<'a>(&'a self, name: &'a str) -> &'a str {
        self.by_name.get(name).map(String::as_str).unwrap_or(name)
    }
}

struct TypescriptRenderer {
    symbols: TsSymbolMap,
}

impl TypescriptRenderer {
    fn new(symbols: TsSymbolMap) -> Self {
        Self { symbols }
    }

    fn symbol(&self, id: SchemaTypeId) -> &str {
        self.symbols.for_id(id)
    }

    fn render_declaration(&self, declaration: &SchemaDeclaration) -> Result<String> {
        match &declaration.kind {
            SchemaDeclarationKind::Object(object) => self.render_interface(declaration, object),
            SchemaDeclarationKind::Enum(enumeration) => {
                Ok(render_enum(self.symbol(declaration.id), enumeration))
            }
            SchemaDeclarationKind::Alias(ty) => Ok(format!(
                "export type {} = {};",
                self.symbol(declaration.id),
                self.render_type(ty)
            )),
        }
    }

    fn render_tool_contract(&self, contract: &ToolSchemaContract) -> ToolTypescriptContract {
        let (open_input, open_requirement) = match &contract.open_input {
            OpenInputContract::Absent => (None, None),
            OpenInputContract::Present { ty, requirement } => {
                (Some(self.render_type(ty)), Some(*requirement))
            }
        };
        ToolTypescriptContract {
            tool_name: contract.tool_name.clone(),
            open_input,
            open_requirement,
            input: self.render_type(&contract.input),
            output: self.render_type(&contract.output),
        }
    }

    fn render_interface(
        &self,
        declaration: &SchemaDeclaration,
        object: &SchemaObject,
    ) -> Result<String> {
        let symbol = self.symbol(declaration.id);
        let mut body = String::new();
        // Graph field order is the canonical wire projection order, so do not sort fields.
        for field in &object.fields {
            let optional = if field.required { "" } else { "?" };
            write_line(
                &mut body,
                &format!(
                    "  {}{optional}: {};",
                    render_property_key(&field.wire_name),
                    self.render_type(&field.ty)
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
                    "interface `{symbol}` at {} mixes fixed fields with typed additional properties, which generated TypeScript cannot represent",
                    declaration.source
                )));
            }
        }
        Ok(format!("export interface {symbol} {{\n{body}}}"))
    }

    fn render_type(&self, ty: &SchemaTypeRef) -> String {
        match ty {
            SchemaTypeRef::String => "string".to_string(),
            SchemaTypeRef::Integer | SchemaTypeRef::Number => "number".to_string(),
            SchemaTypeRef::Boolean => "boolean".to_string(),
            SchemaTypeRef::Null => "null".to_string(),
            SchemaTypeRef::JsonValue => "JsonValue".to_string(),
            SchemaTypeRef::Literal(literal) => render_literal(literal),
            SchemaTypeRef::Array(inner) => self.render_array_suffix(inner),
            SchemaTypeRef::Map(inner) => {
                format!("Record<string, {}>", self.render_type(inner))
            }
            SchemaTypeRef::Named(id) => self.symbol(*id).to_string(),
            SchemaTypeRef::Union(arms) => arms
                .iter()
                .map(|arm| self.render_type(arm))
                .collect::<Vec<_>>()
                .join(" | "),
            SchemaTypeRef::VecOrOne(inner) => {
                let scalar = self.render_type(inner);
                let list = self.render_array_suffix(inner);
                format!("{scalar} | {list}")
            }
        }
    }

    /// Render an array of `inner`, parenthesizing a union element so `[]` does not bind to only
    /// its last arm (`A | B[]` parses as `A | (B[])`). `VecOrOne` also renders as a union.
    fn render_array_suffix(&self, inner: &SchemaTypeRef) -> String {
        let rendered = self.render_type(inner);
        if is_union_like(inner) {
            format!("({rendered})[]")
        } else {
            format!("{rendered}[]")
        }
    }
}

fn sanitize_typescript_identifier(symbol: &str) -> String {
    let mut shaped: String = symbol
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if shaped.is_empty() {
        shaped.push_str("Type");
    }
    if !shaped
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '$')
    {
        shaped.insert(0, '_');
    }
    shaped
}

/// TypeScript keywords that cannot be used as a type alias or interface name.
fn is_typescript_reserved_word(name: &str) -> bool {
    matches!(
        name,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
            | "await"
    )
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
