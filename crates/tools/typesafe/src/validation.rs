// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeMap, fmt, marker::PhantomData};

use baml_rt_observability::record_typesafe_provider_invariant_drift;
use baml_rt_tools::OpaqueJson;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;

use crate::{
    ChoiceAnswer, ChoiceQuestion, NoulAnswer, ScoreAnswer, ScoreQuestion, SystemOneAnswer,
    SystemOneConfig, SystemOneError, SystemOneInput, SystemOneOutput, SystemOneQuestion,
    SystemOneUsage,
};

const MAX_JSON_NESTING_DEPTH: usize = 64;
const MAX_QUESTION_ID_CHARS: usize = 128;
const MAX_CHOICE_OPTION_ID_CHARS: usize = 128;
const DISTRIBUTION_DRIFT_TOLERANCE: f64 = 0.01;
const DISTRIBUTION_HARD_TOLERANCE: f64 = 0.05;
const CHOICE_TIE_TOLERANCE: f64 = 0.01;
const SCORE_WEIGHTED_MEAN_DRIFT_TOLERANCE: f64 = 0.05;
const FLOAT_COMPARISON_EPSILON: f64 = 1e-12;

const INPUT_EXPECTED_QUESTIONS_OBJECT: &str =
    "invalid System One input at questions: expected an object";
const INPUT_EXPECTED_QUESTION_OBJECT: &str =
    "invalid System One input at questions.*: expected an object";
const INPUT_EXPECTED_QUESTION_FIELDS: &str =
    "invalid System One input at questions.*: expected the documented fields for its question type";
const INPUT_EXPECTED_QUESTION_TYPE: &str =
    "invalid System One input at questions.*.type: expected choice, score, or noul";
const INPUT_EXPECTED_CHOICE_CRITERIA: &str =
    "invalid System One input at questions.*.criteria: expected an object for a choice question";
const INPUT_EXPECTED_SCORE_CRITERIA: &str =
    "invalid System One input at questions.*.criteria: expected an array for a score question";
const INPUT_EXPECTED_NOUL_CRITERIA: &str = "invalid System One input at questions.*.criteria: expected an object with true and false fields for a noul question";
const INPUT_EXPECTED_MODEL: &str = "invalid System One input at model: expected a string or null";
const INPUT_EXPECTED_TIMEOUT: &str =
    "invalid System One input at timeout_ms: expected an unsigned integer or null";
const INPUT_EXPECTED_STATE: &str =
    "invalid System One input at state: expected a JSON value or OpaqueJson wrapper";
pub(crate) const INPUT_EXPECTED_REQUEST: &str =
    "invalid System One input: expected the documented request shape";
const INPUT_ERROR_MESSAGES: &[&str] = &[
    INPUT_EXPECTED_QUESTIONS_OBJECT,
    INPUT_EXPECTED_QUESTION_OBJECT,
    INPUT_EXPECTED_QUESTION_FIELDS,
    INPUT_EXPECTED_QUESTION_TYPE,
    INPUT_EXPECTED_CHOICE_CRITERIA,
    INPUT_EXPECTED_SCORE_CRITERIA,
    INPUT_EXPECTED_NOUL_CRITERIA,
    INPUT_EXPECTED_MODEL,
    INPUT_EXPECTED_TIMEOUT,
    INPUT_EXPECTED_STATE,
    INPUT_EXPECTED_REQUEST,
];

/// Request ready for the provider transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedSystemOneRequest {
    body: Vec<u8>,
    model: String,
    timeout_ms: u64,
}

impl ValidatedSystemOneRequest {
    // The HTTP transport will send these exact bytes once that implementation slice lands.
    #[allow(dead_code)]
    pub(crate) fn body(&self) -> &[u8] {
        &self.body
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    // The HTTP transport will use this deadline once that implementation slice lands.
    #[allow(dead_code)]
    pub(crate) fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
}

/// A provider invariant that drifted within the accepted response envelope.
///
/// The pending HTTP transport will return these observations from response validation.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderInvariantDrift {
    pub(crate) question_id: String,
    pub(crate) invariant: &'static str,
}

/// Validated provider output plus non-fatal drift observations.
///
/// The pending HTTP transport will consume this result after each provider response.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct ValidatedSystemOneResponse {
    pub(crate) output: SystemOneOutput,
    pub(crate) drifts: Vec<ProviderInvariantDrift>,
}

#[derive(Serialize)]
struct ProviderRequest<'a> {
    state: &'a OpaqueJson,
    questions: &'a BTreeMap<String, SystemOneQuestion>,
    model: &'a str,
}

/// Validate agent input and serialize the exact provider request body once.
pub(crate) fn validate_system_one_request(
    config: &SystemOneConfig,
    input: &SystemOneInput,
) -> Result<ValidatedSystemOneRequest, SystemOneError> {
    validate_state(input.state.as_value())?;

    if input.questions.is_empty() {
        return Err(SystemOneError::invalid_argument(
            "questions must contain at least one question",
        ));
    }
    if input.questions.len() > config.limits.max_questions as usize {
        return Err(SystemOneError::invalid_argument(format!(
            "questions exceeds the configured maximum of {}",
            config.limits.max_questions
        )));
    }

    for (id, question) in &input.questions {
        validate_identifier("questions", "question ID", id, MAX_QUESTION_ID_CHARS)?;
        validate_question(config, id, question)?;
    }

    let model = input
        .model
        .as_deref()
        .unwrap_or(config.default_model.as_str());
    if !config.allowed_models.iter().any(|allowed| allowed == model) {
        return Err(SystemOneError::invalid_argument(
            "model is not in the operator allowlist",
        ));
    }

    let timeout_ms = input.timeout_ms.unwrap_or(config.timeout_ms);
    if timeout_ms == 0 {
        return Err(SystemOneError::invalid_argument(
            "timeout_ms must be greater than zero",
        ));
    }
    if timeout_ms > config.timeout_ms {
        return Err(SystemOneError::invalid_argument(format!(
            "timeout_ms exceeds the configured ceiling of {}",
            config.timeout_ms
        )));
    }

    let body = serde_json::to_vec(&ProviderRequest {
        state: &input.state,
        questions: &input.questions,
        model,
    })
    .map_err(|_| SystemOneError::invalid_argument("request contains invalid JSON values"))?;
    if body.len() > config.limits.max_body_bytes as usize {
        return Err(SystemOneError::invalid_argument(format!(
            "serialized request exceeds the configured maximum of {} bytes",
            config.limits.max_body_bytes
        )));
    }

    Ok(ValidatedSystemOneRequest {
        body,
        model: model.to_string(),
        timeout_ms,
    })
}

fn validate_state(state: &Value) -> Result<(), SystemOneError> {
    match state {
        Value::String(value) if value.trim().is_empty() => {
            return Err(SystemOneError::invalid_argument(
                "state must not be an empty string",
            ));
        }
        Value::Array(values) if values.is_empty() => {
            return Err(SystemOneError::invalid_argument(
                "state must not be an empty array",
            ));
        }
        Value::Object(values) if values.is_empty() => {
            return Err(SystemOneError::invalid_argument(
                "state must not be an empty object",
            ));
        }
        Value::String(_) | Value::Array(_) | Value::Object(_) => {}
        _ => {
            return Err(SystemOneError::invalid_argument(
                "state must be a non-empty string, array, or object",
            ));
        }
    }
    validate_json_depth("state", state)
}

fn validate_question(
    config: &SystemOneConfig,
    id: &str,
    question: &SystemOneQuestion,
) -> Result<(), SystemOneError> {
    match question {
        SystemOneQuestion::Choice(question) => validate_choice(config, id, question),
        SystemOneQuestion::Score(question) => validate_score(config, id, question),
        SystemOneQuestion::Noul(question) => {
            validate_instructions(id, question.instructions.as_value())?;
            if let Some(criteria) = &question.criteria {
                validate_json_depth_with_enclosing_container(
                    &format!("questions.{id}.criteria.true"),
                    criteria.true_value.as_value(),
                )?;
                validate_json_depth_with_enclosing_container(
                    &format!("questions.{id}.criteria.false"),
                    criteria.false_value.as_value(),
                )?;
            }
            Ok(())
        }
    }
}

fn validate_choice(
    config: &SystemOneConfig,
    id: &str,
    question: &ChoiceQuestion,
) -> Result<(), SystemOneError> {
    validate_instructions(id, question.instructions.as_value())?;
    if question.criteria.is_empty() {
        return Err(SystemOneError::invalid_argument(format!(
            "questions.{id}.criteria must contain at least one choice"
        )));
    }
    if question.criteria.len() > config.limits.max_choice_options as usize {
        return Err(SystemOneError::invalid_argument(format!(
            "questions.{id}.criteria exceeds the configured choice maximum of {}",
            config.limits.max_choice_options
        )));
    }
    for (option, criterion) in &question.criteria {
        validate_identifier(
            &format!("questions.{id}.criteria"),
            "choice option ID",
            option,
            MAX_CHOICE_OPTION_ID_CHARS,
        )?;
        validate_json_depth_with_enclosing_container(
            &format!("questions.{id}.criteria.{option}"),
            criterion.as_value(),
        )?;
    }
    Ok(())
}

fn validate_score(
    config: &SystemOneConfig,
    id: &str,
    question: &ScoreQuestion,
) -> Result<(), SystemOneError> {
    validate_instructions(id, question.instructions.as_value())?;
    if question.criteria.len() < 2 {
        return Err(SystemOneError::invalid_argument(format!(
            "questions.{id}.criteria must contain at least two score levels"
        )));
    }
    if question.criteria.len() > config.limits.max_score_levels as usize {
        return Err(SystemOneError::invalid_argument(format!(
            "questions.{id}.criteria exceeds the configured score maximum of {}",
            config.limits.max_score_levels
        )));
    }
    for (index, criterion) in question.criteria.iter().enumerate() {
        validate_json_depth_with_enclosing_container(
            &format!("questions.{id}.criteria.{index}"),
            criterion.as_value(),
        )?;
    }
    Ok(())
}

fn validate_instructions(id: &str, instructions: &Value) -> Result<(), SystemOneError> {
    if json_value_is_empty(instructions) {
        return Err(SystemOneError::invalid_argument(format!(
            "questions.{id}.instructions must not be empty"
        )));
    }
    validate_json_depth(&format!("questions.{id}.instructions"), instructions)
}

fn json_value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

fn validate_identifier(
    path: &str,
    kind: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), SystemOneError> {
    if value.trim().is_empty() {
        return Err(SystemOneError::invalid_argument(format!(
            "{path}: {kind} must not be empty"
        )));
    }
    if value != value.trim() {
        return Err(SystemOneError::invalid_argument(format!(
            "{path}: {kind} must not have leading or trailing whitespace"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(SystemOneError::invalid_argument(format!(
            "{path}: {kind} must not contain control characters"
        )));
    }
    if value.chars().count() > max_chars {
        return Err(SystemOneError::invalid_argument(format!(
            "{path}: {kind} exceeds the maximum length of {max_chars} characters"
        )));
    }
    Ok(())
}

fn validate_json_depth(path: &str, value: &Value) -> Result<(), SystemOneError> {
    validate_json_depth_from(path, value, 0)
}

fn validate_json_depth_with_enclosing_container(
    path: &str,
    value: &Value,
) -> Result<(), SystemOneError> {
    validate_json_depth_from(path, value, 1)
}

fn validate_json_depth_from(
    path: &str,
    value: &Value,
    enclosing_depth: usize,
) -> Result<(), SystemOneError> {
    if enclosing_depth.saturating_add(json_depth(value)) > MAX_JSON_NESTING_DEPTH {
        return Err(SystemOneError::invalid_argument(format!(
            "{path} exceeds the maximum JSON nesting depth of {MAX_JSON_NESTING_DEPTH}"
        )));
    }
    Ok(())
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(values) => 1 + values.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

pub(crate) fn classify_input_deserialization_error(raw: &str) -> Option<&'static str> {
    INPUT_ERROR_MESSAGES
        .iter()
        .copied()
        .find(|message| raw.starts_with(message))
}

pub(crate) fn deserialize_input_state<'de, D>(deserializer: D) -> Result<OpaqueJson, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    serde_json::from_value(value).map_err(|_| de::Error::custom(INPUT_EXPECTED_STATE))
}

pub(crate) fn deserialize_input_questions<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, SystemOneQuestion>, D::Error>
where
    D: Deserializer<'de>,
{
    let Value::Object(questions) = Value::deserialize(deserializer)? else {
        return Err(de::Error::custom(INPUT_EXPECTED_QUESTIONS_OBJECT));
    };
    let mut parsed = BTreeMap::new();
    for (id, value) in questions {
        let Value::Object(question) = &value else {
            return Err(de::Error::custom(INPUT_EXPECTED_QUESTION_OBJECT));
        };
        let Some(question_type) = question.get("type").and_then(Value::as_str) else {
            return Err(de::Error::custom(INPUT_EXPECTED_QUESTION_TYPE));
        };
        match question_type {
            "choice" => {
                if !question.get("criteria").is_some_and(Value::is_object) {
                    return Err(de::Error::custom(INPUT_EXPECTED_CHOICE_CRITERIA));
                }
            }
            "score" => {
                if !question.get("criteria").is_some_and(Value::is_array) {
                    return Err(de::Error::custom(INPUT_EXPECTED_SCORE_CRITERIA));
                }
            }
            "noul" => {
                if let Some(criteria) = question.get("criteria")
                    && !criteria.is_null()
                {
                    let Some(criteria) = criteria.as_object() else {
                        return Err(de::Error::custom(INPUT_EXPECTED_NOUL_CRITERIA));
                    };
                    if criteria.len() != 2
                        || !criteria.contains_key("true")
                        || !criteria.contains_key("false")
                    {
                        return Err(de::Error::custom(INPUT_EXPECTED_NOUL_CRITERIA));
                    }
                }
            }
            _ => return Err(de::Error::custom(INPUT_EXPECTED_QUESTION_TYPE)),
        }
        let question = serde_json::from_value(value)
            .map_err(|_| de::Error::custom(INPUT_EXPECTED_QUESTION_FIELDS))?;
        parsed.insert(id, question);
    }
    Ok(parsed)
}

pub(crate) fn deserialize_input_model<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(None),
        Value::String(model) => Ok(Some(model)),
        _ => Err(de::Error::custom(INPUT_EXPECTED_MODEL)),
    }
}

pub(crate) fn deserialize_input_timeout<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(None),
        Value::Number(number) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| de::Error::custom(INPUT_EXPECTED_TIMEOUT)),
        _ => Err(de::Error::custom(INPUT_EXPECTED_TIMEOUT)),
    }
}

#[derive(Debug, Deserialize)]
struct ProviderResponse {
    model: String,
    #[serde(deserialize_with = "deserialize_unique_btree_map")]
    answers: BTreeMap<String, ProviderAnswer>,
    #[serde(default)]
    usage: Option<ProviderUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ProviderAnswer {
    Choice(ProviderChoiceAnswer),
    Score(ProviderScoreAnswer),
    Noul(ProviderNoulAnswer),
}

#[derive(Debug, Deserialize)]
struct ProviderChoiceAnswer {
    choice: String,
    #[serde(deserialize_with = "deserialize_unique_btree_map")]
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct ProviderScoreAnswer {
    score: f64,
    #[serde(deserialize_with = "deserialize_unique_btree_map")]
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
    #[serde(default, deserialize_with = "deserialize_optional_unique_btree_map")]
    legend: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
struct ProviderNoulAnswer {
    noul: f64,
}

#[derive(Debug, Deserialize)]
struct ProviderUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

/// Parse and validate a raw provider response before agent code can branch on it.
///
/// The HTTP transport will call this function once that implementation slice lands.
#[allow(dead_code)]
pub(crate) fn validate_system_one_response(
    input: &SystemOneInput,
    request: &ValidatedSystemOneRequest,
    response_body: &[u8],
) -> Result<ValidatedSystemOneResponse, SystemOneError> {
    let response: ProviderResponse = serde_json::from_slice(response_body)
        .map_err(|_| SystemOneError::provider_contract("TypeSafe returned a malformed response"))?;
    if response.model.trim().is_empty() {
        return Err(SystemOneError::provider_contract(
            "TypeSafe response model must not be empty",
        ));
    }
    if response.answers.len() != input.questions.len() {
        return Err(SystemOneError::provider_contract(
            "TypeSafe response answer IDs do not match the request",
        ));
    }

    let ProviderResponse {
        model: response_model,
        answers: provider_answers,
        usage,
    } = response;
    let mut answers = BTreeMap::new();
    let mut drifts = Vec::new();
    for ((id, question), (answer_id, answer)) in input.questions.iter().zip(provider_answers) {
        if id != &answer_id {
            return Err(SystemOneError::provider_contract(
                "TypeSafe response answer IDs do not match the request",
            ));
        }
        let validated = match (question, answer) {
            (SystemOneQuestion::Choice(question), ProviderAnswer::Choice(answer)) => {
                SystemOneAnswer::Choice(validate_choice_answer(id, question, answer, &mut drifts)?)
            }
            (SystemOneQuestion::Score(question), ProviderAnswer::Score(answer)) => {
                SystemOneAnswer::Score(validate_score_answer(id, question, answer, &mut drifts)?)
            }
            (SystemOneQuestion::Noul(_), ProviderAnswer::Noul(answer)) => {
                validate_unit_interval(id, "noul", answer.noul)?;
                SystemOneAnswer::Noul(NoulAnswer { noul: answer.noul })
            }
            _ => {
                return Err(SystemOneError::provider_contract(format!(
                    "TypeSafe response type does not match questions.{id}"
                )));
            }
        };
        answers.insert(id.clone(), validated);
    }

    for drift in &drifts {
        record_typesafe_provider_invariant_drift(request.model(), drift.invariant);
    }

    Ok(ValidatedSystemOneResponse {
        output: SystemOneOutput {
            model: response_model,
            answers,
            usage: usage.map(|usage| SystemOneUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            }),
        },
        drifts,
    })
}

fn validate_choice_answer(
    id: &str,
    question: &ChoiceQuestion,
    answer: ProviderChoiceAnswer,
    drifts: &mut Vec<ProviderInvariantDrift>,
) -> Result<ChoiceAnswer, SystemOneError> {
    if answer.probabilities.keys().ne(question.criteria.keys()) {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe choice probability keys do not match questions.{id}.criteria"
        )));
    }
    let Some(selected_probability) = answer.probabilities.get(&answer.choice).copied() else {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe selected an unknown option for questions.{id}"
        )));
    };
    validate_distribution(id, &answer.probabilities, drifts)?;
    validate_unit_interval(id, "confidence", answer.confidence)?;

    let max_probability = answer
        .probabilities
        .values()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    if selected_probability < max_probability - CHOICE_TIE_TOLERANCE - FLOAT_COMPARISON_EPSILON {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe selected a non-maximal option for questions.{id}"
        )));
    }

    Ok(ChoiceAnswer {
        choice: answer.choice,
        probabilities: answer.probabilities,
        confidence: answer.confidence,
    })
}

fn validate_score_answer(
    id: &str,
    question: &ScoreQuestion,
    answer: ProviderScoreAnswer,
    drifts: &mut Vec<ProviderInvariantDrift>,
) -> Result<ScoreAnswer, SystemOneError> {
    let expected_keys = (0..question.criteria.len())
        .map(|index| index.to_string())
        .collect::<Vec<_>>();
    if !has_exact_keys(&answer.probabilities, &expected_keys) {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe score probability keys do not match questions.{id}.criteria"
        )));
    }
    if let Some(legend) = &answer.legend
        && !has_exact_keys(legend, &expected_keys)
    {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe score legend keys do not match questions.{id}.criteria"
        )));
    }

    validate_distribution(id, &answer.probabilities, drifts)?;
    validate_unit_interval(id, "confidence", answer.confidence)?;
    let max_score = (question.criteria.len() - 1) as f64;
    if !answer.score.is_finite() || !(0.0..=max_score).contains(&answer.score) {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe score is outside the range for questions.{id}"
        )));
    }

    let Some(weighted_mean) = expected_keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            answer
                .probabilities
                .get(key)
                .map(|probability| index as f64 * probability)
        })
        .sum::<Option<f64>>()
    else {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe score probability keys do not match questions.{id}.criteria"
        )));
    };
    if (answer.score - weighted_mean).abs()
        > SCORE_WEIGHTED_MEAN_DRIFT_TOLERANCE + FLOAT_COMPARISON_EPSILON
    {
        drifts.push(ProviderInvariantDrift {
            question_id: id.to_string(),
            invariant: "score_weighted_mean",
        });
    }

    Ok(ScoreAnswer {
        score: answer.score,
        probabilities: answer.probabilities,
        confidence: answer.confidence,
    })
}

fn has_exact_keys<T>(values: &BTreeMap<String, T>, expected_keys: &[String]) -> bool {
    values.len() == expected_keys.len() && expected_keys.iter().all(|key| values.contains_key(key))
}

fn validate_distribution(
    id: &str,
    probabilities: &BTreeMap<String, f64>,
    drifts: &mut Vec<ProviderInvariantDrift>,
) -> Result<(), SystemOneError> {
    for probability in probabilities.values() {
        validate_unit_interval(id, "probability", *probability)?;
    }
    let difference = (probabilities.values().sum::<f64>() - 1.0).abs();
    if difference > DISTRIBUTION_HARD_TOLERANCE + FLOAT_COMPARISON_EPSILON {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe probability distribution does not sum to one for questions.{id}"
        )));
    }
    if difference > DISTRIBUTION_DRIFT_TOLERANCE + FLOAT_COMPARISON_EPSILON {
        drifts.push(ProviderInvariantDrift {
            question_id: id.to_string(),
            invariant: "distribution_sum",
        });
    }
    Ok(())
}

fn validate_unit_interval(id: &str, field: &str, value: f64) -> Result<(), SystemOneError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(SystemOneError::provider_contract(format!(
            "TypeSafe {field} is outside [0, 1] for questions.{id}"
        )));
    }
    Ok(())
}

/// Deserialize a JSON object while rejecting duplicate keys.
pub(crate) fn deserialize_unique_btree_map<'de, D, T>(
    deserializer: D,
) -> Result<BTreeMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct UniqueMapVisitor<T>(PhantomData<T>);

    impl<'de, T> de::Visitor<'de> for UniqueMapVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = BTreeMap<String, T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object with unique keys")
        }

        fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some(key) = access.next_key::<String>()? {
                if values.contains_key(&key) {
                    return Err(de::Error::custom("duplicate object key"));
                }
                let value = access.next_value::<T>()?;
                values.insert(key, value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(UniqueMapVisitor(PhantomData))
}

fn deserialize_optional_unique_btree_map<'de, D, T>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<UniqueMap<T>>::deserialize(deserializer).map(|value| value.map(|map| map.0))
}

struct UniqueMap<T>(BTreeMap<String, T>);

impl<'de, T> Deserialize<'de> for UniqueMap<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_unique_btree_map(deserializer).map(Self)
    }
}
