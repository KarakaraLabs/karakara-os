// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Render a [`ToolSchemaGraph`] into BAML tool DTO declarations.

use super::{
    AdditionalProperties, SchemaDeclaration, SchemaDeclarationKind, SchemaEnum, SchemaEnumVariant,
    SchemaLiteral, SchemaLocation, SchemaObject, SchemaTypeRef, ToolSchemaGraph,
    cycles::baml_emission_order,
};
use crate::builder::error::{BamlBuilderError, Result, write_line};

/// Render every declaration in the graph as BAML, once each, in deterministic dependency order.
///
/// Cycle and literal analysis run before any text is written, so unsupported recursive components
/// and BAML literals fail with graph symbols and source locations instead of a downstream compiler
/// error.
pub fn render_tool_schema_baml(graph: &ToolSchemaGraph) -> Result<String> {
    validate_baml_literals(graph)?;
    let mut output = String::new();
    for id in baml_emission_order(graph)? {
        let decl = graph
            .declarations
            .get(&id)
            .expect("emission order only contains graph declarations");
        render_declaration(graph, &mut output, decl)?;
        write_line(&mut output, "")?;
    }
    Ok(output)
}

/// Reject literals BAML cannot represent without widening their semantics.
///
/// BAML has string, integer, boolean, and null literal types, but no float literal type. Generated
/// enums only carry string and integer wire values; other literal sets lower to literal unions.
fn validate_baml_literals(graph: &ToolSchemaGraph) -> Result<()> {
    for declaration in graph.declarations.values() {
        match &declaration.kind {
            SchemaDeclarationKind::Object(object) => {
                for field in &object.fields {
                    let location = declaration
                        .source
                        .child("properties")
                        .child(&field.wire_name);
                    validate_baml_type_literals(&field.ty, &location)?;
                }
                if let AdditionalProperties::Typed(ty) = &object.additional_properties {
                    validate_baml_type_literals(
                        ty,
                        &declaration.source.child("additionalProperties"),
                    )?;
                }
            }
            SchemaDeclarationKind::Alias(ty) => {
                validate_baml_type_literals(ty, &declaration.source)?;
            }
            SchemaDeclarationKind::Enum(enumeration) => {
                for variant in &enumeration.variants {
                    if !matches!(
                        &variant.wire_value,
                        SchemaLiteral::String(_) | SchemaLiteral::Integer(_)
                    ) {
                        return Err(BamlBuilderError::InvalidArgument(format!(
                            "unsupported generated BAML enum variant `{}` at {}: generated BAML enums only support string and integer wire values; use a literal union or OpaqueJson",
                            variant.preferred_identifier, declaration.source
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_baml_type_literals(ty: &SchemaTypeRef, location: &SchemaLocation) -> Result<()> {
    match ty {
        SchemaTypeRef::Literal(SchemaLiteral::Number(value)) => {
            Err(BamlBuilderError::InvalidArgument(format!(
                "unsupported float literal `{value}` for generated BAML at {location}: BAML has no float literal type; use OpaqueJson or a string representation"
            )))
        }
        SchemaTypeRef::Array(inner)
        | SchemaTypeRef::Map(inner)
        | SchemaTypeRef::VecOrOne(inner) => validate_baml_type_literals(inner, location),
        SchemaTypeRef::Union(arms) => {
            for arm in arms {
                validate_baml_type_literals(arm, location)?;
            }
            Ok(())
        }
        SchemaTypeRef::String
        | SchemaTypeRef::Integer
        | SchemaTypeRef::Number
        | SchemaTypeRef::Boolean
        | SchemaTypeRef::Null
        | SchemaTypeRef::JsonValue
        | SchemaTypeRef::Literal(_)
        | SchemaTypeRef::Named(_) => Ok(()),
    }
}

fn render_declaration(
    graph: &ToolSchemaGraph,
    output: &mut String,
    decl: &SchemaDeclaration,
) -> Result<()> {
    match &decl.kind {
        SchemaDeclarationKind::Object(obj) => render_class(graph, output, &decl.symbol, obj),
        SchemaDeclarationKind::Enum(e) => render_enum(output, &decl.symbol, e),
        SchemaDeclarationKind::Alias(ty) => {
            let rendered = render_schema_type_ref(graph, ty);
            write_line(output, &format!("type {} = {rendered}", decl.symbol))
        }
    }
}

fn render_class(
    graph: &ToolSchemaGraph,
    output: &mut String,
    class_name: &str,
    obj: &SchemaObject,
) -> Result<()> {
    write_line(output, &format!("class {class_name} {{"))?;
    for field in &obj.fields {
        let rendered_ty = render_schema_type_ref(graph, &field.ty);
        let type_str = if field.required {
            rendered_ty
        } else if matches!(&field.ty, SchemaTypeRef::Union(_)) {
            format!("({rendered_ty})?")
        } else {
            format!("{rendered_ty}?")
        };
        // Wire names are not always legal BAML identifiers; fall back to the shaped identifier
        // and preserve the wire spelling with `@alias`.
        let ident = &field.preferred_identifier;
        let alias = (ident.as_str() != field.wire_name)
            .then(|| format!(" @alias(\"{}\")", escape_baml_string(&field.wire_name)));
        let alias = alias.unwrap_or_default();
        let description = field
            .description
            .as_deref()
            .map(|s| format!(" @description(\"{}\")", escape_baml_string(s)))
            .unwrap_or_default();
        write_line(output, &format!("  {ident} {type_str}{alias}{description}"))?;
    }
    match &obj.additional_properties {
        AdditionalProperties::AllowedAny => write_line(output, "  @@dynamic")?,
        AdditionalProperties::Forbidden => {}
        AdditionalProperties::Typed(_) => {
            return Err(BamlBuilderError::InvalidArgument(format!(
                "class `{class_name}` mixes fixed fields with typed additional properties, which generated BAML cannot represent"
            )));
        }
    }
    write_line(output, "}")
}

fn render_enum(output: &mut String, enum_name: &str, e: &SchemaEnum) -> Result<()> {
    write_line(output, &format!("enum {enum_name} {{"))?;
    for variant in &e.variants {
        render_enum_variant(output, variant)?;
    }
    write_line(output, "}")
}

fn render_enum_variant(output: &mut String, variant: &SchemaEnumVariant) -> Result<()> {
    let name = &variant.preferred_identifier;
    let description = variant
        .description
        .as_deref()
        .map(|s| format!(" @description(\"{}\")", escape_baml_string(s)))
        .unwrap_or_default();
    match &variant.wire_value {
        SchemaLiteral::String(wire) => {
            write_line(output, &format!("  {name}{description}"))?;
            if wire != name {
                write_line(
                    output,
                    &format!("    @alias(\"{}\")", escape_baml_string(wire)),
                )?;
            }
        }
        SchemaLiteral::Integer(wire) => {
            write_line(output, &format!("  {name}{description}"))?;
            write_line(output, &format!("    @alias(\"{wire}\")"))?;
        }
        // Literal enums other than strings/integers are not representable in the current BAML
        // contract; lowering rejects them before rendering.
        SchemaLiteral::Number(_) | SchemaLiteral::Boolean(_) | SchemaLiteral::Null => {
            write_line(output, &format!("  {name}{description}"))?;
        }
    }
    Ok(())
}

/// Render a normalized graph type expression in BAML syntax.
///
/// Tool cards and generated session interfaces use this rather than reading JSON Schema or legacy
/// type metadata, so every generated BAML reference comes from the same graph as the DTOs.
pub fn render_schema_type_ref(graph: &ToolSchemaGraph, ty: &SchemaTypeRef) -> String {
    match ty {
        SchemaTypeRef::String => "string".to_string(),
        SchemaTypeRef::Integer => "int".to_string(),
        SchemaTypeRef::Number => "float".to_string(),
        SchemaTypeRef::Boolean => "bool".to_string(),
        SchemaTypeRef::Null => "null".to_string(),
        SchemaTypeRef::JsonValue => "OpaqueJson".to_string(),
        SchemaTypeRef::Literal(l) => render_literal(l),
        SchemaTypeRef::Array(inner) => render_array_suffix(graph, inner),
        SchemaTypeRef::Map(inner) => {
            format!("map<string, {}>", render_schema_type_ref(graph, inner))
        }
        SchemaTypeRef::Named(id) => graph
            .declarations
            .get(id)
            .map(|d| d.symbol.clone())
            .unwrap_or_else(|| "<unresolved>".to_string()),
        SchemaTypeRef::Union(arms) => arms
            .iter()
            .map(|a| render_schema_type_ref(graph, a))
            .collect::<Vec<_>>()
            .join(" | "),
        SchemaTypeRef::VecOrOne(inner) => {
            let scalar = render_schema_type_ref(graph, inner);
            let list = render_array_suffix(graph, inner);
            format!("({scalar} | {list})")
        }
    }
}

/// Render an array of `inner`, parenthesizing a union element type so `[]` does not bind to
/// only its last arm (`A | B[]` parses as `A | (B[])`).
fn render_array_suffix(graph: &ToolSchemaGraph, inner: &SchemaTypeRef) -> String {
    let rendered = render_schema_type_ref(graph, inner);
    if matches!(inner, SchemaTypeRef::Union(_)) {
        format!("({rendered})[]")
    } else {
        format!("{rendered}[]")
    }
}

fn render_literal(lit: &SchemaLiteral) -> String {
    match lit {
        SchemaLiteral::String(s) => format!("\"{}\"", escape_baml_string(s)),
        SchemaLiteral::Integer(n) => n.to_string(),
        SchemaLiteral::Number(n) => n.clone(),
        SchemaLiteral::Boolean(b) => b.to_string(),
        SchemaLiteral::Null => "null".to_string(),
    }
}

fn escape_baml_string(value: &str) -> String {
    value.chars().flat_map(|c| c.escape_default()).collect()
}
