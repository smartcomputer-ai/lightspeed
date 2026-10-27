//! The data-plane client: the runtime's connection to one envd.

use std::{sync::Arc, time::Duration};

use environment_protocol::{
    ENVD_PROTOCOL_VERSION,
    data::handshake::{InitializeParams, InitializeResponse},
    handshake::Plane,
    jsonrpc::{Request, Response},
    manifest::{
        DataInitialize, EnvGetIdle, FsCreateDirectory, FsGetMetadata, FsGlobFiles, FsReadDirectory,
        FsReadFile, FsRemove, FsRename, FsScan, FsSearchText, FsWriteFile, JobCancel, JobRead,
        JobStart, ProcessRead, ProcessStart, ProcessTerminate, ProcessWrite, RequestMethod,
        TransferAbort, TransferAdvance, TransferAppend, TransferBegin, TransferCommit,
        TransferGetStatus, TransferListMissing, TransferRead, TransferReadInventory, TransferWrite,
    },
};
use futures_util::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{
    connection::{Connection, InProcessHandler},
    error::ClientError,
    options::ConnectOptions,
};

/// Typed sugar: one method per manifest entry, each a [`call`] of its
/// marker.
///
/// [`call`]: DataClient::call
macro_rules! typed_calls {
    ($( $(#[$doc:meta])* $name:ident => $marker:ident; )+) => {
        $(
            $(#[$doc])*
            pub async fn $name(
                &self,
                params: &<$marker as RequestMethod>::Params,
            ) -> Result<<$marker as RequestMethod>::Response, ClientError> {
                self.call::<$marker>(params).await
            }
        )+
    };
}
pub(crate) use typed_calls;

/// A data-plane connection after a successful `initialize`.
///
/// Cheap to clone; clones share one socket. Calls take `&self`, may run
/// concurrently and are answered in any order. Dropping a call's future
/// abandons it (an abortable one is sent `$/cancelRequest`). The socket
/// closes when [`close`](Self::close) is called or the last clone is
/// dropped.
#[derive(Clone)]
pub struct DataClient {
    inner: Arc<Inner>,
}

struct Inner {
    connection: Connection,
    initialize: InitializeResponse,
}

impl DataClient {
    /// Dial `endpoint` (`ws://`, `wss://` or `unix:///path`) and run
    /// `initialize` with [`ENVD_PROTOCOL_VERSION`]. Fails with
    /// [`ClientError::VersionMismatch`] or [`ClientError::WrongPlane`]
    /// before the connection is used; sends nothing else.
    pub async fn connect_initialized(
        endpoint: &str,
        options: ConnectOptions,
    ) -> Result<(Self, InitializeResponse), ClientError> {
        let connection = Connection::connect(endpoint, &options, Plane::Data).await?;
        Self::initialize(connection, &options).await
    }

    /// [`connect_initialized`](Self::connect_initialized) over a byte
    /// stream that is already connected to a data server, such as one end
    /// of [`tokio::io::duplex`] or an accepted Unix socket. The upgrade
    /// targets `/` with `Host: localhost`.
    pub async fn connect_initialized_over<S>(
        stream: S,
        options: ConnectOptions,
    ) -> Result<(Self, InitializeResponse), ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let connection = Connection::connect_over(stream, &options, Plane::Data).await?;
        Self::initialize(connection, &options).await
    }

    /// A client whose requests `handler` answers in process, for tests that
    /// need no socket. `initialize` is what
    /// [`initialize_response`](Self::initialize_response) reports; the
    /// handler sees every later request, with the id this client chose.
    /// Requests and responses pass the same size limit and strict decoding
    /// as on a socket; calls have no default deadline. A handler `Err` fails
    /// that one call with the error, a stand-in for a lost response.
    pub fn in_process(
        initialize: InitializeResponse,
        handler: impl Fn(Request) -> BoxFuture<'static, Result<Response, ClientError>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let handler: InProcessHandler = Arc::new(handler);
        Self {
            inner: Arc::new(Inner {
                connection: Connection::in_process(Plane::Data, handler),
                initialize,
            }),
        }
    }

    async fn initialize(
        connection: Connection,
        options: &ConnectOptions,
    ) -> Result<(Self, InitializeResponse), ClientError> {
        let params = InitializeParams {
            protocol_version: ENVD_PROTOCOL_VERSION,
            client_info: options.client_info.clone(),
        };
        let initialized = connection
            .initialize::<_, InitializeResponse>(
                DataInitialize::NAME,
                &params,
                ENVD_PROTOCOL_VERSION,
                options.initialize_timeout,
            )
            .await;
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

    /// The daemon's `initialize` response: instance id, capabilities,
    /// ceilings and default cwd.
    pub fn initialize_response(&self) -> &InitializeResponse {
        &self.inner.initialize
    }

    /// Call any data-plane method, with the connection's default deadline
    /// ([`ConnectOptions::request_timeout`]).
    pub async fn call<M: RequestMethod>(
        &self,
        params: &M::Params,
    ) -> Result<M::Response, ClientError> {
        self.inner.connection.call::<M>(params, None).await
    }

    /// [`call`](Self::call) with this deadline instead of the default one.
    /// When it passes, an abortable method is sent `$/cancelRequest` and the
    /// call fails with [`ClientError::DeadlineExceeded`].
    pub async fn call_with_timeout<M: RequestMethod>(
        &self,
        params: &M::Params,
        timeout: Duration,
    ) -> Result<M::Response, ClientError> {
        self.inner.connection.call::<M>(params, Some(timeout)).await
    }

    /// Close the connection for every clone: pending calls fail with
    /// [`ClientError::Closed`]. Idempotent.
    pub async fn close(&self) -> Result<(), ClientError> {
        self.inner.connection.close().await
    }

    /// True once the connection ended, by either side.
    pub fn is_closed(&self) -> bool {
        self.inner.connection.is_closed()
    }

    typed_calls! {
        /// `env/getIdle`.
        get_idle => EnvGetIdle;
        /// `fs/readFile`.
        read_file => FsReadFile;
        /// `fs/writeFile`.
        write_file => FsWriteFile;
        /// `fs/createDirectory`.
        create_directory => FsCreateDirectory;
        /// `fs/getMetadata`.
        get_metadata => FsGetMetadata;
        /// `fs/readDirectory`.
        read_directory => FsReadDirectory;
        /// `fs/remove`.
        remove => FsRemove;
        /// `fs/rename`.
        rename => FsRename;
        /// `fs/searchText`.
        search_text => FsSearchText;
        /// `fs/globFiles`.
        glob_files => FsGlobFiles;
        /// `fs/scan`.
        scan => FsScan;
        /// `transfer/begin`.
        transfer_begin => TransferBegin;
        /// `transfer/advance`.
        transfer_advance => TransferAdvance;
        /// `transfer/getStatus`.
        transfer_get_status => TransferGetStatus;
        /// `transfer/readInventory`.
        transfer_read_inventory => TransferReadInventory;
        /// `transfer/append`.
        transfer_append => TransferAppend;
        /// `transfer/listMissing`.
        transfer_list_missing => TransferListMissing;
        /// `transfer/read`.
        transfer_read => TransferRead;
        /// `transfer/write`.
        transfer_write => TransferWrite;
        /// `transfer/commit`.
        transfer_commit => TransferCommit;
        /// `transfer/abort`.
        transfer_abort => TransferAbort;
        /// `process/start`.
        start_process => ProcessStart;
        /// `process/read`.
        read_process => ProcessRead;
        /// `process/write`.
        write_process => ProcessWrite;
        /// `process/terminate`.
        terminate_process => ProcessTerminate;
        /// `job/start`.
        start_jobs => JobStart;
        /// `job/read`.
        read_jobs => JobRead;
        /// `job/cancel`.
        cancel_jobs => JobCancel;
    }
}

impl std::fmt::Debug for DataClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DataClient")
            .field(
                "daemon_instance_id",
                &self.inner.initialize.daemon_instance_id,
            )
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}
