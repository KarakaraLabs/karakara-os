// SPDX-FileCopyrightText: 2026 Karakara Labs
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
    OPAQUE_JSON_BAML_TYPE, OPAQUE_JSON_SCHEMA_MARKER_KEY, ToolCapability,
    tools::ToolFunctionMetadata,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    AdditionalProperties, OpenInputClassification, OpenInputContract, OpenInputRequirement,
    SchemaDeclaration, SchemaDeclarationKind, SchemaEnum, SchemaEnumVariant, SchemaField,
    SchemaLiteral, SchemaLocation, SchemaObject, SchemaOwner, SchemaTypeId, SchemaTypeRef,
    ToolSchemaContract, ToolSchemaGraph, ToolSchemaRole, classify_open_input,
    diagnostics::{external_reference, unsupported_json_schema, unsupported_named_schema},
    names::{
        enum_variant_identifier, field_identifier, is_reserved_symbol, is_valid_identifier,
        sanitize_identifier, to_pascal_case,
    },
};
use crate::builder::error::{BamlBuilderError, Result};

/// Document-scoped identity for a schema root or local definition.
///
/// A local `$ref` is resolved against its own schema document. The emitted symbol is deliberately
/// not part of this key because separate documents may define the same final pointer segment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SchemaKey {
    document: String,
    pointer: String,
}

/// A named schema discovered before lowering: a public root or a root `$defs`/`definitions` entry.
struct NamedSchema {
    key: SchemaKey,
    /// Requested emitted symbol.
    symbol: String,
    schema: Value,
    location: SchemaLocation,
    /// Pre-allocated id (assigned in sorted order before lowering).
    id: SchemaTypeId,
    owner: SchemaOwner,
}

/// Per-tool root information used to build [`ToolSchemaContract`]s after lowering.
struct ToolRootPlan {
    tool_name: String,
    class_name: String,
    open_input_key: Option<SchemaKey>,
    open_input_requirement: Option<OpenInputRequirement>,
    input_key: SchemaKey,
    output_key: SchemaKey,
    capability: ToolCapability,
}

pub(crate) fn build_from_tools(metadata: &[&ToolFunctionMetadata]) -> Result<ToolSchemaGraph> {
    let builder = GraphBuilder::new();
    let mut named: BTreeMap<SchemaKey, NamedSchema> = BTreeMap::new();
    let mut tool_plans: Vec<ToolRootPlan> = Vec::new();

    for tool in metadata {
        let location = |role: ToolSchemaRole| SchemaLocation {
            tool_name: Some(tool.name.clone()),
            role: Some(role),
            json_pointer: String::new(),
        };
        let document = |role: ToolSchemaRole| format!("tool:{}:{}", tool.name, role.as_str());

        let open_input_classification = classify_open_input(tool);
        let open_input_key = match open_input_classification {
            OpenInputClassification::Absent => None,
            OpenInputClassification::Present(_) => Some(insert_document(
                &mut named,
                document(ToolSchemaRole::OpenInput),
                tool.open_input_type.name.clone(),
                tool.open_input_schema.clone(),
                location(ToolSchemaRole::OpenInput),
                SchemaOwner::PublicRoot {
                    tool_name: tool.name.to_string(),
                    role: ToolSchemaRole::OpenInput,
                },
                None,
            )?),
        };
        let input_key = insert_document(
            &mut named,
            document(ToolSchemaRole::Input),
            tool.input_type.name.clone(),
            tool.input_schema.clone(),
            location(ToolSchemaRole::Input),
            SchemaOwner::PublicRoot {
                tool_name: tool.name.to_string(),
                role: ToolSchemaRole::Input,
            },
            None,
        )?;
        let output_key = insert_document(
            &mut named,
            document(ToolSchemaRole::Output),
            tool.output_type.name.clone(),
            tool.output_schema.clone(),
            location(ToolSchemaRole::Output),
            SchemaOwner::PublicRoot {
                tool_name: tool.name.to_string(),
                role: ToolSchemaRole::Output,
            },
            None,
        )?;

        let open_input_requirement = match open_input_classification {
            OpenInputClassification::Absent => None,
            OpenInputClassification::Present(requirement) => Some(requirement),
        };

        tool_plans.push(ToolRootPlan {
            tool_name: tool.name.to_string(),
            class_name: tool.class_name.clone(),
            open_input_key,
            open_input_requirement,
            input_key,
            output_key,
            capability: tool.capability,
        });
    }

    builder.lower_all(named, &tool_plans)
}

pub(crate) fn build_from_root_schemas(
    schemas: &HashMap<String, Value>,
    type_names: &HashMap<String, String>,
) -> Result<ToolSchemaGraph> {
    let builder = GraphBuilder::new();
    let mut named: BTreeMap<SchemaKey, NamedSchema> = BTreeMap::new();

    let mut root_names: Vec<String> = schemas.keys().cloned().collect();
    root_names.sort();
    for name in &root_names {
        let symbol = type_names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.clone());
        insert_document(
            &mut named,
            format!("compat:{name}"),
            symbol,
            schemas[name].clone(),
            SchemaLocation {
                tool_name: None,
                role: None,
                json_pointer: String::new(),
            },
            SchemaOwner::StandaloneRoot {
                schema_name: name.clone(),
            },
            Some(type_names),
        )?;
    }

    builder.lower_all(named, &[])
}

/// Register one schema document and its root-level local definitions.
fn insert_document(
    named: &mut BTreeMap<SchemaKey, NamedSchema>,
    document: String,
    root_symbol: String,
    schema: Value,
    location: SchemaLocation,
    owner: SchemaOwner,
    definition_type_names: Option<&HashMap<String, String>>,
) -> Result<SchemaKey> {
    let root_key = SchemaKey {
        document: document.clone(),
        pointer: String::new(),
    };
    if named.contains_key(&root_key) {
        return Err(BamlBuilderError::InvalidArgument(format!(
            "internal: duplicate schema document `{document}`"
        )));
    }

    named.insert(
        root_key.clone(),
        NamedSchema {
            key: root_key.clone(),
            symbol: root_symbol,
            schema: schema.clone(),
            location: location.clone(),
            id: SchemaTypeId::new(0),
            owner,
        },
    );

    collect_named_definitions(
        named,
        &document,
        &schema,
        "",
        &location,
        definition_type_names,
    );

    Ok(root_key)
}

fn collect_named_definitions(
    named: &mut BTreeMap<SchemaKey, NamedSchema>,
    document: &str,
    schema: &Value,
    pointer: &str,
    root_location: &SchemaLocation,
    definition_type_names: Option<&HashMap<String, String>>,
) {
    match schema {
        Value::Object(object) => {
            for (keyword, child) in object {
                let child_pointer = format!("{pointer}/{}", super::escape_json_pointer(keyword));
                if (keyword == "$defs" || keyword == "definitions")
                    && let Some(definitions) = child.as_object()
                {
                    for (name, definition) in definitions {
                        let definition_pointer =
                            format!("{child_pointer}/{}", super::escape_json_pointer(name));
                        let key = SchemaKey {
                            document: document.to_string(),
                            pointer: definition_pointer.clone(),
                        };
                        let requested_symbol = definition_type_names
                            .and_then(|names| names.get(name))
                            .cloned()
                            .or_else(|| {
                                definition
                                    .as_object()
                                    .and_then(usable_schema_title)
                                    .map(str::to_string)
                            })
                            .unwrap_or_else(|| name.clone());
                        let symbol = if is_valid_identifier(&requested_symbol) {
                            requested_symbol
                        } else {
                            sanitize_identifier(&requested_symbol)
                        };
                        named.entry(key.clone()).or_insert_with(|| NamedSchema {
                            key,
                            symbol,
                            schema: definition.clone(),
                            location: SchemaLocation {
                                tool_name: root_location.tool_name.clone(),
                                role: root_location.role,
                                json_pointer: definition_pointer.clone(),
                            },
                            id: SchemaTypeId::new(0),
                            owner: SchemaOwner::SharedDefinition {
                                document: document.to_string(),
                                tool_name: root_location
                                    .tool_name
                                    .as_ref()
                                    .map(ToString::to_string),
                                role: root_location.role,
                                original_name: name.clone(),
                                json_pointer: definition_pointer.clone(),
                            },
                        });
                        collect_named_definitions(
                            named,
                            document,
                            definition,
                            &definition_pointer,
                            root_location,
                            definition_type_names,
                        );
                    }
                } else {
                    collect_named_definitions(
                        named,
                        document,
                        child,
                        &child_pointer,
                        root_location,
                        definition_type_names,
                    );
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_named_definitions(
                    named,
                    document,
                    item,
                    &format!("{pointer}/{index}"),
                    root_location,
                    definition_type_names,
                );
            }
        }
        _ => {}
    }
}

struct GraphBuilder {
    declarations: BTreeMap<SchemaTypeId, SchemaDeclaration>,
    symbols: BTreeMap<String, SchemaTypeId>,
    key_to_id: BTreeMap<SchemaKey, SchemaTypeId>,
    /// Pre-allocated ids that deduplicated into an existing declaration.
    id_aliases: BTreeMap<SchemaTypeId, SchemaTypeId>,
    /// Symbols claimed by public roots; nested declarations must never take one.
    root_symbols: BTreeSet<String>,
    next_id: u32,
    next_order: u32,
}

impl GraphBuilder {
    fn new() -> Self {
        Self {
            declarations: BTreeMap::new(),
            symbols: BTreeMap::new(),
            key_to_id: BTreeMap::new(),
            id_aliases: BTreeMap::new(),
            root_symbols: BTreeSet::new(),
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
        mut named: BTreeMap<SchemaKey, NamedSchema>,
        tool_plans: &[ToolRootPlan],
    ) -> Result<ToolSchemaGraph> {
        // Assign ids by requested symbol and then document-scoped key. This makes allocation stable
        // while allowing the same `$defs` key to exist independently in several root documents.
        let mut keys: Vec<SchemaKey> = named.keys().cloned().collect();
        keys.sort_by(|a, b| named[a].symbol.cmp(&named[b].symbol).then_with(|| a.cmp(b)));
        for key in &keys {
            let id = self.alloc_id();
            named.get_mut(key).unwrap().id = id;
            // Pre-register every exact pointer so forward and recursive `$ref`s resolve.
            self.key_to_id.insert(key.clone(), id);
        }

        // Reserve and validate public symbols before nested lowering can claim one.
        for key in &keys {
            let n = &named[key];
            if n.owner.is_public_root() {
                if is_reserved_symbol(&n.symbol) || !is_valid_identifier(&n.symbol) {
                    return Err(BamlBuilderError::InvalidArgument(format!(
                        "public root `{}` at {} is not a valid generated BAML symbol or is reserved",
                        n.symbol, n.location
                    )));
                }
                self.root_symbols.insert(n.symbol.clone());
            }
        }

        // Public roots keep priority over definitions with the same preferred symbol.
        let mut ordered: Vec<SchemaKey> = keys
            .iter()
            .filter(|key| named[*key].owner.is_public_root())
            .cloned()
            .collect();
        ordered.sort_by(|a, b| named[a].symbol.cmp(&named[b].symbol).then_with(|| a.cmp(b)));
        let mut definitions: Vec<SchemaKey> = keys
            .iter()
            .filter(|key| !named[*key].owner.is_public_root())
            .cloned()
            .collect();
        definitions.sort_by(|a, b| named[a].symbol.cmp(&named[b].symbol).then_with(|| a.cmp(b)));
        ordered.extend(definitions);

        for key in &ordered {
            let n = named.get(key).unwrap();
            let id = self.lower_named(n)?;
            self.key_to_id.insert(n.key.clone(), id);
        }
        self.remap_deduplicated_references();
        // Public-root registration is deferred until every definition and inline declaration has
        // been lowered. This lets root equivalence use canonical fingerprints rather than the
        // preallocation ids embedded in the first-pass declaration fingerprints.
        self.finalize_public_roots()?;
        self.remap_deduplicated_references();

        let mut tools = BTreeMap::new();
        for plan in tool_plans {
            let input = self.named_ref(&plan.input_key)?;
            let output = self.named_ref(&plan.output_key)?;
            let open_input = match (&plan.open_input_key, plan.open_input_requirement) {
                (None, None) => OpenInputContract::Absent,
                (Some(key), Some(requirement)) => OpenInputContract::Present {
                    ty: self.named_ref(key)?,
                    requirement,
                },
                _ => {
                    return Err(BamlBuilderError::InvalidArgument(format!(
                        "internal: inconsistent open-input classification for tool `{}`",
                        plan.tool_name
                    )));
                }
            };
            tools.insert(
                plan.tool_name.clone(),
                ToolSchemaContract {
                    tool_name: plan.tool_name.clone(),
                    class_name: plan.class_name.clone(),
                    open_input,
                    input,
                    output,
                    capability: plan.capability,
                },
            );
        }

        Ok(ToolSchemaGraph {
            tools,
            declarations: self.declarations,
            symbols: self.symbols,
        })
    }

    fn named_ref(&self, key: &SchemaKey) -> Result<SchemaTypeRef> {
        self.key_to_id
            .get(key)
            .map(|id| SchemaTypeRef::Named(*id))
            .ok_or_else(|| {
                BamlBuilderError::InvalidArgument(format!(
                    "internal: no declaration lowered for schema document `{}` pointer `{}`",
                    key.document, key.pointer
                ))
            })
    }

    fn lower_named(&mut self, n: &NamedSchema) -> Result<SchemaTypeId> {
        let kind = self.compute_kind(&n.symbol, &n.schema, &n.key.document, &n.location)?;
        self.register(self.new_declaration(
            n.id,
            n.symbol.clone(),
            kind,
            n.owner.clone(),
            &n.location,
        ))
    }

    /// Lower an inline declaration discovered during field traversal (title or parent+field name).
    ///
    /// Inline declarations may deduplicate: a second occurrence with the same preferred symbol and
    /// identical fingerprint reuses the first declaration instead of allocating a new id (so no
    /// other reference can be left pointing at a dangling pre-allocated id).
    fn lower_inline(
        &mut self,
        name: &str,
        schema: &Value,
        document: &str,
        location: &SchemaLocation,
    ) -> Result<SchemaTypeId> {
        let kind = self.compute_kind(name, schema, document, location)?;
        let fingerprint = fingerprint_of_kind(&kind);
        let owner = nested_owner(document, location);
        if let Some(existing_id) = self.symbols.get(name).copied()
            && self.declarations[&existing_id].fingerprint == fingerprint
        {
            self.declarations
                .get_mut(&existing_id)
                .expect("symbol registry points to a declaration")
                .owners
                .insert(owner);
            return Ok(existing_id);
        }
        let id = self.alloc_id();
        self.register(self.new_declaration(id, name.to_string(), kind, owner, location))
    }

    /// Build a declaration (dependencies and fingerprint) with an unassigned render order.
    fn new_declaration(
        &self,
        id: SchemaTypeId,
        symbol: String,
        kind: SchemaDeclarationKind,
        owner: SchemaOwner,
        source: &SchemaLocation,
    ) -> SchemaDeclaration {
        let dependencies = collect_dependencies(&kind);
        let fingerprint = fingerprint_of_kind(&kind);
        SchemaDeclaration {
            id,
            preferred_symbol: symbol.clone(),
            symbol,
            owners: BTreeSet::from([owner]),
            kind,
            dependencies,
            source: source.clone(),
            fingerprint,
            order: 0,
        }
    }

    /// Register a declaration, resolving symbol collisions deterministically.
    ///
    /// Public roots keep their requested name (a conflict is a hard error); shared/nested
    /// declarations are qualified with their owning tool and schema path when their preferred
    /// symbol is reserved, claimed by a root, or already taken by an incompatible declaration.
    /// Inline deduplication is handled by [`Self::lower_inline`] before an id is allocated.
    fn register(&mut self, mut decl: SchemaDeclaration) -> Result<SchemaTypeId> {
        let preferred = decl.preferred_symbol.clone();
        let fingerprint = decl.fingerprint.clone();
        let is_public_root = decl.owners.iter().any(SchemaOwner::is_public_root);

        let symbol = if is_public_root {
            if is_reserved_symbol(&preferred) || !is_valid_identifier(&preferred) {
                return Err(BamlBuilderError::InvalidArgument(format!(
                    "public root `{preferred}` at {} is not a valid generated BAML symbol or is reserved",
                    decl.source
                )));
            }
            // Public roots are finalized after all declarations are lowered so equivalent roots
            // can be compared by canonical fingerprints. Keep the requested symbol provisional.
            preferred
        } else if let Some(existing_id) = self.symbols.get(&preferred).copied()
            && self.declarations[&existing_id].fingerprint == fingerprint
        {
            self.declarations
                .get_mut(&existing_id)
                .expect("symbol registry points to a declaration")
                .owners
                .extend(decl.owners);
            self.id_aliases.insert(decl.id, existing_id);
            return Ok(existing_id);
        } else if is_reserved_symbol(&preferred)
            || self.root_symbols.contains(&preferred)
            || self.symbols.contains_key(&preferred)
        {
            self.qualify_symbol(&preferred, &decl.source, &fingerprint)?
        } else {
            preferred
        };

        if is_reserved_symbol(&symbol) {
            return Err(BamlBuilderError::InvalidArgument(format!(
                "declaration `{symbol}` at {} collides with a reserved generated BAML symbol",
                decl.source
            )));
        }

        decl.symbol = symbol.clone();
        decl.order = self.next_order;
        self.next_order += 1;
        let id = decl.id;
        if !is_public_root {
            self.symbols.insert(symbol, id);
        }
        self.declarations.insert(id, decl);
        Ok(id)
    }

    /// Resolve deferred public-root symbols after every declaration has been lowered.
    ///
    /// Root fingerprints are structural (via [`canonical_fingerprints`]), so equivalent roots are
    /// independent of the ids preallocated for their local `$defs`. Identical roots share one
    /// declaration and retain every owner; differing roots with one requested symbol are rejected.
    fn finalize_public_roots(&mut self) -> Result<()> {
        let canonical = canonical_fingerprints(&self.declarations);
        let mut groups: BTreeMap<String, Vec<SchemaTypeId>> = BTreeMap::new();
        for (id, declaration) in &self.declarations {
            groups
                .entry(declaration.preferred_symbol.clone())
                .or_default()
                .push(*id);
        }

        let mut root_aliases = BTreeMap::new();
        for (symbol, mut ids) in groups {
            ids.sort_unstable();
            let public_ids: Vec<SchemaTypeId> = ids
                .iter()
                .copied()
                .filter(|id| {
                    self.declarations[id]
                        .owners
                        .iter()
                        .any(SchemaOwner::is_public_root)
                })
                .collect();
            let Some(&keep) = public_ids.first() else {
                continue;
            };
            let keep_fingerprint = canonical
                .get(&keep)
                .expect("canonical fingerprints cover every declaration");

            let mut merge = BTreeSet::new();
            for duplicate in &ids {
                if *duplicate == keep {
                    continue;
                }
                let is_public = self.declarations[duplicate]
                    .owners
                    .iter()
                    .any(SchemaOwner::is_public_root);
                let duplicate_fingerprint = canonical
                    .get(duplicate)
                    .expect("canonical fingerprints cover every declaration");
                if duplicate_fingerprint != keep_fingerprint {
                    if is_public {
                        let existing = &self.declarations[&keep];
                        let incoming = &self.declarations[duplicate];
                        return Err(BamlBuilderError::InvalidArgument(format!(
                            "conflicting public roots named `{symbol}` at {} and {}",
                            existing.source, incoming.source
                        )));
                    }
                    // A definition that requested the root symbol but has a different shape keeps
                    // its qualified symbol.
                    continue;
                }
                merge.insert(*duplicate);
            }

            if !merge.is_empty() {
                let owners = self.declarations[&keep].owners.clone();
                let mut owners = owners;
                for duplicate in &merge {
                    let declaration = self
                        .declarations
                        .remove(duplicate)
                        .expect("merged declaration exists");
                    if self.symbols.get(&declaration.symbol) == Some(duplicate) {
                        self.symbols.remove(&declaration.symbol);
                    }
                    owners.extend(declaration.owners);
                    root_aliases.insert(*duplicate, keep);
                }
                self.declarations
                    .get_mut(&keep)
                    .expect("public root declaration exists")
                    .owners = owners;
            }

            if let Some(existing) = self.symbols.insert(symbol.clone(), keep) {
                return Err(BamlBuilderError::InvalidArgument(format!(
                    "public root `{symbol}` collides with non-root declaration {}",
                    existing.as_u32()
                )));
            }
        }

        if !root_aliases.is_empty() {
            self.id_aliases.extend(root_aliases);
            for id in self.key_to_id.values_mut() {
                *id = canonical_id(*id, &self.id_aliases);
            }
        }
        Ok(())
    }

    fn remap_deduplicated_references(&mut self) {
        if self.id_aliases.is_empty() {
            return;
        }
        let aliases = self.id_aliases.clone();
        for declaration in self.declarations.values_mut() {
            rewrite_kind_ids(&mut declaration.kind, &aliases);
            declaration.dependencies = collect_dependencies(&declaration.kind);
            declaration.fingerprint = fingerprint_of_kind(&declaration.kind);
        }
    }

    /// Choose a unique, deterministic symbol for a nested declaration whose preferred symbol is
    /// reserved, claimed by a root, or already taken by an incompatible declaration.
    fn qualify_symbol(
        &self,
        preferred: &str,
        source: &SchemaLocation,
        fingerprint: &str,
    ) -> Result<String> {
        let mut candidates: Vec<String> = Vec::new();
        if let Some(tool) = &source.tool_name {
            candidates.push(format!("{preferred}{}", tool_qualifier(tool)));
        }
        let path = path_qualifier(&source.json_pointer);
        if !path.is_empty() {
            match &source.tool_name {
                Some(tool) => {
                    candidates.push(format!("{preferred}{}{}", tool_qualifier(tool), path))
                }
                None => candidates.push(format!("{preferred}{path}")),
            }
        }
        candidates.push(format!("{preferred}{}", short_digest(fingerprint)));

        for candidate in candidates {
            if !is_reserved_symbol(&candidate)
                && !self.root_symbols.contains(&candidate)
                && !self.symbols.contains_key(&candidate)
            {
                return Ok(candidate);
            }
        }
        Err(BamlBuilderError::InvalidArgument(format!(
            "could not allocate a unique symbol for `{preferred}` at {source}"
        )))
    }

    /// Compute the declaration kind for a schema with a known symbol. Mirrors the historical
    /// `generate_baml_type`.
    fn compute_kind(
        &mut self,
        symbol: &str,
        schema: &Value,
        document: &str,
        location: &SchemaLocation,
    ) -> Result<SchemaDeclarationKind> {
        match schema {
            Value::Bool(true) => {
                return Ok(SchemaDeclarationKind::Alias(SchemaTypeRef::JsonValue));
            }
            Value::Bool(false) => {
                return Err(unsupported_json_schema(
                    "the `false` boolean schema accepts no values and generated BAML has no `never` type",
                    schema,
                    location,
                ));
            }
            _ => {}
        }
        let obj = schema.as_object().ok_or_else(|| {
            unsupported_json_schema(
                format!("schema for `{symbol}` must be an object"),
                schema,
                location,
            )
        })?;

        if is_opaque_marker(obj) || obj.is_empty() {
            return Ok(SchemaDeclarationKind::Alias(SchemaTypeRef::JsonValue));
        }

        if let Some(const_value) = obj.get("const") {
            let literal = require_schema_literal(const_value, schema, location, "`const`")?;
            return Ok(SchemaDeclarationKind::Alias(SchemaTypeRef::Literal(
                literal,
            )));
        }

        if let Some(enum_values) = obj.get("enum").and_then(Value::as_array) {
            return self.build_enum_declaration(enum_values, schema, location);
        }

        if let Some(one_of) = obj.get("oneOf").and_then(Value::as_array) {
            if one_of
                .iter()
                .all(|v| v.as_object().is_some_and(|o| o.contains_key("const")))
            {
                return self.build_const_declaration(one_of, schema, location);
            }
            return self.build_root_union(symbol, "oneOf", one_of, document, location);
        }

        if let Some(any_of) = obj.get("anyOf").and_then(Value::as_array) {
            return self.build_root_union(symbol, "anyOf", any_of, document, location);
        }

        if obj.get("type").and_then(Value::as_str) == Some("object") {
            if is_map_schema(obj) {
                let value = obj.get("additionalProperties").unwrap();
                let value_type = self.lower_type_ref(
                    value,
                    Some((symbol, "Value")),
                    document,
                    &location.child("additionalProperties"),
                )?;
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
            return self.build_object_class(symbol, obj, document, location);
        }

        if obj.contains_key("properties") {
            if !is_inline_object_schema(obj) {
                return Err(unsupported_json_schema(
                    "object schema declares empty properties",
                    schema,
                    location,
                ));
            }
            return self.build_object_class(symbol, obj, document, location);
        }

        Err(unsupported_named_schema(symbol, location))
    }

    fn build_enum_declaration(
        &self,
        values: &[Value],
        schema: &Value,
        location: &SchemaLocation,
    ) -> Result<SchemaDeclarationKind> {
        if values.is_empty() {
            return Err(unsupported_json_schema(
                "empty `enum` array",
                schema,
                location,
            ));
        }
        let mut variants = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let literal = require_schema_literal(
                value,
                schema,
                &location.child("enum").index(index),
                "`enum` value",
            )?;
            variants.push((literal, None));
        }
        Ok(enum_or_literal_declaration(variants))
    }

    fn build_const_declaration(
        &self,
        one_of: &[Value],
        schema: &Value,
        location: &SchemaLocation,
    ) -> Result<SchemaDeclarationKind> {
        if one_of.is_empty() {
            return Err(unsupported_json_schema(
                "empty `oneOf` const variant list",
                schema,
                location,
            ));
        }
        let mut variants = Vec::with_capacity(one_of.len());
        for (index, variant) in one_of.iter().enumerate() {
            let variant_location = location.child("oneOf").index(index);
            let obj = variant.as_object().ok_or_else(|| {
                unsupported_json_schema(
                    "non-object `const` enum variant",
                    variant,
                    &variant_location,
                )
            })?;
            let const_value = obj.get("const").ok_or_else(|| {
                unsupported_json_schema(
                    "`oneOf` const variant is missing `const`",
                    variant,
                    &variant_location,
                )
            })?;
            let literal =
                require_schema_literal(const_value, variant, &variant_location, "`const`")?;
            let description = obj
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string);
            variants.push((literal, description));
        }
        Ok(enum_or_literal_declaration(variants))
    }

    /// Root-level `oneOf`/`anyOf` union: object variants become named `VariantN` declarations,
    /// non-object variants are lowered inline. Mirrors `generate_baml_type`.
    fn build_root_union(
        &mut self,
        symbol: &str,
        variants_key: &str,
        variants: &[Value],
        document: &str,
        location: &SchemaLocation,
    ) -> Result<SchemaDeclarationKind> {
        let alt_names: Vec<String> = (0..variants.len()).map(|i| format!("Alt{i}")).collect();
        let mut arms = Vec::new();
        for (idx, variant) in variants.iter().enumerate() {
            let variant_location = location.child(variants_key).index(idx);
            let is_object_like = variant.as_object().is_some_and(|o| {
                o.contains_key("properties")
                    || o.get("type").and_then(Value::as_str) == Some("object")
            });
            if is_object_like {
                let variant_name = variant
                    .as_object()
                    .and_then(|obj| nested_name(obj, Some((symbol, &alt_names[idx]))).ok())
                    .unwrap_or_else(|| format!("{symbol}Variant{}", idx + 1));
                let id = self.lower_inline(&variant_name, variant, document, &variant_location)?;
                arms.push(SchemaTypeRef::Named(id));
            } else {
                let r = self.lower_type_ref(
                    variant,
                    Some((symbol, &alt_names[idx])),
                    document,
                    &variant_location,
                )?;
                arms.push(r);
            }
        }
        Ok(SchemaDeclarationKind::Alias(SchemaTypeRef::Union(arms)))
    }

    fn build_object_class(
        &mut self,
        class_name: &str,
        obj: &serde_json::Map<String, Value>,
        document: &str,
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
                additional_properties: lower_additional_properties(
                    obj,
                    &location.child("additionalProperties"),
                )?,
                description: None,
            }));
        }

        // Pre-emit nested inline declarations (in field order) before this class, matching the
        // historical renderer.
        for (prop_name, prop_schema) in properties {
            let prop_location = location.child("properties").child(prop_name);
            self.preemit_inline(
                prop_schema,
                Some((class_name, prop_name.as_str())),
                document,
                &prop_location,
            )?;
        }

        let required: HashSet<&str> = obj
            .get("required")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        let mut fields = Vec::new();
        let mut field_identifiers = BTreeSet::new();
        for (prop_name, prop_schema) in properties {
            let prop_location = location.child("properties").child(prop_name);
            let ty = self.lower_type_ref(
                prop_schema,
                Some((class_name, prop_name.as_str())),
                document,
                &prop_location,
            )?;
            let description = prop_schema
                .as_object()
                .and_then(|o| o.get("description"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let identifier = unique_identifier(field_identifier(prop_name), &mut field_identifiers);
            fields.push(SchemaField {
                wire_name: prop_name.clone(),
                preferred_identifier: identifier,
                ty,
                required: required.contains(prop_name.as_str()),
                description,
            });
        }

        Ok(SchemaDeclarationKind::Object(SchemaObject {
            fields,
            additional_properties: lower_additional_properties(
                obj,
                &location.child("additionalProperties"),
            )?,
            description: None,
        }))
    }

    /// Recursively emit nested inline declarations reachable from a field schema, mirroring
    /// `preemit_nested_inline_classes`.
    fn preemit_inline(
        &mut self,
        schema: &Value,
        hint: Option<(&str, &str)>,
        document: &str,
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
        // `const` lowers inline to a literal; no named declaration is needed.
        if obj.contains_key("const") {
            return Ok(());
        }
        if is_map_schema(obj) {
            if let Some(value_schema) = obj.get("additionalProperties") {
                self.preemit_inline(
                    value_schema,
                    hint,
                    document,
                    &location.child("additionalProperties"),
                )?;
            }
            return Ok(());
        }
        if is_inline_object_schema(obj) {
            let nested_name = nested_name(obj, hint)?;
            if !self.symbols.contains_key(&nested_name) {
                self.lower_inline(&nested_name, schema, document, location)?;
            }
            return Ok(());
        }
        if obj.get("type").and_then(Value::as_str) == Some("string")
            && let Some(enum_array) = obj.get("enum").and_then(Value::as_array)
            && !enum_array.is_empty()
        {
            let nested_name = nested_name(obj, hint)?;
            if !self.symbols.contains_key(&nested_name) {
                self.lower_inline(&nested_name, schema, document, location)?;
            }
            return Ok(());
        }
        if let Some(Value::Array(type_array)) = obj.get("type") {
            for value in type_array {
                match value {
                    Value::String(s) if s == "array" => {
                        if let Some(items) = obj.get("items") {
                            self.preemit_inline(items, hint, document, &location.child("items"))?;
                        }
                    }
                    Value::Object(_) => self.preemit_inline(value, hint, document, location)?,
                    _ => {}
                }
            }
            return Ok(());
        }
        if obj.get("type").and_then(Value::as_str) == Some("array")
            && let Some(items) = obj.get("items")
        {
            self.preemit_inline(items, hint, document, &location.child("items"))?;
            return Ok(());
        }
        if let Some(any_of) = obj.get("anyOf").and_then(Value::as_array) {
            if let Some((index, other)) = nullable_other(any_of) {
                self.preemit_inline(other, hint, document, &location.child("anyOf").index(index))?;
            } else {
                let base_name =
                    hint.map(|(parent, field)| format!("{parent}{}", to_pascal_case(field)));
                for (idx, variant) in any_of.iter().enumerate() {
                    let alt = format!("Alt{idx}");
                    let variant_hint = base_name.as_deref().map(|base| (base, alt.as_str()));
                    self.preemit_inline(
                        variant,
                        variant_hint,
                        document,
                        &location.child("anyOf").index(idx),
                    )?;
                }
            }
        }
        if let Some(one_of) = obj.get("oneOf").and_then(Value::as_array) {
            if let Some(items) = vec_or_one_item(one_of) {
                self.preemit_inline(
                    items,
                    hint,
                    document,
                    &location.child("oneOf").index(1).child("items"),
                )?;
            } else if let Some((index, other)) = nullable_other(one_of) {
                self.preemit_inline(other, hint, document, &location.child("oneOf").index(index))?;
            } else {
                let base_name =
                    hint.map(|(parent, field)| format!("{parent}{}", to_pascal_case(field)));
                for (idx, variant) in one_of.iter().enumerate() {
                    let alt = format!("Alt{idx}");
                    let variant_hint = base_name.as_deref().map(|base| (base, alt.as_str()));
                    self.preemit_inline(
                        variant,
                        variant_hint,
                        document,
                        &location.child("oneOf").index(idx),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Convert a JSON Schema into a graph type expression. Mirrors `json_schema_to_baml_type`.
    fn lower_type_ref(
        &mut self,
        schema: &Value,
        hint: Option<(&str, &str)>,
        document: &str,
        location: &SchemaLocation,
    ) -> Result<SchemaTypeRef> {
        match schema {
            Value::Bool(true) => return Ok(SchemaTypeRef::JsonValue),
            Value::Bool(false) => {
                return Err(unsupported_json_schema(
                    "the `false` boolean schema accepts no values and generated BAML has no `never` type",
                    schema,
                    location,
                ));
            }
            _ => {}
        }
        let obj = schema
            .as_object()
            .ok_or_else(|| unsupported_json_schema("schema must be an object", schema, location))?;

        if is_opaque_marker(obj) {
            return Ok(SchemaTypeRef::JsonValue);
        }

        if let Some(ref_path) = obj.get("$ref").and_then(Value::as_str) {
            return self.lower_ref(ref_path, document, location);
        }

        if let Some(const_value) = obj.get("const") {
            let literal = require_schema_literal(const_value, schema, location, "`const`")?;
            return Ok(SchemaTypeRef::Literal(literal));
        }

        // Inline object with properties.
        if is_inline_object_schema(obj) {
            let nested_name = nested_name(obj, hint)?;
            let id = self.lower_inline(&nested_name, schema, document, location)?;
            return Ok(SchemaTypeRef::Named(id));
        }

        if is_map_schema(obj) {
            let value_type = if let Some(value_schema) = obj.get("additionalProperties") {
                self.lower_type_ref(
                    value_schema,
                    hint,
                    document,
                    &location.child("additionalProperties"),
                )?
            } else {
                SchemaTypeRef::String
            };
            return Ok(SchemaTypeRef::Map(Box::new(value_type)));
        }

        // Inline enums (with or without an explicit `type`) become named enum/literal declarations.
        if obj.get("enum").and_then(Value::as_array).is_some() {
            let nested_name = nested_name(obj, hint)?;
            let id = self.lower_inline(&nested_name, schema, document, location)?;
            return Ok(SchemaTypeRef::Named(id));
        }

        // `type: ["T", "null"]` and other type arrays. Nullability is preserved: `null` is a
        // first-class union arm, never collapsed into the scalar type.
        if let Some(Value::Array(type_array)) = obj.get("type") {
            let mut mapped = Vec::new();
            for (index, value) in type_array.iter().enumerate() {
                let type_location = location.child("type").index(index);
                match value {
                    Value::String(type_str) => {
                        mapped.push(self.primitive_type_ref(
                            type_str,
                            obj,
                            hint,
                            document,
                            &type_location,
                        )?);
                    }
                    Value::Object(_) => {
                        mapped.push(self.lower_type_ref(value, hint, document, &type_location)?)
                    }
                    other => {
                        return Err(unsupported_json_schema(
                            "non-string entry inside `type` array",
                            other,
                            &type_location,
                        ));
                    }
                }
            }
            return Ok(normalize_union(mapped));
        }

        if let Some(any_of) = obj.get("anyOf").and_then(Value::as_array) {
            // Nullable wrapper `anyOf: [T, null]` keeps T's name and adds a `Null` arm.
            if let Some((index, other)) = nullable_other(any_of) {
                let t = self.lower_type_ref(
                    other,
                    hint,
                    document,
                    &location.child("anyOf").index(index),
                )?;
                return Ok(normalize_union(vec![t, SchemaTypeRef::Null]));
            }
            let base_name =
                hint.map(|(parent, field)| format!("{parent}{}", to_pascal_case(field)));
            let mut types = Vec::new();
            for (idx, variant) in any_of.iter().enumerate() {
                let alt = format!("Alt{idx}");
                let variant_hint = base_name.as_deref().map(|base| (base, alt.as_str()));
                types.push(self.lower_type_ref(
                    variant,
                    variant_hint,
                    document,
                    &location.child("anyOf").index(idx),
                )?);
            }
            return Ok(normalize_union(types));
        }

        if let Some(one_of) = obj.get("oneOf").and_then(Value::as_array) {
            // vec_or_one wire shape: `T | T[]` where the array item equals the scalar arm.
            if let Some(items) = vec_or_one_item(one_of) {
                let t = self.lower_type_ref(
                    items,
                    hint,
                    document,
                    &location.child("oneOf").index(1).child("items"),
                )?;
                return Ok(SchemaTypeRef::VecOrOne(Box::new(t)));
            }
            // Nullable wrapper `oneOf: [T, null]` keeps T's name and adds a `Null` arm.
            if let Some((index, other)) = nullable_other(one_of) {
                let t = self.lower_type_ref(
                    other,
                    hint,
                    document,
                    &location.child("oneOf").index(index),
                )?;
                return Ok(normalize_union(vec![t, SchemaTypeRef::Null]));
            }
            let base_name =
                hint.map(|(parent, field)| format!("{parent}{}", to_pascal_case(field)));
            let mut types = Vec::new();
            for (idx, variant) in one_of.iter().enumerate() {
                let alt = format!("Alt{idx}");
                let variant_hint = base_name.as_deref().map(|base| (base, alt.as_str()));
                types.push(self.lower_type_ref(
                    variant,
                    variant_hint,
                    document,
                    &location.child("oneOf").index(idx),
                )?);
            }
            return Ok(normalize_union(types));
        }

        // Effective scalar type string (single `type` value).
        if let Some(type_str) = obj.get("type").and_then(Value::as_str) {
            return self.primitive_type_ref(type_str, obj, hint, document, location);
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
        document: &str,
        location: &SchemaLocation,
    ) -> Result<SchemaTypeRef> {
        match type_str {
            "string" => Ok(SchemaTypeRef::String),
            "integer" => Ok(SchemaTypeRef::Integer),
            "number" => Ok(SchemaTypeRef::Number),
            "boolean" => Ok(SchemaTypeRef::Boolean),
            "null" => Ok(SchemaTypeRef::Null),
            "array" => {
                if let Some(items) = obj.get("items") {
                    let item_type =
                        self.lower_type_ref(items, hint, document, &location.child("items"))?;
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
                    let id = self.lower_inline(
                        &nested_name,
                        &Value::Object(obj.clone()),
                        document,
                        location,
                    )?;
                    return Ok(SchemaTypeRef::Named(id));
                }
                if is_map_schema(obj)
                    && let Some(value_schema) = obj.get("additionalProperties")
                {
                    let value_type = self.lower_type_ref(
                        value_schema,
                        hint,
                        document,
                        &location.child("additionalProperties"),
                    )?;
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

    fn lower_ref(
        &self,
        ref_path: &str,
        document: &str,
        location: &SchemaLocation,
    ) -> Result<SchemaTypeRef> {
        if !ref_path.starts_with('#') {
            return Err(external_reference(ref_path, location));
        }

        let pointer = canonical_local_definition_pointer(ref_path).ok_or_else(|| {
            BamlBuilderError::InvalidArgument(format!(
                "unresolved local JSON Schema reference `{ref_path}` at {location}; only `#/$defs/...` and `#/definitions/...` are supported"
            ))
        })?;
        let key = SchemaKey {
            document: document.to_string(),
            pointer,
        };
        self.key_to_id
            .get(&key)
            .copied()
            .map(SchemaTypeRef::Named)
            .ok_or_else(|| {
                BamlBuilderError::InvalidArgument(format!(
                    "unresolved local JSON Schema reference `{ref_path}` at {location}"
                ))
            })
    }
}

fn canonical_local_definition_pointer(reference: &str) -> Option<String> {
    let body = reference.strip_prefix('#')?;
    let raw_segments: Vec<&str> = body.split('/').collect();
    if raw_segments.len() < 3
        || !raw_segments[0].is_empty()
        || (raw_segments[1] != "$defs" && raw_segments[1] != "definitions")
    {
        return None;
    }

    let mut pointer = format!("/{}", raw_segments[1]);
    for raw_segment in &raw_segments[2..] {
        let segment = decode_json_pointer_segment(raw_segment)?;
        pointer.push('/');
        pointer.push_str(&super::escape_json_pointer(&segment));
    }
    Some(pointer)
}

fn decode_json_pointer_segment(segment: &str) -> Option<String> {
    let mut decoded = String::with_capacity(segment.len());
    let mut chars = segment.chars();
    while let Some(ch) = chars.next() {
        if ch != '~' {
            decoded.push(ch);
            continue;
        }
        match chars.next()? {
            '0' => decoded.push('~'),
            '1' => decoded.push('/'),
            _ => return None,
        }
    }
    Some(decoded)
}

fn is_null_schema(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.get("type").and_then(Value::as_str) == Some("null"))
}

/// When `arms` is a nullable wrapper `[T, null]` (in either order), return T's index and value.
fn nullable_other(arms: &[Value]) -> Option<(usize, &Value)> {
    if arms.len() != 2 {
        return None;
    }
    if is_null_schema(&arms[0]) {
        Some((1, &arms[1]))
    } else if is_null_schema(&arms[1]) {
        Some((0, &arms[0]))
    } else {
        None
    }
}

/// When `one_of` is the `vec_or_one` wire shape `[T, { type: "array", items: T }]`, return `T`.
fn vec_or_one_item(one_of: &[Value]) -> Option<&Value> {
    if one_of.len() != 2 {
        return None;
    }
    let second = one_of[1].as_object()?;
    if second.get("type").and_then(Value::as_str) != Some("array") {
        return None;
    }
    let items = second.get("items")?;
    if items == &one_of[0] {
        Some(items)
    } else {
        None
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
        && obj
            .get("additionalProperties")
            .is_some_and(Value::is_object)
        && !is_inline_object_schema(obj)
}

/// Resolve the additional-properties policy for a fixed object. A typed
/// `additionalProperties` schema alongside fixed fields is not representable in generated BAML,
/// so it is rejected rather than silently widened to a closed object.
fn lower_additional_properties(
    obj: &serde_json::Map<String, Value>,
    location: &SchemaLocation,
) -> Result<AdditionalProperties> {
    match obj.get("additionalProperties") {
        None | Some(Value::Bool(true)) => Ok(AdditionalProperties::AllowedAny),
        Some(Value::Bool(false)) => Ok(AdditionalProperties::Forbidden),
        Some(schema @ Value::Object(_)) => Err(unsupported_json_schema(
            "fixed objects with a typed `additionalProperties` schema cannot be represented in generated BAML; use a pure map schema or `OpaqueJson`",
            schema,
            location,
        )),
        Some(other) => Err(unsupported_json_schema(
            "unsupported `additionalProperties` value",
            other,
            location,
        )),
    }
}

/// Stable name for an inline object/enum: explicit `title` else parent + PascalCase field.
fn nested_owner(document: &str, location: &SchemaLocation) -> SchemaOwner {
    let path = location
        .json_pointer
        .split('/')
        .skip(1)
        .map(|segment| decode_json_pointer_segment(segment).unwrap_or_else(|| segment.to_string()))
        .collect();
    SchemaOwner::Nested {
        document: document.to_string(),
        tool_name: location.tool_name.as_ref().map(ToString::to_string),
        role: location.role,
        path,
    }
}

/// Convert a JSON `const` or `enum` value into a renderer-neutral literal.
///
/// Object and array literals have no generated-language literal form and return `None`.
fn schema_literal(value: &Value) -> Option<SchemaLiteral> {
    match value {
        Value::String(value) => Some(SchemaLiteral::String(value.clone())),
        Value::Bool(value) => Some(SchemaLiteral::Boolean(*value)),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Some(SchemaLiteral::Integer(integer))
            } else {
                Some(SchemaLiteral::Number(value.to_string()))
            }
        }
        Value::Null => Some(SchemaLiteral::Null),
        Value::Array(_) | Value::Object(_) => None,
    }
}

fn require_schema_literal(
    value: &Value,
    schema: &Value,
    location: &SchemaLocation,
    context: &str,
) -> Result<SchemaLiteral> {
    schema_literal(value).ok_or_else(|| {
        unsupported_json_schema(
            format!(
                "{context} must be a string, integer, number, boolean, or null to become a generated literal"
            ),
            schema,
            location,
        )
    })
}

/// Emit a generated enum when the wire values are homogeneous strings or integers. Other literal
/// sets use a literal union so boolean, null, mixed, and (for the TypeScript renderer) numeric
/// values keep their wire shape.
fn enum_or_literal_declaration(
    variants: Vec<(SchemaLiteral, Option<String>)>,
) -> SchemaDeclarationKind {
    let all_strings = variants
        .iter()
        .all(|(literal, _)| matches!(literal, SchemaLiteral::String(_)));
    let all_integers = variants
        .iter()
        .all(|(literal, _)| matches!(literal, SchemaLiteral::Integer(_)));

    if all_strings || all_integers {
        let mut identifiers = BTreeSet::new();
        let variants = variants
            .into_iter()
            .enumerate()
            .map(|(index, (wire_value, description))| {
                let base = match &wire_value {
                    SchemaLiteral::String(value) => enum_variant_identifier(value, index),
                    SchemaLiteral::Integer(value) => format!("Variant{value}"),
                    _ => format!("Variant{index}"),
                };
                SchemaEnumVariant {
                    wire_value,
                    preferred_identifier: unique_identifier(base, &mut identifiers),
                    description,
                }
            })
            .collect();
        SchemaDeclarationKind::Enum(SchemaEnum {
            variants,
            description: None,
        })
    } else {
        let arms = variants
            .into_iter()
            .map(|(literal, _)| SchemaTypeRef::Literal(literal))
            .collect();
        SchemaDeclarationKind::Alias(normalize_union(arms))
    }
}

fn unique_identifier(base: String, used: &mut BTreeSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}_{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Explicit, usable `title` for a generated declaration.
///
/// Titles are preferred over `$defs` keys and parent/field hints, but a title that is not a legal
/// generated identifier or that collides with a reserved symbol is ignored.
fn usable_schema_title(obj: &serde_json::Map<String, Value>) -> Option<&str> {
    obj.get("title")
        .and_then(Value::as_str)
        .filter(|title| is_valid_identifier(title) && !is_reserved_symbol(title))
}

fn nested_name(obj: &serde_json::Map<String, Value>, hint: Option<(&str, &str)>) -> Result<String> {
    if let Some(title) = usable_schema_title(obj) {
        return Ok(title.to_string());
    }
    let Some((parent, field)) = hint else {
        return Err(BamlBuilderError::InvalidArgument(
            "inline schema has no valid title and no parent/field hint for a generated name"
                .to_string(),
        ));
    };
    let candidate = format!("{parent}{}", to_pascal_case(field));
    Ok(if is_valid_identifier(&candidate) {
        candidate
    } else {
        sanitize_identifier(&candidate)
    })
}

/// Stable identifier suffix derived from a qualified tool name (`support/calculate` ->
/// `SupportCalculate`).
fn tool_qualifier(tool: &baml_rt_tools::tools::ToolName) -> String {
    let mut out = String::new();
    let mut capitalize = true;
    for ch in tool.to_string().chars() {
        if ch.is_ascii_alphanumeric() {
            if capitalize {
                out.extend(ch.to_uppercase());
                capitalize = false;
            } else {
                out.push(ch);
            }
        } else {
            capitalize = true;
        }
    }
    out
}

/// Stable identifier suffix derived from a JSON pointer path (`/$defs/Foo` -> `DefsFoo`).
fn path_qualifier(json_pointer: &str) -> String {
    if json_pointer.is_empty() {
        return String::new();
    }
    let cleaned: String = json_pointer
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    to_pascal_case(&cleaned)
}

/// Short deterministic digest of a fingerprint, used as a final symbol-qualification fallback.
fn short_digest(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let full = format!("{hash:016x}");
    full[..8].to_string()
}

fn canonical_id(
    mut id: SchemaTypeId,
    aliases: &BTreeMap<SchemaTypeId, SchemaTypeId>,
) -> SchemaTypeId {
    while let Some(next) = aliases.get(&id) {
        id = *next;
    }
    id
}

fn rewrite_kind_ids(
    kind: &mut SchemaDeclarationKind,
    aliases: &BTreeMap<SchemaTypeId, SchemaTypeId>,
) {
    match kind {
        SchemaDeclarationKind::Object(object) => {
            for field in &mut object.fields {
                rewrite_type_ref_ids(&mut field.ty, aliases);
            }
            if let AdditionalProperties::Typed(value) = &mut object.additional_properties {
                rewrite_type_ref_ids(value, aliases);
            }
        }
        SchemaDeclarationKind::Enum(_) => {}
        SchemaDeclarationKind::Alias(ty) => rewrite_type_ref_ids(ty, aliases),
    }
}

fn rewrite_type_ref_ids(ty: &mut SchemaTypeRef, aliases: &BTreeMap<SchemaTypeId, SchemaTypeId>) {
    match ty {
        SchemaTypeRef::Named(id) => *id = canonical_id(*id, aliases),
        SchemaTypeRef::Array(inner)
        | SchemaTypeRef::Map(inner)
        | SchemaTypeRef::VecOrOne(inner) => rewrite_type_ref_ids(inner, aliases),
        SchemaTypeRef::Union(arms) => {
            for arm in arms {
                rewrite_type_ref_ids(arm, aliases);
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

/// Compute canonical fingerprints that identify declarations by structure, not allocation id.
///
/// References are replaced by the referenced declaration's canonical fingerprint. Cyclic
/// components use a small Weisfeiler-Lehman-style color refinement so equivalent recursive
/// definitions from different schema documents still compare equal.
fn canonical_fingerprints(
    declarations: &BTreeMap<SchemaTypeId, SchemaDeclaration>,
) -> BTreeMap<SchemaTypeId, String> {
    let nodes: Vec<SchemaTypeId> = declarations.keys().copied().collect();
    let edges: BTreeMap<SchemaTypeId, BTreeSet<SchemaTypeId>> = declarations
        .iter()
        .map(|(id, declaration)| (*id, declaration.dependencies.clone()))
        .collect();
    let mut components = super::cycles::strongly_connected_components(&nodes, &edges);
    // Kosaraju returns source components before their dependencies; reverse for dependency-first
    // processing so every external reference already has a canonical fingerprint.
    components.reverse();

    let mut fingerprints: BTreeMap<SchemaTypeId, String> = BTreeMap::new();
    for component in components {
        let members: BTreeSet<SchemaTypeId> = component.iter().copied().collect();
        let self_recursive = component.len() == 1
            && edges
                .get(&component[0])
                .is_some_and(|dependencies| dependencies.contains(&component[0]));

        if component.len() == 1 && !self_recursive {
            let id = component[0];
            let fingerprint = fingerprint_of_kind_with(&declarations[&id].kind, |target| {
                fingerprints
                    .get(&target)
                    .cloned()
                    .unwrap_or_else(|| format!("ref:{}", target.as_u32()))
            });
            fingerprints.insert(id, fingerprint);
            continue;
        }

        // Seed each node with its local shape. Internal references start as an opaque marker and
        // are refined from neighbouring colors below.
        let mut colors: BTreeMap<SchemaTypeId, String> = component
            .iter()
            .map(|id| {
                let fingerprint = fingerprint_of_kind_with(&declarations[id].kind, |target| {
                    if members.contains(&target) {
                        "@internal".to_string()
                    } else {
                        fingerprints
                            .get(&target)
                            .cloned()
                            .unwrap_or_else(|| format!("ref:{}", target.as_u32()))
                    }
                });
                (*id, canonical_digest(&fingerprint))
            })
            .collect();

        for _ in 0..=component.len() {
            let next: BTreeMap<SchemaTypeId, String> = component
                .iter()
                .map(|id| {
                    let fingerprint = fingerprint_of_kind_with(&declarations[id].kind, |target| {
                        if members.contains(&target) {
                            format!("@color:{}", colors[&target])
                        } else {
                            fingerprints
                                .get(&target)
                                .cloned()
                                .unwrap_or_else(|| format!("ref:{}", target.as_u32()))
                        }
                    });
                    (*id, canonical_digest(&fingerprint))
                })
                .collect();
            let stable = next == colors;
            colors = next;
            if stable {
                break;
            }
        }

        for id in component {
            fingerprints.insert(id, colors[&id].clone());
        }
    }

    fingerprints
}

fn canonical_digest(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

/// Deterministic structural fingerprint of a declaration kind (descriptions excluded).
fn fingerprint_of_kind(kind: &SchemaDeclarationKind) -> String {
    fingerprint_of_kind_with(kind, |id| format!("ref:{}", id.as_u32()))
}

fn fingerprint_of_kind_with<F>(kind: &SchemaDeclarationKind, mut reference: F) -> String
where
    F: FnMut(SchemaTypeId) -> String,
{
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
                        type_ref_fingerprint_with(&f.ty, &mut reference)
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
        SchemaDeclarationKind::Alias(t) => {
            format!("alias[{}]", type_ref_fingerprint_with(t, &mut reference))
        }
    }
}

fn type_ref_fingerprint(ty: &SchemaTypeRef) -> String {
    type_ref_fingerprint_with(ty, &mut |id| format!("ref:{}", id.as_u32()))
}

fn type_ref_fingerprint_with<F>(ty: &SchemaTypeRef, reference: &mut F) -> String
where
    F: FnMut(SchemaTypeId) -> String,
{
    match ty {
        SchemaTypeRef::String => "string".to_string(),
        SchemaTypeRef::Integer => "int".to_string(),
        SchemaTypeRef::Number => "float".to_string(),
        SchemaTypeRef::Boolean => "bool".to_string(),
        SchemaTypeRef::Null => "null".to_string(),
        SchemaTypeRef::JsonValue => "json".to_string(),
        SchemaTypeRef::Literal(l) => format!("lit:{l:?}"),
        SchemaTypeRef::Array(inner) => {
            format!("arr[{}]", type_ref_fingerprint_with(inner, reference))
        }
        SchemaTypeRef::Map(inner) => {
            format!("map[{}]", type_ref_fingerprint_with(inner, reference))
        }
        SchemaTypeRef::Named(id) => reference(*id),
        SchemaTypeRef::Union(arms) => {
            let parts: Vec<String> = arms
                .iter()
                .map(|arm| type_ref_fingerprint_with(arm, reference))
                .collect();
            format!("union[{}]", parts.join("|"))
        }
        SchemaTypeRef::VecOrOne(inner) => {
            format!(
                "vec_or_one[{}]",
                type_ref_fingerprint_with(inner, reference)
            )
        }
    }
}

/// Flatten and deduplicate union arms without dropping `Null`.
fn normalize_union(types: Vec<SchemaTypeRef>) -> SchemaTypeRef {
    let mut flattened = Vec::new();
    for ty in types {
        match ty {
            SchemaTypeRef::Union(arms) => flattened.extend(arms),
            other => flattened.push(other),
        }
    }

    let mut seen = BTreeSet::new();
    flattened.retain(|ty| seen.insert(type_ref_fingerprint(ty)));
    match flattened.len() {
        0 => SchemaTypeRef::Null,
        1 => flattened.remove(0),
        _ => SchemaTypeRef::Union(flattened),
    }
}
