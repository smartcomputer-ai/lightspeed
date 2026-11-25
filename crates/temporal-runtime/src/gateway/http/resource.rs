//! Resource HTTP transport. Route bindings are expanded from the public catalog.
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiService as _, DeploymentApiService as _, HttpBinding,
    HttpSuccess, HttpTransfer, HttpVerb,
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{FromRequestParts as _, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{MethodFilter, on},
};
use serde::Serialize;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{
    Admitted, ApiAdmission, GatewayState, RequestContext, UNIVERSE_HEADER, UniverseResolution,
    authentication, request_context,
};

mod bytes;

pub(super) fn router(max_body: usize, max_blob_upload: usize) -> Router<Arc<GatewayState>> {
    let mut router = Router::new();
    macro_rules! invoke_operation {
        (initialize, Json, $params:ty, $state:ident, $received:ident, $binding:ident, $id:path) => {{
            let default_universe = match &$state.resolution {
                UniverseResolution::FixedApi { api } => Some(api.universe_id().to_string()),
                UniverseResolution::Multi {
                    mode: crate::config::GatewayAuthMode::Single { universe_id },
                    ..
                } => Some(universe_id.to_string()),
                _ => None,
            };
            success(
                request_context::connection_response(
                    $received.context.caller_access(),
                    default_universe,
                ),
                $binding.success,
                $id,
            )
        }};
        ($service:ident, Sse, $params:ty, $state:ident, $received:ident, $binding:ident, $id:path) => {{
            let service = $state.api_for_request(&$received.context).await?;
            watch(
                &$state,
                service,
                &$received.context,
                $received.params,
                &$received.headers,
            )
            .await
        }};
        ($service:ident, BinaryDownload, $params:ty, $state:ident, $received:ident, $binding:ident, $id:path) => {{
            let service = $state.api_for_request(&$received.context).await?;
            bytes::download_operation(service, $id, $received.params, &$received.headers).await
        }};
        ($service:ident, Json, $params:ty, $state:ident, $received:ident, $binding:ident, $id:path) => {{
            let params: $params = serde_json::from_value($received.params).map_err(invalid_json)?;
            let service = $state.api_for_request(&$received.context).await?;
            success(service.$service(params).await?, $binding.success, $id)
        }};
    }
    macro_rules! universe_routes {
        ($($id:path => $service:ident($params:ty) -> $result:ty => [$summary:expr, $description:expr], $(streaming: $emits:expr,)? access: $access:expr,
          http: ($verb:ident, $path:literal, [$(($parameter:literal, $field:literal)),*], [$($query:literal),*], $transfer:ident, $success:ident)),+ $(,)?) => {
            $({
                const BINDING: HttpBinding = HttpBinding {
                    verb: HttpVerb::$verb, path: $path,
                    path_fields: &[$(api::HttpPathField {parameter: $parameter, field: $field}),*],
                    query_fields: &[$($query),*], transfer: HttpTransfer::$transfer, success: HttpSuccess::$success,
                };
                static SCHEMA: OnceLock<Value> = OnceLock::new();
                let route = on(method_filter(BINDING.verb), move |State(state): State<Arc<GatewayState>>, request: Request| async move {
                    let result: Result<Response, Failure> = async {
                        let schema = SCHEMA.get_or_init(api::http_input_schema::<$params>);
                        let received = receive(&state, $id, BINDING, schema, request, max_body).await?;
                        request_context::with_request_context(received.context.clone(), async {
                            invoke_operation!($service, $transfer, $params, state, received, BINDING, $id)
                        }).await
                    }.await;
                    result.unwrap_or_else(IntoResponse::into_response)
                });
                router = router.route(BINDING.path, if BINDING.verb == HttpVerb::GET { route.on(MethodFilter::HEAD, head_unavailable) } else { route });
            })+
        };
    }
    api::for_each_universe_operation!(universe_routes);
    macro_rules! deployment_routes {
        ($($id:path => $service:ident($params:ty) -> $result:ty => [$summary:expr, $description:expr], access: $access:expr,
          http: ($verb:ident, $path:literal, [$(($parameter:literal, $field:literal)),*], [$($query:literal),*], $transfer:ident, $success:ident)),+ $(,)?) => {
            $({
                const BINDING: HttpBinding = HttpBinding {
                    verb: HttpVerb::$verb, path: $path,
                    path_fields: &[$(api::HttpPathField {parameter: $parameter, field: $field}),*],
                    query_fields: &[$($query),*], transfer: HttpTransfer::$transfer, success: HttpSuccess::$success,
                };
                static SCHEMA: OnceLock<Value> = OnceLock::new();
                let route = on(method_filter(BINDING.verb), move |State(state): State<Arc<GatewayState>>, request: Request| async move {
                    let result: Result<Response, Failure> = async {
                        let schema = SCHEMA.get_or_init(api::http_input_schema::<$params>);
                        let received = receive(&state, $id, BINDING, schema, request, max_body).await?;
                        let params: $params = serde_json::from_value(received.params).map_err(invalid_json)?;
                        let UniverseResolution::Multi { deployment, .. } = &state.resolution else {
                            return Err(AgentApiError::invalid_request("deployment operations are unavailable on this gateway").into());
                        };
                        request_context::with_request_context(received.context, async {
                            success(deployment.$service(params).await?, BINDING.success, $id)
                        }).await
                    }.await;
                    result.unwrap_or_else(IntoResponse::into_response)
                });
                router = router.route(BINDING.path, if BINDING.verb == HttpVerb::GET { route.on(MethodFilter::HEAD, head_unavailable) } else { route });
            })+
        };
    }
    api::for_each_deployment_operation!(deployment_routes);
    router
        .route(
            api::blob_upload_binding().path,
            on(
                MethodFilter::PUT,
                move |State(state): State<Arc<GatewayState>>, request: Request| async move {
                    bytes::upload(state, request, max_blob_upload)
                        .await
                        .unwrap_or_else(IntoResponse::into_response)
                },
            ),
        )
        .method_not_allowed_fallback(method_unavailable)
        .fallback(route_unavailable)
}

fn method_filter(verb: HttpVerb) -> MethodFilter {
    match verb {
        HttpVerb::GET => MethodFilter::GET,
        HttpVerb::POST => MethodFilter::POST,
        HttpVerb::PUT => MethodFilter::PUT,
        HttpVerb::PATCH => MethodFilter::PATCH,
        HttpVerb::DELETE => MethodFilter::DELETE,
    }
}

async fn admission_only(state: &GatewayState, headers: &HeaderMap) -> Result<(), Failure> {
    let mut auth_headers = headers.clone();
    auth_headers.remove(UNIVERSE_HEADER);
    let caller = state.api_admission().admit(&auth_headers).await?;
    if headers.contains_key(UNIVERSE_HEADER) {
        return Err(AgentApiError::invalid_request("universe selection belongs in the URL").into());
    }
    if matches!(caller, Admitted::Local { .. }) {
        authentication::reject_identity_headers(headers)?;
    }
    Ok(())
}
async fn head_unavailable(State(state): State<Arc<GatewayState>>, request: Request) -> Response {
    match admission_only(&state, request.headers()).await {
        Ok(()) => Failure::status(
            StatusCode::METHOD_NOT_ALLOWED,
            "HEAD is not supported for this operation",
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
}
async fn method_unavailable(State(state): State<Arc<GatewayState>>, request: Request) -> Response {
    // Router::merge also merges fallbacks. Integration and health routes keep
    // their own admission semantics when mounted beside the application API.
    if !is_resource_path(request.uri().path()) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    match admission_only(&state, request.headers()).await {
        Ok(()) => Failure::status(
            StatusCode::METHOD_NOT_ALLOWED,
            "method is not supported for this resource",
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
}
async fn route_unavailable(State(state): State<Arc<GatewayState>>, request: Request) -> Response {
    if !is_resource_path(request.uri().path()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match admission_only(&state, request.headers()).await {
        Ok(()) => {
            Failure::from(AgentApiError::not_found("resource route not found")).into_response()
        }
        Err(error) => error.into_response(),
    }
}

fn is_resource_path(path: &str) -> bool {
    path == "/api/v1" || path.starts_with("/api/v1/")
}

#[derive(Debug)]
struct Failure {
    error: AgentApiError,
    status: StatusCode,
}
impl Failure {
    fn status(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            error: AgentApiError::invalid_request(detail),
            status,
        }
    }
}
impl From<AgentApiError> for Failure {
    fn from(error: AgentApiError) -> Self {
        let status = match error.kind {
            AgentApiErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
            AgentApiErrorKind::NotFound => StatusCode::NOT_FOUND,
            AgentApiErrorKind::Gone => StatusCode::GONE,
            AgentApiErrorKind::Conflict
            | AgentApiErrorKind::ModelDefaultUnset
            | AgentApiErrorKind::SessionBootstrapFailed
            | AgentApiErrorKind::SessionExecutionStopped => StatusCode::CONFLICT,
            AgentApiErrorKind::Rejected => StatusCode::UNPROCESSABLE_ENTITY,
            AgentApiErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
            AgentApiErrorKind::Forbidden => StatusCode::FORBIDDEN,
            AgentApiErrorKind::AudioBlobTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            AgentApiErrorKind::EnvironmentNotReady => StatusCode::SERVICE_UNAVAILABLE,
            AgentApiErrorKind::OperationPending => StatusCode::GATEWAY_TIMEOUT,
            AgentApiErrorKind::Internal | AgentApiErrorKind::ResponseTooLarge => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        Self { error, status }
    }
}
impl IntoResponse for Failure {
    fn into_response(mut self) -> Response {
        if self.error.kind == AgentApiErrorKind::Internal {
            tracing::error!(target: "temporal_runtime", error = %self.error, "resource API failed");
            self.error.message = "internal server error".to_owned();
        }
        let problem = api::HttpProblem::from_error(
            self.error,
            self.status.as_u16(),
            self.status.canonical_reason().unwrap_or("Request failed"),
        );
        let mut response = (self.status, Json(problem)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
fn invalid_json(error: serde_json::Error) -> Failure {
    AgentApiError::invalid_request(error.to_string()).into()
}

fn success<T: Serialize>(
    outcome: T,
    policy: HttpSuccess,
    operation: &str,
) -> Result<Response, Failure> {
    let value = serde_json::to_value(outcome)
        .map_err(|error| AgentApiError::internal(error.to_string()))?;
    let status = match policy {
        HttpSuccess::Ok => StatusCode::OK,
        HttpSuccess::Created => StatusCode::CREATED,
        HttpSuccess::CreatedFlag => match value.get("created").and_then(Value::as_bool) {
            Some(true) => StatusCode::CREATED,
            Some(false) => StatusCode::OK,
            None => {
                return Err(AgentApiError::internal(
                    "created response binding requires a boolean created field",
                )
                .into());
            }
        },
    };
    let body =
        serde_json::to_vec(&value).map_err(|error| AgentApiError::internal(error.to_string()))?;
    if !super::response_budget_exempt(operation) && body.len() > 2 * 1024 * 1024 {
        return Err(AgentApiError::response_too_large(
            "serialized response exceeds 2097152 bytes; retry with a smaller page limit",
        )
        .into());
    }
    Ok((
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response())
}

struct Received {
    context: RequestContext,
    params: Value,
    headers: HeaderMap,
}

async fn context_for_path(
    admitted: Admitted,
    local_pool: &sqlx::PgPool,
    headers: &HeaderMap,
    operation: &str,
    universe: Option<&str>,
) -> Result<RequestContext, Failure> {
    if headers.contains_key(UNIVERSE_HEADER) {
        return Err(AgentApiError::invalid_request(
            "universe selection belongs in the URL; x-lightspeed-universe is not accepted",
        )
        .into());
    }
    match admitted {
        Admitted::Caller(caller) => {
            let universe = match universe {
                Some(reference) => Some(
                    store_pg::resolve_universe_reference(local_pool, reference)
                        .await
                        .map_err(|error| AgentApiError::internal(error.to_string()))?
                        .ok_or_else(|| AgentApiError::not_found("unknown universe"))?,
                ),
                None => None,
            };
            Ok(authentication::request_context(
                *caller, operation, universe,
            )?)
        }
        Admitted::Local { universe_id } => {
            authentication::reject_identity_headers(headers)?;
            let scope = api::method_access(operation)
                .ok_or_else(authentication::unknown_method)?
                .scope();
            if matches!(
                scope,
                api::MethodScope::Universe | api::MethodScope::Service
            ) {
                let reference = universe
                    .ok_or_else(|| AgentApiError::invalid_request("universe path is required"))?;
                let target = match Uuid::parse_str(reference) {
                    Ok(id) => Some(id),
                    Err(_) => store_pg::resolve_universe_reference(local_pool, reference)
                        .await
                        .map_err(|error| AgentApiError::internal(error.to_string()))?,
                };
                if target != Some(universe_id) {
                    return Err(AgentApiError::not_found("unknown universe").into());
                }
                Ok(RequestContext::local(api::AccessScope::Universe {
                    universe_id,
                }))
            } else {
                Ok(RequestContext::local(api::AccessScope::Deployment))
            }
        }
    }
}

async fn receive(
    state: &GatewayState,
    operation: &str,
    binding: HttpBinding,
    schema: &Value,
    request: Request,
    max_body: usize,
) -> Result<Received, Failure> {
    let store = state.deployment_store();
    receive_with(
        state.api_admission(),
        store.pool(),
        operation,
        binding,
        schema,
        request,
        max_body,
    )
    .await
}

async fn receive_with(
    admission: ApiAdmission<'_>,
    pool: &sqlx::PgPool,
    operation: &str,
    binding: HttpBinding,
    schema: &Value,
    request: Request,
    max_body: usize,
) -> Result<Received, Failure> {
    let (mut parts, body) = request.into_parts();
    // Authentication precedes path/query parsing as well as body consumption.
    let mut authentication_headers = parts.headers.clone();
    authentication_headers.remove(UNIVERSE_HEADER);
    let admitted = admission.admit(&authentication_headers).await?;
    validate_percent_encoding(parts.uri.path())?;
    let Path(path) = Path::<HashMap<String, String>>::from_request_parts(&mut parts, &())
        .await
        .map_err(|error| Failure::status(StatusCode::BAD_REQUEST, error.body_text()))?;
    let scope = api::method_access(operation)
        .ok_or_else(authentication::unknown_method)?
        .scope();
    let universe = if matches!(
        scope,
        api::MethodScope::Universe | api::MethodScope::Service
    ) {
        path.get("universe").map(String::as_str)
    } else {
        None
    };
    let context = context_for_path(admitted, pool, &parts.headers, operation, universe).await?;
    let query = decode_query(binding, schema, parts.uri.query().unwrap_or(""))?;
    let headers = parts.headers.clone();
    ensure_identity_encoding(&headers)?;
    if binding.verb.has_json_body()
        && headers.contains_key(header::CONTENT_TYPE)
        && !is_media_type(&headers, "application/json")
    {
        return Err(Failure::status(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        ));
    }
    let body = axum::body::to_bytes(body, max_body)
        .await
        .map_err(|error| {
            use std::error::Error as _;
            if error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
            {
                Failure::status(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body exceeds its limit",
                )
            } else {
                Failure::status(StatusCode::BAD_REQUEST, "request body could not be read")
            }
        })?;
    let mut params = if binding.verb.has_json_body() {
        if !body.is_empty() && !is_media_type(&headers, "application/json") {
            return Err(Failure::status(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected application/json",
            ));
        }
        let value = if body.is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_slice::<UniqueJson>(&body)
                .map_err(invalid_json)?
                .0
        };
        let Value::Object(map) = value else {
            return Err(AgentApiError::invalid_request("request body must be an object").into());
        };
        reject_unknown_fields(schema, schema, &Value::Object(map.clone()))?;
        map
    } else {
        if !body.is_empty() {
            return Err(AgentApiError::invalid_request(
                "this operation does not accept a request body",
            )
            .into());
        }
        Map::new()
    };
    for (name, value) in query {
        if params.insert(name, value).is_some() {
            return Err(
                AgentApiError::invalid_request("a query field was repeated in the body").into(),
            );
        }
    }
    for field in binding.path_fields {
        let raw = path
            .get(field.parameter)
            .ok_or_else(|| AgentApiError::invalid_request("missing path parameter"))?;
        let field_schema = api::http_field_schema(schema, field.field)
            .ok_or_else(|| AgentApiError::internal("path binding has no schema"))?;
        let value = decode_scalar(schema, field_schema, raw)?;
        insert_path_field(&mut params, field.field, value)?;
    }
    Ok(Received {
        context,
        params: Value::Object(params),
        headers,
    })
}

fn is_media_type(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}
fn ensure_identity_encoding(headers: &HeaderMap) -> Result<(), Failure> {
    let mut encodings = headers.get_all(header::CONTENT_ENCODING).iter();
    if let Some(value) = encodings.next()
        && (encodings.next().is_some()
            || !value
                .to_str()
                .is_ok_and(|value| value.trim().eq_ignore_ascii_case("identity")))
    {
        return Err(Failure::status(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "encoded request bodies are not supported",
        ));
    }
    Ok(())
}

fn validate_percent_encoding(value: &str) -> Result<(), Failure> {
    let bytes = value.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] == b'%' {
            if offset + 2 >= bytes.len()
                || !bytes[offset + 1].is_ascii_hexdigit()
                || !bytes[offset + 2].is_ascii_hexdigit()
            {
                return Err(AgentApiError::invalid_request("malformed percent encoding").into());
            }
            offset += 3;
        } else {
            offset += 1;
        }
    }
    Ok(())
}

fn decode_query(
    binding: HttpBinding,
    schema: &Value,
    raw: &str,
) -> Result<Map<String, Value>, Failure> {
    validate_percent_encoding(raw)?;
    let mut values: HashMap<String, Vec<String>> = HashMap::new();
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode_form_component(key)?;
        let value = decode_form_component(value)?;
        if !binding.query_fields.contains(&key.as_str()) {
            return Err(
                AgentApiError::invalid_request(format!("unknown query field: {key}")).into(),
            );
        }
        values.entry(key).or_default().push(value);
    }
    let mut result = Map::new();
    for (name, values) in values {
        let field = api::http_field_schema(schema, &name)
            .ok_or_else(|| AgentApiError::internal("query binding has no schema"))?;
        let effective = effective_schema(schema, field);
        let value = if schema_type(effective) == Some("array") {
            let item = effective
                .get("items")
                .ok_or_else(|| AgentApiError::internal("array query binding has no item schema"))?;
            Value::Array(
                values
                    .iter()
                    .map(|value| decode_scalar(schema, item, value))
                    .collect::<Result<_, _>>()?,
            )
        } else {
            if values.len() != 1 {
                return Err(AgentApiError::invalid_request(format!(
                    "duplicate scalar query field: {name}"
                ))
                .into());
            }
            decode_scalar(schema, field, &values[0])?
        };
        reject_unknown_fields(schema, field, &value)?;
        result.insert(name, value);
    }
    Ok(result)
}
fn decode_form_component(raw: &str) -> Result<String, Failure> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut input = raw.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let high = input.next().and_then(|byte| (byte as char).to_digit(16));
                let low = input.next().and_then(|byte| (byte as char).to_digit(16));
                let (Some(high), Some(low)) = (high, low) else {
                    return Err(AgentApiError::invalid_request("malformed percent encoding").into());
                };
                ((high << 4) | low) as u8
            }
            other => other,
        });
    }
    String::from_utf8(bytes)
        .map_err(|_| AgentApiError::invalid_request("query is not valid UTF-8").into())
}

fn schema_type(schema: &Value) -> Option<&str> {
    match schema.get("type")? {
        Value::String(value) => Some(value),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .find(|value| *value != "null"),
        _ => None,
    }
}
fn effective_schema<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    let schema = api::http_resolve_schema(root, schema);
    for keyword in ["anyOf", "oneOf"] {
        if let Some(variants) = schema.get(keyword).and_then(Value::as_array) {
            let mut non_null = variants
                .iter()
                .map(|value| api::http_resolve_schema(root, value))
                .filter(|value| schema_type(value) != Some("null"));
            if let Some(first) = non_null.next() {
                let kind = schema_type(first);
                if kind.is_some() && non_null.all(|value| schema_type(value) == kind) {
                    return first;
                }
            }
        }
    }
    schema
}
fn decode_scalar(root: &Value, schema: &Value, raw: &str) -> Result<Value, Failure> {
    let schema = effective_schema(root, schema);
    if schema_type(schema) == Some("string") {
        return Ok(Value::String(raw.to_owned()));
    }
    let value = serde_json::from_str::<UniqueJson>(raw)
        .map_err(invalid_json)?
        .0;
    let valid = match schema_type(schema) {
        Some("boolean") => value.is_boolean() || value.is_null(),
        Some("integer") => value.is_i64() || value.is_u64() || value.is_null(),
        Some("number") => value.is_number() || value.is_null(),
        _ => true,
    };
    if !valid {
        return Err(AgentApiError::invalid_request("query value has the wrong type").into());
    }
    Ok(value)
}
fn insert_path_field(
    map: &mut Map<String, Value>,
    field: &str,
    value: Value,
) -> Result<(), Failure> {
    if let Some((head, tail)) = field.split_once('.') {
        let child = map.entry(head).or_insert_with(|| Value::Object(Map::new()));
        let Value::Object(child) = child else {
            return Err(
                AgentApiError::invalid_request("path-bound body parent must be an object").into(),
            );
        };
        insert_path_field(child, tail, value)
    } else if map.contains_key(field) {
        Err(AgentApiError::invalid_request(format!(
            "path field {field} must not also appear in the body or query"
        ))
        .into())
    } else {
        map.insert(field.to_owned(), value);
        Ok(())
    }
}
fn object_properties<'a>(
    root: &'a Value,
    schema: &'a Value,
    value: &Value,
    properties: &mut HashMap<&'a str, &'a Value>,
) {
    let schema = api::http_resolve_schema(root, schema);
    if let Some(fields) = schema.get("properties").and_then(Value::as_object) {
        properties.extend(fields.iter().map(|(key, value)| (key.as_str(), value)));
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(variants) = schema.get(keyword).and_then(Value::as_array) {
            for variant in variants {
                let variant = api::http_resolve_schema(root, variant);
                let matches = variant
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_none_or(|fields| {
                        fields.iter().all(|(name, field)| {
                            let Some(actual) = value.get(name) else {
                                return true;
                            };
                            let field = api::http_resolve_schema(root, field);
                            field.get("const").is_none_or(|expected| actual == expected)
                                && field
                                    .get("enum")
                                    .and_then(Value::as_array)
                                    .is_none_or(|allowed| allowed.contains(actual))
                        })
                    });
                if matches {
                    object_properties(root, variant, value, properties);
                }
            }
        }
    }
}
fn reject_unknown_fields(root: &Value, schema: &Value, value: &Value) -> Result<(), Failure> {
    let schema = api::http_resolve_schema(root, schema);
    // An optional record/array is emitted as a union with null. Only unwrap
    // a single non-null branch; tagged unions must retain all their branches.
    for keyword in ["anyOf", "oneOf"] {
        if let Some(variants) = schema.get(keyword).and_then(Value::as_array) {
            let non_null: Vec<_> = variants
                .iter()
                .map(|variant| api::http_resolve_schema(root, variant))
                .filter(|variant| schema_type(variant) != Some("null"))
                .collect();
            if non_null.len() == 1 && schema.get("properties").is_none() {
                return reject_unknown_fields(root, non_null[0], value);
            }
        }
    }
    if let Some(object) = value.as_object() {
        let mut properties = HashMap::new();
        object_properties(root, schema, value, &mut properties);
        if !properties.is_empty()
            || schema.get("properties").is_some()
            || (schema_type(schema) == Some("object")
                && schema
                    .get("additionalProperties")
                    .is_none_or(|extra| extra == &Value::Bool(false)))
        {
            for (name, value) in object {
                if let Some(field) = properties.get(name.as_str()) {
                    reject_unknown_fields(root, field, value)?;
                } else if schema
                    .get("additionalProperties")
                    .is_none_or(|extra| extra == &Value::Bool(false))
                {
                    return Err(AgentApiError::invalid_request(format!(
                        "unknown body field: {name}"
                    ))
                    .into());
                }
            }
        } else if let Some(extra) = schema
            .get("additionalProperties")
            .filter(|extra| extra.is_object())
        {
            for value in object.values() {
                reject_unknown_fields(root, extra, value)?;
            }
        }
    } else if let Some(items) = schema.get("items")
        && let Some(values) = value.as_array()
    {
        for value in values {
            reject_unknown_fields(root, items, value)?;
        }
    }
    Ok(())
}

/// Preserve Serde's duplicate-field refusal while assembling split inputs.
struct UniqueJson(Value);
impl<'de> serde::Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueJson;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object fields")
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Bool(value)))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::from(value)))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::from(value)))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|value| UniqueJson(Value::Number(value)))
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::String(value.to_owned())))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::String(value)))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Null))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueJson>()? {
                    values.push(value.0);
                }
                Ok(UniqueJson(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut object: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some((name, value)) = object.next_entry::<String, UniqueJson>()? {
                    if values.insert(name, value.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON object field"));
                    }
                }
                Ok(UniqueJson(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

async fn watch(
    state: &GatewayState,
    service: Arc<super::super::GatewayAgentApi>,
    context: &RequestContext,
    params: Value,
    headers: &HeaderMap,
) -> Result<Response, Failure> {
    if !headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().split(';').next() == Some("text/event-stream"))
        })
    {
        return Err(
            AgentApiError::invalid_request("events require Accept: text/event-stream").into(),
        );
    }
    let params = serde_json::from_value(params).map_err(invalid_json)?;
    service
        .authorize_method(api::METHOD_UNIVERSE_WATCH, None)
        .await?;
    Ok(state
        .watches
        .subscribe(service.store().clone(), context, params)?
        .resource_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;
    use std::sync::atomic::{AtomicBool, Ordering};

    const UNIVERSE: Uuid = Uuid::from_u128(7);
    const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";
    fn binding(operation: &str) -> HttpBinding {
        api::method_manifest()
            .into_iter()
            .chain(api::deployment_method_manifest())
            .find(|method| method.method == operation)
            .unwrap()
            .http
    }
    fn pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://127.0.0.1:1/unreachable")
            .unwrap()
    }
    struct Server {
        origin: String,
        read: Arc<AtomicBool>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn serve(operation: &'static str, schema: Value, authenticated: bool) -> Server {
        let binding = binding(operation);
        let read = Arc::new(AtomicBool::new(false));
        let seen = read.clone();
        let tokens = Arc::new(
            crate::config::RuntimeTokens::from_configuration(
                [("LIGHTSPEED_API_TOKEN".to_owned(), TOKEN.to_owned())],
                |_| unreachable!(),
            )
            .unwrap(),
        );
        let router = Router::new().route(
            binding.path,
            on(method_filter(binding.verb), move |request: Request| {
                let tokens = tokens.clone();
                let schema = schema.clone();
                let seen = seen.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let stream = body.into_data_stream().inspect(move |_| {
                        seen.store(true, Ordering::SeqCst);
                    });
                    let request = Request::from_parts(parts, Body::from_stream(stream));
                    let pool = pool();
                    let admission = if authenticated {
                        ApiAdmission::Token { tokens: &tokens }
                    } else {
                        ApiAdmission::Local {
                            universe_id: UNIVERSE,
                        }
                    };
                    match receive_with(admission, &pool, operation, binding, &schema, request, 128)
                        .await
                    {
                        Ok(received) => {
                            if operation == api::METHOD_INITIALIZE {
                                success(
                                    request_context::connection_response(
                                        received.context.caller_access(),
                                        None,
                                    ),
                                    HttpSuccess::Ok,
                                    operation,
                                )
                                .unwrap()
                            } else {
                                success(received.params, HttpSuccess::Ok, operation).unwrap()
                            }
                        }
                        Err(error) => error.into_response(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Server { origin, read, task }
    }

    #[test]
    fn all_declared_routes_mount_without_collisions() {
        let _ = router(1024, 1024);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authentication_answers_before_an_unending_http_body_is_polled() {
        let server = serve(
            api::METHOD_INITIALIZE,
            api::http_input_schema::<api::ConnectionParams>(),
            true,
        )
        .await;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reqwest::Client::new()
                .get(format!("{}/api/v1/connection", server.origin))
                .body(reqwest::Body::wrap_stream(futures_util::stream::pending::<
                    Result<Bytes, std::io::Error>,
                >()))
                .send(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        assert!(!server.read.load(Ordering::SeqCst));
        let problem: api::HttpProblem = response.json().await.unwrap();
        assert_eq!(problem.kind, AgentApiErrorKind::Unauthenticated);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn connection_is_a_direct_dto_without_database_or_universe_selection() {
        let server = serve(
            api::METHOD_INITIALIZE,
            api::http_input_schema::<api::ConnectionParams>(),
            true,
        )
        .await;
        let client = reqwest::Client::new();
        let response = client
            .get(format!("{}/api/v1/connection", server.origin))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["runtime"], "runtime");
        assert_eq!(body["contractRevision"], api::HTTP_CONTRACT_REVISION);
        assert_eq!(body["caller"]["tokenName"], "default");
        assert!(body.get("result").is_none());
        assert!(body.get("id").is_none());
        let rejected = client
            .get(format!("{}/api/v1/connection", server.origin))
            .bearer_auth(TOKEN)
            .header(UNIVERSE_HEADER, UNIVERSE.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_http_path_query_and_body_partitions_are_authoritative() {
        let server = serve(
            api::METHOD_SESSION_READ,
            api::http_input_schema::<api::SessionReadParams>(),
            false,
        )
        .await;
        let client = reqwest::Client::new();
        let url = format!(
            "{}/api/v1/universes/{UNIVERSE}/sessions/session%20one",
            server.origin
        );
        let response: Value = client
            .get(format!("{url}?runLimit=3"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["sessionId"], "session one");
        assert_eq!(response["runLimit"], 3);
        for query in [
            "runLimit=1&runLimit=2",
            "sessionId=elsewhere",
            "runLimit=abc",
            "runLimit=%FF",
            "runLimit=%zz",
            "unknown=yes",
        ] {
            let response = client.get(format!("{url}?{query}")).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/problem+json"
            );
        }
        let server = serve(
            api::METHOD_PROFILES_PUT,
            api::http_input_schema::<api::ProfilePutParams>(),
            false,
        )
        .await;
        let url = format!(
            "{}/api/v1/universes/{UNIVERSE}/profiles/example",
            server.origin
        );
        let response: Value = client
            .put(&url)
            .json(&serde_json::json!({"profile": {}}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["profile"]["profileId"], "example");
        for body in [
            r#"{"profile":{"profileId":"other"}}"#,
            r#"{"profile":{},"unexpected":true}"#,
            r#"{"profile":{},"profile":{}}"#,
            "not json",
        ] {
            let response = client
                .put(&url)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
        }
        let response = client
            .put(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(" ".repeat(129))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let response = client
            .put(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_ENCODING, "gzip")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn optional_command_body_may_be_omitted_without_content_type() {
        let server = serve(
            api::METHOD_SESSION_RESTART,
            api::http_input_schema::<api::SessionRestartParams>(),
            false,
        )
        .await;
        let url = format!(
            "{}/api/v1/universes/{UNIVERSE}/sessions/example/restart",
            server.origin
        );
        let client = reqwest::Client::new();
        let response = client.post(&url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let params: api::SessionRestartParams = response.json().await.unwrap();
        assert_eq!(params.session_id, "example");
        for request in [
            client.post(&url).body("{}"),
            client.post(&url).header(header::CONTENT_TYPE, "text/plain"),
        ] {
            assert_eq!(
                request.send().await.unwrap().status(),
                StatusCode::UNSUPPORTED_MEDIA_TYPE
            );
        }
    }

    #[test]
    fn query_arrays_and_nullable_string_enums_use_declared_encodings() {
        let schema = api::http_input_schema::<api::UniverseWatchParams>();
        let parsed = decode_query(
            binding(api::METHOD_UNIVERSE_WATCH),
            &schema,
            "kinds=session&kinds=bot&visibleTo=null",
        )
        .unwrap();
        assert_eq!(parsed["kinds"], serde_json::json!(["session", "bot"]));
        assert_eq!(parsed["visibleTo"], "null");
        assert!(serde_json::from_value::<api::UniverseWatchParams>(Value::Object(parsed)).is_ok());
        assert_eq!(decode_form_component("%EF%BF%BD").unwrap(), "\u{fffd}");
    }

    #[test]
    fn flattened_tagged_objects_refuse_unknown_fields_in_the_selected_variant() {
        let schema = api::http_input_schema::<api::BotTriggerPutParams>();
        assert!(
            reject_unknown_fields(
                &schema,
                &schema,
                &serde_json::json!({"trigger":{"kind":"webhook"}})
            )
            .is_ok()
        );
        assert_eq!(
            reject_unknown_fields(
                &schema,
                &schema,
                &serde_json::json!({"trigger":{"kind":"webhook","surprise":true}})
            )
            .unwrap_err()
            .error
            .kind,
            AgentApiErrorKind::InvalidRequest
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn problem_statuses_preserve_domain_extensions_and_redact_internal_details() {
        let mut error = AgentApiError::operation_pending("command may still complete");
        error.submission_id = Some("dedup-id".into());
        let response = Failure::from(error).into_response();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let problem: api::HttpProblem = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(problem.submission_id.as_deref(), Some("dedup-id"));
        let response = Failure::from(AgentApiError::internal("postgres://secret")).into_response();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(!String::from_utf8(body.to_vec()).unwrap().contains("secret"));
        assert_eq!(
            Failure::from(AgentApiError::response_too_large("reduce limit")).status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn declared_response_status_selects_creation_without_inventing_admission() {
        for (created, expected) in [(false, StatusCode::OK), (true, StatusCode::CREATED)] {
            let response = success(
                serde_json::json!({"created":created}),
                HttpSuccess::CreatedFlag,
                api::METHOD_INITIALIZE,
            )
            .unwrap();
            assert_eq!(response.status(), expected);
        }
        assert!(
            success(
                serde_json::json!({}),
                HttpSuccess::CreatedFlag,
                api::METHOD_INITIALIZE
            )
            .is_err()
        );
    }
}
