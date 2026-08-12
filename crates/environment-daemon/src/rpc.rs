use host_protocol::error::{HostError, HostErrorCode};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub method: Option<String>,
    pub params: Value,
}

pub fn parse_request(value: Value) -> Result<RpcRequest, HostError> {
    if !value.is_object() {
        return Err(HostError::new(
            HostErrorCode::InvalidRequest,
            "JSON-RPC request must be an object",
        ));
    }
    let id = value.get("id").cloned();
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    Ok(RpcRequest { id, method, params })
}

pub fn decode_params<T>(params: Value) -> Result<T, HostError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(params)
        .map_err(|error| HostError::new(HostErrorCode::InvalidRequest, error.to_string()))
}

pub fn encode_result<T>(value: T) -> Result<Value, HostError>
where
    T: Serialize,
{
    serde_json::to_value(value)
        .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))
}

pub fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

pub fn error_response(id: Option<Value>, error: HostError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": error
    })
}

pub fn method_not_found(method: &str) -> HostError {
    HostError::new(
        HostErrorCode::Unsupported,
        format!("unsupported host-protocol method: {method}"),
    )
}

pub fn invalid_request(message: impl Into<String>) -> HostError {
    HostError::new(HostErrorCode::InvalidRequest, message)
}

pub fn not_found(message: impl Into<String>) -> HostError {
    HostError::new(HostErrorCode::NotFound, message)
}

pub fn unsupported(message: impl Into<String>) -> HostError {
    HostError::new(HostErrorCode::Unsupported, message)
}
