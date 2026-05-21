//! Typed data-plane client methods.

use host_protocol::data::{
    fs::{
        CopyParams, CopyResponse, CreateDirectoryParams, CreateDirectoryResponse,
        GetMetadataParams, GetMetadataResponse, ReadDirectoryParams, ReadDirectoryResponse,
        ReadFileParams, ReadFileResponse, RemoveParams, RemoveResponse, WriteFileParams,
        WriteFileResponse,
    },
    handshake::{InitializeParams, InitializeResponse, InitializedParams},
    methods::{
        FS_COPY_METHOD, FS_CREATE_DIRECTORY_METHOD, FS_GET_METADATA_METHOD,
        FS_READ_DIRECTORY_METHOD, FS_READ_FILE_METHOD, FS_REMOVE_METHOD, FS_WRITE_FILE_METHOD,
        INITIALIZE_METHOD, INITIALIZED_METHOD, PROCESS_READ_METHOD, PROCESS_RESIZE_METHOD,
        PROCESS_START_METHOD, PROCESS_TERMINATE_METHOD, PROCESS_WRITE_METHOD,
    },
    process::{
        ReadProcessParams, ReadProcessResponse, ResizeProcessParams, ResizeProcessResponse,
        StartProcessParams, StartProcessResponse, TerminateProcessParams, TerminateProcessResponse,
        WriteProcessParams, WriteProcessResponse,
    },
};

use crate::{
    error::HostClientResult,
    rpc::{JsonRpcClient, JsonRpcNotification, JsonRpcTransport},
    transport::{WebSocketConnectOptions, WebSocketTransport},
};

pub struct HostDataClient<T> {
    rpc: JsonRpcClient<T>,
}

impl<T> HostDataClient<T>
where
    T: JsonRpcTransport,
{
    pub fn new(transport: T) -> Self {
        Self {
            rpc: JsonRpcClient::new(transport),
        }
    }

    pub fn from_rpc(rpc: JsonRpcClient<T>) -> Self {
        Self { rpc }
    }

    pub fn into_rpc(self) -> JsonRpcClient<T> {
        self.rpc
    }

    pub async fn initialize(
        &mut self,
        params: &InitializeParams,
    ) -> HostClientResult<InitializeResponse> {
        self.rpc.request(INITIALIZE_METHOD, params).await
    }

    pub async fn initialized(&mut self, params: &InitializedParams) -> HostClientResult<()> {
        self.rpc.notify(INITIALIZED_METHOD, params).await
    }

    pub async fn read_file(
        &mut self,
        params: &ReadFileParams,
    ) -> HostClientResult<ReadFileResponse> {
        self.rpc.request(FS_READ_FILE_METHOD, params).await
    }

    pub async fn write_file(
        &mut self,
        params: &WriteFileParams,
    ) -> HostClientResult<WriteFileResponse> {
        self.rpc.request(FS_WRITE_FILE_METHOD, params).await
    }

    pub async fn create_directory(
        &mut self,
        params: &CreateDirectoryParams,
    ) -> HostClientResult<CreateDirectoryResponse> {
        self.rpc.request(FS_CREATE_DIRECTORY_METHOD, params).await
    }

    pub async fn get_metadata(
        &mut self,
        params: &GetMetadataParams,
    ) -> HostClientResult<GetMetadataResponse> {
        self.rpc.request(FS_GET_METADATA_METHOD, params).await
    }

    pub async fn read_directory(
        &mut self,
        params: &ReadDirectoryParams,
    ) -> HostClientResult<ReadDirectoryResponse> {
        self.rpc.request(FS_READ_DIRECTORY_METHOD, params).await
    }

    pub async fn remove(&mut self, params: &RemoveParams) -> HostClientResult<RemoveResponse> {
        self.rpc.request(FS_REMOVE_METHOD, params).await
    }

    pub async fn copy(&mut self, params: &CopyParams) -> HostClientResult<CopyResponse> {
        self.rpc.request(FS_COPY_METHOD, params).await
    }

    pub async fn start_process(
        &mut self,
        params: &StartProcessParams,
    ) -> HostClientResult<StartProcessResponse> {
        self.rpc.request(PROCESS_START_METHOD, params).await
    }

    pub async fn read_process(
        &mut self,
        params: &ReadProcessParams,
    ) -> HostClientResult<ReadProcessResponse> {
        self.rpc.request(PROCESS_READ_METHOD, params).await
    }

    pub async fn write_process(
        &mut self,
        params: &WriteProcessParams,
    ) -> HostClientResult<WriteProcessResponse> {
        self.rpc.request(PROCESS_WRITE_METHOD, params).await
    }

    pub async fn terminate_process(
        &mut self,
        params: &TerminateProcessParams,
    ) -> HostClientResult<TerminateProcessResponse> {
        self.rpc.request(PROCESS_TERMINATE_METHOD, params).await
    }

    pub async fn resize_process(
        &mut self,
        params: &ResizeProcessParams,
    ) -> HostClientResult<ResizeProcessResponse> {
        self.rpc.request(PROCESS_RESIZE_METHOD, params).await
    }

    pub async fn next_notification(&mut self) -> HostClientResult<Option<JsonRpcNotification>> {
        self.rpc.next_notification().await
    }
}

impl HostDataClient<WebSocketTransport> {
    pub async fn connect(
        endpoint: &str,
        options: WebSocketConnectOptions,
    ) -> HostClientResult<Self> {
        Ok(Self::new(
            WebSocketTransport::connect(endpoint, options).await?,
        ))
    }
}
