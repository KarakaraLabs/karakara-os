// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AgentDiscoveryEntry {
    pub agent_card: AgentCard,
}

#[derive(Debug, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub agent_package: String,
}
