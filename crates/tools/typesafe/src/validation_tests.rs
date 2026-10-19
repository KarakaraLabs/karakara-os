// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use baml_rt_core::{BamlRtError, ErrorDisposition};
use baml_rt_tools::{BamlTool, OpaqueJson};
use serde_json::{Value, json};

use crate::{
    ChoiceQuestion, NoulQuestion, ScoreQuestion, SystemOneConfig, SystemOneError, SystemOneInput,
    SystemOneQuestion, SystemOneTool, validation::*,
};

fn valid_input() -> SystemOneInput {
    SystemOneInput {
        state: OpaqueJson::from(json!({"alert": "pump pressure changed", "samples": [4, 7]})),
        questions: BTreeMap::from([
            (
                "route".to_string(),
                SystemOneQuestion::Choice(ChoiceQuestion {
                    instructions: OpaqueJson::from(json!("Choose a route")),
                    criteria: BTreeMap::from([
                        ("investigate".to_string(), OpaqueJson::from(json!(null))),
                        ("none".to_string(), OpaqueJson::from(json!("abstain"))),
                    ]),
                }),
            ),
            (
                "severity".to_string(),
                SystemOneQuestion::Score(ScoreQuestion {
                    instructions: OpaqueJson::from(json!({"task": "score severity"})),
                    criteria: vec![
                        OpaqueJson::from(json!(null)),
                        OpaqueJson::from(json!("medium")),
                        OpaqueJson::from(json!({"label": "high"})),
                    ],
                }),
            ),
            (
                "has_baseline".to_string(),
                SystemOneQuestion::Noul(NoulQuestion {
                    instructions: OpaqueJson::from(json!("Is there a baseline?")),
                    criteria: None,
                }),
            ),
        ]),
        model: None,
        timeout_ms: None,
    }
}

fn enabled_config() -> SystemOneConfig {
    SystemOneConfig {
        enabled: true,
        ..SystemOneConfig::default()
    }
}

fn response_bytes(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).expect("serialize response")
}

fn validate_response_for(
    input: &SystemOneInput,
    response_body: &[u8],
) -> Result<ValidatedSystemOneResponse, SystemOneError> {
    let mut config = enabled_config();
    if let Some(model) = &input.model
        && !config.allowed_models.contains(model)
    {
        config.allowed_models.push(model.clone());
    }
    let request = validate_system_one_request(&config, input)?;
    crate::validation::validate_system_one_response(input, &request, response_body)
}

fn valid_response() -> Value {
    json!({
        "model": "jev-1.13.0",
        "answers": {
            "route": {
                "type": "choice",
                "choice": "investigate",
                "probabilities": {"investigate": 0.8, "none": 0.2},
                "confidence": 0.6
            },
            "severity": {
                "type": "score",
                "score": 1.1,
                "probabilities": {"0": 0.1, "1": 0.7, "2": 0.2},
                "confidence": 0.7,
                "legend": {"0": null, "1": "medium", "2": {"label": "high"}}
            },
            "has_baseline": {"type": "noul", "noul": 0.25}
        },
        "usage": {"input_tokens": 100, "output_tokens": 12}
    })
}

#[test]
fn request_validation_accepts_supported_shapes_and_resolves_defaults() {
    let input = valid_input();
    let config = enabled_config();
    let validated = validate_system_one_request(&config, &input).expect("valid request");

    assert_eq!(validated.model(), crate::DEFAULT_MODEL);
    let body: Value = serde_json::from_slice(validated.body()).expect("provider body");
    assert_eq!(body["model"], crate::DEFAULT_MODEL);
    assert!(body.get("timeout_ms").is_none());
    assert_eq!(body["state"], *input.state.as_value());
}

#[test]
fn request_validation_matrix() {
    struct Case {
        name: &'static str,
        mutate: fn(&mut SystemOneConfig, &mut SystemOneInput),
        expected_fragment: &'static str,
    }

    let cases = [
        Case {
            name: "empty string state",
            mutate: |_, input| input.state = OpaqueJson::from(json!("  ")),
            expected_fragment: "state",
        },
        Case {
            name: "empty object state",
            mutate: |_, input| input.state = OpaqueJson::from(json!({})),
            expected_fragment: "state",
        },
        Case {
            name: "scalar state",
            mutate: |_, input| input.state = OpaqueJson::from(json!(42)),
            expected_fragment: "state",
        },
        Case {
            name: "no questions",
            mutate: |_, input| input.questions.clear(),
            expected_fragment: "at least one",
        },
        Case {
            name: "too many questions",
            mutate: |config, _| config.limits.max_questions = 2,
            expected_fragment: "configured maximum",
        },
        Case {
            name: "blank question id",
            mutate: |_, input| {
                let question = input.questions.remove("route").expect("route");
                input.questions.insert(" ".to_string(), question);
            },
            expected_fragment: "question ID",
        },
        Case {
            name: "question id too long",
            mutate: |_, input| {
                let question = input.questions.remove("route").expect("route");
                input.questions.insert("x".repeat(129), question);
            },
            expected_fragment: "maximum length",
        },
        Case {
            name: "padded question id",
            mutate: |_, input| {
                let question = input.questions.remove("route").expect("route");
                input.questions.insert(" route".to_string(), question);
            },
            expected_fragment: "leading or trailing whitespace",
        },
        Case {
            name: "question id with a control character",
            mutate: |_, input| {
                let question = input.questions.remove("route").expect("route");
                input.questions.insert("rou\nte".to_string(), question);
            },
            expected_fragment: "control characters",
        },
        Case {
            name: "disallowed model",
            mutate: |_, input| input.model = Some("jev-preview".to_string()),
            expected_fragment: "allowlist",
        },
        Case {
            name: "zero timeout",
            mutate: |_, input| input.timeout_ms = Some(0),
            expected_fragment: "greater than zero",
        },
        Case {
            name: "timeout above ceiling",
            mutate: |config, input| input.timeout_ms = Some(config.timeout_ms + 1),
            expected_fragment: "ceiling",
        },
        Case {
            name: "empty instructions",
            mutate: |_, input| {
                let SystemOneQuestion::Choice(question) = input.questions.get_mut("route").unwrap()
                else {
                    unreachable!()
                };
                question.instructions = OpaqueJson::from(json!(null));
            },
            expected_fragment: "instructions",
        },
        Case {
            name: "no choice options",
            mutate: |_, input| {
                let SystemOneQuestion::Choice(question) = input.questions.get_mut("route").unwrap()
                else {
                    unreachable!()
                };
                question.criteria.clear();
            },
            expected_fragment: "at least one choice",
        },
        Case {
            name: "too many choice options",
            mutate: |config, _| config.limits.max_choice_options = 1,
            expected_fragment: "choice maximum",
        },
        Case {
            name: "blank choice option id",
            mutate: |_, input| {
                let SystemOneQuestion::Choice(question) = input.questions.get_mut("route").unwrap()
                else {
                    unreachable!()
                };
                question
                    .criteria
                    .insert(" ".to_string(), OpaqueJson::from(json!(null)));
            },
            expected_fragment: "questions.route.criteria: choice option ID",
        },
        Case {
            name: "choice option id too long",
            mutate: |_, input| {
                let SystemOneQuestion::Choice(question) = input.questions.get_mut("route").unwrap()
                else {
                    unreachable!()
                };
                question
                    .criteria
                    .insert("x".repeat(129), OpaqueJson::from(json!(null)));
            },
            expected_fragment: "questions.route.criteria: choice option ID",
        },
        Case {
            name: "padded choice option id",
            mutate: |_, input| {
                let SystemOneQuestion::Choice(question) = input.questions.get_mut("route").unwrap()
                else {
                    unreachable!()
                };
                question
                    .criteria
                    .insert(" none".to_string(), OpaqueJson::from(json!(null)));
            },
            expected_fragment: "leading or trailing whitespace",
        },
        Case {
            name: "choice option id with a control character",
            mutate: |_, input| {
                let SystemOneQuestion::Choice(question) = input.questions.get_mut("route").unwrap()
                else {
                    unreachable!()
                };
                question
                    .criteria
                    .insert("no\u{0}ne".to_string(), OpaqueJson::from(json!(null)));
            },
            expected_fragment: "control characters",
        },
        Case {
            name: "too few score levels",
            mutate: |_, input| {
                let SystemOneQuestion::Score(question) =
                    input.questions.get_mut("severity").unwrap()
                else {
                    unreachable!()
                };
                question.criteria.truncate(1);
            },
            expected_fragment: "at least two",
        },
        Case {
            name: "too many score levels",
            mutate: |config, _| config.limits.max_score_levels = 2,
            expected_fragment: "score maximum",
        },
    ];

    for case in cases {
        let mut config = enabled_config();
        let mut input = valid_input();
        (case.mutate)(&mut config, &mut input);
        let error = validate_system_one_request(&config, &input).expect_err(case.name);
        assert_eq!(error.code(), "invalid_argument", "{}", case.name);
        assert!(
            error.to_string().contains(case.expected_fragment),
            "{}: {error}",
            case.name
        );
    }
}

#[test]
fn request_summaries_escape_control_characters_before_validation() {
    let mut input = valid_input();
    let question = input.questions.remove("route").expect("route");
    input
        .questions
        .insert("route\ninjected".to_string(), question);

    let description = crate::describe_request(&input);
    assert_eq!(description.lines().count(), 1);
    assert!(description.contains(r"route\ninjected:choice"));

    let identity = crate::request_action_identity(&input);
    let questions = identity
        .fields
        .iter()
        .find_map(|(name, value)| (*name == "questions").then(|| value.as_str()).flatten())
        .expect("questions identity field");
    assert!(!questions.contains('\n'));
    assert!(questions.contains(r"route\ninjected:choice"));
}

#[test]
fn timeout_at_ceiling_and_configured_model_are_accepted() {
    let mut config = enabled_config();
    config.allowed_models.push("jev-preview".to_string());
    let mut input = valid_input();
    input.model = Some("jev-preview".to_string());
    input.timeout_ms = Some(config.timeout_ms);

    let validated = validate_system_one_request(&config, &input).expect("boundary request");
    assert_eq!(validated.model(), "jev-preview");
}

#[test]
fn request_body_limit_uses_the_exact_serialized_provider_bytes() {
    let input = valid_input();
    let mut config = enabled_config();
    config.limits.max_body_bytes = u32::MAX;
    let initial = validate_system_one_request(&config, &input).expect("measure body");
    let exact = u32::try_from(initial.body().len()).expect("small test body");

    config.limits.max_body_bytes = exact;
    let accepted = validate_system_one_request(&config, &input).expect("exact limit");
    assert_eq!(accepted.body(), initial.body());

    config.limits.max_body_bytes = exact - 1;
    let error = validate_system_one_request(&config, &input).expect_err("one byte over");
    assert!(error.to_string().contains("serialized request"));
}

#[test]
fn request_nesting_depth_is_bounded() {
    let mut at_limit = json!("leaf");
    for _ in 0..64 {
        at_limit = json!([at_limit]);
    }
    let mut input = valid_input();
    input.state = OpaqueJson::from(at_limit);
    validate_system_one_request(&enabled_config(), &input).expect("64 levels accepted");

    let mut over_limit = input.state.as_value().clone();
    over_limit = json!([over_limit]);
    input.state = OpaqueJson::from(over_limit);
    let error = validate_system_one_request(&enabled_config(), &input).expect_err("65 levels");
    assert!(error.to_string().contains("nesting depth"));
}

#[test]
fn criteria_nesting_depth_counts_the_criteria_container() {
    let mut at_limit = json!("leaf");
    for _ in 0..63 {
        at_limit = json!([at_limit]);
    }

    let mut input = valid_input();
    let SystemOneQuestion::Choice(question) = input.questions.get_mut("route").unwrap() else {
        unreachable!()
    };
    question
        .criteria
        .insert("investigate".to_string(), OpaqueJson::from(at_limit));
    validate_system_one_request(&enabled_config(), &input)
        .expect("the criteria container plus 63 nested arrays is 64 levels");

    let SystemOneQuestion::Choice(question) = input.questions.get_mut("route").unwrap() else {
        unreachable!()
    };
    let over_limit = json!([question.criteria["investigate"].as_value().clone()]);
    question
        .criteria
        .insert("investigate".to_string(), OpaqueJson::from(over_limit));
    let error = validate_system_one_request(&enabled_config(), &input)
        .expect_err("the criteria container makes the total depth 65");
    assert!(
        error
            .to_string()
            .contains("questions.route.criteria.investigate")
    );
    assert!(error.to_string().contains("nesting depth"));
}

#[test]
fn malformed_noul_criteria_is_rejected_during_deserialization() {
    let malformed_noul = r#"{
        "state":"x",
        "questions":{
            "safe":{
                "type":"noul",
                "instructions":"safe?",
                "criteria":{"true":"yes"}
            }
        }
    }"#;
    let error = serde_json::from_str::<SystemOneInput>(malformed_noul)
        .expect_err("noul criteria must have exactly true and false keys");
    assert!(
        error
            .to_string()
            .contains("invalid System One input at questions.*.criteria: expected an object with true and false fields for a noul question"),
        "unexpected error: {error}"
    );
}

#[test]
fn typed_error_mapping_uses_the_frozen_codes_and_nonfatal_dispositions() {
    let cases = [
        SystemOneError::invalid_argument("bad input"),
        SystemOneError::disabled("disabled"),
        SystemOneError::MissingCredential,
        SystemOneError::Unauthorized,
        SystemOneError::CapacityExceeded,
        SystemOneError::QuotaExceeded,
        SystemOneError::ProviderRateLimited {
            retry_after_ms: Some(500),
        },
        SystemOneError::provider_unavailable("unavailable"),
        SystemOneError::provider_contract("bad response"),
    ];
    let expected = [
        ("invalid_argument", ErrorDisposition::LlmCorrectable, None),
        (
            "typesafe_disabled",
            ErrorDisposition::InformAndContinue,
            None,
        ),
        (
            "typesafe_missing_credential",
            ErrorDisposition::InformAndContinue,
            None,
        ),
        (
            "typesafe_unauthorized",
            ErrorDisposition::InformAndContinue,
            None,
        ),
        (
            "typesafe_capacity_exceeded",
            ErrorDisposition::InformAndContinue,
            None,
        ),
        (
            "typesafe_quota_exceeded",
            ErrorDisposition::InformAndContinue,
            None,
        ),
        (
            "typesafe_rate_limited",
            ErrorDisposition::InformAndContinue,
            Some(500),
        ),
        (
            "typesafe_provider_unavailable",
            ErrorDisposition::InformAndContinue,
            None,
        ),
        (
            "typesafe_provider_contract",
            ErrorDisposition::InformAndContinue,
            None,
        ),
    ];

    for (error, (code, disposition, retry_after_ms)) in cases.into_iter().zip(expected) {
        let classified = error.into_classified();
        assert_eq!(classified.code, code);
        assert_eq!(classified.disposition, disposition);
        assert_eq!(classified.retry_after_ms, retry_after_ms);
    }
}

#[test]
fn malformed_input_classification_matrix_does_not_echo_bad_values() {
    let cases = [
        (
            "questions object",
            json!({
                "state": "state",
                "questions": ["QUESTIONS_OBJECT_SENTINEL"]
            }),
            "QUESTIONS_OBJECT_SENTINEL",
            "at questions: expected an object",
        ),
        (
            "question object",
            json!({
                "state": "state",
                "questions": {"route": "QUESTION_OBJECT_SENTINEL"}
            }),
            "QUESTION_OBJECT_SENTINEL",
            "at questions.*: expected an object",
        ),
        (
            "score criteria",
            json!({
                "state": "state",
                "questions": {
                    "severity": {
                        "type": "score",
                        "instructions": "score it",
                        "criteria": "SCORE_CRITERIA_SENTINEL"
                    }
                }
            }),
            "SCORE_CRITERIA_SENTINEL",
            "expected an array for a score question",
        ),
        (
            "choice criteria",
            json!({
                "state": "state",
                "questions": {
                    "route": {
                        "type": "choice",
                        "instructions": "choose",
                        "criteria": "CHOICE_CRITERIA_SENTINEL"
                    }
                }
            }),
            "CHOICE_CRITERIA_SENTINEL",
            "expected an object for a choice question",
        ),
        (
            "noul criteria",
            json!({
                "state": "state",
                "questions": {
                    "safe": {
                        "type": "noul",
                        "instructions": "safe?",
                        "criteria": {"true": "NOUL_CRITERIA_SENTINEL"}
                    }
                }
            }),
            "NOUL_CRITERIA_SENTINEL",
            "expected an object with true and false fields for a noul question",
        ),
        (
            "missing question field",
            json!({
                "state": "state",
                "questions": {
                    "route": {
                        "type": "choice",
                        "criteria": {"none": "MISSING_INSTRUCTIONS_SENTINEL"}
                    }
                }
            }),
            "MISSING_INSTRUCTIONS_SENTINEL",
            "expected the documented fields for its question type",
        ),
        (
            "unknown question field",
            json!({
                "state": "state",
                "questions": {
                    "route": {
                        "type": "choice",
                        "instructions": "choose",
                        "criteria": {"none": null},
                        "UNKNOWN_FIELD_SENTINEL": true
                    }
                }
            }),
            "UNKNOWN_FIELD_SENTINEL",
            "expected the documented fields for its question type",
        ),
        (
            "unknown variant",
            json!({
                "state": "state",
                "questions": {
                    "route": {
                        "type": "UNKNOWN_VARIANT_SENTINEL",
                        "instructions": "choose",
                        "criteria": {}
                    }
                }
            }),
            "UNKNOWN_VARIANT_SENTINEL",
            "questions.*.type",
        ),
        (
            "model type",
            json!({
                "state": "state",
                "questions": {},
                "model": ["MODEL_SENTINEL"]
            }),
            "MODEL_SENTINEL",
            "at model: expected a string or null",
        ),
        (
            "negative timeout",
            json!({
                "state": "TIMEOUT_NEGATIVE_SENTINEL",
                "questions": {},
                "timeout_ms": -1
            }),
            "TIMEOUT_NEGATIVE_SENTINEL",
            "at timeout_ms: expected an unsigned integer or null",
        ),
        (
            "non-integer timeout",
            json!({
                "state": "TIMEOUT_FRACTION_SENTINEL",
                "questions": {},
                "timeout_ms": 1.5
            }),
            "TIMEOUT_FRACTION_SENTINEL",
            "at timeout_ms: expected an unsigned integer or null",
        ),
        (
            "state wrapper",
            json!({
                "state": {"__baml_opaque_json": "STATE_WRAPPER_SENTINEL"},
                "questions": {}
            }),
            "STATE_WRAPPER_SENTINEL",
            "at state: expected a JSON value or OpaqueJson wrapper",
        ),
        (
            "generic input fallback",
            json!({
                "state": "state",
                "questions": {},
                "GENERIC_FALLBACK_SENTINEL": true
            }),
            "GENERIC_FALLBACK_SENTINEL",
            "documented request shape",
        ),
    ];

    for (name, input, sentinel, expected_message) in cases {
        let error = serde_json::from_value::<SystemOneInput>(input).expect_err(name);
        let classified = SystemOneTool::classify_execution_error(&BamlRtError::Json(error));

        assert_eq!(classified.code, "invalid_argument", "{name}");
        assert_eq!(
            classified.disposition,
            ErrorDisposition::LlmCorrectable,
            "{name}"
        );
        assert!(
            classified.message.contains(expected_message),
            "{name}: {}",
            classified.message
        );
        assert!(!classified.message.contains(sentinel), "{name}");
    }
}

#[test]
fn input_error_classification_returns_only_an_allowlisted_message() {
    let expected = "invalid System One input at model: expected a string or null";
    let raw = format!("{expected} SECRET_SUFFIX at line 1 column 2");

    assert_eq!(classify_input_deserialization_error(&raw), Some(expected));
    assert_eq!(
        classify_input_deserialization_error("invalid System One input FAKE_PREFIX_SENTINEL"),
        None
    );
}

#[test]
fn non_input_json_errors_are_classified_as_provider_contract_failures() {
    for sentinel in [
        "NON_INPUT_JSON_SENTINEL",
        "invalid System One input FAKE_PREFIX_SENTINEL",
    ] {
        let encoded = serde_json::to_string(sentinel).expect("encode sentinel");
        let error = serde_json::from_str::<u64>(&encoded).expect_err("string is not an integer");
        let classified = SystemOneTool::classify_execution_error(&BamlRtError::Json(error));
        assert_eq!(classified.code, "typesafe_provider_contract");
        assert_eq!(classified.disposition, ErrorDisposition::InformAndContinue);
        assert!(!classified.message.contains(sentinel));
    }
}

#[test]
fn valid_mixed_provider_response_is_projected_without_legend() {
    let validated = validate_response_for(&valid_input(), &response_bytes(valid_response()))
        .expect("valid response");

    assert_eq!(validated.output.model, "jev-1.13.0");
    assert_eq!(validated.output.answers.len(), 3);
    assert_eq!(
        validated.output.usage.as_ref().unwrap().input_tokens,
        Some(100)
    );
    assert!(validated.drifts.is_empty());
    let serialized = serde_json::to_value(validated.output).expect("serialize output");
    assert!(serialized["answers"]["severity"].get("legend").is_none());
}

#[test]
fn recorded_structured_score_fixture_is_accepted() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/reference/structured_score.recorded.json"
    ))
    .expect("fixture JSON");
    let input: SystemOneInput =
        serde_json::from_value(fixture["request"].clone()).expect("fixture request");
    let response = response_bytes(fixture["response"].clone());

    let validated = validate_response_for(&input, &response).expect("recorded provider response");
    assert!(validated.drifts.is_empty());
}

#[test]
fn recorded_score_rounding_observation_is_accepted() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/reference/score_rounding.observation.json"
    ))
    .expect("score fixture");
    let input: SystemOneInput = serde_json::from_value(json!({
        "state": "score this state",
        "questions": fixture["request_excerpt"]["questions"]
    }))
    .expect("score request");
    let response = response_bytes(json!({
        "model": "jev-1.13.0",
        "answers": {"new_score_1": fixture["displayed_answer"]}
    }));

    let validated = validate_response_for(&input, &response).expect("score observation");
    assert!(validated.drifts.is_empty());
}

#[test]
fn recorded_null_choice_and_tie_contract_are_accepted() {
    let null_fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/reference/null_choice.recorded.json"
    ))
    .expect("null fixture");
    let input: SystemOneInput = serde_json::from_value(json!({
        "state": null_fixture["request_excerpt"]["state"],
        "model": null_fixture["request_excerpt"]["model"],
        "questions": null_fixture["request_excerpt"]["questions"]
    }))
    .expect("null choice request");
    let response = response_bytes(json!({
        "model": null_fixture["response_model"],
        "answers": {"tone": null_fixture["answer"]}
    }));
    validate_response_for(&input, &response).expect("null choice response");

    let tie_fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/reference/choice_tie.contract.json"
    ))
    .expect("tie fixture");
    let input: SystemOneInput = serde_json::from_value(json!({
        "state": "tie",
        "questions": {"choice": tie_fixture["question"]}
    }))
    .expect("tie request");
    let response = response_bytes(json!({
        "model": "jev-1.13.0",
        "answers": {"choice": tie_fixture["derived_answer"]}
    }));
    validate_response_for(&input, &response).expect("tied maximum accepted");
}

#[test]
fn provider_response_contract_failure_matrix() {
    struct Case {
        name: &'static str,
        mutate: fn(&mut Value),
    }

    let cases = [
        Case {
            name: "missing answer",
            mutate: |response| {
                response["answers"].as_object_mut().unwrap().remove("route");
            },
        },
        Case {
            name: "extra answer",
            mutate: |response| {
                response["answers"]["extra"] = json!({"type": "noul", "noul": 0.5});
            },
        },
        Case {
            name: "replaced answer ID",
            mutate: |response| {
                let answer = response["answers"]
                    .as_object_mut()
                    .unwrap()
                    .remove("route")
                    .unwrap();
                response["answers"]["other"] = answer;
            },
        },
        Case {
            name: "answer type mismatch",
            mutate: |response| {
                response["answers"]["route"] = json!({"type": "noul", "noul": 0.5});
            },
        },
        Case {
            name: "unknown choice",
            mutate: |response| response["answers"]["route"]["choice"] = json!("other"),
        },
        Case {
            name: "choice keys mismatch",
            mutate: |response| {
                response["answers"]["route"]["probabilities"] =
                    json!({"investigate": 0.8, "other": 0.2});
            },
        },
        Case {
            name: "choice below maximum",
            mutate: |response| {
                response["answers"]["route"]["choice"] = json!("none");
            },
        },
        Case {
            name: "score keys mismatch",
            mutate: |response| {
                response["answers"]["severity"]["probabilities"] =
                    json!({"0": 0.1, "1": 0.7, "3": 0.2});
            },
        },
        Case {
            name: "legend keys mismatch",
            mutate: |response| {
                response["answers"]["severity"]["legend"] =
                    json!({"0": null, "1": null, "3": null});
            },
        },
        Case {
            name: "score outside range",
            mutate: |response| response["answers"]["severity"]["score"] = json!(3.0),
        },
        Case {
            name: "distribution outside tolerance",
            mutate: |response| {
                response["answers"]["route"]["probabilities"] =
                    json!({"investigate": 0.9, "none": 0.2});
            },
        },
        Case {
            name: "probability outside unit interval",
            mutate: |response| {
                response["answers"]["route"]["probabilities"] =
                    json!({"investigate": 1.1, "none": -0.1});
            },
        },
        Case {
            name: "confidence outside unit interval",
            mutate: |response| response["answers"]["route"]["confidence"] = json!(1.1),
        },
        Case {
            name: "noul outside unit interval",
            mutate: |response| response["answers"]["has_baseline"]["noul"] = json!(-0.1),
        },
        Case {
            name: "empty response model",
            mutate: |response| response["model"] = json!(""),
        },
    ];

    for case in cases {
        let mut response = valid_response();
        (case.mutate)(&mut response);
        let error =
            validate_response_for(&valid_input(), &response_bytes(response)).expect_err(case.name);
        assert_eq!(error.code(), "typesafe_provider_contract", "{}", case.name);
    }
}

#[test]
fn score_weighted_mean_uses_numeric_keys_instead_of_btree_order() {
    let input = valid_input();
    let request = validate_system_one_request(&enabled_config(), &input).expect("valid request");
    let mut expanded_input = input;
    let SystemOneQuestion::Score(question) = expanded_input.questions.get_mut("severity").unwrap()
    else {
        unreachable!()
    };
    question.criteria = (0..11)
        .map(|index| OpaqueJson::from(json!(index)))
        .collect();

    let probabilities = (0..11)
        .map(|index| {
            (
                index.to_string(),
                if index == 10 { json!(1.0) } else { json!(0.0) },
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let mut response = valid_response();
    response["answers"]["severity"]["score"] = json!(10.0);
    response["answers"]["severity"]["probabilities"] = Value::Object(probabilities);
    response["answers"]["severity"]
        .as_object_mut()
        .unwrap()
        .remove("legend");

    let validated = crate::validation::validate_system_one_response(
        &expanded_input,
        &request,
        &response_bytes(response),
    )
    .expect("numeric score keys");
    assert!(!validated.drifts.iter().any(|drift| {
        drift.question_id == "severity" && drift.invariant == "score_weighted_mean"
    }));
}

#[test]
fn response_tolerances_accept_the_documented_boundaries() {
    let mut response = valid_response();
    response["answers"]["route"]["choice"] = json!("none");
    response["answers"]["route"]["probabilities"] = json!({"investigate": 0.505, "none": 0.495});
    response["answers"]["severity"]["score"] = json!(1.15);
    response["answers"]["severity"]["probabilities"] = json!({"0": 0.1, "1": 0.7, "2": 0.25});

    let validated = validate_response_for(&valid_input(), &response_bytes(response))
        .expect("hard and tie boundaries are inclusive");
    assert!(validated.drifts.contains(&ProviderInvariantDrift {
        question_id: "severity".to_string(),
        invariant: "distribution_sum",
    }));
    assert!(
        !validated.drifts.iter().any(|drift| {
            drift.question_id == "severity" && drift.invariant == "score_weighted_mean"
        }),
        "a weighted-mean difference of exactly 0.05 is accepted without drift"
    );
}

#[test]
fn tolerated_provider_drift_is_reported_without_rejecting_the_response() {
    let mut response = valid_response();
    response["answers"]["route"]["probabilities"] = json!({"investigate": 0.81, "none": 0.21});
    response["answers"]["severity"]["score"] = json!(1.8);

    let validated = validate_response_for(&valid_input(), &response_bytes(response))
        .expect("drift is telemetry, not failure");
    assert!(validated.drifts.contains(&ProviderInvariantDrift {
        question_id: "route".to_string(),
        invariant: "distribution_sum",
    }));
    assert!(validated.drifts.contains(&ProviderInvariantDrift {
        question_id: "severity".to_string(),
        invariant: "score_weighted_mean",
    }));
}

#[test]
fn unknown_additive_response_fields_are_ignored() {
    let mut response = valid_response();
    response["provider_revision"] = json!("new");
    response["usage"]["cached_tokens"] = json!(10);
    response["answers"]["route"]["new_field"] = json!({"anything": true});

    validate_response_for(&valid_input(), &response_bytes(response))
        .expect("unknown additive fields");
}

#[test]
fn malformed_and_duplicate_provider_json_is_rejected() {
    let malformed =
        validate_response_for(&valid_input(), b"not json").expect_err("malformed provider body");
    assert_eq!(malformed.code(), "typesafe_provider_contract");

    let duplicate = br#"{
        "model":"jev-1.13.0",
        "answers":{
            "route":{"type":"choice","choice":"investigate","probabilities":{"investigate":0.8,"investigate":0.2,"none":0.2},"confidence":0.6},
            "severity":{"type":"score","score":1.1,"probabilities":{"0":0.1,"1":0.7,"2":0.2},"confidence":0.7},
            "has_baseline":{"type":"noul","noul":0.25}
        }
    }"#;
    let duplicate_error =
        validate_response_for(&valid_input(), duplicate).expect_err("duplicate probability key");
    assert_eq!(duplicate_error.code(), "typesafe_provider_contract");
    assert_eq!(
        duplicate_error.to_string(),
        "TypeSafe returned a malformed response"
    );
}
