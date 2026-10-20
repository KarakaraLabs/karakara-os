// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Parsing of `#[baml(...)]` attributes on structs, enums, fields, and variants.

use syn::{Attribute, Expr, ExprLit, Lit, Meta, MetaNameValue};

/// Container-level (struct/enum) attributes from `#[baml(...)]`.
#[derive(Debug, Default)]
pub(crate) struct ContainerAttrs {
    /// `#[baml(dynamic)]` → BAML `@@dynamic`
    pub dynamic: bool,
    /// `#[baml(union)]` → generate `type Foo = A | B` instead of `enum Foo`
    pub union: bool,
}

/// Field-level attributes from `#[baml(...)]`.
#[derive(Debug, Default)]
pub(crate) struct FieldAttrs {
    /// `#[baml(alias = "...")]`
    pub alias: Option<String>,
    /// `#[baml(description = "...")]`
    pub description: Option<String>,
    /// `#[baml(skip)]`
    pub skip: bool,
    /// `#[baml(type = "...")]` — escape hatch for explicit BAML type override.
    pub type_override: Option<String>,
    /// `#[baml(ts_type = "...")]` — override for the emitted TypeScript type.
    ///
    /// Use this when the automatic Rust→TypeScript mapping is insufficient, e.g.:
    /// ```rust,ignore
    /// #[baml(ts_type = "Record<string, unknown> | null")]
    /// pub payload: Option<BTreeMap<String, serde_json::Value>>,
    /// ```
    pub ts_type: Option<String>,
    /// `#[baml(vec_or_one)]` — field is `Vec<T>` or `Option<Vec<T>>` but wire JSON may be one `T` or
    /// `T[]`. JSON Schema and TS use `T | T[]` (and `| null` when optional); pair with
    /// `serde_one_or_many` + `deserialize_with` on the Rust field.
    pub vec_or_one: bool,
}

/// Container-level `#[serde(...)]` attributes that change the JSON wire shape.
#[derive(Debug, Default)]
pub(crate) struct SerdeContainerWire {
    /// `#[serde(rename_all = "...")]` applied to struct fields or enum variants.
    pub rename_all: Option<String>,
    /// `#[serde(deny_unknown_fields)]` → `"additionalProperties": false`.
    pub deny_unknown_fields: bool,
    /// Container-level `#[serde(default)]` accepts omitted fields.
    pub default: bool,
    /// `#[serde(untagged)]`.
    pub untagged: bool,
    /// `#[serde(tag = "...")]` (internally tagged).
    pub tag: Option<String>,
    /// `#[serde(content = "...")]` (adjacently tagged with `tag`).
    pub content: Option<String>,
}

/// Field-level `#[serde(...)]` attributes that change the JSON wire shape.
#[derive(Debug, Default)]
pub(crate) struct SerdeFieldWire {
    /// Input wire name from `#[serde(rename = "...")]` or the `deserialize` side of an
    /// asymmetric rename.
    pub rename: Option<String>,
    /// `#[serde(default)]` or `#[serde(default = "...")]` accepts an omitted field.
    pub default: bool,
    /// `#[serde(skip)]` or `#[serde(skip_deserializing)]` removes the field from input.
    pub skip: bool,
    /// `#[serde(flatten)]` inlines another object; the derive cannot model it faithfully.
    pub flatten: bool,
    /// `#[serde(with = "...")]` or `#[serde(deserialize_with = "...")]`.
    pub custom_deserializer: bool,
    /// `#[serde(rename(serialize = ..., deserialize = ...))]` with differing names.
    pub asymmetric_rename: bool,
}

/// Variant-level `#[serde(...)]` attributes that change the JSON wire shape.
#[derive(Debug, Default)]
pub(crate) struct SerdeVariantWire {
    /// Input wire name from `#[serde(rename = "...")]` or the deserialize side of a
    /// symmetric split rename.
    pub rename: Option<String>,
    /// `#[serde(skip)]` or `#[serde(skip_deserializing)]` removes the variant from input.
    pub skip: bool,
}

/// Variant-level attributes from `#[baml(...)]`.
#[derive(Debug, Default)]
pub(crate) struct VariantAttrs {
    /// `#[baml(alias = "...")]`
    pub alias: Option<String>,
    /// `#[baml(description = "...")]`
    pub description: Option<String>,
    /// `#[baml(skip)]`
    pub skip: bool,
}

/// Parse container-level `#[baml(...)]` attributes.
pub(crate) fn parse_container_attrs(attrs: &[Attribute]) -> syn::Result<ContainerAttrs> {
    let mut result = ContainerAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("baml") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("dynamic") {
                result.dynamic = true;
                return Ok(());
            }
            if meta.path.is_ident("union") {
                result.union = true;
                return Ok(());
            }
            Err(meta.error("unknown baml container attribute; expected `dynamic` or `union`"))
        })?;
    }

    Ok(result)
}

/// Parse field-level `#[baml(...)]` attributes.
pub(crate) fn parse_field_attrs(attrs: &[Attribute]) -> syn::Result<FieldAttrs> {
    let mut result = FieldAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("baml") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                result.skip = true;
                return Ok(());
            }
            if meta.path.is_ident("alias") {
                result.alias = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("description") {
                result.description = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("type") {
                result.type_override = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("ts_type") {
                result.ts_type = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("vec_or_one") {
                result.vec_or_one = true;
                return Ok(());
            }
            Err(meta.error(
                "unknown baml field attribute; expected `alias`, `description`, `skip`, `type`, `ts_type`, or `vec_or_one`",
            ))
        })?;
    }

    Ok(result)
}

/// Parse variant-level `#[baml(...)]` attributes.
pub(crate) fn parse_variant_attrs(attrs: &[Attribute]) -> syn::Result<VariantAttrs> {
    let mut result = VariantAttrs::default();

    for attr in attrs {
        if !attr.path().is_ident("baml") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("skip") {
                result.skip = true;
                return Ok(());
            }
            if meta.path.is_ident("alias") {
                result.alias = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("description") {
                result.description = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            Err(meta.error(
                "unknown baml variant attribute; expected `alias`, `description`, or `skip`",
            ))
        })?;
    }

    Ok(result)
}

/// Extract the doc string from `/// ...` doc-comment attributes.
pub(crate) fn extract_doc_comment(attrs: &[Attribute]) -> Option<String> {
    let mut lines = Vec::new();

    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(MetaNameValue {
            value: Expr::Lit(ExprLit {
                lit: Lit::Str(lit), ..
            }),
            ..
        }) = &attr.meta
        {
            lines.push(lit.value());
        }
    }

    if lines.is_empty() {
        return None;
    }

    // Doc attributes typically have a leading space: `/// foo` → `" foo"`.
    // We trim each line.
    let doc = lines
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");

    Some(doc)
}

/// Helper: parse `= "value"` from a name-value meta item.
fn parse_string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let value = meta.value()?;
    let lit: syn::LitStr = value.parse()?;
    Ok(lit.value())
}

/// Parse container-level `#[serde(...)]` attributes that affect the generated schema.
pub(crate) fn parse_serde_container_wire(attrs: &[Attribute]) -> syn::Result<SerdeContainerWire> {
    let mut result = SerdeContainerWire::default();

    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                if meta.input.peek(syn::Token![=]) {
                    result.rename_all = Some(parse_string_value(&meta)?);
                } else {
                    // `rename_all(serialize = ..., deserialize = ...)`: only the symmetric form is
                    // representable by one JsonSchemaType. A differing input/output rule would
                    // silently emit the wrong input wire names, so reject it.
                    let (rename, asymmetric) = parse_split_rename(&meta)?;
                    if asymmetric {
                        return Err(meta.error(
                            "asymmetric `rename_all(serialize = ..., deserialize = ...)` cannot be represented by one JsonSchemaType; implement JsonSchemaType manually",
                        ));
                    }
                    result.rename_all = rename;
                }
                return Ok(());
            }
            if meta.path.is_ident("deny_unknown_fields") {
                result.deny_unknown_fields = true;
                return Ok(());
            }
            if meta.path.is_ident("default") {
                result.default = true;
                consume_optional_value(&meta)?;
                return Ok(());
            }
            if meta.path.is_ident("untagged") {
                result.untagged = true;
                return Ok(());
            }
            if meta.path.is_ident("tag") {
                result.tag = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            if meta.path.is_ident("content") {
                result.content = Some(parse_string_value(&meta)?);
                return Ok(());
            }
            consume_unrecognized(&meta)
        })?;
    }

    Ok(result)
}

/// Parse field-level `#[serde(...)]` attributes that affect the generated schema.
pub(crate) fn parse_serde_field_wire(attrs: &[Attribute]) -> syn::Result<SerdeFieldWire> {
    let mut result = SerdeFieldWire::default();

    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                if meta.input.peek(syn::token::Paren) {
                    let (rename, asymmetric) = parse_split_rename(&meta)?;
                    result.rename = rename;
                    result.asymmetric_rename |= asymmetric;
                } else {
                    result.rename = Some(parse_string_value(&meta)?);
                }
                return Ok(());
            }
            if meta.path.is_ident("alias") {
                // Alias spellings stay accepted by serde but are not enumerated in the
                // schema; the schema lists the canonical wire name. Consume and continue.
                let _ = parse_string_value(&meta)?;
                return Ok(());
            }
            if meta.path.is_ident("default") {
                result.default = true;
                consume_optional_value(&meta)?;
                return Ok(());
            }
            if meta.path.is_ident("skip") || meta.path.is_ident("skip_deserializing") {
                result.skip = true;
                return Ok(());
            }
            if meta.path.is_ident("flatten") {
                result.flatten = true;
                return Ok(());
            }
            if meta.path.is_ident("with") || meta.path.is_ident("deserialize_with") {
                result.custom_deserializer = true;
                consume_optional_value(&meta)?;
                return Ok(());
            }
            consume_unrecognized(&meta)
        })?;
    }

    Ok(result)
}

/// Parse `rename(serialize = "...", deserialize = "...")`.
///
/// Returns the input-side name and whether the two sides disagree.
fn parse_split_rename(
    meta: &syn::meta::ParseNestedMeta<'_>,
) -> syn::Result<(Option<String>, bool)> {
    let content;
    syn::parenthesized!(content in meta.input);

    let mut serialize = None;
    let mut deserialize = None;
    while !content.is_empty() {
        let key: syn::Ident = content.parse()?;
        content.parse::<syn::Token![=]>()?;
        let value: syn::LitStr = content.parse()?;
        match key.to_string().as_str() {
            "serialize" => serialize = Some(value.value()),
            "deserialize" => deserialize = Some(value.value()),
            _ => {}
        }
        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        }
    }

    let asymmetric = match (&serialize, &deserialize) {
        (Some(serialize), Some(deserialize)) => serialize != deserialize,
        // A one-sided rename is asymmetric: the input and output spellings differ.
        (Some(_), None) | (None, Some(_)) => true,
        (None, None) => false,
    };

    Ok((deserialize.or(serialize), asymmetric))
}

/// Consume an optional `= <expr>` value so nested serde parsing can continue.
fn consume_optional_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        meta.value()?.parse::<syn::Expr>()?;
    }
    Ok(())
}

/// Consume an unrecognized serde attribute, including `= value` and `( ... )` forms.
fn consume_unrecognized(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        meta.value()?.parse::<syn::Expr>()?;
    } else if meta.input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in meta.input);
        let _: proc_macro2::TokenStream = content.parse()?;
    }
    Ok(())
}

/// Parse variant-level `#[serde(...)]` attributes that affect the generated schema.
pub(crate) fn parse_serde_variant_wire(attrs: &[Attribute]) -> syn::Result<SerdeVariantWire> {
    let mut result = SerdeVariantWire::default();

    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                if meta.input.peek(syn::token::Paren) {
                    let (rename, asymmetric) = parse_split_rename(&meta)?;
                    if asymmetric {
                        return Err(meta.error(
                            "asymmetric serde variant renames cannot be represented by one JsonSchemaType; implement JsonSchemaType manually",
                        ));
                    }
                    result.rename = rename;
                } else {
                    result.rename = Some(parse_string_value(&meta)?);
                }
                return Ok(());
            }
            if meta.path.is_ident("alias") {
                // Alias spellings stay accepted by serde but are not enumerated in the
                // schema; the schema lists the canonical wire name. Consume and continue.
                let _ = parse_string_value(&meta)?;
                return Ok(());
            }
            if meta.path.is_ident("skip") || meta.path.is_ident("skip_deserializing") {
                result.skip = true;
                return Ok(());
            }
            consume_unrecognized(&meta)
        })?;
    }

    Ok(result)
}

/// Apply Serde's `rename_all` rules to a snake_case Rust struct field identifier.
pub(crate) fn serde_rename_all_field(ident: &str, rule: &str) -> String {
    match rule {
        "lowercase" => ident.to_lowercase(),
        "UPPERCASE" => ident.to_uppercase(),
        "PascalCase" => snake_to_pascal(ident),
        "camelCase" => snake_to_camel(ident),
        "snake_case" => ident.to_string(),
        "SCREAMING_SNAKE_CASE" => ident.to_uppercase(),
        "kebab-case" => ident.replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => ident.to_uppercase().replace('_', "-"),
        _ => ident.to_string(),
    }
}

fn snake_to_pascal(ident: &str) -> String {
    ident
        .split('_')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn snake_to_camel(ident: &str) -> String {
    let pascal = snake_to_pascal(ident);
    let mut chars = pascal.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Apply Serde's `rename_all` rules to a Rust enum variant identifier (Serde 1.x container attr).
pub(crate) fn serde_rename_all_variant(ident: &str, rule: &str) -> String {
    match rule {
        "lowercase" => ident.to_lowercase(),
        "UPPERCASE" => ident.to_uppercase(),
        "PascalCase" => ident.to_string(),
        "camelCase" => to_camel_case(ident),
        "snake_case" => to_snake_case(ident),
        "SCREAMING_SNAKE_CASE" => to_snake_case(ident).to_uppercase(),
        "kebab-case" => to_snake_case(ident).replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => to_snake_case(ident).to_uppercase().replace('_', "-"),
        _ => ident.to_string(),
    }
}

fn to_camel_case(ident: &str) -> String {
    let mut c = ident.chars();
    let Some(first) = c.next() else {
        return String::new();
    };
    first.to_lowercase().collect::<String>() + c.as_str()
}

fn to_snake_case(ident: &str) -> String {
    let mut out = String::new();
    for (i, ch) in ident.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{parse_serde_container_wire, parse_serde_variant_wire};

    #[test]
    fn rejects_asymmetric_container_rename_all() {
        let attr: syn::Attribute = syn::parse_quote!(
            #[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
        );
        let error = parse_serde_container_wire(&[attr]).expect_err("asymmetric rename_all");
        assert!(error.to_string().contains("asymmetric `rename_all"));
    }

    #[test]
    fn accepts_symmetric_container_rename_all() {
        let attr: syn::Attribute = syn::parse_quote!(
            #[serde(rename_all(serialize = "camelCase", deserialize = "camelCase"))]
        );
        let parsed = parse_serde_container_wire(&[attr]).expect("symmetric rename_all");
        assert_eq!(parsed.rename_all.as_deref(), Some("camelCase"));
    }

    #[test]
    fn rejects_asymmetric_variant_rename() {
        let attrs: [syn::Attribute; 3] = [
            syn::parse_quote!(#[serde(rename(serialize = "out", deserialize = "in"))]),
            syn::parse_quote!(#[serde(rename(serialize = "out"))]),
            syn::parse_quote!(#[serde(rename(deserialize = "in"))]),
        ];
        for attr in attrs {
            let error = parse_serde_variant_wire(&[attr]).expect_err("asymmetric variant rename");
            assert!(
                error
                    .to_string()
                    .contains("asymmetric serde variant renames"),
                "unexpected diagnostic: {error}"
            );
        }
    }

    #[test]
    fn accepts_symmetric_variant_rename() {
        let attr: syn::Attribute = syn::parse_quote!(
            #[serde(rename(serialize = "same", deserialize = "same"))]
        );
        let parsed = parse_serde_variant_wire(&[attr]).expect("symmetric variant rename");
        assert_eq!(parsed.rename.as_deref(), Some("same"));
    }

    #[test]
    fn parses_variant_input_skips() {
        let skip: syn::Attribute = syn::parse_quote!(#[serde(skip)]);
        let skip_deserializing: syn::Attribute = syn::parse_quote!(#[serde(skip_deserializing)]);
        let skip_serializing: syn::Attribute = syn::parse_quote!(#[serde(skip_serializing)]);

        assert!(parse_serde_variant_wire(&[skip]).expect("skip").skip);
        assert!(
            parse_serde_variant_wire(&[skip_deserializing])
                .expect("skip_deserializing")
                .skip
        );
        assert!(
            !parse_serde_variant_wire(&[skip_serializing])
                .expect("skip_serializing")
                .skip,
            "skip_serializing variants remain valid input"
        );
    }
}
