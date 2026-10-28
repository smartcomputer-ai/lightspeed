//! The controller-plane client: the runtime's connection to one provider
//! controller.

use std::{sync::Arc, time::Duration};

use environment_protocol::{
    CONTROLLER_PROTOCOL_VERSION,
    control::handshake::{ControllerInitializeParams, ControllerInitializeResponse},
    handshake::Plane,
    jsonrpc::{Request, Response},
    manifest::{
        ControllerInitialize, IngressEnsure, IngressRemove, RequestMethod, TargetClose,
        TargetCreate, TargetGet, TargetSetPower, TemplateList,
    },
};
use futures_util::future::BoxFuture;

use crate::{
    connection::{Connection, InProcessHandler},
    data::typed_calls,
    error::ClientError,
    options::ConnectOptions,
};

/// A controller-plane connection after a successful `initialize`.
///
/// Same shape as [`DataClient`](crate::DataClient): cheap to clone, calls
/// take `&self` and may run concurrently, a dropped call is abandoned. The
/// controller plane has no `$/cancelRequest`; a mutation abandoned by its
/// caller still runs to completion on the provider.
#[derive(Clone)]
pub struct ControllerClient {
    inner: Arc<Inner>,
}

struct Inner {
    connection: Connection,
    initialize: ControllerInitializeResponse,
}

impl ControllerClient {
    /// Dial the controller URL and run `initialize` with
    /// [`CONTROLLER_PROTOCOL_VERSION`]. Pass the provider credential, when
    /// one is configured, as [`ConnectOptions::bearer_token`]; without one
    /// no `Authorization` header is sent. Fails with
    /// [`ClientError::VersionMismatch`] or [`ClientError::WrongPlane`]
    /// before the connection is used.
    pub async fn connect_initialized(
        endpoint: &str,
        options: ConnectOptions,
    ) -> Result<(Self, ControllerInitializeResponse), ClientError> {
        let connection = Connection::connect(endpoint, &options, Plane::Controller).await?;
        let initialized = initialize(&connection, &options).await;
        match initialized {
            Ok(initialize) => Ok((
                Self {
                    inner: Arc::new(Inner {
                        connection,
                        initialize: initialize.clone(),
                    }),
                },
                initialize,
            )),
            Err(error) => {
                let _ = connection.close().await;
                Err(error)
            }
        }
    }

    /// A client whose requests `handler` answers in process, for tests that
    /// need no socket (see [`DataClient::in_process`](crate::DataClient::in_process)).
    pub fn in_process(
        initialize: ControllerInitializeResponse,
        handler: impl Fn(Request) -> BoxFuture<'static, Result<Response, ClientError>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let handler: InProcessHandler = Arc::new(handler);
        Self {
            inner: Arc::new(Inner {
                connection: Connection::in_process(Plane::Controller, handler),
                initialize,
            }),
        }
    }

    /// The controller's `initialize` response: its capabilities.
    pub fn initialize_response(&self) -> &ControllerInitializeResponse {
        &self.inner.initialize
    }

    /// Call any controller-plane method, with the connection's default
    /// deadline ([`ConnectOptions::request_timeout`]).
    pub async fn call<M: RequestMethod>(
        &self,
        params: &M::Params,
    ) -> Result<M::Response, ClientError> {
        self.inner.connection.call::<M>(params, None).await
    }

    /// [`call`](Self::call) with this deadline instead of the default one.
    pub async fn call_with_timeout<M: RequestMethod>(
        &self,
        params: &M::Params,
        timeout: Duration,
    ) -> Result<M::Response, ClientError> {
        self.inner.connection.call::<M>(params, Some(timeout)).await
    }

    /// Close the connection for every clone. Idempotent.
    pub async fn close(&self) -> Result<(), ClientError> {
        self.inner.connection.close().await
    }

    /// True once the connection ended, by either side.
    pub fn is_closed(&self) -> bool {
        self.inner.connection.is_closed()
    }

    typed_calls! {
        /// `template/list`.
        list_templates => TemplateList;
        /// `target/create`.
        create_target => TargetCreate;
        /// `target/get`.
        get_target => TargetGet;
        /// `target/setPower`.
        set_target_power => TargetSetPower;
        /// `target/close`.
        close_target => TargetClose;
        /// `ingress/ensure`.
        ensure_ingress => IngressEnsure;
        /// `ingress/remove`.
        remove_ingress => IngressRemove;
    }
}

impl std::fmt::Debug for ControllerClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControllerClient")
            .field("server_info", &self.inner.initialize.server_info)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

/// Run the controller `initialize` on a dialed connection.
pub(crate) async fn initialize(
    connection: &Connection,
    options: &ConnectOptions,
) -> Result<ControllerInitializeResponse, ClientError> {
    connection
        .initialize(
            ControllerInitialize::NAME,
            &ControllerInitializeParams {
                protocol_version: CONTROLLER_PROTOCOL_VERSION,
                client_info: options.client_info.clone(),
            },
            CONTROLLER_PROTOCOL_VERSION,
            options.initialize_timeout,
        )
        .await
}
