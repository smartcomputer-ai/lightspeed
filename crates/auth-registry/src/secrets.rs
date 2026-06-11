use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{AuthRegistryError, SecretId, validate_token_component};

pub const SECRET_KIND_STATIC_BEARER: &str = "auth.static_bearer";
pub const SECRET_KIND_MODEL_API_KEY: &str = "auth.model.api_key";

/// An in-memory secret value. `Debug` output is redacted and the type is
/// deliberately not serializable, so values cannot leak through derived
/// logging or accidental persistence. Read the value with [`SecretValue::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

impl From<String> for SecretValue {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretValue {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRecordMeta {
    pub secret_id: SecretId,
    pub secret_kind: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Input for storing a new secret. Not serializable and `Debug`-redacted via
/// the wrapped [`SecretValue`].
#[derive(Clone)]
pub struct PutSecretRecord {
    pub secret_id: SecretId,
    pub secret_kind: String,
    pub value: SecretValue,
    pub created_at_ms: i64,
}

impl PutSecretRecord {
    pub fn validate(&self) -> Result<(), AuthRegistryError> {
        validate_token_component("secret kind", &self.secret_kind)?;
        if self.value.is_empty() {
            return Err(AuthRegistryError::InvalidInput {
                message: "secret value must not be empty".to_owned(),
            });
        }
        crate::validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        Ok(())
    }
}

impl fmt::Debug for PutSecretRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PutSecretRecord")
            .field("secret_id", &self.secret_id)
            .field("secret_kind", &self.secret_kind)
            .field("value", &self.value)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

/// Encrypted-at-rest secret storage. Adapters must encrypt values before
/// persisting them and must never log or serialize plaintext.
#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn put_secret(
        &self,
        record: PutSecretRecord,
    ) -> Result<SecretRecordMeta, AuthRegistryError>;

    async fn read_secret(
        &self,
        secret_id: &SecretId,
    ) -> Result<(SecretRecordMeta, SecretValue), AuthRegistryError>;

    async fn delete_secret(&self, secret_id: &SecretId) -> Result<(), AuthRegistryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_values_redact_debug_output() {
        let value = SecretValue::new("super-secret-token");

        let debug = format!("{value:?}");

        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("<redacted>"));
        assert_eq!(value.expose(), "super-secret-token");
    }

    #[test]
    fn put_secret_records_redact_debug_output() {
        let record = PutSecretRecord {
            secret_id: SecretId::new("authsec_1"),
            secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
            value: SecretValue::new("super-secret-token"),
            created_at_ms: 10,
        };

        let debug = format!("{record:?}");

        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("authsec_1"));
    }

    #[test]
    fn put_secret_records_reject_empty_values() {
        let record = PutSecretRecord {
            secret_id: SecretId::new("authsec_1"),
            secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
            value: SecretValue::new(""),
            created_at_ms: 10,
        };

        assert!(matches!(
            record.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }
}
