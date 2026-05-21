//! Typed client for host protocol data-plane and controller-plane calls.
//!
//! The crate owns transport and JSON-RPC mechanics while reusing pure protocol
//! records from `host-protocol`.

pub mod control;
pub mod data;
pub mod error;
pub mod rpc;
pub mod transport;

pub use control::HostControllerClient;
pub use data::HostDataClient;
pub use error::{HostClientError, HostClientResult};
pub use rpc::{JsonRpcClient, JsonRpcNotification, JsonRpcTransport};
pub use transport::{WebSocketConnectOptions, WebSocketTransport};
