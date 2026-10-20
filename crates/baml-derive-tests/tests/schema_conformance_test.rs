// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conformance matrix for the serde wire contract emitted by `#[derive(BamlType)]`.
//!
//! Each fixture is serialized, then validated against `root_json_schema::<T>()`
//! (the same document `json_schema_value::<T>()` produces). The matrix covers the
//! supported serde inputs from the tool-schema-graph plan: `rename_all`, field
//! `rename`, `Option`/`default` omission, `deny_unknown_fields`, enum wire values,
//! and an `anyOf` union.

use baml_derive::BamlType;
use baml_derive_core::{JsonSchemaType, root_json_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConformanceRequest {
    request_id: String,
    #[serde(rename = "payloadKind")]
    kind: ConformanceKind,
    #[serde(default)]
    retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    tags: Vec<String>,
    #[serde(skip)]
    internal_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "snake_case")]
enum ConformanceKind {
    FastPath,
    SlowPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConformanceRead {
    resource_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, BamlType)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct ConformanceWrite {
    resource_id: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[baml(union)]
#[serde(untagged)]
enum ConformanceToolCall {
    Read(ConformanceRead),
    Write(ConformanceWrite),
}

fn compiled<T: JsonSchemaType>() -> jsonschema::JSONSchema {
    jsonschema::JSONSchema::compile(&root_json_schema::<T>())
        .unwrap_or_else(|error| panic!("derived schema should compile: {error}"))
}

fn assert_wire_and_schema<T: JsonSchemaType + Serialize>(
    label: &str,
    value: &T,
    expected_wire: Value,
) -> Value {
    let instance = serde_json::to_value(value).expect("fixture serializes");
    assert_eq!(instance, expected_wire, "{label}: canonical wire shape");
    if let Err(errors) = compiled::<T>().validate(&instance) {
        let messages = errors.map(|error| error.to_string()).collect::<Vec<_>>();
        panic!("{label}: serialized value must validate against its schema: {messages:?}");
    }
    instance
}

#[test]
fn static_dto_schema_conformance_matrix() {
    let request = ConformanceRequest {
        request_id: "req-1".to_string(),
        kind: ConformanceKind::FastPath,
        retries: 0,
        note: None,
        tags: vec!["alpha".to_string()],
        internal_note: "not on the wire".to_string(),
    };
    assert_eq!(request.internal_note, "not on the wire");
    assert_wire_and_schema(
        "rename_all + field rename + enum wire value",
        &request,
        json!({
            "requestId": "req-1",
            "payloadKind": "fast_path",
            "retries": 0,
            "tags": ["alpha"],
        }),
    );

    let request_schema = root_json_schema::<ConformanceRequest>();
    assert_eq!(request_schema["title"], "ConformanceRequest");
    assert_eq!(request_schema["additionalProperties"], false);
    assert!(request_schema["properties"].get("requestId").is_some());
    assert!(request_schema["properties"].get("payloadKind").is_some());
    assert!(request_schema["properties"].get("request_id").is_none());
    assert!(
        request_schema["properties"].get("internal_note").is_none(),
        "`#[serde(skip)]` fields are absent from the input schema"
    );
    let required = request_schema["required"]
        .as_array()
        .expect("required array");
    assert!(required.iter().any(|field| field == "requestId"));
    assert!(
        !required.iter().any(|field| field == "retries"),
        "`#[serde(default)]` fields may be omitted"
    );
    assert!(
        !required.iter().any(|field| field == "note"),
        "`Option<T>` fields may be omitted"
    );

    let kind_schema = root_json_schema::<ConformanceKind>();
    assert_eq!(kind_schema["title"], "ConformanceKind");
    assert_eq!(kind_schema["enum"], json!(["fast_path", "slow_path"]));

    let request_validator = compiled::<ConformanceRequest>();
    let omitted_defaults = json!({
        "requestId": "req-2",
        "payloadKind": "slow_path",
        "tags": [],
    });
    assert!(
        request_validator.validate(&omitted_defaults).is_ok(),
        "omitted `default` and `Option` fields must validate"
    );
    let parsed: ConformanceRequest =
        serde_json::from_value(omitted_defaults).expect("serde accepts omitted fields");
    assert_eq!(parsed.retries, 0, "default value fills the omitted field");
    assert!(parsed.note.is_none(), "omitted Option stays None");

    let unknown_field = json!({
        "requestId": "req-3",
        "payloadKind": "fast_path",
        "tags": [],
        "unexpected": true,
    });
    assert!(
        request_validator.validate(&unknown_field).is_err(),
        "deny_unknown_fields emits additionalProperties: false"
    );
    assert!(
        serde_json::from_value::<ConformanceRequest>(unknown_field).is_err(),
        "serde deny_unknown_fields rejects unknown keys"
    );

    assert_wire_and_schema(
        "union read variant",
        &ConformanceToolCall::Read(ConformanceRead {
            resource_id: "res-1".to_string(),
        }),
        json!({ "resourceId": "res-1" }),
    );
    assert_wire_and_schema(
        "union write variant",
        &ConformanceToolCall::Write(ConformanceWrite {
            resource_id: "res-2".to_string(),
            dry_run: false,
        }),
        json!({ "resourceId": "res-2", "dryRun": false }),
    );

    let write_schema = root_json_schema::<ConformanceWrite>();
    assert!(
        write_schema.get("required").is_none(),
        "container `#[serde(default)]` makes every field omittable"
    );
    assert!(
        compiled::<ConformanceWrite>().validate(&json!({})).is_ok(),
        "an omitted container-default DTO validates"
    );
    let default_write: ConformanceWrite =
        serde_json::from_value(json!({})).expect("serde applies the container default");
    assert_eq!(default_write.resource_id, "");
    assert!(!default_write.dry_run);

    let call_schema = root_json_schema::<ConformanceToolCall>();
    assert_eq!(call_schema["title"], "ConformanceToolCall");
    assert!(
        call_schema["anyOf"]
            .as_array()
            .is_some_and(|arms| arms.len() == 2),
        "union schema is an anyOf over its variants"
    );
}
