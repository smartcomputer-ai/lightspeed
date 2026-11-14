//! Desired Platform presentation and access policy. Runtime stores declarations;
//! Platform enforces them independently of Runtime API-key authority.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UniverseConfiguration {
    /// Runtime-owned name. Named universes are adopted automatically by Platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Declared grants, independent of manual memberships. Omit to leave unmanaged;
    /// an empty list clears declared members. Email selectors require verified email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<DeclaredUniverseMember>>,
    /// Exact issuer-qualified directory groups. Omit to leave unchanged;
    /// an empty list clears declared group grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory_group_mappings: Option<Vec<UniverseDirectoryGroupMapping>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum UniverseMemberRole {
    Viewer,
    Contributor,
    Operator,
    Admin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclaredUniverseMember {
    /// Select by verified email, or by issuer and subject, never both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub role: UniverseMemberRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UniverseDirectoryGroupMapping {
    pub issuer: String,
    pub group: String,
    pub role: UniverseMemberRole,
}

impl UniverseConfiguration {
    /// Validate before persisting or planning a declaration. No identity I/O.
    pub fn validate(&self) -> Result<(), &'static str> {
        let text =
            |s: &str| !s.trim().is_empty() && s.len() <= 2048 && !s.chars().any(char::is_control);
        if self
            .display_name
            .as_ref()
            .is_some_and(|s| !text(s) || s.chars().count() > 120 || s.trim() != s)
        {
            return Err(
                "displayName must be 1-120 characters without surrounding whitespace or controls",
            );
        }
        let mut seen = BTreeSet::new();
        if let Some(members) = &self.members {
            if members.len() > 1000 {
                return Err("at most 1000 members may be declared");
            }
            for member in members {
                let selector = match (&member.email, &member.issuer, &member.subject) {
                    (Some(email), None, None)
                        if text(email)
                            && email.contains('@')
                            && !email.chars().any(char::is_whitespace) =>
                    {
                        ("email", email.to_lowercase(), String::new())
                    }
                    (None, Some(issuer), Some(subject)) if text(issuer) && text(subject) => {
                        ("subject", issuer.clone(), subject.clone())
                    }
                    _ => return Err("a member requires either email or both issuer and subject"),
                };
                if !seen.insert(selector) {
                    return Err("duplicate member selector");
                }
            }
        }
        let mut seen = BTreeSet::new();
        if let Some(mappings) = &self.directory_group_mappings {
            if mappings.len() > 1000 {
                return Err("at most 1000 directory group mappings may be declared");
            }
            for mapping in mappings {
                if !text(&mapping.issuer) || !text(&mapping.group) {
                    return Err("directory mappings require nonempty issuer and group");
                }
                if !seen.insert((&mapping.issuer, &mapping.group)) {
                    return Err("duplicate directory group mapping");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_identity_selectors_and_preserves_unmanaged_sections() {
        let config: UniverseConfiguration = serde_json::from_value(json!({"displayName":"Research", "members":[{"issuer":"https://id.example", "subject":"owner", "role":"admin"}]})).unwrap();
        assert!(config.validate().is_ok());
        assert!(config.directory_group_mappings.is_none());
        for invalid in [
            json!({"displayName":" "}),
            json!({"members":[{"email":"owner@example.com", "issuer":"id", "subject":"a", "role":"admin"}]}),
            json!({"members":[{"email":"A@example.com", "role":"admin"},{"email":"a@example.com", "role":"viewer"}]}),
            json!({"directoryGroupMappings":[{"issuer":"id", "group":"", "role":"viewer"}]}),
        ] {
            assert!(
                serde_json::from_value::<UniverseConfiguration>(invalid)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<UniverseConfiguration>(
                json!({"members":[{"email":"a@b", "role":"superadmin"}]})
            )
            .is_err()
        );
    }
}
