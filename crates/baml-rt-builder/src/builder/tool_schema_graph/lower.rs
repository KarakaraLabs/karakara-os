// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON Schema traversal and normalization into the graph.
//!
//! This mirrors the historical `schema_to_baml` traversal, so the compatibility path keeps its
//! observable BAML contract while producing a reusable renderer-neutral graph. Lowering is
//! deterministic: named schemas (roots then `$defs`/`definitions`) are pre-registered in sorted
//! order, and inline declarations are assigned ids in discovery order.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use baml_rt_tools::{
    OPAQUE_JSON_BAML_TYPE, OPAQUE_JSON_SCHEMA_MARKER_KEY,
    schema_allows_empty_or_optional_open_input, tools::ToolFunctionMetadata,
};
use serde_json::Value;

use super::{
    AdditionalProperties, OpenInputContract, OpenInputRequirement, SchemaDeclaration,
    SchemaDeclarationKind, SchemaEnum, SchemaEnumVariant, SchemaField, SchemaLiteral,
    SchemaLocation, SchemaObject, SchemaTypeId, SchemaTypeRef, ToolSchemaContract, ToolSchemaGraph,
    ToolSchemaRole,
    diagnostics::{external_reference, unsupported_json_schema, unsupported_named_schema},
    names::{enum_variant_identifier, to_pascal_case},
};
use crate::builder::error::{BamlBuilderError, Result};

/// A named schema discovered before lowering: a public root or a `$defs`/`definitions` entry.
struct NamedSchema {
    /// Registry key used by `$ref` (root schema name or def key).
    schema_name: String,
    /// Requested emitted symbol.
    symbol: String,
    schema: Value,
    location: SchemaLocation,
    /// Pre-allocated id (assigned in sorted order before lowering).
    id: SchemaTypeId,
}

/// Per-tool root information used to build [`ToolSchemaContract`]s after lowering.
struct ToolRootPlan {
    tool_name: String,
    class_name: String,
    open_input_schema_name: Option<String>,
    open_input_requirement: OpenInputRequirement,
    input_schema_name: String,
    output_schema_name: String,
}

pub(crate) fn build_from_tools(metadata: &[&ToolFunctionMetadata]) -> Result<ToolSchemaGraph> {
    let builder = GraphBuilder::new();

    // Collect roots across tools. Two tools may contribute the same root *schema name*; we key by
    // (name, schema) and keep the first, matching the single-registry semantics of the builder.
    let mut named: BTreeMap<String, NamedSchema> = BTreeMap::new();
    let mut tool_plans: Vec<ToolRootPlan> = Vec::new();

    for tool in metadata {
        let location = |role: ToolSchemaRole| SchemaLocation {
            tool_name: Some(tool.name.clone()),
            role: Some(role),
            json_pointer: String::new(),
        };

        let open_unit = tool.open_input_type.name == "()";
        let mut open_schema_name = None;
        if !open_unit {
            let name = tool.open_input_type.name.clone();
            insert_root(
                &mut named,
                name.clone(),
                name.clone(),
                tool.open_input_schema.clone(),
                location(ToolSchemaRole::OpenInput),
            )?;
            open_schema_name = Some(name);
        }
        insert_root(
            &mut named,
            tool.input_type.name.clone(),
            tool.input_type.name.clone(),
            tool.input_schema.clone(),
            location(ToolSchemaRole::Input),
        )?;
        insert_root(
            &mut named,
            tool.output_type.name.clone(),
            tool.output_type.name.clone(),
            tool.output_schema.clone(),
            location(ToolSchemaRole::Output),
        )?;

        let requirement =
            if open_unit || schema_allows_empty_or_optional_open_input(&tool.open_input_schema) {
                OpenInputRequirement::Optional
            } else {
                OpenInputRequirement::Required
            };

        tool_plans.push(ToolRootPlan {
            tool_name: tool.name.to_string(),
            class_name: tool.class_name.clone(),
            open_input_schema_name: open_schema_name,
            open_input_requirement: requirement,
            input_schema_name: tool.input_type.name.clone(),
            output_schema_name: tool.output_type.name.clone(),
        });
    }

    builder.lower_all(named, &tool_plans)
}

fn insert_root(
    named: &mut BTreeMap<String, NamedSchema>,
    schema_name: String,
    symbol: String,
    schema: Value,
    location: SchemaLocation,
) -> Result<()> {
    if let Some(existing) = named.get(&schema_name) {
        if existing.schema == schema {
            return Ok(());
        }
        return Err(BamlBuilderError::InvalidArgument(format!(
            "conflicting schema roots named `{schema_name}` at {} and {}",
            existing.location, location
        )));
    }
    // Id is a placeholder; assigned in `lower_all`.
    named.insert(
        schema_name.clone(),
        NamedSchema {
            schema_name,
            symbol,
            schema,
            location,
            id: SchemaTypeId::new(0),
        },
    );
    Ok(())
}

pub(crate) fn build_from_root_schemas(
    schemas: &HashMap<String, Value>,
    type_names: &HashMap<String, String>,
) -> Result<ToolSchemaGraph> {
    let builder = GraphBuilder::new();
    let mut named: BTreeMap<String, NamedSchema> = BTreeMap::new();

    // Roots, in deterministic order (sorted by requested symbol).
    let mut root_names: Vec<String> = schemas.keys().cloned().collect();
    root_names.sort();
    for name in &root_names {
        let symbol = type_names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.clone());
        named.insert(
            name.clone(),
            NamedSchema {
                schema_name: name.clone(),
                symbol,
                schema: schemas[name].clone(),
                location: SchemaLocation {
                    tool_name: None,
                    role: None,
                    json_pointer: String::new(),
                },
                id: SchemaTypeId::new(0),
            },
        );
    }

    // Collect `$defs`/`definitions` recursively; roots shadow defs (existing behavior).
    let mut defs: BTreeMap<String, Value> = BTreeMap::new();
    for name in &root_names {
        extract_defs(&schemas[name], &mut defs);
    }
    for (def_name, def_schema) in defs {
        if named.contains_key(&def_name) {
            continue;
        }
        let symbol = type_names
            .get(&def_name)
            .cloned()
            .unwrap_or_else(|| def_name.clone());
        named.insert(
            def_name.clone(),
            NamedSchema {
                schema_name: def_name.clone(),
                symbol,
                schema: def_schema,
                location: SchemaLocation {
                    tool_name: None,
                    role: None,
                    json_pointer: format!("/$defs/{def_name}"),
                },
                id: SchemaTypeId::new(0),
            },
        );
    }

    builder.lower_all(named, &[])
}

fn extract_defs(schema: &Value, defs: &mut BTreeMap<String, Value>) {
    if let Some(obj) = schema.as_object() {
        if let Some(d) = obj.get("$defs").and_then(Value::as_object) {
            for (k, v) in d {
                defs.insert(k.clone(), v.clone());
            }
        }
        if let Some(d) = obj.get("definitions").and_then(Value::as_object) {
            for (k, v) in d {
                defs.insert(k.clone(), v.clone());
            }
        }
        for value in obj.values() {
            extract_defs(value, defs);
        }
    } else if let Some(arr) = schema.as_array() {
        for item in arr {
            extract_defs(item, defs);
        }
    }
}

struct GraphBuilder {
    declarations: BTreeMap<SchemaTypeId, SchemaDeclaration>,
    symbols: BTreeMap<String, SchemaTypeId>,
    name_to_id: BTreeMap<String, SchemaTypeId>,
    next_id: u32,
    next_order: u32,
}

impl GraphBuilder {
    fn new() -> Self {
        Self {
            declarations: BTreeMap::new(),
            symbols: BTreeMap::new(),
            name_to_id: BTreeMap::new(),
            next_id: 0,
            next_order: 0,
        }
    }

    fn alloc_id(&mut self) -> SchemaTypeId {
        let id = SchemaTypeId::new(self.next_id);
        self.next_id += 1;
        id
    }

    fn lower_all(
        mut self,
        mut named: BTreeMap<String, NamedSchema>,
        tool_plans: &[ToolRootPlan],
    ) -> Result<ToolSchemaGraph> {
        // Assign ids to named schemas in sorted-by-symbol order (deterministic).
        let mut names: Vec<String> = named.keys().cloned().collect();
        names.sort_by(|a, b| named[a].symbol.cmp(&named[b].symbol));
        for name in &names {
            let id = self.alloc_id();
            named.get_mut(name).unwrap().id = id;
            self.name_to_id.insert(name.clone(), id);
        }

        // Lower named schemas in the same sorted order.
        for name in &names {
            let n = named.get(name).unwrap();
            if self.symbols.contains_key(&n.symbol) {
                // A nested inline declaration claimed this symbol first; point `$ref`s at it.
                let existing = self.symbols[&n.symbol];
                self.name_to_id.insert(n.schema_name.clone(), existing);
                continue;
            }
            self.lower_named(n)?;
        }

        // Build per-tool contracts.
        let mut tools = BTreeMap::new();
        for plan in tool_plans {
            let input = self.named_ref(&plan.input_schema_name)?;
            let output = self.named_ref(&plan.output_schema_name)?;
            let open_input = match &plan.open_input_schema_name {
                None => OpenInputContract::Absent,
                Some(name) => OpenInputContract::Present {
                    ty: self.named_ref(name)?,
                    requirement: plan.open_input_requirement,
                },
            };
            tools.insert(
                plan.tool_name.clone(),
                ToolSchemaContract {
                    tool_name: plan.tool_name.clone(),
                    class_name: plan.class_name.clone(),
                    open_input,
                    input,
                    output,
                },
            );
        }

        Ok(ToolSchemaGraph {
            tools,
            declarations: self.declarations,
            symbols: self.symbols,
        })
    }

    fn named_ref(&self, schema_name: &str) -> Result<SchemaTypeRef> {
        self.name_to_id
            .get(schema_name)
            .map(|id| SchemaTypeRef::Named(*id))
            .ok_or_else(|| {
                BamlBuilderError::InvalidArgument(format!(
                    "internal: no declaration lowered for schema root `{schema_name}`"
                ))
            })
    }

    fn lower_named(&mut self, n: &NamedSchema) -> Result<SchemaTypeId> {
        let kind = self.compute_kind(&n.symbol, &n.schema, &n.location)?;
        self.register(self.new_declaration(n.id, n.symbol.clone(), kind, &n.location))
    }

    /// Lower an inline declaration discovered during field traversal (title or parent+field name).
    fn lower_inline(
        &mut self,
        name: &str,
        schema: &Value,
        location: &SchemaLocation,
    ) -> Result<SchemaTypeId> {
        if let Some(existing) = self.symbols.get(name) {
            return Ok(*existing);
        }
        let id = self.alloc_id();
        let kind = self.compute_kind(name, schema, location)?;
        self.register(self.new_declaration(id, name.to_string(), kind, location))
    }

    /// Build a declaration (dependencies and fingerprint) with an unassigned render order.
    fn new_declaration(
        &self,
        id: SchemaTypeId,
        symbol: String,
        kind: SchemaDeclarationKind,
        source: &SchemaLocation,
    ) -> SchemaDeclaration {
        let dependencies = collect_dependencies(&kind);
        let fingerprint = fingerprint_of_kind(&kind);
        SchemaDeclaration {
            id,
            preferred_symbol: symbol.clone(),
            symbol,
            kind,
            dependencies,
            source: source.clone(),
            fingerprint,
            order: 0,
        }
    }

    fn register(&mut self, mut decl: SchemaDeclaration) -> Result<SchemaTypeId> {
        self.symbols.insert(decl.symbol.clone(), decl.id);
        decl.order = self.next_order;
        self.next_order += 1;
        let id = decl.id;
        self.declarations.insert(id, decl);
        Ok(id)
    }

    /// Compute the declaration kind for a schema with a known symbol. Mirrors the historical
    /// `generate_baml_type`.
    fn compute_kind(
        &mut self,
        symbol: &str,
        schema: &Value,
        location: &SchemaLocation,
    ) -> Result<SchemaDeclarationKind> {
        let obj = schema.as_object().ok_or_else(|| {
            BamlBuilderError::InvalidArgument(format!("Schema for {symbol} must be an object"))
        })?;

        if is_opaque_marker(obj) {
            return Ok(SchemaDeclarationKind::Alias(SchemaTypeRef::JsonValue));
        }

        if let Some(enum_values) = obj.get("enum").and_then(Value::as_array) {
            return Ok(SchemaDeclarationKind::Enum(self.build_flat_enum(
                enum_values,
                schema,
                location,
            )?));
        }

        if let Some(one_of) = obj.get("oneOf").and_then(Value::as_array) {
            if one_of
                .iter()
                .all(|v| v.as_object().is_some_and(|o| o.contains_key("const")))
            {
                return Ok(SchemaDeclarationKind::Enum(
                    self.build_const_enum(one_of, schema, location)?,
                ));
            }
            return self.build_root_union(symbol, one_of, location);
        }

        if let Some(any_of) = obj.get("anyOf").and_then(Value::as_array) {
            return self.build_root_union(symbol, any_of, location);
        }

        if obj.get("type").and_then(Value::as_str) == Some("object") {
            if is_map_schema(obj) {
                let value = obj.get("additionalProperties").unwrap();
                let value_type = self.lower_type_ref(value, Some((symbol, "Value")), location)?;
                return Ok(SchemaDeclarationKind::Alias(SchemaTypeRef::Map(Box::new(
                    value_type,
                ))));
            }
            if !is_inline_object_schema(obj) {
                return Err(unsupported_json_schema(
                    "object schema without properties or additionalProperties cannot be represented in generated BAML",
                    schema,
                    location,
                ));
            }
            return self.build_object_class(symbol, obj, location);
        }

        if obj.contains_key("properties") {
            if !is_inline_object_schema(obj) {
                return Err(unsupported_json_schema(
                    "object schema declares empty properties",
                    schema,
                    location,
                ));
            }
            return self.build_object_class(symbol, obj, location);
        }

        Err(unsupported_named_schema(symbol))
    }

    fn build_flat_enum(
        &self,
        values: &[Value],
        schema: &Value,
        location: &SchemaLocation,
    ) -> Result<SchemaEnum> {
        if values.is_empty() {
            return Err(unsupported_json_schema(
                "empty `enum` array",
                schema,
                location,
            ));
        }
        let mut variants = Vec::new();
        for (index, value) in values.iter().enumerate() {
            if let Some(s) = value.as_str() {
                let name = enum_variant_identifier(s, index);
                variants.push(SchemaEnumVariant {
                    wire_value: SchemaLiteral::String(s.to_string()),
                    preferred_identifier: name,
                    description: None,
                });
            } else if let Some(n) = value.as_i64() {
                variants.push(SchemaEnumVariant {
                    wire_value: SchemaLiteral::Integer(n),
                    preferred_identifier: format!("Variant{n}"),
                    description: None,
                });
            } else {
                return Err(unsupported_json_schema(
                    "enum values must all be strings or all be integers",
                    schema,
                    location,
                ));
            }
        }
        Ok(SchemaEnum {
            variants,
            description: None,
        })
    }

    fn build_const_enum(
        &self,
        one_of: &[Value],
        schema: &Value,
        location: &SchemaLocation,
    ) -> Result<SchemaEnum> {
        let mut variants = Vec::new();
        for (index, variant) in one_of.iter().enumerate() {
            let obj = variant.as_object().ok_or_else(|| {
                unsupported_json_schema("non-object `const` enum variant", schema, location)
            })?;
            let const_val = obj.get("const").and_then(Value::as_str).ok_or_else(|| {
                unsupported_json_schema("non-string `const` enum variant", schema, location)
            })?;
            let name = enum_variant_identifier(const_val, index);
            let description = obj
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string);
            variants.push(SchemaEnumVariant {
                wire_value: SchemaLiteral::String(const_val.to_string()),
                preferred_identifier: name,
                description,
            });
        }
        Ok(SchemaEnum {
            variants,
            description: None,
        })
    }

    /// Root-level `oneOf`/`anyOf` union: object variants become named `VariantN` declarations,
    /// non-object variants are lowered inline. Mirrors `generate_baml_type`.
    fn build_root_union(
        &mut self,
        symbol: &str,
        variants: &[Value],
        location: &SchemaLocation,
    ) -> Result<SchemaDeclarationKind> {
        let alt_names: Vec<String> = (0..variants.len()).map(|i| format!("Alt{i}")).collect();
        let mut arms = Vec::new();
        for (idx, variant) in variants.iter().enumerate() {
            let is_object_like = variant.as_object().is_some_and(|o| {
                o.contains_key("properties")
                    || o.get("type").and_then(Value::as_str) == Some("object")
            });
            if is_object_like {
                let variant_name = variant
                    .as_object()
                    .and_then(|obj| nested_name(obj, Some((symbol, &alt_names[idx]))).ok())
                    .unwrap_or_else(|| format!("{symbol}Variant{}", idx + 1));
                let id = self.lower_inline(&variant_name, variant, location)?;
                arms.push(SchemaTypeRef::Named(id));
            } else {
                let r = self.lower_type_ref(variant, Some((symbol, &alt_names[idx])), location)?;
                arms.push(r);
            }
        }
        Ok(SchemaDeclarationKind::Alias(SchemaTypeRef::Union(arms)))
    }

    fn build_object_class(
        &mut self,
        class_name: &str,
        obj: &serde_json::Map<String, Value>,
        location: &SchemaLocation,
    ) -> Result<SchemaDeclarationKind> {
        let properties = obj.get("properties").and_then(Value::as_object);
        let Some(properties) = properties else {
            return Ok(SchemaDeclarationKind::Object(SchemaObject {
                fields: Vec::new(),
                additional_properties: AdditionalProperties::AllowedAny,
                description: None,
            }));
        };
        if properties.is_empty() {
            return Ok(SchemaDeclarationKind::Object(SchemaObject {
                fields: Vec::new(),
                additional_properties: lower_additional_properties(obj),
                description: None,
            }));
        }

        // Pre-emit nested inline declarations (in field order) before this class, matching the
        // historical renderer.
        for (prop_name, prop_schema) in properties {
            self.preemit_inline(
                prop_schema,
                Some((class_name, prop_name.as_str())),
                location,
            )?;
        }

        let required: HashSet<&str> = obj
            .get("required")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        let mut fields = Vec::new();
        for (prop_name, prop_schema) in properties {
            let ty = self.lower_type_ref(
                prop_schema,
                Some((class_name, prop_name.as_str())),
                location,
            )?;
            let description = prop_schema
                .as_object()
                .and_then(|o| o.get("description"))
                .and_then(Value::as_str)
                .map(str::to_string);
            fields.push(SchemaField {
                wire_name: prop_name.clone(),
                preferred_identifier: prop_name.clone(),
                ty,
                required: required.contains(prop_name.as_str()),
                description,
            });
        }

        Ok(SchemaDeclarationKind::Object(SchemaObject {
            fields,
            additional_properties: lower_additional_properties(obj),
            description: None,
        }))
    }

    /// Recursively emit nested inline declarations reachable from a field schema, mirroring
    /// `preemit_nested_inline_classes`.
    fn preemit_inline(
        &mut self,
        schema: &Value,
        hint: Option<(&str, &str)>,
        location: &SchemaLocation,
    ) -> Result<()> {
        if schema.is_boolean() {
            return Ok(());
        }
        let Some(obj) = schema.as_object() else {
            return Ok(());
        };
        if obj.contains_key("$ref") {
            return Ok(());
        }
        if is_map_schema(obj) {
            if let Some(value_schema) = obj.get("additionalProperties") {
                self.preemit_inline(value_schema, hint, location)?;
            }
            return Ok(());
        }
        if is_inline_object_schema(obj) {
            let nested_name = nested_name(obj, hint)?;
            if !self.symbols.contains_key(&nested_name) {
                self.lower_inline(&nested_name, schema, location)?;
            }
            return Ok(());
        }
        if obj.get("type").and_then(Value::as_str) == Some("string")
            && let Some(enum_array) = obj.get("enum").and_then(Value::as_array)
            && !enum_array.is_empty()
        {
            let nested_name = nested_name(obj, hint)?;
            if !self.symbols.contains_key(&nested_name) {
                self.lower_inline(&nested_name, schema, location)?;
            }
            return Ok(());
        }
        if let Some(Value::Array(type_array)) = obj.get("type") {
            for value in type_array {
                match value {
                    Value::String(s) if s == "array" => {
                        if let Some(items) = obj.get("items") {
                            self.preemit_inline(items, hint, location)?;
                        }
                    }
                    Value::Object(_) => self.preemit_inline(value, hint, location)?,
                    _ => {}
                }
            }
            return Ok(());
        }
        if obj.get("type").and_then(Value::as_str) == Some("array")
            && let Some(items) = obj.get("items")
        {
            self.preemit_inline(items, hint, location)?;
            return Ok(());
        }
        if let Some(any_of) = obj.get("anyOf").and_then(Value::as_array) {
            for variant in any_of {
                self.preemit_inline(variant, hint, location)?;
            }
        }
        if let Some(one_of) = obj.get("oneOf").and_then(Value::as_array) {
            for variant in one_of {
                self.preemit_inline(variant, hint, location)?;
            }
        }
        Ok(())
    }

    /// Convert a JSON Schema into a graph type expression. Mirrors `json_schema_to_baml_type`.
    fn lower_type_ref(
        &mut self,
        schema: &Value,
        hint: Option<(&str, &str)>,
        location: &SchemaLocation,
    ) -> Result<SchemaTypeRef> {
        if schema.is_boolean() {
            return Err(unsupported_json_schema(
                "boolean schemas cannot be represented in generated BAML",
                schema,
                location,
            ));
        }
        let obj = schema.as_object().ok_or_else(|| {
            BamlBuilderError::InvalidArgument("Schema must be an object".to_string())
        })?;

        if is_opaque_marker(obj) {
            return Ok(SchemaTypeRef::JsonValue);
        }

        if let Some(ref_path) = obj.get("$ref").and_then(Value::as_str) {
            return self.lower_ref(ref_path, location);
        }

        // Inline object with properties.
        if is_inline_object_schema(obj) {
            let nested_name = nested_name(obj, hint)?;
            let id = self.lower_inline(&nested_name, schema, location)?;
            return Ok(SchemaTypeRef::Named(id));
        }

        if is_map_schema(obj) {
            let value_type = if let Some(value_schema) = obj.get("additionalProperties") {
                self.lower_type_ref(value_schema, hint, location)?
            } else {
                SchemaTypeRef::String
            };
            return Ok(SchemaTypeRef::Map(Box::new(value_type)));
        }

        // Nullable `type: ["T", "null"]` and other type arrays.
        if let Some(Value::Array(type_array)) = obj.get("type") {
            let mut mapped = Vec::new();
            for value in type_array {
                match value {
                    Value::String(type_str) => {
                        if type_str == "null" {
                            continue;
                        }
                        mapped.push(self.primitive_type_ref(type_str, obj, hint, location)?);
                    }
                    Value::Object(_) => mapped.push(self.lower_type_ref(value, hint, location)?),
                    other => {
                        return Err(unsupported_json_schema(
                            "non-string entry inside `type` array",
                            other,
                            location,
                        ));
                    }
                }
            }
            if mapped.is_empty() {
                return Ok(SchemaTypeRef::Null);
            }
            if mapped.len() == 1 {
                return Ok(mapped.remove(0));
            }
            return Ok(SchemaTypeRef::Union(mapped));
        }

        if let Some(any_of) = obj.get("anyOf").and_then(Value::as_array) {
            let mut types = Vec::new();
            for variant in any_of {
                types.push(self.lower_type_ref(variant, hint, location)?);
            }
            return Ok(SchemaTypeRef::Union(types));
        }

        if let Some(one_of) = obj.get("oneOf").and_then(Value::as_array) {
            // vec_or_one wire shape: `T | T[]` where the array item equals the scalar arm.
            if one_of.len() == 2
                && let Some(second) = one_of[1].as_object()
                && second.get("type").and_then(Value::as_str) == Some("array")
                && let Some(items) = second.get("items")
                && items == &one_of[0]
            {
                let t = self.lower_type_ref(items, hint, location)?;
                return Ok(SchemaTypeRef::VecOrOne(Box::new(t)));
            }
            let mut types = Vec::new();
            for variant in one_of {
                types.push(self.lower_type_ref(variant, hint, location)?);
            }
            let mut seen = BTreeSet::new();
            let mut deduped: Vec<SchemaTypeRef> = Vec::new();
            for t in types {
                let key = type_ref_key(&t);
                if seen.insert(key) {
                    deduped.push(t);
                }
            }
            return Ok(SchemaTypeRef::Union(deduped));
        }

        // Effective scalar type string (single `type` value).
        if let Some(type_str) = obj.get("type").and_then(Value::as_str) {
            return self.primitive_type_ref(type_str, obj, hint, location);
        }

        if obj.contains_key("enum") {
            return Err(unsupported_json_schema(
                "enum schema could not be normalized into a generated BAML enum",
                schema,
                location,
            ));
        }

        Err(unsupported_json_schema(
            "schema is missing a supported type discriminator",
            schema,
            location,
        ))
    }

    fn primitive_type_ref(
        &mut self,
        type_str: &str,
        obj: &serde_json::Map<String, Value>,
        hint: Option<(&str, &str)>,
        location: &SchemaLocation,
    ) -> Result<SchemaTypeRef> {
        match type_str {
            "string" => {
                if let Some(enum_array) = obj.get("enum").and_then(Value::as_array)
                    && !enum_array.is_empty()
                {
                    let nested_name = nested_name(obj, hint)?;
                    let id =
                        self.lower_inline(&nested_name, &Value::Object(obj.clone()), location)?;
                    return Ok(SchemaTypeRef::Named(id));
                }
                Ok(SchemaTypeRef::String)
            }
            "integer" => Ok(SchemaTypeRef::Integer),
            "number" => Ok(SchemaTypeRef::Number),
            "boolean" => Ok(SchemaTypeRef::Boolean),
            "null" => Ok(SchemaTypeRef::Null),
            "array" => {
                if let Some(items) = obj.get("items") {
                    let item_type = self.lower_type_ref(items, hint, location)?;
                    Ok(SchemaTypeRef::Array(Box::new(item_type)))
                } else {
                    Err(unsupported_json_schema(
                        "array schema is missing `items`",
                        &Value::Object(obj.clone()),
                        location,
                    ))
                }
            }
            "object" => {
                if is_inline_object_schema(obj) {
                    let nested_name = nested_name(obj, hint)?;
                    let id =
                        self.lower_inline(&nested_name, &Value::Object(obj.clone()), location)?;
                    return Ok(SchemaTypeRef::Named(id));
                }
                if is_map_schema(obj)
                    && let Some(value_schema) = obj.get("additionalProperties")
                {
                    let value_type = self.lower_type_ref(value_schema, hint, location)?;
                    return Ok(SchemaTypeRef::Map(Box::new(value_type)));
                }
                Err(unsupported_json_schema(
                    "object schema without properties or additionalProperties cannot be represented in generated BAML",
                    &Value::Object(obj.clone()),
                    location,
                ))
            }
            other => Err(unsupported_json_schema(
                format!("unknown JSON Schema type `{other}`"),
                &Value::Object(obj.clone()),
                location,
            )),
        }
    }

    fn lower_ref(&self, ref_path: &str, location: &SchemaLocation) -> Result<SchemaTypeRef> {
        if !ref_path.starts_with('#') {
            return Err(external_reference(ref_path, location));
        }
        let name = ref_path.split('/').next_back().filter(|s| !s.is_empty());
        match name.and_then(|n| self.name_to_id.get(n)) {
            Some(id) => Ok(SchemaTypeRef::Named(*id)),
            None => Err(BamlBuilderError::InvalidArgument(format!(
                "unresolved local JSON Schema reference `{ref_path}` at {location}"
            ))),
        }
    }
}

fn is_opaque_marker(obj: &serde_json::Map<String, Value>) -> bool {
    obj.get(OPAQUE_JSON_SCHEMA_MARKER_KEY)
        .and_then(Value::as_str)
        .is_some_and(|v| v == OPAQUE_JSON_BAML_TYPE)
}

fn is_inline_object_schema(obj: &serde_json::Map<String, Value>) -> bool {
    // Empty fixed objects occur in valid static DTOs (for example a no-argument tagged-union
    // variant). BAML permits an empty class, so keep the closed-object contract instead of
    // mistaking `additionalProperties: false` for a typed map.
    obj.get("properties").and_then(Value::as_object).is_some()
}

fn is_map_schema(obj: &serde_json::Map<String, Value>) -> bool {
    obj.get("type").and_then(Value::as_str) == Some("object")
        && obj.get("additionalProperties").is_some()
        && !is_inline_object_schema(obj)
}

fn lower_additional_properties(obj: &serde_json::Map<String, Value>) -> AdditionalProperties {
    match obj.get("additionalProperties") {
        None | Some(Value::Bool(true)) => AdditionalProperties::AllowedAny,
        Some(Value::Bool(false)) => AdditionalProperties::Forbidden,
        Some(Value::Object(_)) | Some(Value::Array(_)) => {
            // Mixed fixed-fields + typed additional-properties is not representable in the current
            // BAML renderer; model it as a typed map without lowering the inner schema.
            AdditionalProperties::Typed(Box::new(SchemaTypeRef::JsonValue))
        }
        Some(_) => AdditionalProperties::AllowedAny,
    }
}

/// Stable name for an inline object/enum: explicit `title` else parent + PascalCase field.
fn nested_name(obj: &serde_json::Map<String, Value>, hint: Option<(&str, &str)>) -> Result<String> {
    if let Some(title) = obj.get("title").and_then(Value::as_str)
        && !title.is_empty()
    {
        return Ok(title.to_string());
    }
    let Some((parent, field)) = hint else {
        return Err(BamlBuilderError::InvalidArgument(
            "inline schema has no title and no parent/field hint for a generated name".to_string(),
        ));
    };
    Ok(format!("{parent}{}", to_pascal_case(field)))
}

/// Collect referenced declaration ids from a declaration kind.
fn collect_dependencies(kind: &SchemaDeclarationKind) -> BTreeSet<SchemaTypeId> {
    let mut deps = BTreeSet::new();
    match kind {
        SchemaDeclarationKind::Object(o) => {
            for field in &o.fields {
                collect_type_ref_deps(&field.ty, &mut deps);
            }
        }
        SchemaDeclarationKind::Enum(_) => {}
        SchemaDeclarationKind::Alias(t) => collect_type_ref_deps(t, &mut deps),
    }
    deps
}

fn collect_type_ref_deps(ty: &SchemaTypeRef, deps: &mut BTreeSet<SchemaTypeId>) {
    match ty {
        SchemaTypeRef::Named(id) => {
            deps.insert(*id);
        }
        SchemaTypeRef::Array(inner)
        | SchemaTypeRef::Map(inner)
        | SchemaTypeRef::VecOrOne(inner) => {
            collect_type_ref_deps(inner, deps);
        }
        SchemaTypeRef::Union(arms) => {
            for arm in arms {
                collect_type_ref_deps(arm, deps);
            }
        }
        SchemaTypeRef::String
        | SchemaTypeRef::Integer
        | SchemaTypeRef::Number
        | SchemaTypeRef::Boolean
        | SchemaTypeRef::Null
        | SchemaTypeRef::JsonValue
        | SchemaTypeRef::Literal(_) => {}
    }
}

/// Deterministic structural fingerprint of a declaration kind (descriptions excluded).
fn fingerprint_of_kind(kind: &SchemaDeclarationKind) -> String {
    match kind {
        SchemaDeclarationKind::Object(o) => {
            let mut parts: Vec<String> = o
                .fields
                .iter()
                .map(|f| {
                    format!(
                        "{}:{}:{}",
                        f.wire_name,
                        f.required,
                        type_ref_fingerprint(&f.ty)
                    )
                })
                .collect();
            parts.push(format!("ap:{:?}", o.additional_properties));
            format!("object[{}]", parts.join(","))
        }
        SchemaDeclarationKind::Enum(e) => {
            let parts: Vec<String> = e
                .variants
                .iter()
                .map(|v| format!("{:?}", v.wire_value))
                .collect();
            format!("enum[{}]", parts.join(","))
        }
        SchemaDeclarationKind::Alias(t) => format!("alias[{}]", type_ref_fingerprint(t)),
    }
}

fn type_ref_fingerprint(ty: &SchemaTypeRef) -> String {
    match ty {
        SchemaTypeRef::String => "string".to_string(),
        SchemaTypeRef::Integer => "int".to_string(),
        SchemaTypeRef::Number => "float".to_string(),
        SchemaTypeRef::Boolean => "bool".to_string(),
        SchemaTypeRef::Null => "null".to_string(),
        SchemaTypeRef::JsonValue => "json".to_string(),
        SchemaTypeRef::Literal(l) => format!("lit:{l:?}"),
        SchemaTypeRef::Array(inner) => format!("arr[{}]", type_ref_fingerprint(inner)),
        SchemaTypeRef::Map(inner) => format!("map[{}]", type_ref_fingerprint(inner)),
        SchemaTypeRef::Named(id) => format!("ref:{}", id.as_u32()),
        SchemaTypeRef::Union(arms) => {
            let parts: Vec<String> = arms.iter().map(type_ref_fingerprint).collect();
            format!("union[{}]", parts.join("|"))
        }
        SchemaTypeRef::VecOrOne(inner) => format!("vec_or_one[{}]", type_ref_fingerprint(inner)),
    }
}

/// Canonical key for deduplicating union arms.
fn type_ref_key(ty: &SchemaTypeRef) -> String {
    type_ref_fingerprint(ty)
}
