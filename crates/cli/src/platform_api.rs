//! Native Platform request assembly from its generated shared contract.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Operation {
    operation_id: String,
    method: String,
    path: String,
}
#[derive(Deserialize)]
struct Catalog {
    operations: Vec<Operation>,
}

pub(crate) fn request_parts(operation: &str, params: Value) -> Result<api::HttpRequestParts> {
    let catalog: Catalog =
        serde_json::from_str(include_str!("../../../platform/shared/contract/http.json"))?;
    let descriptor = catalog
        .operations
        .into_iter()
        .find(|entry| entry.operation_id == operation)
        .with_context(|| format!("unknown Platform operation: {operation}"))?;
    let mut fields = params
        .as_object()
        .context("operation params must be an object")?
        .clone();
    let mut path = descriptor.path;
    while let Some(start) = path.find('{') {
        let end = path[start..].find('}').context("invalid generated path")? + start;
        let field = &path[start + 1..end];
        let value = fields
            .remove(field)
            .with_context(|| format!("missing path parameter {field}"))?;
        let value = value.as_str().context("path parameters must be strings")?;
        ensure!(
            !value.is_empty() && value != "." && value != "..",
            "invalid path parameter"
        );
        let encoded = value
            .as_bytes()
            .iter()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(byte) {
                    (*byte as char).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect::<String>();
        path.replace_range(start..=end, &encoded);
    }
    let method = match descriptor.method.as_str() {
        "GET" => api::HttpVerb::GET,
        "POST" => api::HttpVerb::POST,
        "PUT" => api::HttpVerb::PUT,
        "PATCH" => api::HttpVerb::PATCH,
        "DELETE" => api::HttpVerb::DELETE,
        other => bail!("unsupported generated HTTP method: {other}"),
    };
    let body = if method.has_json_body() {
        Some(Value::Object(fields))
    } else {
        if !fields.is_empty() {
            let mut url = reqwest::Url::parse("https://request.invalid")?;
            for (key, value) in fields {
                url.query_pairs_mut().append_pair(
                    &key,
                    &match value {
                        Value::String(value) => value,
                        other => other.to_string(),
                    },
                );
            }
            path.push('?');
            path.push_str(url.query().unwrap_or_default());
        }
        None
    };
    Ok(api::HttpRequestParts {
        method,
        path,
        body,
        transfer: api::HttpTransfer::Json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn native_paths_remove_owned_fields_from_body_and_encode_segments() {
        let parts = request_parts(
            "platform/organizations/members/put",
            json!({"organizationId":"a/b", "userId":"u?#", "role":"creator"}),
        )
        .unwrap();
        assert_eq!(
            parts.path,
            "/api/v1/platform/organizations/a%2Fb/members/u%3F%23"
        );
        assert_eq!(parts.body, Some(json!({"role":"creator"})));
        assert!(
            request_parts(
                "platform/organizations/read",
                json!({"organizationId":".."})
            )
            .is_err()
        );
        assert!(request_parts("platform/invented", json!({})).is_err());
    }
}
