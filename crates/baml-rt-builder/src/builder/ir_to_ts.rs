// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Map BAML IR types (TypeNonStreaming) to TypeScript type expressions.
//!
//! Semantics aligned with upstream BoundaryML TS generator: primitives, optional
//! as T | null, unions, classes, enums, lists, maps. No BAML source parsing.

use std::collections::{BTreeSet, HashSet};

use baml_types::ir_type::{LiteralValue, TypeNonStreaming, TypeValue, UnionTypeViewGeneric};
use genco::{lang::js, prelude::*};
use internal_baml_core::ir::ir_hasher::IRSignature;

use crate::builder::{
    error::{BamlBuilderError, Result},
    tool_schema_graph::typescript::TsSymbolMap,
    ts_declarations::{TsDeclaration, TsDeclarationOrigin},
};

/// Result of mapping one IR type: TS type expression and any type names that must be declared.
#[derive(Debug, Clone)]
pub struct TsTypeFrag {
    pub expr: String,
    pub deps: Vec<String>,
}

/// Map a non-streaming BAML type to a TypeScript type expression and collect referenced type names.
pub fn type_to_ts_expr(ty: &TypeNonStreaming, _ir: &IRSignature) -> Result<TsTypeFrag> {
    type_to_ts_expr_with_symbols(ty, &TsSymbolMap::default())
}

/// Map a BAML type using the graph's TypeScript symbol mapping.
///
/// Dependency names stay in their original BAML spelling so IR lookup still works; only the emitted
/// expression uses the mapped TypeScript name.
pub fn type_to_ts_expr_with_symbols(
    ty: &TypeNonStreaming,
    symbols: &TsSymbolMap,
) -> Result<TsTypeFrag> {
    let mut deps = Vec::new();
    let expr = type_to_ts_inner(ty, &mut deps, symbols)?;
    Ok(TsTypeFrag { expr, deps })
}

fn type_to_ts_inner(
    ty: &TypeNonStreaming,
    deps: &mut Vec<String>,
    symbols: &TsSymbolMap,
) -> Result<String> {
    use TypeNonStreaming as T;

    let mut recursive =
        |t: &TypeNonStreaming| -> Result<String> { type_to_ts_inner(t, deps, symbols) };

    Ok(match ty {
        T::Primitive(tv, _) => match tv {
            TypeValue::String => "string".to_string(),
            TypeValue::Int | TypeValue::Float => "number".to_string(),
            TypeValue::Bool => "boolean".to_string(),
            TypeValue::Null => "null".to_string(),
            TypeValue::Media(_) => "string".to_string(), // media as string in TS surface
        },
        T::Enum { name, .. } => {
            deps.push(name.clone());
            symbols.map_name(name).to_string()
        }
        T::Literal(lit, _) => match lit {
            LiteralValue::String(s) => format!("{:?}", s),
            LiteralValue::Int(n) => n.to_string(),
            LiteralValue::Bool(b) => b.to_string(),
        },
        T::Class { name, .. } => {
            deps.push(name.clone());
            symbols.map_name(name).to_string()
        }
        T::List(inner, _) => {
            let inner_ts = recursive(inner)?;
            // `A | B[]` parses as `A | (B[])` — parenthesize union element types.
            if inner_ts.contains(" | ") {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        T::Map(_k, v, _) => {
            let v_ts = recursive(v)?;
            format!("Record<string, {v_ts}>")
        }
        T::RecursiveTypeAlias { name, .. } => {
            deps.push(name.clone());
            symbols.map_name(name).to_string()
        }
        T::Tuple(inner, _) => {
            let parts: Vec<String> = inner.iter().map(&mut recursive).collect::<Result<_>>()?;
            format!("[{}]", parts.join(", "))
        }
        T::Arrow(..) => {
            return Err(BamlBuilderError::InvalidArgument(
                "Arrow types are not supported in generated TypeScript".to_string(),
            ));
        }
        T::Union(union_gen, _) => match union_gen.view() {
            UnionTypeViewGeneric::Null => "null".to_string(),
            UnionTypeViewGeneric::Optional(inner) => {
                let inner_ts = recursive(inner)?;
                format!("{inner_ts} | null")
            }
            UnionTypeViewGeneric::OneOf(variants) => {
                let parts: Vec<String> = variants
                    .iter()
                    .map(|t| recursive(t))
                    .collect::<Result<_>>()?;
                parts.join(" | ")
            }
            UnionTypeViewGeneric::OneOfOptional(variants) => {
                let parts: Vec<String> = variants
                    .iter()
                    .map(|t| recursive(t))
                    .collect::<Result<_>>()?;
                format!("{} | null", parts.join(" | "))
            }
        },
        T::Top(_) => {
            return Err(BamlBuilderError::InvalidArgument(
                "Top/any type should not appear in TS generation".to_string(),
            ));
        }
    })
}

/// Collect all type names that need declarations (classes, enums, type aliases) from a fragment's deps.
pub fn collect_type_decl_deps(frag: &TsTypeFrag) -> HashSet<String> {
    frag.deps.iter().cloned().collect()
}

/// Collect structured TypeScript declarations for the given set of type names.
///
/// Transitively closes over:
/// - **class** fields (nested classes, enums, aliases in field types)
/// - **type alias** bodies (`type A = B | C` pulls in `B`, `C`, and their fields)
///
/// so TS never references a BAML class/enum/alias name without a matching `export`. Runtime-owned
/// symbols are returned too; [`crate::builder::ts_declarations::TsDeclarationRegistry`] owns their
/// suppression.
pub fn collect_ir_type_declarations(
    ir: &IRSignature,
    needed: &HashSet<String>,
) -> Result<Vec<TsDeclaration>> {
    collect_ir_type_declarations_with_symbols(ir, needed, &TsSymbolMap::default())
}

/// Collect structured IR declarations using the graph's TypeScript symbol mapping.
pub fn collect_ir_type_declarations_with_symbols(
    ir: &IRSignature,
    needed: &HashSet<String>,
    symbols: &TsSymbolMap,
) -> Result<Vec<TsDeclaration>> {
    let mut all_needed = needed.clone();
    let mut worklist: Vec<String> = needed.iter().cloned().collect();
    while let Some(name) = worklist.pop() {
        if let Some((_, class_details)) = ir.classes.get(&name) {
            for (_, fty) in class_details.fields.iter() {
                let frag = type_to_ts_expr_with_symbols(fty.as_ref(), symbols)?;
                for dep in frag.deps {
                    if all_needed.insert(dep.clone()) {
                        worklist.push(dep);
                    }
                }
            }
        } else if let Some(alias_node) = ir.type_aliases.get(&name) {
            // Return/arg types often surface as a `RecursiveTypeAlias` name only; the nested classes
            // live in the alias RHS and were previously never pulled into `all_needed`.
            let frag = type_to_ts_expr_with_symbols(alias_node.field_type.as_ref(), symbols)?;
            for dep in frag.deps {
                if all_needed.insert(dep.clone()) {
                    worklist.push(dep);
                }
            }
        }
    }
    let mut names: Vec<&String> = all_needed.iter().collect();
    names.sort();
    let mut declarations = Vec::with_capacity(names.len());
    for name in names {
        if let Some(declaration) = ir_type_declaration(ir, name, symbols)? {
            declarations.push(declaration);
        }
    }
    Ok(declarations)
}

fn ir_type_declaration(
    ir: &IRSignature,
    name: &str,
    symbols: &TsSymbolMap,
) -> Result<Option<TsDeclaration>> {
    let ts_symbol = symbols.map_name(name);
    if let Some((_, class_details)) = ir.classes.get(name) {
        let mut fields: js::Tokens = quote!();
        let mut dependencies = BTreeSet::new();
        for (fname, fty) in class_details.fields.iter() {
            let frag = type_to_ts_expr_with_symbols(fty.as_ref(), symbols)?;
            dependencies.extend(
                frag.deps
                    .iter()
                    .map(|dep| symbols.map_name(dep).to_string()),
            );
            let type_expr = frag.expr.as_str();
            quote_in!(fields => $(fname): $(type_expr););
            fields.push();
        }
        let mut tokens: js::Tokens = quote!();
        quote_in!(tokens => export interface $(ts_symbol) { $(fields) });
        Ok(Some(TsDeclaration {
            symbol: ts_symbol.to_string(),
            body: render_tokens(tokens)?,
            dependencies,
            origin: TsDeclarationOrigin::BamlIr,
        }))
    } else if let Some((_, enum_details)) = ir.enums.get(name) {
        let variants: Vec<String> = enum_details
            .values
            .iter()
            .map(|v| {
                let escaped = v.replace('\\', "\\\\").replace('\"', "\\\"");
                format!("\"{escaped}\"")
            })
            .collect();
        let union_expr = variants.join(" | ");
        Ok(Some(TsDeclaration {
            symbol: ts_symbol.to_string(),
            body: format!("export type {ts_symbol} = {union_expr};"),
            dependencies: BTreeSet::new(),
            origin: TsDeclarationOrigin::BamlIr,
        }))
    } else if let Some(type_node) = ir.type_aliases.get(name) {
        let (type_expr, dependencies) = if name == "OpaqueJson" {
            ("JsonValue".to_string(), BTreeSet::new())
        } else {
            let frag = type_to_ts_expr_with_symbols(type_node.field_type.as_ref(), symbols)?;
            (
                frag.expr,
                frag.deps
                    .iter()
                    .map(|dep| symbols.map_name(dep).to_string())
                    .collect(),
            )
        };
        Ok(Some(TsDeclaration {
            symbol: ts_symbol.to_string(),
            body: format!("export type {ts_symbol} = {type_expr};"),
            dependencies,
            origin: TsDeclarationOrigin::BamlIr,
        }))
    } else {
        Ok(None)
    }
}

fn render_tokens(tokens: js::Tokens) -> Result<String> {
    tokens
        .to_file_string()
        .map(|rendered| rendered.trim_end().to_string())
        .map_err(|error| {
            BamlBuilderError::InvalidArgument(format!("TypeScript render error: {error}"))
        })
}

/// Emit TypeScript type declarations (interfaces, enums, type aliases) for the given set of type names.
///
/// Compatibility renderer for callers that still want one token stream. Symbols owned by the static
/// runtime block are skipped because [`render_runtime_typescript`](baml_rt_tools::ts_gen::render_runtime_typescript)
/// emits them.
pub fn emit_type_declarations_tokens(
    ir: &IRSignature,
    needed: &HashSet<String>,
) -> Result<js::Tokens> {
    let declarations = collect_ir_type_declarations(ir, needed)?;
    let runtime_symbols: HashSet<&str> = baml_rt_tools::ts_gen::runtime_typescript_symbols()
        .iter()
        .copied()
        .collect();

    let mut out: js::Tokens = quote!();
    for declaration in declarations {
        if runtime_symbols.contains(declaration.symbol.as_str()) {
            continue;
        }
        for line in declaration.body.lines() {
            quote_in!(out => $(line));
            out.push();
        }
        out.line();
    }
    Ok(out)
}
