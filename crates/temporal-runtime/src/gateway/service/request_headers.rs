//! Configured request headers at the API boundary.
//!
//! MCP servers and model endpoints carry the same headers: a written value or
//! a credential reference. The conversions and the credential rule are shared;
//! each owner passes its own [`credentials::RequestHeaderPolicy`].

use super::*;

/// The stored form of a header map as a client wrote it: names lowercased,
/// two spellings of one name refused, and every shared and owner rule checked
/// before anything is read or written.
pub(super) fn request_headers_from_api(
    policy: &credentials::RequestHeaderPolicy,
    headers: BTreeMap<String, api::RequestHeader>,
) -> Result<credentials::RequestHeaders, AgentApiError> {
    let mut values = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        let value = match value {
            api::RequestHeader::Literal { value } => {
                credentials::RequestHeaderValue::Literal { value }
            }
            api::RequestHeader::Credential { grant_id } => {
                credentials::RequestHeaderValue::Credential {
                    grant_id: parse_credential_grant_id(grant_id)?,
                }
            }
        };
        values.push((name, value));
    }
    let headers = policy.normalize(values).map_err(map_request_header_error)?;
    policy
        .validate(&headers)
        .map_err(map_request_header_error)?;
    Ok(headers)
}

/// Reads return header names, written values and credential ids, never a
/// token.
pub(super) fn request_headers_view(
    headers: credentials::RequestHeaders,
) -> BTreeMap<String, api::RequestHeader> {
    headers
        .into_iter()
        .map(|(name, value)| {
            let value = match value {
                credentials::RequestHeaderValue::Literal { value } => {
                    api::RequestHeader::Literal { value }
                }
                credentials::RequestHeaderValue::Credential { grant_id } => {
                    api::RequestHeader::Credential {
                        grant_id: grant_id.as_str().to_owned(),
                    }
                }
            };
            (name, value)
        })
        .collect()
}

/// Whether `grant` may be named by `header`, whose owner sends it to
/// `target_url`.
pub(super) fn validate_header_credential(
    policy: &credentials::RequestHeaderPolicy,
    header: &str,
    target_url: &str,
    grant: &credentials::CredentialGrantRecord,
) -> Result<(), AgentApiError> {
    policy
        .check_credential(header, target_url, grant)
        .map_err(|error| AgentApiError::rejected(error.to_string()))
}

fn map_request_header_error(error: credentials::RequestHeaderError) -> AgentApiError {
    AgentApiError::invalid_request(error.to_string())
}
