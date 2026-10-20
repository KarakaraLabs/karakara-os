// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Structured TypeScript declarations shared by every codegen source.
//!
//! Graph, BAML IR, runtime, and step-executor renderers all produce [`TsDeclaration`] values so a
//! single owner can decide symbol conflicts and emission order. Renderers never coordinate by
//! parsing already-rendered TypeScript.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    error::{BamlBuilderError, Result},
    tool_schema_graph::{SchemaLocation, SchemaOwner, ToolSchemaRole},
};

/// One TypeScript declaration and the ownership metadata needed to merge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsDeclaration {
    /// The symbol this declaration defines.
    pub symbol: String,
    /// Complete declaration text (including `export`), without a trailing newline.
    pub body: String,
    /// Symbols this declaration references. Used for diagnostics and ordering evidence.
    pub dependencies: BTreeSet<String>,
    /// Which codegen source owns the declaration.
    pub origin: TsDeclarationOrigin,
}

/// Ownership class for a [`TsDeclaration`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TsDeclarationOrigin {
    /// Hand-written declarations owned by the runtime (`baml-rt-tools::ts_gen`).
    Runtime,
    /// A DTO or alias rendered from the canonical [`super::tool_schema_graph::ToolSchemaGraph`].
    ToolSchema {
        owners: BTreeSet<SchemaOwner>,
        source: SchemaLocation,
    },
    /// A class, enum, or alias collected from BAML IR.
    BamlIr,
    /// A declaration owned by generated step-executor support.
    StepExecutor,
}

impl TsDeclarationOrigin {
    /// Short label used in conflict diagnostics.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::ToolSchema { .. } => "tool schema graph",
            Self::BamlIr => "BAML IR",
            Self::StepExecutor => "step executor",
        }
    }
}

/// One registry owns the final symbol set across runtime, graph, IR, and step-executor sources.
///
/// Runtime symbols are reserved because their bodies are emitted by the static runtime block.
/// Graph declarations are submitted first; an IR copy of a graph-owned symbol is suppressed by
/// ownership, not by comparing rendered text or by a hard-coded skip list.
#[derive(Debug, Default)]
pub struct TsDeclarationRegistry {
    declarations: BTreeMap<String, TsDeclaration>,
    reserved: BTreeMap<String, TsDeclarationOrigin>,
}

impl TsDeclarationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve a symbol whose body is emitted outside the registry.
    pub fn reserve(&mut self, symbol: &str, origin: TsDeclarationOrigin) -> Result<()> {
        if let Some(existing) = self.declarations.get(symbol) {
            return Err(conflict_error(symbol, existing.origin.clone(), origin));
        }
        if let Some(existing) = self.reserved.get(symbol) {
            if existing == &origin {
                return Ok(());
            }
            return Err(conflict_error(symbol, existing.clone(), origin));
        }
        self.reserved.insert(symbol.to_string(), origin);
        Ok(())
    }

    /// Reserve every symbol exported by the static runtime TypeScript block.
    pub fn reserve_runtime_symbols(&mut self, symbols: &[&str]) -> Result<()> {
        for symbol in symbols {
            self.reserve(symbol, TsDeclarationOrigin::Runtime)?;
        }
        Ok(())
    }

    /// Submit one declaration, resolving ownership and duplicates.
    ///
    /// Returns an error when two incompatible bodies claim the same symbol. A graph declaration
    /// always wins over the BAML IR copy of the same DTO, and the runtime block wins over an IR
    /// copy of a runtime-authored symbol.
    pub fn insert(&mut self, declaration: TsDeclaration) -> Result<()> {
        let symbol = declaration.symbol.clone();

        if let Some(reserved_origin) = self.reserved.get(&symbol).cloned() {
            if matches!(declaration.origin, TsDeclarationOrigin::BamlIr) {
                // BAML IR carries copies of runtime-authored declarations (for example
                // `StructuredReply`). The runtime block owns them.
                return Ok(());
            }
            return Err(conflict_error(&symbol, reserved_origin, declaration.origin));
        }

        let Some(existing) = self.declarations.get(&symbol).cloned() else {
            self.declarations.insert(symbol, declaration);
            return Ok(());
        };

        if existing.body == declaration.body {
            return Ok(());
        }
        if matches!(existing.origin, TsDeclarationOrigin::ToolSchema { .. })
            && matches!(declaration.origin, TsDeclarationOrigin::BamlIr)
        {
            // Graph-owned DTOs are emitted exactly once, even when the legacy BAML declaration
            // renders a different body.
            return Ok(());
        }
        if matches!(existing.origin, TsDeclarationOrigin::BamlIr)
            && matches!(declaration.origin, TsDeclarationOrigin::ToolSchema { .. })
        {
            // Graph declarations normally arrive first; accept them in either order.
            self.declarations.insert(symbol, declaration);
            return Ok(());
        }

        Err(conflict_error(&symbol, existing.origin, declaration.origin))
    }

    /// Whether a symbol is owned by the static runtime block.
    pub fn is_reserved(&self, symbol: &str) -> bool {
        self.reserved.contains_key(symbol)
    }

    pub fn get(&self, symbol: &str) -> Option<&TsDeclaration> {
        self.declarations.get(symbol)
    }

    pub fn iter(&self) -> impl Iterator<Item = &TsDeclaration> {
        self.declarations.values()
    }

    pub fn len(&self) -> usize {
        self.declarations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    /// Render accepted declarations in stable lexical symbol order.
    pub fn render(&self) -> Result<String> {
        let mut output = String::new();
        for (index, declaration) in self.declarations.values().enumerate() {
            if index > 0 {
                output.push('\n');
            }
            output.push_str(declaration.body.trim_end());
            output.push('\n');
        }
        Ok(output)
    }
}

fn conflict_error(
    symbol: &str,
    left: TsDeclarationOrigin,
    right: TsDeclarationOrigin,
) -> BamlBuilderError {
    BamlBuilderError::InvalidArgument(format!(
        "conflicting TypeScript declaration `{symbol}`: {} vs {}",
        describe_origin(&left),
        describe_origin(&right)
    ))
}

fn describe_origin(origin: &TsDeclarationOrigin) -> String {
    match origin {
        TsDeclarationOrigin::Runtime => "runtime block".to_string(),
        TsDeclarationOrigin::BamlIr => "BAML IR".to_string(),
        TsDeclarationOrigin::StepExecutor => "generated step executor".to_string(),
        TsDeclarationOrigin::ToolSchema { owners, source } => {
            let owners: Vec<String> = owners.iter().map(describe_owner).collect();
            format!("tool schema graph [{}] at {source}", owners.join(", "))
        }
    }
}

fn describe_owner(owner: &SchemaOwner) -> String {
    match owner {
        SchemaOwner::PublicRoot { tool_name, role } => {
            format!("public root {tool_name} {}", role.as_str())
        }
        SchemaOwner::StandaloneRoot { schema_name } => {
            format!("standalone root {schema_name}")
        }
        SchemaOwner::Nested {
            document,
            tool_name,
            role,
            path,
        } => format!(
            "nested {document} {} {} /{}",
            tool_label(tool_name.as_deref()),
            role_label(*role),
            path.join("/")
        ),
        SchemaOwner::SharedDefinition {
            document,
            tool_name,
            role,
            original_name,
            json_pointer,
        } => format!(
            "definition {document} {} {} {original_name} {json_pointer}",
            tool_label(tool_name.as_deref()),
            role_label(*role)
        ),
    }
}

fn role_label(role: Option<ToolSchemaRole>) -> &'static str {
    role.map(ToolSchemaRole::as_str).unwrap_or("<none>")
}

fn tool_label(tool: Option<&str>) -> &str {
    tool.unwrap_or("<standalone>")
}
