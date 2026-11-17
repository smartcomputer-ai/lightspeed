//! Runtime-owned universe settings. Account membership and grants belong to Platform.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UniverseConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Replaces the complete metadata map; omission leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    /// Deletion is refused while enabled. Cannot be enabled while an expiry exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected: Option<bool>,
}
impl UniverseConfiguration {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.display_name.as_ref().is_some_and(|s| {
            s.trim().is_empty()
                || s.chars().count() > 120
                || s.trim() != s
                || s.chars().any(char::is_control)
        }) {
            return Err(
                "displayName must be 1-120 characters without surrounding whitespace or controls",
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn resource_patch_preserves_omission_and_rejects_access_policy() {
        let empty: UniverseConfiguration = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, UniverseConfiguration::default());
        let explicit: UniverseConfiguration =
            serde_json::from_value(json!({"metadata":{},"protected":false})).unwrap();
        assert_eq!(explicit.metadata, Some(BTreeMap::new()));
        assert_eq!(explicit.protected, Some(false));
        for value in [json!({"members":[]}), json!({"directoryGroupMappings":[]})] {
            assert!(serde_json::from_value::<UniverseConfiguration>(value).is_err());
        }
    }
    #[test]
    fn expiry_requires_a_value_or_explicit_null() {
        assert!(
            serde_json::from_value::<crate::DeploymentUniverseExpiryPutParams>(
                json!({"universe":"u"})
            )
            .is_err()
        );
        assert_eq!(
            serde_json::from_value::<crate::DeploymentUniverseExpiryPutParams>(
                json!({"universe":"u","expiresAtMs":null})
            )
            .unwrap()
            .expires_at_ms,
            None
        );
        assert_eq!(
            serde_json::from_value::<crate::DeploymentUniverseExpiryPutParams>(
                json!({"universe":"u","expiresAtMs":42})
            )
            .unwrap()
            .expires_at_ms,
            Some(42)
        );
    }
}
