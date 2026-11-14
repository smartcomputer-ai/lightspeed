//! Configured request headers at the API boundary.
//!
//! MCP servers and model endpoints carry the same headers: a written value or
//! a credential reference. The conversions and the credential rule are shared;
//! each owner passes its own [`auth::RequestHeaderPolicy`].

use super::*;

/// The stored form of a header map as a client wrote it: names lowercased,
/// two spellings of one name refused, and every shared and owner rule checked
/// before anything is read or written.
pub(super) fn request_headers_from_api(
    policy: &auth::RequestHeaderPolicy,
    headers: BTreeMap<String, api::RequestHeader>,
) -> Result<auth::RequestHeaders, AgentApiError> {
    let mut values = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        let value = match value {
            api::RequestHeader::Literal { value } => auth::RequestHeaderValue::Literal { value },
            api::RequestHeader::Credential { grant_id } => auth::RequestHeaderValue::Credential {
                grant_id: parse_auth_grant_id(grant_id)?,
            },
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
    headers: auth::RequestHeaders,
) -> BTreeMap<String, api::RequestHeader> {
    headers
        .into_iter()
        .map(|(name, value)| {
            let value = match value {
                auth::RequestHeaderValue::Literal { value } => {
                    api::RequestHeader::Literal { value }
                }
                auth::RequestHeaderValue::Credential { grant_id } => {
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
    policy: &auth::RequestHeaderPolicy,
    header: &str,
    target_url: &str,
    grant: &auth::AuthGrantRecord,
) -> Result<(), AgentApiError> {
    policy
        .check_credential(header, target_url, grant)
        .map_err(|error| AgentApiError::rejected(error.to_string()))
}

fn map_request_header_error(error: auth::RequestHeaderError) -> AgentApiError {
    AgentApiError::invalid_request(error.to_string())
}
