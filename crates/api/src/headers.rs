use super::*;

/// Header names whose value is a secret by convention. A literal value is
/// returned to every reader of the header's owner, so these names accept only
/// a `credential` value. Sorted, lowercase.
pub const CREDENTIAL_ONLY_HEADER_NAMES: &[&str] = &["api-key", "proxy-authorization", "x-api-key"];

/// Whether a header of this name, in any letter case, accepts only a
/// `credential` value.
pub fn is_credential_only_header(name: &str) -> bool {
    CREDENTIAL_ONLY_HEADER_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// Value of one configured request header, as an MCP server and a model
/// provider's endpoint both carry them. A name listed in
/// [`CREDENTIAL_ONLY_HEADER_NAMES`] accepts only a `credential` value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RequestHeader {
    /// Sent as written. Not secret: reads return the value.
    Literal { value: String },
    /// The token of a static-token credential in this universe, resolved at
    /// each request and sent raw, without a scheme prefix.
    Credential { grant_id: String },
}
