// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Structured TypeScript declarations shared by every codegen source.
//!
//! Graph, BAML IR, runtime, and step-executor renderers all produce [`TsDeclaration`] values so a
//! single owner can decide symbol conflicts and emission order. Rendering this module's types into
//! final file text is intentionally separate: renderers never coordinate by parsing TypeScript.

use std::collections::BTreeSet;

use super::tool_schema_graph::{SchemaLocation, SchemaOwner};

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
