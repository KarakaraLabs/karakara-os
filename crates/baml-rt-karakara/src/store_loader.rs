// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Load stored karakara gate config from the config service.

use baml_rt_tools::{BundleName, config_resolver::ConfigResolver};

use crate::config::{KARAKARA_CONFIG_BUNDLE_NAME, KarakaraConfig};

/// Load karakara config from the canonical `karakara` bundle, with defaults on miss/parse failure.
pub async fn load_stored_config(resolver: &dyn ConfigResolver) -> KarakaraConfig {
    let bundle = BundleName::new(KARAKARA_CONFIG_BUNDLE_NAME).expect("karakara bundle name valid");
    match resolver.get_config(&bundle).await {
        Ok(Some(v)) => KarakaraConfig::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "stored karakara config parse failed; using default");
            KarakaraConfig::default()
        }),
        Ok(None) => KarakaraConfig::default(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load karakara config; using default");
            KarakaraConfig::default()
        }
    }
}
