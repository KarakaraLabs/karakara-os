// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Canonical tool schema graph.
//!
//! This module owns the renderer-neutral model produced by lowering each resolved tool's JSON
//! Schemas. Renderers (BAML today, TypeScript in a later change) consume the same graph so
//! static and imported tools stop drifting.
//!
//! The graph is a code-generation model, not a validator: the original JSON Schema remains
//! authoritative for runtime validation and stays on `ToolFunctionMetadata`.

pub mod baml;
pub mod diagnostics;
pub mod lower;
pub mod names;

use std::collections::{BTreeMap, BTreeSet};

use baml_rt_tools::tools::{ToolFunctionMetadata, ToolName};
use serde_json::Value;

use crate::builder::error::Result;

/// Stable internal identity for one schema declaration.
///
/// Renderers refer to declarations by `SchemaTypeId`, never by string, so symbol renaming cannot
/// leave stale string references behind. IDs are assigned from a sorted traversal and are not part
/// of any public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaTypeId(u32);

impl SchemaTypeId {
    pub(crate) fn new(n: u32) -> Self {
        Self(n)
    }

    /// Raw id, exposed only for diagnostics and snapshots.
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// Which of a tool's three schemas a declaration came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolSchemaRole {
    OpenInput,
    Input,
    Output,
}

impl ToolSchemaRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenInput => "open_input",
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

/// Where a schema (and therefore a declaration) came from. Included in every unsupported-schema,
/// unresolved-reference, and collision diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaLocation {
    /// Qualified tool name. `None` for the standalone compatibility path that lowers bare
    /// `(name, schema)` pairs.
    pub tool_name: Option<ToolName>,
    /// Tool schema role. `None` when `tool_name` is `None`.
    pub role: Option<ToolSchemaRole>,
    /// JSON Pointer (RFC 6901) into the owning schema, or `""` for a root.
    pub json_pointer: String,
}

impl std::fmt::Display for SchemaLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.tool_name, self.role) {
            (Some(tool), Some(role)) => {
                write!(f, "tool {tool} {}{}", role.as_str(), self.json_pointer)
            }
            _ => {
                if self.json_pointer.is_empty() {
                    write!(f, "<root schema>")
                } else {
                    write!(f, "{}", self.json_pointer)
                }
            }
        }
    }
}

/// The normalized schema graph for one build.
#[derive(Debug, Clone)]
pub struct ToolSchemaGraph {
    /// Per-tool root contracts, keyed by qualified tool name.
    pub tools: BTreeMap<String, ToolSchemaContract>,
    /// Every declaration, keyed by stable id.
    pub declarations: BTreeMap<SchemaTypeId, SchemaDeclaration>,
    /// Emitted symbol -> declaration id. The final name registry boundary.
    pub symbols: BTreeMap<String, SchemaTypeId>,
}

impl ToolSchemaGraph {
    /// Lower every resolved tool's schemas into one graph.
    ///
    /// Callers pass the already-resolved `ToolFunctionMetadata` inventory; this function does not
    /// resolve catalogs or imports again. Tools are sorted by qualified name before lowering.
    pub fn from_tools(metadata: &[ToolFunctionMetadata]) -> Result<Self> {
        let mut sorted: Vec<&ToolFunctionMetadata> = metadata.iter().collect();
        sorted.sort_by_key(|a| a.name.to_string());
        lower::build_from_tools(&sorted)
    }

    /// Lower a set of bare `(schema name, schema)` roots.
    ///
    /// This is the standalone entry point behind the `generate_baml_types_from_schemas`
    /// compatibility API. `type_names` maps a schema name to its requested BAML symbol; when a
    /// name is absent the schema name is used verbatim.
    pub fn from_root_schemas(
        schemas: &std::collections::HashMap<String, Value>,
        type_names: &std::collections::HashMap<String, String>,
    ) -> Result<Self> {
        lower::build_from_root_schemas(schemas, type_names)
    }
}

/// One tool's root DTO contract inside the graph.
#[derive(Debug, Clone)]
pub struct ToolSchemaContract {
    pub tool_name: String,
    pub class_name: String,
    pub open_input: OpenInputContract,
    pub input: SchemaTypeRef,
    pub output: SchemaTypeRef,
}

#[derive(Debug, Clone)]
pub enum OpenInputContract {
    /// Unit open input (`open_input_type.name == "()"`).
    Absent,
    Present {
        ty: SchemaTypeRef,
        requirement: OpenInputRequirement,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenInputRequirement {
    Optional,
    Required,
}

#[derive(Debug, Clone)]
pub struct SchemaDeclaration {
    pub id: SchemaTypeId,
    /// The symbol this declaration is emitted under.
    pub symbol: String,
    /// The symbol requested before collision/dedup resolution.
    pub preferred_symbol: String,
    pub kind: SchemaDeclarationKind,
    pub dependencies: BTreeSet<SchemaTypeId>,
    pub source: SchemaLocation,
    /// Deterministic structural fingerprint (excludes descriptions).
    pub fingerprint: String,
    /// Render order captured during lowering. Internal; renders emit declarations once in this
    /// order so inline nested declarations precede their owner, mirroring the historical renderer.
    pub(crate) order: u32,
}

#[derive(Debug, Clone)]
pub enum SchemaDeclarationKind {
    Object(SchemaObject),
    Enum(SchemaEnum),
    /// `type X = <inner>` alias (maps, unions, opaque JSON).
    Alias(SchemaTypeRef),
}

#[derive(Debug, Clone)]
pub struct SchemaObject {
    pub fields: Vec<SchemaField>,
    pub additional_properties: AdditionalProperties,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SchemaField {
    pub wire_name: String,
    pub preferred_identifier: String,
    pub ty: SchemaTypeRef,
    pub required: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AdditionalProperties {
    AllowedAny,
    Forbidden,
    Typed(Box<SchemaTypeRef>),
}

#[derive(Debug, Clone)]
pub struct SchemaEnum {
    pub variants: Vec<SchemaEnumVariant>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SchemaEnumVariant {
    pub wire_value: SchemaLiteral,
    pub preferred_identifier: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SchemaTypeRef {
    String,
    Integer,
    Number,
    Boolean,
    Null,
    /// Arbitrary JSON. BAML renders this as the `OpaqueJson` alias.
    JsonValue,
    Literal(SchemaLiteral),
    Array(Box<SchemaTypeRef>),
    Map(Box<SchemaTypeRef>),
    Named(SchemaTypeId),
    Union(Vec<SchemaTypeRef>),
    /// The `vec_or_one` wire shape `T | T[]`. Semantically a two-arm union; kept distinct so the
    /// BAML renderer can preserve the parenthesized spelling the historical renderers emit.
    VecOrOne(Box<SchemaTypeRef>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchemaLiteral {
    String(String),
    Integer(i64),
    /// Stable textual form for non-integral JSON numbers.
    Number(String),
    Boolean(bool),
    Null,
}
