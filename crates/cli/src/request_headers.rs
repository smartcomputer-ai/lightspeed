//! The `--header` and `--header-credential` flags shared by the commands that
//! configure extra request headers: MCP servers and model provider endpoints.

use std::collections::BTreeMap;

use anyhow::Result;

/// `--header` and `--header-credential` values as a header map. Names are
/// sent as given; the server lowercases them and applies the reserved names
/// of the server or endpoint the headers belong to.
pub(crate) fn request_headers_from_args(
    literals: &[String],
    credentials: &[String],
) -> Result<BTreeMap<String, api::RequestHeader>> {
    let mut headers = BTreeMap::new();
    let entries = literals
        .iter()
        .map(|entry| (entry, "--header", false))
        .chain(
            credentials
                .iter()
                .map(|entry| (entry, "--header-credential", true)),
        );
    for (entry, flag, credential) in entries {
        let Some((name, value)) = entry.split_once('=') else {
            anyhow::bail!("{flag} expects NAME=VALUE, got {entry:?}");
        };
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("{flag} needs a header name: {entry:?}");
        }
        let header = if credential {
            api::RequestHeader::Credential {
                grant_id: value.trim().to_owned(),
            }
        } else {
            // The value is never echoed: it is the secret being steered away.
            if api::is_credential_only_header(name) {
                anyhow::bail!(
                    "header {name:?} carries a secret, and a plain value is shown to everyone \
                     who can read this configuration: save the value as a token credential and \
                     pass --header-credential {name}=GRANT_ID"
                );
            }
            api::RequestHeader::Literal {
                value: value.to_owned(),
            }
        };
        if headers
            .keys()
            .any(|existing: &String| existing.eq_ignore_ascii_case(name))
        {
            anyhow::bail!("header {name:?} is given more than once");
        }
        headers.insert(name.to_owned(), header);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn flags_build_literal_and_credential_headers() {
        let headers = request_headers_from_args(
            &args(&["X-Upstream-Url=https://tracker.example.com/?a=b"]),
            &args(&["X-Upstream-Token= authgrant_upstream "]),
        )
        .expect("headers");
        // A literal value keeps everything after the first `=`.
        assert_eq!(
            headers.get("X-Upstream-Url"),
            Some(&api::RequestHeader::Literal {
                value: "https://tracker.example.com/?a=b".to_owned()
            })
        );
        assert_eq!(
            headers.get("X-Upstream-Token"),
            Some(&api::RequestHeader::Credential {
                grant_id: "authgrant_upstream".to_owned()
            })
        );
    }

    #[test]
    fn flags_refuse_malformed_and_repeated_names() {
        assert!(request_headers_from_args(&args(&["no-equals"]), &[]).is_err());
        assert!(request_headers_from_args(&args(&["=value"]), &[]).is_err());
        assert!(
            request_headers_from_args(&args(&["X-Tenant=a"]), &args(&["x-tenant=authgrant_1"]))
                .is_err(),
            "one header cannot have two value sources"
        );
    }

    #[test]
    fn a_name_that_carries_a_secret_takes_only_a_credential() {
        for name in ["api-key", "X-Api-Key", "Proxy-Authorization"] {
            let error = request_headers_from_args(&[format!("{name}=sk-plain")], &[])
                .expect_err("a plain secret")
                .to_string();
            assert!(error.contains("--header-credential"), "{error}");
            assert!(!error.contains("sk-plain"), "{error}");

            request_headers_from_args(&[], &[format!("{name}=authgrant_1")])
                .expect("a credential under the same name");
        }
    }
}
