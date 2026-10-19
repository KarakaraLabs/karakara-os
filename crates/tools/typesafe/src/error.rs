// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

use std::fmt;

use baml_rt_core::{BamlRtError, ClassifiedToolError, ErrorDisposition};

/// Typed failures returned by the TypeSafe System One adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemOneError {
    InvalidArgument { message: String },
    Disabled { message: String },
    MissingCredential,
    Unauthorized,
    CapacityExceeded,
    QuotaExceeded,
    ProviderRateLimited { retry_after_ms: Option<u64> },
    ProviderUnavailable { message: String },
    ProviderContract { message: String },
}

impl SystemOneError {
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::InvalidArgument {
            message: message.into(),
        }
    }

    pub fn disabled(message: impl Into<String>) -> Self {
        Self::Disabled {
            message: message.into(),
        }
    }

    pub fn provider_unavailable(message: impl Into<String>) -> Self {
        Self::ProviderUnavailable {
            message: message.into(),
        }
    }

    pub fn provider_contract(message: impl Into<String>) -> Self {
        Self::ProviderContract {
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::Disabled { .. } => "typesafe_disabled",
            Self::MissingCredential => "typesafe_missing_credential",
            Self::Unauthorized => "typesafe_unauthorized",
            Self::CapacityExceeded => "typesafe_capacity_exceeded",
            Self::QuotaExceeded => "typesafe_quota_exceeded",
            Self::ProviderRateLimited { .. } => "typesafe_rate_limited",
            Self::ProviderUnavailable { .. } => "typesafe_provider_unavailable",
            Self::ProviderContract { .. } => "typesafe_provider_contract",
        }
    }

    pub fn disposition(&self) -> ErrorDisposition {
        match self {
            Self::InvalidArgument { .. } => ErrorDisposition::LlmCorrectable,
            Self::Disabled { .. }
            | Self::MissingCredential
            | Self::Unauthorized
            | Self::CapacityExceeded
            | Self::QuotaExceeded
            | Self::ProviderRateLimited { .. }
            | Self::ProviderUnavailable { .. }
            | Self::ProviderContract { .. } => ErrorDisposition::InformAndContinue,
        }
    }

    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::ProviderRateLimited { retry_after_ms } => *retry_after_ms,
            _ => None,
        }
    }

    pub fn into_classified(self) -> ClassifiedToolError {
        ClassifiedToolError {
            code: self.code().to_string(),
            disposition: self.disposition(),
            message: self.to_string(),
            hint: None,
            retry_after_ms: self.retry_after_ms(),
        }
    }
}

impl fmt::Display for SystemOneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument { message }
            | Self::Disabled { message }
            | Self::ProviderUnavailable { message }
            | Self::ProviderContract { message } => f.write_str(message),
            Self::MissingCredential => f.write_str("TypeSafe API credential is unavailable"),
            Self::Unauthorized => f.write_str("TypeSafe rejected the configured API credential"),
            Self::CapacityExceeded => {
                f.write_str("TypeSafe System One local concurrency capacity was not available")
            }
            Self::QuotaExceeded => {
                f.write_str("TypeSafe System One local token budget was exhausted")
            }
            Self::ProviderRateLimited { .. } => {
                f.write_str("TypeSafe System One provider rate limit was reached")
            }
        }
    }
}

impl std::error::Error for SystemOneError {}

impl From<SystemOneError> for ClassifiedToolError {
    fn from(error: SystemOneError) -> Self {
        error.into_classified()
    }
}

impl From<SystemOneError> for BamlRtError {
    fn from(error: SystemOneError) -> Self {
        Self::ToolClassified(error.into_classified())
    }
}
