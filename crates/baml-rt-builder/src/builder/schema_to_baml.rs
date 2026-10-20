// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON Schema to BAML type generation.
//!
//! Compatibility wrapper around the canonical [`crate::builder::tool_schema_graph`] lowering and
//! renderer. Callers pass a map of root schemas plus requested symbols; this function lowers them
//! once into a `ToolSchemaGraph` and renders that graph as BAML.

use std::collections::HashMap;

use serde_json::Value;

use super::tool_schema_graph::{ToolSchemaGraph, baml::render_tool_schema_baml};
use crate::builder::error::Result;

/// Generate BAML type definitions from JSON schemas.
///
/// `schemas` maps a schema name to its root JSON Schema document; `type_names` maps a schema name
/// to its requested BAML type name (falling back to the schema name itself).
pub fn generate_baml_types_from_schemas(
    schemas: &HashMap<String, Value>,
    type_names: &HashMap<String, String>,
) -> Result<String> {
    let graph = ToolSchemaGraph::from_root_schemas(schemas, type_names)?;
    render_tool_schema_baml(&graph)
}
