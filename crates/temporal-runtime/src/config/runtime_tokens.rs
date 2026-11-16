//! Deployment credentials for trusted API callers. Names provide attribution only.
use std::{collections::BTreeMap, fmt};

use anyhow::{Context, bail};
use subtle::ConstantTimeEq;

#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeTokens(BTreeMap<String, String>);

impl fmt::Debug for RuntimeTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeTokens")
            .field("names", &self.0.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl RuntimeTokens {
    pub fn from_env() -> anyhow::Result<Self> {
        let mut entries = Vec::new();
        for (name, value) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("LIGHTSPEED_API_TOKEN") {
                let name = name
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("invalid API token variable name"))?;
                let value = value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("{name} must contain UTF-8"))?;
                entries.push((name, value));
            }
        }
        Self::from_configuration(entries, |path| {
            std::fs::read_to_string(path).map_err(Into::into)
        })
    }

    /// Parse an explicit environment snapshot; tests need no process-global mutation.
    pub fn from_configuration(
        entries: impl IntoIterator<Item = (String, String)>,
        read_file: impl Fn(&str) -> anyhow::Result<String>,
    ) -> anyhow::Result<Self> {
        let mut tokens = BTreeMap::new();
        for (variable, value) in entries {
            let Some(suffix) = variable.strip_prefix("LIGHTSPEED_API_TOKEN") else {
                continue;
            };
            let (suffix, file) = if suffix == "_FILE" {
                ("", true)
            } else if let Some(name) = suffix.strip_suffix("_FILE") {
                (name, true)
            } else {
                (suffix, false)
            };
            let name = if suffix.is_empty() {
                "default".to_owned()
            } else {
                let Some(name) = suffix.strip_prefix('_').filter(|name| !name.is_empty()) else {
                    bail!("invalid API token variable: {variable}")
                };
                if !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                    bail!("invalid API token name in {variable}");
                }
                name.to_ascii_lowercase()
            };
            let secret = if file {
                // Mounted text secrets commonly end with a newline. Other bytes are significant.
                let contents = read_file(&value)
                    .with_context(|| format!("cannot read secret file for {variable}"))?;
                contents
                    .strip_suffix("\r\n")
                    .or_else(|| contents.strip_suffix('\n'))
                    .unwrap_or(&contents)
                    .to_owned()
            } else {
                value
            };
            if secret.chars().count() < 32 {
                bail!("{variable} must supply a secret of at least 32 characters");
            }
            if !secret.bytes().all(|c| (b'!'..=b'~').contains(&c)) {
                bail!("{variable} must supply a non-whitespace printable ASCII HTTP bearer token");
            }
            if tokens.values().any(|previous| previous == &secret) {
                bail!("{variable} duplicates another API token secret");
            }
            if tokens.insert(name.clone(), secret).is_some() {
                bail!("API token {name} has multiple configuration sources");
            }
        }
        if tokens.is_empty() {
            bail!("authenticated mode requires LIGHTSPEED_API_TOKEN or a named API token");
        }
        Ok(Self(tokens))
    }

    pub fn authenticate(&self, secret: &str) -> Option<&str> {
        let mut matched = None;
        for (name, expected) in &self.0 {
            if bool::from(expected.as_bytes().ct_eq(secret.as_bytes())) {
                matched = Some(name.as_str());
            }
        }
        matched
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SECRET: &str = "0123456789abcdef0123456789abcdef";
    fn parse(entries: &[(&str, &str)]) -> anyhow::Result<RuntimeTokens> {
        RuntimeTokens::from_configuration(
            entries.iter().map(|(k, v)| (k.to_string(), v.to_string())),
            |_| bail!("unavailable"),
        )
    }
    #[test]
    fn simple_and_named_tokens_authenticate_without_revealing_secrets() {
        let tokens = parse(&[
            ("LIGHTSPEED_API_TOKEN", SECRET),
            (
                "LIGHTSPEED_API_TOKEN_PLATFORM",
                "platform-secret-with-at-least-32-characters",
            ),
        ])
        .unwrap();
        assert_eq!(tokens.authenticate(SECRET), Some("default"));
        assert_eq!(
            tokens.authenticate("platform-secret-with-at-least-32-characters"),
            Some("platform")
        );
        assert_eq!(tokens.authenticate("unknown"), None);
        assert!(!format!("{tokens:?}").contains(SECRET));
    }
    #[test]
    fn mounted_secrets_remove_only_line_endings() {
        let tokens = RuntimeTokens::from_configuration(
            [(
                "LIGHTSPEED_API_TOKEN_OPERATOR_FILE".into(),
                "/secret".into(),
            )],
            |path| {
                assert_eq!(path, "/secret");
                Ok(format!("{SECRET}\r\n"))
            },
        )
        .unwrap();
        assert_eq!(tokens.authenticate(SECRET), Some("operator"));
    }
    #[test]
    fn invalid_or_ambiguous_configuration_fails_closed_without_secrets() {
        for entries in [
            vec![],
            vec![("LIGHTSPEED_API_TOKEN", "short")],
            vec![("LIGHTSPEED_API_TOKEN", "                                ")],
            vec![("LIGHTSPEED_API_TOKEN", "0123456789abcdef0123456789abcdef ")],
            vec![("LIGHTSPEED_API_TOKEN_FILE", "/missing")],
            vec![
                ("LIGHTSPEED_API_TOKEN", SECRET),
                (
                    "LIGHTSPEED_API_TOKEN_DEFAULT",
                    "another-secret-with-at-least-32-characters",
                ),
            ],
            vec![
                ("LIGHTSPEED_API_TOKEN_A", SECRET),
                ("LIGHTSPEED_API_TOKEN_B", SECRET),
            ],
            vec![("LIGHTSPEED_API_TOKEN_", SECRET)],
            vec![("LIGHTSPEED_API_TOKEN", "0123456789abcdef0123456789abcdef\n")],
        ] {
            let error = parse(&entries).unwrap_err().to_string();
            assert!(!error.contains(SECRET));
        }
        assert!(
            RuntimeTokens::from_configuration(
                [
                    ("LIGHTSPEED_API_TOKEN_A".into(), SECRET.into()),
                    ("LIGHTSPEED_API_TOKEN_A_FILE".into(), "/secret".into())
                ],
                |_| Ok("another-secret-with-at-least-32-characters".into())
            )
            .is_err()
        );
    }
}
