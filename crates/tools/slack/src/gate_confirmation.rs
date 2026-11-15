// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Runtime-owned Slack confirmation messages for irreversible actions.

use std::time::Duration;

use baml_rt_core::{BamlRtError, Result};
use serde_json::json;

/// Upper bound on one `chat.postMessage` call. Without it a hung Slack request
/// would stall the gate indefinitely.
const SLACK_POST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct SlackGateConfirmation<'a> {
    pub channel: &'a str,
    pub tool_name: &'a str,
    pub prompt: &'a str,
    pub gate_id: &'a str,
    pub context_id: &'a str,
    pub task_id: &'a str,
    pub agent_id: &'a str,
}

pub async fn post_gate_confirmation(request: SlackGateConfirmation<'_>) -> Result<()> {
    let token = std::env::var("SLACK_BOT_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            BamlRtError::Configuration(
                "SLACK_BOT_TOKEN is required for Slack gate confirmation".to_string(),
            )
        })?;
    let base_url = std::env::var("SLACK_API_BASE_URL")
        .unwrap_or_else(|_| "https://slack.com/api".to_string())
        .trim_end_matches('/')
        .to_string();
    let action_value = json!({
        "gate_id": request.gate_id,
        "context_id": request.context_id,
        "task_id": request.task_id,
        "agent_id": request.agent_id,
    })
    .to_string();
    let body = json!({
        "channel": request.channel,
        "text": format!("Confirm irreversible action: {}", request.tool_name),
        "blocks": [
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("*Irreversible action requested*\n{}", request.prompt)
                }
            },
            {
                "type": "actions",
                "block_id": format!("karakara_gate:{}", request.gate_id),
                "elements": [
                    {
                        "type": "button",
                        "action_id": "karakara_gate_allow",
                        "text": { "type": "plain_text", "text": "Approve" },
                        "style": "primary",
                        "value": action_value
                    },
                    {
                        "type": "button",
                        "action_id": "karakara_gate_deny",
                        "text": { "type": "plain_text", "text": "Deny" },
                        "style": "danger",
                        "value": action_value
                    }
                ]
            }
        ]
    });
    let client = reqwest::Client::builder()
        .timeout(SLACK_POST_TIMEOUT)
        .build()
        .map_err(|error| {
            BamlRtError::ToolExecution(format!("could not build Slack HTTP client: {error}"))
        })?;
    let response = client
        .post(format!("{base_url}/chat.postMessage"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            BamlRtError::ToolExecution(format!("Slack gate confirmation request failed: {error}"))
        })?;
    let status = response.status();
    let value: serde_json::Value = response.json().await.map_err(|error| {
        BamlRtError::ToolExecution(format!(
            "Slack gate confirmation response was invalid: {error}"
        ))
    })?;
    if !status.is_success() || value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(BamlRtError::ToolExecution(format!(
            "Slack rejected gate confirmation: {value}"
        )));
    }
    Ok(())
}
