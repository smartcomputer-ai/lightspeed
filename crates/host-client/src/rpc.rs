//! Minimal JSON-RPC 2.0 client machinery.

use std::collections::VecDeque;

use async_trait::async_trait;
use host_protocol::error::{HostError, HostErrorCode};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::error::{HostClientError, HostClientResult};

#[async_trait]
pub trait JsonRpcTransport: Send {
    async fn send(&mut self, message: Value) -> HostClientResult<()>;
    async fn recv(&mut self) -> HostClientResult<Option<Value>>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct JsonRpcNotification {
    pub method: String,
    pub params: Value,
}

pub struct JsonRpcClient<T> {
    transport: T,
    next_id: u64,
    notifications: VecDeque<JsonRpcNotification>,
}

impl<T> JsonRpcClient<T>
where
    T: JsonRpcTransport,
{
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 1,
            notifications: VecDeque::new(),
        }
    }

    pub fn into_inner(self) -> T {
        self.transport
    }

    pub async fn request<P, R>(&mut self, method: &str, params: &P) -> HostClientResult<R>
    where
        P: Serialize + Sync,
        R: DeserializeOwned,
    {
        let id = self.next_request_id();
        self.transport
            .send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": serde_json::to_value(params)?,
            }))
            .await?;

        loop {
            let message = self
                .transport
                .recv()
                .await?
                .ok_or(HostClientError::TransportClosed)?;
            if let Some(notification) = parse_notification(&message)? {
                self.notifications.push_back(notification);
                continue;
            }

            let Some(response_id) = message.get("id").and_then(Value::as_u64) else {
                return Err(HostClientError::InvalidMessage(
                    "response missing numeric id".to_owned(),
                ));
            };
            if response_id != id {
                return Err(HostClientError::InvalidMessage(format!(
                    "unexpected response id {response_id}, expected {id}"
                )));
            }

            if let Some(error) = message.get("error") {
                return Err(HostClientError::Host(parse_error(error)?));
            }

            let result = message.get("result").ok_or_else(|| {
                HostClientError::InvalidMessage("response missing result".to_owned())
            })?;
            return serde_json::from_value(result.clone()).map_err(Into::into);
        }
    }

    pub async fn notify<P>(&mut self, method: &str, params: &P) -> HostClientResult<()>
    where
        P: Serialize + Sync,
    {
        self.transport
            .send(json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": serde_json::to_value(params)?,
            }))
            .await
    }

    pub async fn next_notification(&mut self) -> HostClientResult<Option<JsonRpcNotification>> {
        if let Some(notification) = self.notifications.pop_front() {
            return Ok(Some(notification));
        }

        loop {
            let Some(message) = self.transport.recv().await? else {
                return Ok(None);
            };
            if let Some(notification) = parse_notification(&message)? {
                return Ok(Some(notification));
            }
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

fn parse_notification(message: &Value) -> HostClientResult<Option<JsonRpcNotification>> {
    if message.get("id").is_some() {
        return Ok(None);
    }

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    Ok(Some(JsonRpcNotification {
        method: method.to_owned(),
        params,
    }))
}

fn parse_error(value: &Value) -> HostClientResult<HostError> {
    if let Ok(error) = serde_json::from_value::<HostError>(value.clone()) {
        return Ok(error);
    }

    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("JSON-RPC error");
    Ok(HostError::new(HostErrorCode::Internal, message))
}
