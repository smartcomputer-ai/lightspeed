//! Generic auth registry contracts: grants, secrets, and the token broker.
//!
//! This crate owns provider-independent control-plane models and store traits
//! for the P69 auth substrate. Concrete persistence adapters, such as
//! `store-pg`, implement these traits outside this crate; OAuth and provider
//! drivers arrive in later milestones. Secret values only ever cross these
//! boundaries wrapped in [`SecretValue`], whose `Debug` output is redacted.

use engine::{StringIdError, validate_general_string_id};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

macro_rules! auth_string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                let value = value.into();
                Self::try_new(value)
                    .unwrap_or_else(|error| panic!("invalid {}: {error}", stringify!($name)))
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, StringIdError> {
                let value = value.into();
                validate_general_string_id(stringify!($name), &value)?;
                Ok(Self(value))
            }

            pub fn parse(value: impl Into<String>) -> Result<Self, StringIdError> {
                Self::try_new(value)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = StringIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = StringIdError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl FromStr for $name {
            type Err = StringIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

auth_string_id!(AuthGrantId);
auth_string_id!(SecretId);

mod broker;
mod grants;
mod memory;
mod secrets;

pub use broker::{
    AuthBrokerError, AuthTokenBroker, RegistryTokenBroker, TokenAudience, audience_covers,
};
pub use grants::{
    AuthGrantRecord, AuthGrantStatus, AuthGrantStore, AuthProviderKind, CreateAuthGrantRecord,
    ListAuthGrants, PrincipalKind, PrincipalRef,
};
pub use memory::{InMemoryAuthGrantStore, InMemorySecretStore};
pub use secrets::{
    PutSecretRecord, SECRET_KIND_STATIC_BEARER, SecretRecordMeta, SecretStore, SecretValue,
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthRegistryError {
    #[error("auth grant already exists: {grant_id}")]
    GrantAlreadyExists { grant_id: AuthGrantId },

    #[error("auth grant not found: {grant_id}")]
    GrantNotFound { grant_id: AuthGrantId },

    #[error("secret already exists: {secret_id}")]
    SecretAlreadyExists { secret_id: SecretId },

    #[error("secret not found: {secret_id}")]
    SecretNotFound { secret_id: SecretId },

    #[error("invalid auth registry request: {message}")]
    InvalidInput { message: String },

    #[error("auth registry store failure: {message}")]
    Store { message: String },
}

const AUTH_URL_MAX_LEN: usize = 2048;
const AUTH_COMPONENT_MAX_LEN: usize = 128;

pub(crate) fn validate_nonempty_string(
    name: &'static str,
    value: &str,
) -> Result<(), AuthRegistryError> {
    if value.is_empty() {
        return Err(AuthRegistryError::InvalidInput {
            message: format!("{name} must not be empty"),
        });
    }
    Ok(())
}

pub(crate) fn validate_nonempty_optional(
    name: &'static str,
    value: Option<&str>,
) -> Result<(), AuthRegistryError> {
    if let Some(value) = value {
        validate_nonempty_string(name, value)?;
    }
    Ok(())
}

pub(crate) fn validate_nonnegative_i64(
    value: i64,
    name: &'static str,
) -> Result<(), AuthRegistryError> {
    if value < 0 {
        return Err(AuthRegistryError::InvalidInput {
            message: format!("{name} must be nonnegative: {value}"),
        });
    }
    Ok(())
}

pub(crate) fn validate_token_component(
    name: &'static str,
    value: &str,
) -> Result<(), AuthRegistryError> {
    validate_nonempty_string(name, value)?;
    if value.len() > AUTH_COMPONENT_MAX_LEN {
        return Err(AuthRegistryError::InvalidInput {
            message: format!(
                "{name} is too long: {} bytes, max {}",
                value.len(),
                AUTH_COMPONENT_MAX_LEN
            ),
        });
    }
    if value.chars().any(char::is_whitespace) || value.chars().any(|ch| ch.is_control()) {
        return Err(AuthRegistryError::InvalidInput {
            message: format!("{name} must not contain whitespace or control characters"),
        });
    }
    Ok(())
}

/// Validate an audience/resource identifier: an absolute http(s) URL without
/// credentials, fragments, whitespace, or control characters.
pub(crate) fn validate_audience_url(value: &str) -> Result<(), AuthRegistryError> {
    if value.is_empty() {
        return Err(AuthRegistryError::InvalidInput {
            message: "audience URL must not be empty".to_owned(),
        });
    }
    if value.len() > AUTH_URL_MAX_LEN {
        return Err(AuthRegistryError::InvalidInput {
            message: format!(
                "audience URL is too long: {} bytes, max {}",
                value.len(),
                AUTH_URL_MAX_LEN
            ),
        });
    }
    if value.chars().any(char::is_whitespace) || value.chars().any(|ch| ch.is_control()) {
        return Err(AuthRegistryError::InvalidInput {
            message: "audience URL must not contain whitespace or control characters".to_owned(),
        });
    }
    if value.contains('#') {
        return Err(AuthRegistryError::InvalidInput {
            message: "audience URL must not contain a fragment".to_owned(),
        });
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return Err(AuthRegistryError::InvalidInput {
            message: "audience URL must include http:// or https:// scheme".to_owned(),
        });
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(AuthRegistryError::InvalidInput {
            message: format!("audience URL scheme {scheme:?} is not supported"),
        });
    }
    let authority_end = rest
        .find(|ch| matches!(ch, '/' | '?' | '#'))
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err(AuthRegistryError::InvalidInput {
            message: "audience URL host must not be empty".to_owned(),
        });
    }
    if authority.contains('@') {
        return Err(AuthRegistryError::InvalidInput {
            message: "audience URL must not include credentials".to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_scopes(values: &[String]) -> Result<(), AuthRegistryError> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        validate_token_component("scope", value)?;
        if !seen.insert(value.as_str()) {
            return Err(AuthRegistryError::InvalidInput {
                message: format!("duplicate scope {value}"),
            });
        }
    }
    Ok(())
}
