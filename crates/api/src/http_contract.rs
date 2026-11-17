//! Resource HTTP bindings shared by the runtime router and contract exporter.
use crate::*;
use schemars::generate::SchemaSettings;

pub const HTTP_CONTRACT_REVISION: &str = "1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum HttpVerb {
    GET,
    POST,
    PUT,
    PATCH,
    DELETE,
}
impl HttpVerb {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GET => "GET",
            Self::POST => "POST",
            Self::PUT => "PUT",
            Self::PATCH => "PATCH",
            Self::DELETE => "DELETE",
        }
    }
    pub const fn has_json_body(self) -> bool {
        matches!(self, Self::POST | Self::PUT | Self::PATCH)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HttpTransfer {
    Json,
    Sse,
    BinaryDownload,
    BinaryUpload,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HttpSuccess {
    Ok,
    Created,
    CreatedFlag,
}
impl HttpSuccess {
    pub const fn statuses(self) -> &'static [u16] {
        match self {
            Self::Ok => &[200],
            Self::Created => &[201],
            Self::CreatedFlag => &[200, 201],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HttpPathField {
    pub parameter: &'static str,
    /// Dot-separated path into the service input; never accepted from the body.
    pub field: &'static str,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpBinding {
    pub verb: HttpVerb,
    pub path: &'static str,
    pub path_fields: &'static [HttpPathField],
    pub query_fields: &'static [&'static str],
    pub transfer: HttpTransfer,
    pub success: HttpSuccess,
}

/// Connection discovery through the resource HTTP API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionResponse {
    pub runtime: RuntimeKind,
    pub contract_revision: String,
    pub server_info: ServerInfo,
    pub caller: CallerAccess,
    /// Default URL universe for a gateway configured with a single universe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_universe: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeKind {
    Runtime,
    Platform,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlobUploadParams {
    pub blob_ref: String,
}
pub const fn blob_upload_binding() -> HttpBinding {
    HttpBinding {
        verb: HttpVerb::PUT,
        path: "/api/v1/universes/{universe}/blobs/{blobRef}/content",
        path_fields: &[HttpPathField {
            parameter: "blobRef",
            field: "blobRef",
        }],
        query_fields: &[],
        transfer: HttpTransfer::BinaryUpload,
        success: HttpSuccess::Ok,
    }
}

/// Stable public HTTP error body. Service error details are retained as typed
/// extensions; the runtime redacts internal failures before constructing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HttpProblem {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    pub kind: AgentApiErrorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_default_slot: Option<ModelDefaultSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_universe: Option<Box<DeletedUniverseView>>,
}
impl HttpProblem {
    pub fn from_error(error: AgentApiError, status: u16, title: impl Into<String>) -> Self {
        Self {
            problem_type: "about:blank".into(),
            title: title.into(),
            status,
            detail: error.message,
            kind: error.kind,
            model_default_slot: error.model_default_slot,
            submission_id: error.submission_id,
            deleted_universe: error.deleted_universe,
        }
    }
}

/// Input shape used only for parameter codecs and rejecting unknown fields.
/// Typed Serde extraction and service validation remain authoritative.
pub fn http_input_schema<T: JsonSchema>() -> Value {
    serde_json::to_value(
        SchemaSettings::draft07()
            .into_generator()
            .into_root_schema_for::<T>(),
    )
    .expect("input schema serializes")
}
/// Follow local references without expanding recursive definitions.
pub fn http_resolve_schema<'a>(root: &'a Value, mut schema: &'a Value) -> &'a Value {
    for _ in 0..64 {
        if let Some(reference) = schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|s| s.strip_prefix('#'))
            && let Some(target) = root.pointer(reference)
        {
            schema = target;
            continue;
        }
        if let Some(all) = schema.get("allOf").and_then(Value::as_array)
            && all.len() == 1
            && schema.get("properties").is_none()
        {
            schema = &all[0];
            continue;
        }
        break;
    }
    schema
}
pub fn http_field_schema<'a>(root: &'a Value, field: &str) -> Option<&'a Value> {
    let mut current = http_resolve_schema(root, root);
    for part in field.split('.') {
        current = http_resolve_schema(root, current)
            .get("properties")?
            .get(part)?;
    }
    Some(http_resolve_schema(root, current))
}

pub(crate) fn canonical_type_name<T>() -> &'static str {
    std::any::type_name::<T>()
        .rsplit("::")
        .next()
        .expect("type name")
}

/// An HTTP request assembled from the canonical operation catalog. Binary upload
/// bytes are supplied separately by the caller; JSON bodies contain no URL targets.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequestParts {
    pub method: HttpVerb,
    pub path: String,
    pub body: Option<Value>,
    pub transfer: HttpTransfer,
}

pub fn http_request(
    operation: &str,
    universe: Option<&str>,
    mut params: Value,
) -> Result<HttpRequestParts, AgentApiError> {
    let binding = crate::schema_export::full_method_manifest()
        .into_iter()
        .find(|spec| spec.method == operation)
        .map(|spec| spec.http)
        .ok_or_else(|| AgentApiError::invalid_request(format!("unknown operation: {operation}")))?;
    if !params.is_object() {
        return Err(AgentApiError::invalid_request(
            "operation parameters must be an object",
        ));
    }
    fn take(value: &mut Value, field: &str) -> Option<Value> {
        if let Some((head, rest)) = field.split_once('.') {
            let object = value.as_object_mut()?;
            let child = object.get_mut(head)?;
            let taken = take(child, rest);
            if child.as_object().is_some_and(|v| v.is_empty()) {
                object.remove(head);
            }
            taken
        } else {
            value.as_object_mut()?.remove(field)
        }
    }
    fn encode(value: &str) -> String {
        let mut encoded = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                encoded.push(char::from(byte));
            } else {
                use std::fmt::Write as _;
                write!(&mut encoded, "%{byte:02X}").expect("write string");
            }
        }
        encoded
    }
    fn scalar(value: &Value) -> String {
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string())
    }
    let mut path = binding.path.to_owned();
    for mapping in binding.path_fields {
        let value = take(&mut params, mapping.field).ok_or_else(|| {
            AgentApiError::invalid_request(format!("missing path parameter: {}", mapping.field))
        })?;
        if !value.is_string() && !value.is_number() {
            return Err(AgentApiError::invalid_request(format!(
                "invalid path parameter: {}",
                mapping.field
            )));
        }
        path = path.replace(
            &format!("{{{}}}", mapping.parameter),
            &encode(&scalar(&value)),
        );
    }
    if path.contains("{universe}") {
        let universe = universe.filter(|v| !v.is_empty()).ok_or_else(|| {
            AgentApiError::invalid_request("a universe is required for this operation")
        })?;
        path = path.replace("{universe}", &encode(universe));
    }
    let mut query = Vec::new();
    for field in binding.query_fields {
        if let Some(value) = take(&mut params, field) {
            let values = match value {
                Value::Array(values) => values,
                value => vec![value],
            };
            for value in values {
                query.push(format!("{}={}", encode(field), encode(&scalar(&value))));
            }
        }
    }
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query.join("&"));
    }
    let body = if binding.verb.has_json_body() && binding.transfer == HttpTransfer::Json {
        Some(params)
    } else if params.as_object().is_some_and(|v| v.is_empty()) {
        None
    } else {
        return Err(AgentApiError::invalid_request(
            "operation has undeclared query parameters",
        ));
    };
    Ok(HttpRequestParts {
        method: binding.verb,
        path,
        body,
        transfer: binding.transfer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_schemas_resolve_nested_fields_without_expanding_recursion() {
        let schema = http_input_schema::<ProfilePutParams>();
        let field = http_field_schema(&schema, "profile.profileId").unwrap();
        assert_eq!(field["type"], "string");
        assert!(http_field_schema(&schema, "profile.missing").is_none());
    }
    #[test]
    fn request_builder_partitions_nested_targets_queries_and_binary_inputs() {
        let request = http_request(
            "profiles/put",
            Some("my universe"),
            serde_json::json!({
                "profile":{"profileId":"id/with spaces", "displayName":"Name"}, "expectedRevision":7
            }),
        )
        .unwrap();
        assert_eq!(request.method, HttpVerb::PUT);
        assert_eq!(
            request.path,
            "/api/v1/universes/my%20universe/profiles/id%2Fwith%20spaces"
        );
        assert_eq!(
            request.body.unwrap(),
            serde_json::json!({"profile":{"displayName":"Name"},"expectedRevision":7})
        );
        let request = http_request(
            "session/list",
            Some("u"),
            serde_json::json!({"trees":["a/b","c"],"metadata":{"team":"A B"},"limit":2}),
        )
        .unwrap();
        assert!(request.path.contains("trees=a%2Fb&trees=c"));
        assert!(
            request
                .path
                .contains("metadata=%7B%22team%22%3A%22A%20B%22%7D")
        );
        assert_eq!(request.body, None);
        let request = http_request(
            METHOD_BLOBS_UPLOAD,
            Some("u"),
            serde_json::json!({"blobRef":"sha256:abc"}),
        )
        .unwrap();
        assert_eq!(request.transfer, HttpTransfer::BinaryUpload);
        assert!(request.path.ends_with("/blobs/sha256%3Aabc/content"));
        assert_eq!(request.body, None);
        assert!(http_request("session/list", None, serde_json::json!({})).is_err());
        assert!(http_request("session/list", Some("u"), serde_json::json!({"typo":true})).is_err());
        assert!(http_request("unknown", None, serde_json::json!({})).is_err());
    }

    #[test]
    fn problem_extensions_retain_submission_identity() {
        let mut error = AgentApiError::operation_pending("Still pending");
        error.submission_id = Some("retry-id".into());
        let problem = HttpProblem::from_error(error, 504, "Gateway Timeout");
        let value = serde_json::to_value(problem).unwrap();
        assert_eq!(value["submissionId"], "retry-id");
        assert_eq!(value["type"], "about:blank");
        assert!(value.get("message").is_none());
    }
}
