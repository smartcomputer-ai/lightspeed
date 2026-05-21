use std::collections::VecDeque;

use async_trait::async_trait;
use host_client::{
    HostClientError, HostClientResult, HostControllerClient, HostDataClient, JsonRpcTransport,
};
use host_protocol::{
    control::handshake::ControllerInitializeParams,
    data::{fs::ReadFileParams, methods::PROCESS_OUTPUT_METHOD},
    error::HostErrorCode,
    shared::{ByteChunk, CURRENT_PROTOCOL_VERSION, HostPath},
};
use serde_json::{Value, json};

#[derive(Default)]
struct MockTransport {
    sent: Vec<Value>,
    recv: VecDeque<Value>,
}

impl MockTransport {
    fn with_recv(messages: impl IntoIterator<Item = Value>) -> Self {
        Self {
            sent: Vec::new(),
            recv: messages.into_iter().collect(),
        }
    }
}

#[async_trait]
impl JsonRpcTransport for MockTransport {
    async fn send(&mut self, message: Value) -> HostClientResult<()> {
        self.sent.push(message);
        Ok(())
    }

    async fn recv(&mut self) -> HostClientResult<Option<Value>> {
        Ok(self.recv.pop_front())
    }
}

#[tokio::test]
async fn data_client_sends_typed_request_and_decodes_response() {
    let transport = MockTransport::with_recv([json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "data": "aGk="
        }
    })]);
    let mut client = HostDataClient::new(transport);

    let response = client
        .read_file(&ReadFileParams {
            path: HostPath::new("README.md").expect("path"),
        })
        .await
        .expect("response");

    assert_eq!(response.data, ByteChunk::from(b"hi".as_slice()));

    let transport = client.into_rpc().into_inner();
    assert_eq!(
        transport.sent,
        vec![json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "fs/readFile",
            "params": {
                "path": "README.md"
            }
        })]
    );
}

#[tokio::test]
async fn data_client_stashes_notifications_seen_while_waiting_for_response() {
    let transport = MockTransport::with_recv([
        json!({
            "jsonrpc": "2.0",
            "method": PROCESS_OUTPUT_METHOD,
            "params": {
                "chunk": "b2sK",
                "processId": "proc-1",
                "seq": 1,
                "stream": "stdout"
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "data": "aGk="
            }
        }),
    ]);
    let mut client = HostDataClient::new(transport);

    client
        .read_file(&ReadFileParams {
            path: HostPath::new("README.md").expect("path"),
        })
        .await
        .expect("response");

    let notification = client
        .next_notification()
        .await
        .expect("notification read")
        .expect("notification");
    assert_eq!(notification.method, PROCESS_OUTPUT_METHOD);
    assert_eq!(notification.params["processId"], "proc-1");
}

#[tokio::test]
async fn data_client_maps_protocol_error_payloads() {
    let transport = MockTransport::with_recv([json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {
            "code": "notFound",
            "message": "missing"
        }
    })]);
    let mut client = HostDataClient::new(transport);

    let error = client
        .read_file(&ReadFileParams {
            path: HostPath::new("missing.txt").expect("path"),
        })
        .await
        .expect_err("host error");

    match error {
        HostClientError::Host(error) => assert_eq!(error.code, HostErrorCode::NotFound),
        other => panic!("unexpected error {other:?}"),
    }
}

#[tokio::test]
async fn controller_client_sends_typed_initialize_request() {
    let transport = MockTransport::with_recv([json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "capabilities": {
                "attachTarget": true,
                "closeTarget": true,
                "createTarget": true,
                "getTarget": true,
                "listTargets": true
            },
            "implementation": {
                "name": "test-controller",
                "version": "0.1.0"
            },
            "protocolVersion": 1
        }
    })]);
    let mut client = HostControllerClient::new(transport);

    let response = client
        .initialize(&ControllerInitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "forge-test".to_owned(),
        })
        .await
        .expect("response");

    assert!(response.capabilities.create_target);
    assert_eq!(response.implementation.name, "test-controller");

    let transport = client.into_rpc().into_inner();
    assert_eq!(transport.sent[0]["method"], "controller/initialize");
    assert_eq!(transport.sent[0]["params"]["clientName"], "forge-test");
}
