//! Clients of the environment protocol's data and controller planes.
//!
//! [`DataClient`] talks to one envd, [`ControllerClient`] to one provider
//! controller. Both are opened with `connect_initialized`, which dials
//! (`ws://`, `wss://` or `unix:///path`), runs `initialize` and fails with a
//! typed [`ClientError::VersionMismatch`] or [`ClientError::WrongPlane`]
//! before anything else is sent. Calls are typed by the protocol's method
//! markers ([`environment_protocol::manifest`]):
//!
//! ```no_run
//! # async fn example() -> Result<(), environment_client::ClientError> {
//! use environment_client::{ConnectOptions, DataClient};
//! use environment_protocol::{
//!     data::idle::GetIdleParams, handshake::ImplementationInfo, manifest::EnvGetIdle,
//! };
//!
//! let info = ImplementationInfo::new("lightspeed-runtime").expect("valid name");
//! let (client, initialize) =
//!     DataClient::connect_initialized("ws://127.0.0.1:7070", ConnectOptions::new(info)).await?;
//! let idle = client.call::<EnvGetIdle>(&GetIdleParams {}).await?;
//! # let _ = (initialize, idle);
//! # Ok(())
//! # }
//! ```
//!
//! # Connection behaviour
//!
//! - **Multiplexed.** Calls take `&self`, may be issued concurrently on
//!   one connection and are matched to responses by id. Clients are cheap
//!   to clone.
//! - **Cancel-safe.** A call whose future is dropped, or whose deadline
//!   passes, is forgotten; its late response is discarded. On the data
//!   plane an abandoned abortable call (`process/read`, `job/read`,
//!   `fs/searchText`, `fs/globFiles`, `fs/scan`) is also sent
//!   `$/cancelRequest`.
//! - **Deadlines.** [`ConnectOptions`] bounds connect and `initialize`
//!   (10 s each by default) and can set a default deadline for every call;
//!   `call_with_timeout` overrides it per call.
//! - **Keepalive.** While any call is pending the client sends a WebSocket
//!   ping every [`ConnectOptions::keepalive_interval`] (20 s by default),
//!   so relays never idle-close a long wait.
//! - **Limits.** Messages and frames are limited to
//!   [`MAX_MESSAGE_BYTES`](environment_protocol::limits::MAX_MESSAGE_BYTES)
//!   in both directions; a larger request fails with
//!   [`ClientError::PayloadTooLarge`] without being sent.
//! - **Servers only answer.** A request or notification from the server, a
//!   binary frame or an unparseable frame closes the connection and fails
//!   every pending call with [`ClientError::InvalidMessage`].
//!
//! Tests that need no socket build clients with `in_process`.

mod connection;
mod controller;
mod data;
mod dial;
mod error;
mod options;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use connection::InProcessHandler;
pub use controller::ControllerClient;
pub use data::DataClient;
pub use error::ClientError;
pub use options::{ConnectOptions, DEFAULT_CONNECT_TIMEOUT, DEFAULT_INITIALIZE_TIMEOUT};
