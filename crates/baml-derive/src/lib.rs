// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Procedural derive macro for generating BAML type definitions from Rust types.
//!
//! # Usage
//!
//! ```rust,ignore
//! use baml_derive::BamlType;
//!
//! #[derive(BamlType)]
//! struct MyInput {
//!     pub name: String,
//!     pub count: Option<i32>,
//! }
//! ```
//!
//! See the `baml-derive-core` crate for the trait definition and rendering.

mod attrs;
mod expand;
mod resolve;
mod schema_resolve;
mod ts_resolve;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Unified derive macro that generates both `BamlType` and `TsType` implementations
/// for the annotated type.
///
/// # Supported Types
///
/// | Rust form | BAML output | TypeScript output |
/// |---|---|---|
/// | Struct with named fields | `class Foo { … }` | `export interface Foo { … }` |
/// | Enum with unit variants | `enum Foo { … }` | `export type Foo = "A" \| "B";` |
/// | Enum with `#[baml(union)]` + newtype variants | `type Foo = A \| B` | `export type Foo = A \| B;` |
///
/// # Container Attributes
///
/// - `#[baml(dynamic)]` — adds `@@dynamic` to the BAML class (BAML only)
/// - `#[baml(union)]` — generates a BAML union type alias and a TypeScript union
///
/// # Field Attributes
///
/// - `#[baml(alias = "...")]` — adds `@alias("...")` in BAML (BAML only; TypeScript uses the Rust field name)
/// - `#[baml(description = "...")]` — adds `@description("...")` in BAML (BAML only)
/// - `#[baml(skip)]` — omits the field from BAML, TypeScript, and the JSON Schema even if serde
///   still accepts it; use this to keep an internal value out of the model-facing surface
/// - `#[baml(type = "...")]` — overrides automatic BAML type resolution; TypeScript falls back to `any`
///
/// `#[serde(skip)]` and `#[serde(skip_deserializing)]` also omit a field automatically because
/// the generated schema describes what serde accepts on input.
///
/// # Variant Attributes (for unit enums)
///
/// - `#[baml(alias = "...")]` — adds `@alias("...")` in BAML (BAML only)
/// - `#[baml(description = "...")]` — adds `@description("...")` in BAML (BAML only)
/// - `#[baml(skip)]` — omits the variant from BAML, TypeScript, and the JSON Schema even if serde
///   still accepts it
///
/// `#[serde(skip)]` and `#[serde(skip_deserializing)]` on a unit variant also remove it from all
/// three generated surfaces. `#[serde(skip_serializing)]` does not, because the variant is still
/// valid input and the derived schema describes the input contract.
///
/// # Serde wire attributes
///
/// The generated `JsonSchemaType` follows the supported serde input contract:
/// container `rename_all`, field and variant `rename`, `Option<T>` / `default` omission,
/// `deny_unknown_fields`, and `skip` / `skip_deserializing`. Unsupported forms (`flatten`,
/// tagged or untagged non-union enums, asymmetric renames, custom deserializers without a
/// BAML wire annotation) fail the derive. See `JsonSchemaType` for details.
#[proc_macro_derive(BamlType, attributes(baml))]
pub fn derive_baml_type(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match expand::expand_derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
