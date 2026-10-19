// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Adapts the runner's shared secret resolver to metadata-scoped tool sessions.

use std::sync::Arc;

use baml_rt_tools::{ToolSecretResolver, ToolSecretValue};

use crate::SecretResolver;

#[derive(Clone)]
pub struct SecretResolverToToolAdapter {
    inner: Arc<dyn SecretResolver>,
}

impl SecretResolverToToolAdapter {
    pub fn new(inner: Arc<dyn SecretResolver>) -> Self {
        Self { inner }
    }
}

impl ToolSecretResolver for SecretResolverToToolAdapter {
    fn resolve_tool_secret(&self, name: &str) -> Option<ToolSecretValue> {
        self.inner
            .resolve_credential(name)
            .map(|value| ToolSecretValue::new(value.into_string()))
    }
}
