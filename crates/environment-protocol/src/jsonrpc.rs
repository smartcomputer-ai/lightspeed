//! The JSON-RPC 2.0 envelope every socket uses: the data plane, the
//! controller plane and the registration channel.
//!
//! Rules, applied by [`parse`] and the constructors here:
//!
//! - One message per WebSocket text frame, at most
//!   [`MAX_MESSAGE_BYTES`]. Binary frames
//!   are not part of any plane.
//! - `jsonrpc` must be `"2.0"`.
//! - Params are named: an object, or absent, which means `{}`.
//! - A request has a string or integer `id`; a notification has none and is
//!   never answered, not even with an error. A malformed frame that has a
//!   string `method` and no `id` is a malformed notification: [`parse`]
//!   rejects it, and a server drops it silently
//!   ([`Rejection::into_response`] returns `None`).
//! - Batches (JSON arrays) are not supported and are answered with one
//!   `invalidRequest` error with a null id.
//! - A frame that is not JSON is answered with `parseError` and a null id;
//!   the connection stays open.
//! - A server answers every other rejection, with the request's id when it
//!   could be read and a null id otherwise; a client that rejects a frame
//!   closes the connection.
//! - Requests may be pipelined. Responses arrive in any order and are
//!   matched by id.
//! - [`CANCEL_REQUEST_METHOD`] asks the server to abort a pending request.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::{
    error::{ErrorObject, ProtocolError},
    limits::MAX_MESSAGE_BYTES,
};

pub const JSONRPC_VERSION: &str = "2.0";

/// Client-to-server notification asking the server to abort the request
/// with this id. Read-only waits stop and answer `aborted` with reason
/// `cancelled`; methods with side effects run to a consistent state and
/// answer normally. An unknown or finished id is ignored.
pub const CANCEL_REQUEST_METHOD: &str = "$/cancelRequest";

/// A request id: an integer or a string, echoed in the response.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    String(String),
}

impl From<i64> for Id {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(number) => number.fmt(formatter),
            Self::String(text) => text.fmt(formatter),
        }
    }
}

/// `$/cancelRequest` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelRequestParams {
    /// Id of the request to abort.
    pub id: Id,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: Id,
    pub method: String,
    /// Always a JSON object.
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub method: String,
    /// Always a JSON object.
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Response {
    /// Absent (null on the wire) only when the request's id could not be
    /// read.
    pub id: Option<Id>,
    pub result: Result<Value, ProtocolError>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

/// A frame that cannot be accepted, with the error to answer it with.
///
/// A server answers a rejection with [`into_response`](Self::into_response)
/// unless the frame was a malformed notification, which is dropped
/// silently. A client that rejects a frame closes the connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Rejection {
    /// The rejected request's id; absent when it could not be read.
    pub id: Option<Id>,
    pub error: ProtocolError,
    /// The frame had a string `method` and no `id`.
    notification: bool,
}

impl Rejection {
    fn of_frame(error: ProtocolError) -> Self {
        Self {
            id: None,
            error,
            notification: false,
        }
    }

    /// True when the frame was a malformed notification: it must not be
    /// answered.
    pub fn is_notification(&self) -> bool {
        self.notification
    }

    /// The error response a server sends for this rejection, or `None` for
    /// a malformed notification, which is never answered.
    pub fn into_response(self) -> Option<Response> {
        (!self.notification).then_some(Response {
            id: self.id,
            result: Err(self.error),
        })
    }
}

/// Classify one text frame.
pub fn parse(text: &str) -> Result<Message, Rejection> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        Rejection::of_frame(ProtocolError::parse_error(format!(
            "frame is not JSON: {error}"
        )))
    })?;
    parse_value(value)
}

/// Classify one decoded frame.
pub fn parse_value(value: Value) -> Result<Message, Rejection> {
    let mut object = match value {
        Value::Object(object) => object,
        Value::Array(_) => {
            return Err(Rejection::of_frame(ProtocolError::invalid_request(
                "batches are not supported",
            )));
        }
        _ => {
            return Err(Rejection::of_frame(ProtocolError::invalid_request(
                "a message must be a JSON object",
            )));
        }
    };
    let notification =
        !object.contains_key("id") && matches!(object.get("method"), Some(Value::String(_)));
    let id = match object.remove("id") {
        None => None,
        Some(value) => match serde_json::from_value::<Id>(value.clone()) {
            Ok(id) => Some(id),
            Err(_) if value.is_null() && object.contains_key("error") => None,
            Err(_) => {
                return Err(Rejection::of_frame(ProtocolError::invalid_request(
                    "id must be a string or an integer",
                )));
            }
        },
    };
    let reject = |error: ProtocolError| Rejection {
        id: id.clone(),
        error,
        notification,
    };
    match object.remove("jsonrpc") {
        Some(Value::String(version)) if version == JSONRPC_VERSION => {}
        _ => {
            return Err(reject(ProtocolError::invalid_request(
                "jsonrpc must be \"2.0\"",
            )));
        }
    }

    if let Some(method) = object.remove("method") {
        let Value::String(method) = method else {
            return Err(reject(ProtocolError::invalid_request(
                "method must be a string",
            )));
        };
        let params = match object.remove("params") {
            None => Value::Object(Map::new()),
            Some(params @ Value::Object(_)) => params,
            Some(_) => {
                return Err(reject(ProtocolError::invalid_params(
                    "params must be an object",
                )));
            }
        };
        if let Some(member) = object.keys().next() {
            return Err(reject(ProtocolError::invalid_request(format!(
                "unknown member {member:?}"
            ))));
        }
        return Ok(match id {
            Some(id) => Message::Request(Request { id, method, params }),
            None => Message::Notification(Notification { method, params }),
        });
    }

    let result = match (object.remove("result"), object.remove("error")) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => Err(ProtocolError::from_error_value(&error)),
        _ => {
            return Err(reject(ProtocolError::invalid_request(
                "a response carries exactly one of result and error",
            )));
        }
    };
    if let Some(member) = object.keys().next() {
        return Err(reject(ProtocolError::invalid_request(format!(
            "unknown member {member:?}"
        ))));
    }
    if id.is_none() && result.is_ok() {
        return Err(reject(ProtocolError::invalid_request(
            "a result needs an id",
        )));
    }
    Ok(Message::Response(Response { id, result }))
}

impl Request {
    pub fn new<P: Serialize>(id: Id, method: &str, params: &P) -> Result<Self, ProtocolError> {
        Ok(Self {
            id,
            method: method.to_owned(),
            params: params_value(params)?,
        })
    }

    /// Decode the params strictly; a failure is `invalidParams`.
    pub fn params<T: DeserializeOwned>(&self) -> Result<T, ProtocolError> {
        decode_params(&self.params)
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": self.id,
            "method": self.method,
            "params": self.params,
        })
    }

    pub fn to_text(&self) -> String {
        self.to_value().to_string()
    }
}

impl Notification {
    pub fn new<P: Serialize>(method: &str, params: &P) -> Result<Self, ProtocolError> {
        Ok(Self {
            method: method.to_owned(),
            params: params_value(params)?,
        })
    }

    /// Decode the params strictly; a failure is `invalidParams`.
    pub fn params<T: DeserializeOwned>(&self) -> Result<T, ProtocolError> {
        decode_params(&self.params)
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "jsonrpc": JSONRPC_VERSION,
            "method": self.method,
            "params": self.params,
        })
    }

    pub fn to_text(&self) -> String {
        self.to_value().to_string()
    }
}

impl Response {
    pub fn success<T: Serialize>(id: Id, result: &T) -> Self {
        let result = serde_json::to_value(result)
            .map_err(|error| ProtocolError::internal(format!("encode result: {error}")));
        Self {
            id: Some(id),
            result,
        }
    }

    pub fn failure(id: Option<Id>, error: ProtocolError) -> Self {
        Self {
            id,
            result: Err(error),
        }
    }

    pub fn from_result<T: Serialize>(id: Id, result: Result<T, ProtocolError>) -> Self {
        match result {
            Ok(value) => Self::success(id, &value),
            Err(error) => Self::failure(Some(id), error),
        }
    }

    /// Decode a successful result strictly. The two failures stay apart: the
    /// peer answered with an error ([`ResponseError::Failed`]), or its result
    /// breaks the contract ([`ResponseError::Undecodable`]).
    pub fn decode<T: DeserializeOwned>(self) -> Result<T, ResponseError> {
        let value = self.result.map_err(ResponseError::Failed)?;
        serde_path_to_error::deserialize(&value)
            .map_err(|error| ResponseError::Undecodable(describe_path_error(&error)))
    }

    pub fn to_value(&self) -> Value {
        let id = self.id.as_ref().map_or(Value::Null, |id| {
            serde_json::to_value(id).unwrap_or(Value::Null)
        });
        match &self.result {
            Ok(result) => {
                serde_json::json!({"jsonrpc": JSONRPC_VERSION, "id": id, "result": result})
            }
            Err(error) => serde_json::json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": id,
                "error": ErrorObject::from(error.clone()),
            }),
        }
    }

    /// Encode for sending. A response larger than
    /// [`MAX_MESSAGE_BYTES`] is replaced by a `payloadTooLarge` error for the
    /// same id, so a server never sends a frame its peer must drop.
    pub fn to_text(&self) -> String {
        let text = self.to_value().to_string();
        if (text.len() as u64) <= MAX_MESSAGE_BYTES {
            return text;
        }
        Self::failure(
            self.id.clone(),
            ProtocolError::payload_too_large(
                MAX_MESSAGE_BYTES,
                format!("response of {} bytes exceeds the message limit", text.len()),
            ),
        )
        .to_value()
        .to_string()
    }
}

/// Why a response yielded no typed result.
#[derive(Clone, Debug, PartialEq, Error)]
pub enum ResponseError {
    /// The peer answered with this error.
    #[error(transparent)]
    Failed(ProtocolError),

    /// The result does not decode as the method's response type: the peer
    /// broke the contract.
    #[error("result does not decode: {0}")]
    Undecodable(String),
}

/// Check an encoded outgoing message against the message limit.
pub fn check_message_size(text: &str) -> Result<(), ProtocolError> {
    let length = text.len() as u64;
    if length > MAX_MESSAGE_BYTES {
        return Err(ProtocolError::payload_too_large(
            MAX_MESSAGE_BYTES,
            format!("message of {length} bytes exceeds the {MAX_MESSAGE_BYTES}-byte limit"),
        ));
    }
    Ok(())
}

fn params_value<P: Serialize>(params: &P) -> Result<Value, ProtocolError> {
    match serde_json::to_value(params) {
        Ok(value @ Value::Object(_)) => Ok(value),
        Ok(_) => Err(ProtocolError::internal(
            "params must serialize as an object",
        )),
        Err(error) => Err(ProtocolError::internal(format!("encode params: {error}"))),
    }
}

/// Strict decode; a failure is `invalidParams` naming the offending field by
/// its wire path when the decoder knows it.
pub fn decode_params<T: DeserializeOwned>(params: &Value) -> Result<T, ProtocolError> {
    serde_path_to_error::deserialize(params).map_err(|error| {
        let field = path_of(&error);
        let message = describe_path_error(&error);
        match field {
            Some(field) => ProtocolError::invalid_field(field, message),
            None => ProtocolError::invalid_params(message),
        }
    })
}

fn path_of(error: &serde_path_to_error::Error<serde_json::Error>) -> Option<String> {
    let path = error.path();
    path.iter().next().is_some().then(|| path.to_string())
}

fn describe_path_error(error: &serde_path_to_error::Error<serde_json::Error>) -> String {
    match path_of(error) {
        Some(path) => format!("{path}: {}", error.inner()),
        None => error.inner().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::error::{ErrorData, ProtocolErrorKind};

    fn rejected(text: &str) -> Rejection {
        parse(text).expect_err("rejected")
    }

    #[test]
    fn missing_params_mean_an_empty_object() {
        let Message::Request(request) =
            parse(r#"{"jsonrpc":"2.0","id":1,"method":"env/getIdle"}"#).expect("request")
        else {
            panic!("expected a request");
        };
        assert_eq!(request.params, json!({}));
        assert_eq!(request.id, Id::Number(1));
    }

    #[test]
    fn framing_errors_use_reserved_kinds_and_null_ids() {
        let parse_error = rejected("not json");
        assert_eq!(parse_error.error.kind(), ProtocolErrorKind::ParseError);
        assert_eq!(parse_error.id, None);
        let batch = rejected(r#"[{"jsonrpc":"2.0","id":1,"method":"x"}]"#);
        assert_eq!(batch.error.kind(), ProtocolErrorKind::InvalidRequest);
        assert_eq!(batch.id, None);
        let version = rejected(r#"{"jsonrpc":"1.0","id":7,"method":"x"}"#);
        assert_eq!(version.error.kind(), ProtocolErrorKind::InvalidRequest);
        assert_eq!(version.id, Some(Id::Number(7)));
        let params = rejected(r#"{"jsonrpc":"2.0","id":"a","method":"x","params":[1]}"#);
        assert_eq!(params.error.kind(), ProtocolErrorKind::InvalidParams);
        let member = rejected(r#"{"jsonrpc":"2.0","id":1,"method":"x","extra":true}"#);
        assert_eq!(member.error.kind(), ProtocolErrorKind::InvalidRequest);
    }

    #[test]
    fn notifications_carry_no_id() {
        let message = parse(r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":3}}"#)
            .expect("notification");
        let Message::Notification(notification) = message else {
            panic!("expected a notification");
        };
        let params: CancelRequestParams = notification.params().expect("params");
        assert_eq!(params.id, Id::Number(3));
    }

    #[test]
    fn error_responses_round_trip_with_null_ids() {
        let response = Response::failure(None, ProtocolError::parse_error("bad"));
        let text = response.to_text();
        let Message::Response(parsed) = parse(&text).expect("response") else {
            panic!("expected a response");
        };
        assert_eq!(parsed, response);
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["id"],
            Value::Null
        );
    }

    #[test]
    fn oversized_responses_become_payload_too_large() {
        let big = "x".repeat(MAX_MESSAGE_BYTES as usize + 1);
        let text = Response::success(Id::Number(4), &json!({"data": big})).to_text();
        let Message::Response(parsed) = parse(&text).expect("response") else {
            panic!("expected a response");
        };
        assert_eq!(parsed.id, Some(Id::Number(4)));
        assert_eq!(
            parsed.result.unwrap_err().kind(),
            ProtocolErrorKind::PayloadTooLarge
        );
    }

    #[test]
    fn params_decode_failures_are_invalid_params_naming_the_field() {
        let request = Request::new(Id::Number(1), "x", &json!({"id": true})).unwrap();
        let error = request.params::<CancelRequestParams>().unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
        assert!(matches!(
            error.data,
            ErrorData::InvalidParams { field: Some(ref field), .. } if field == "id"
        ));
    }

    #[test]
    fn malformed_notifications_are_never_answered() {
        for text in [
            r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":[3]}"#,
            r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":3},"extra":1}"#,
            r#"{"jsonrpc":"1.0","method":"$/cancelRequest","params":{"id":3}}"#,
            r#"{"method":"$/cancelRequest"}"#,
        ] {
            let rejection = rejected(text);
            assert!(rejection.is_notification(), "{text}");
            assert_eq!(rejection.into_response(), None, "{text}");
        }
    }

    #[test]
    fn malformed_requests_and_frames_are_answered() {
        for (text, id) in [
            ("not json", None),
            ("[]", None),
            ("7", None),
            (
                r#"{"jsonrpc":"2.0","id":5,"method":"x","params":[1]}"#,
                Some(Id::Number(5)),
            ),
            (r#"{"jsonrpc":"2.0","id":{},"method":"x"}"#, None),
            (r#"{"jsonrpc":"2.0","method":7}"#, None),
            (
                r#"{"jsonrpc":"2.0","id":"r","result":1,"error":{}}"#,
                Some(Id::String("r".to_owned())),
            ),
        ] {
            let rejection = rejected(text);
            assert!(!rejection.is_notification(), "{text}");
            let response = rejection.into_response().expect("answered");
            assert_eq!(response.id, id, "{text}");
            assert!(response.result.is_err());
        }
    }

    #[test]
    fn responses_parse_strictly() {
        let Message::Response(ok) =
            parse(r#"{"jsonrpc":"2.0","id":1,"result":{"idleForMs":0}}"#).expect("response")
        else {
            panic!("expected a response");
        };
        assert_eq!(ok.id, Some(Id::Number(1)));
        for bad in [
            r#"{"jsonrpc":"2.0","id":null,"result":{}}"#,
            r#"{"jsonrpc":"2.0","result":{}}"#,
            r#"{"id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{},"extra":true}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let Message::Response(failed) = parse(
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"m","data":{"kind":"parseError"}}}"#,
        )
        .expect("error response") else {
            panic!("expected a response");
        };
        assert_eq!(failed.id, None);
        assert_eq!(
            failed.result.unwrap_err().kind(),
            ProtocolErrorKind::ParseError
        );
    }

    #[test]
    fn decode_tells_a_failed_call_from_a_broken_result() {
        let failed = Response::failure(Some(Id::Number(1)), ProtocolError::internal("x"));
        assert!(matches!(
            failed.decode::<CancelRequestParams>(),
            Err(ResponseError::Failed(_))
        ));
        let broken = Response::success(Id::Number(1), &json!({"id": [1]}));
        assert!(matches!(
            broken.decode::<CancelRequestParams>(),
            Err(ResponseError::Undecodable(_))
        ));
    }
}
