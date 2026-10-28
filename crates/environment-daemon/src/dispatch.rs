//! Data-plane request dispatch, driven by the protocol's method manifest.
//!
//! The connection loop ([`crate::server`]) owns the handshake and hands
//! every request after `initialize` to [`handle_request`]. Dispatch looks
//! the method up in [`manifest`], refuses it when its capability is absent,
//! touches the idle clock for `use` methods, decodes the params strictly
//! and calls the handler. Handlers receive a [`RequestContext`] whose
//! cancellation token fires on `$/cancelRequest` and when the connection
//! closes.

use std::{future::Future, sync::Arc};

use environment_protocol::{
    error::{Capability, ProtocolError},
    handshake::Plane,
    jsonrpc,
    manifest::{
        self, Activity, DataInitialize, Direction, EnvGetIdle, FsCreateDirectory, FsGetMetadata,
        FsGlobFiles, FsReadDirectory, FsReadFile, FsRemove, FsRename, FsScan, FsSearchText,
        FsWriteFile, JobCancel, JobRead, JobStart, MessageKind, ProcessRead, ProcessStart,
        ProcessTerminate, ProcessWrite, RequestMethod, TransferAbort, TransferAdvance,
        TransferAppend, TransferBegin, TransferCommit, TransferGetStatus, TransferListMissing,
        TransferRead, TransferReadInventory, TransferWrite,
    },
};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::DaemonRuntime;

/// One data connection.
pub struct ConnectionInfo {
    /// Daemon-unique, increasing; for logs and per-connection ownership.
    pub connection_id: u64,
    /// Cancelled when the connection's reader loop ends (peer close,
    /// transport error, daemon shutdown).
    pub closed: CancellationToken,
}

/// Passed to every handler.
#[derive(Clone)]
pub struct RequestContext {
    pub connection: Arc<ConnectionInfo>,
    /// Child of `connection.closed`; also cancelled by `$/cancelRequest`
    /// naming this request. Abortable handlers select on
    /// `cancel.cancelled()` and return `Err(ProtocolError::cancelled())`
    /// (the reply is dropped anyway when the connection closed).
    pub cancel: CancellationToken,
}

impl RequestContext {
    /// A context for one request on `connection`.
    pub fn new(connection: Arc<ConnectionInfo>) -> Self {
        let cancel = connection.closed.child_token();
        Self { connection, cancel }
    }

    /// A context on a connection of its own, for in-process callers and
    /// tests.
    pub fn detached() -> Self {
        Self::new(Arc::new(ConnectionInfo {
            connection_id: 0,
            closed: CancellationToken::new(),
        }))
    }
}

/// Runs once, after the response frame was written to the socket.
pub type WrittenHook = Box<dyn FnOnce() + Send + 'static>;

/// A successful answer plus an optional hook the writer task runs only
/// after the response frame was written to the socket.
pub struct Reply<T> {
    pub value: T,
    pub on_written: Option<WrittenHook>,
}

impl<T> Reply<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            on_written: None,
        }
    }

    pub fn with_commit(value: T, commit: impl FnOnce() + Send + 'static) -> Self {
        Self {
            value,
            on_written: Some(Box::new(commit)),
        }
    }

    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Reply<U> {
        Reply {
            value: map(self.value),
            on_written: self.on_written,
        }
    }
}

impl<T> std::fmt::Debug for Reply<T>
where
    T: std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reply")
            .field("value", &self.value)
            .field("on_written", &self.on_written.is_some())
            .finish()
    }
}

/// Dispatch one already-initialized data request: manifest lookup
/// (methodNotFound), capability check (capabilityUnavailable), activity
/// touch for `Activity::Use`, strict params decode with
/// `jsonrpc::decode_params` (invalidParams naming the field), handler call,
/// result encode. Public for in-process tests.
pub async fn handle_request(
    runtime: &DaemonRuntime,
    ctx: &RequestContext,
    method: &str,
    params: Value,
) -> Result<Reply<Value>, ProtocolError> {
    let spec = manifest::find(Plane::Data, method)
        .filter(|spec| {
            spec.kind == MessageKind::Request && spec.direction == Direction::ClientToServer
        })
        .ok_or_else(|| ProtocolError::method_not_found(method))?;
    if let Some(capability) = spec.capability {
        runtime.require_capability(capability)?;
    }
    if spec.activity == Some(Activity::Use) {
        runtime.touch_activity();
    }
    let params = &params;
    let filesystem = runtime.filesystem();
    let processes = runtime.processes();
    let jobs = runtime.jobs();
    match spec.name {
        DataInitialize::NAME => Err(ProtocolError::invalid_request(
            "initialize was already answered on this connection",
        )),
        EnvGetIdle::NAME => {
            answer::<EnvGetIdle, _, _>(params, |_| async { Ok(runtime.idle_report().await) }).await
        }
        FsReadFile::NAME => {
            answer::<FsReadFile, _, _>(params, |p| filesystem.read_file(ctx, p)).await
        }
        FsWriteFile::NAME => {
            answer::<FsWriteFile, _, _>(params, |p| filesystem.write_file(ctx, p)).await
        }
        FsCreateDirectory::NAME => {
            answer::<FsCreateDirectory, _, _>(params, |p| filesystem.create_directory(ctx, p)).await
        }
        FsGetMetadata::NAME => {
            answer::<FsGetMetadata, _, _>(params, |p| filesystem.get_metadata(ctx, p)).await
        }
        FsReadDirectory::NAME => {
            answer::<FsReadDirectory, _, _>(params, |p| filesystem.read_directory(ctx, p)).await
        }
        FsRemove::NAME => answer::<FsRemove, _, _>(params, |p| filesystem.remove(ctx, p)).await,
        FsRename::NAME => answer::<FsRename, _, _>(params, |p| filesystem.rename(ctx, p)).await,
        FsSearchText::NAME => {
            answer::<FsSearchText, _, _>(params, |p| filesystem.search_text(ctx, p)).await
        }
        FsGlobFiles::NAME => {
            answer::<FsGlobFiles, _, _>(params, |p| filesystem.glob_files(ctx, p)).await
        }
        FsScan::NAME => answer::<FsScan, _, _>(params, |p| filesystem.scan(ctx, p)).await,
        TransferBegin::NAME => {
            answer::<TransferBegin, _, _>(params, |p| filesystem.transfer_begin(ctx, p)).await
        }
        TransferAdvance::NAME => {
            answer::<TransferAdvance, _, _>(params, |p| filesystem.transfer_advance(ctx, p)).await
        }
        TransferGetStatus::NAME => {
            answer::<TransferGetStatus, _, _>(params, |p| filesystem.transfer_get_status(ctx, p))
                .await
        }
        TransferReadInventory::NAME => {
            answer::<TransferReadInventory, _, _>(params, |p| {
                filesystem.transfer_read_inventory(ctx, p)
            })
            .await
        }
        TransferAppend::NAME => {
            answer::<TransferAppend, _, _>(params, |p| filesystem.transfer_append(ctx, p)).await
        }
        TransferListMissing::NAME => {
            answer::<TransferListMissing, _, _>(params, |p| {
                filesystem.transfer_list_missing(ctx, p)
            })
            .await
        }
        TransferRead::NAME => {
            answer::<TransferRead, _, _>(params, |p| filesystem.transfer_read(ctx, p)).await
        }
        TransferWrite::NAME => {
            answer::<TransferWrite, _, _>(params, |p| filesystem.transfer_write(ctx, p)).await
        }
        TransferCommit::NAME => {
            answer::<TransferCommit, _, _>(params, |p| filesystem.transfer_commit(ctx, p)).await
        }
        TransferAbort::NAME => {
            answer::<TransferAbort, _, _>(params, |p| filesystem.transfer_abort(ctx, p)).await
        }
        ProcessStart::NAME => {
            answer::<ProcessStart, _, _>(params, |p| processes.start(ctx, p)).await
        }
        ProcessRead::NAME => reply::<ProcessRead, _, _>(params, |p| processes.read(ctx, p)).await,
        ProcessWrite::NAME => {
            answer::<ProcessWrite, _, _>(params, |p| processes.write(ctx, p)).await
        }
        ProcessTerminate::NAME => {
            answer::<ProcessTerminate, _, _>(params, |p| processes.terminate(ctx, p)).await
        }
        JobStart::NAME => answer::<JobStart, _, _>(params, |p| jobs.start(ctx, p)).await,
        JobRead::NAME => answer::<JobRead, _, _>(params, |p| jobs.read(ctx, p)).await,
        JobCancel::NAME => answer::<JobCancel, _, _>(params, |p| jobs.cancel(ctx, p)).await,
        _ => Err(ProtocolError::method_not_found(method)),
    }
}

/// Decode, call a handler that answers a plain response, encode.
async fn answer<M, F, Fut>(params: &Value, handler: F) -> Result<Reply<Value>, ProtocolError>
where
    M: RequestMethod,
    F: FnOnce(M::Params) -> Fut,
    Fut: Future<Output = Result<M::Response, ProtocolError>>,
{
    let params = jsonrpc::decode_params::<M::Params>(params)?;
    let response = handler(params).await?;
    Ok(Reply::new(encode(&response)?))
}

/// Decode, call a handler that answers a [`Reply`], encode.
async fn reply<M, F, Fut>(params: &Value, handler: F) -> Result<Reply<Value>, ProtocolError>
where
    M: RequestMethod,
    F: FnOnce(M::Params) -> Fut,
    Fut: Future<Output = Result<Reply<M::Response>, ProtocolError>>,
{
    let params = jsonrpc::decode_params::<M::Params>(params)?;
    let reply = handler(params).await?;
    let value = encode(&reply.value)?;
    Ok(Reply {
        value,
        on_written: reply.on_written,
    })
}

fn encode<T: Serialize>(value: &T) -> Result<Value, ProtocolError> {
    serde_json::to_value(value)
        .map_err(|error| ProtocolError::internal(format!("encode result: {error}")))
}

/// Whether the daemon's capabilities include `capability`.
pub(crate) fn has_capability(
    capabilities: &environment_protocol::data::handshake::DataCapabilities,
    capability: Capability,
) -> bool {
    match capability {
        Capability::Filesystem => capabilities.filesystem.is_some(),
        Capability::FilesystemWrite => capabilities
            .filesystem
            .as_ref()
            .is_some_and(|filesystem| filesystem.write),
        Capability::Process => capabilities.process.is_some(),
        Capability::ProcessPty => capabilities
            .process
            .as_ref()
            .is_some_and(|process| process.pty),
        Capability::Job => capabilities.job.is_some(),
        Capability::Power | Capability::Ingress => false,
    }
}
