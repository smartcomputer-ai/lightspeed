//! Typed controller-plane client methods.

use host_protocol::control::{
    handshake::{ControllerInitializeParams, ControllerInitializeResponse},
    ingress::{EnsureIngressParams, IngressResponse, RemoveIngressParams},
    methods::{
        CLOSE_TARGET_METHOD, CREATE_TARGET_METHOD, ENSURE_INGRESS_METHOD, GET_TARGET_METHOD,
        INITIALIZE_METHOD, LIST_TARGETS_METHOD, LIST_TEMPLATES_METHOD, REMOVE_INGRESS_METHOD,
    },
    targets::{
        CloseTargetParams, CloseTargetResponse, CreateTargetParams, CreateTargetResponse,
        GetTargetParams, GetTargetResponse, ListTargetsParams, ListTargetsResponse,
        ListTemplatesParams, ListTemplatesResponse,
    },
};

use crate::{
    error::HostClientResult,
    rpc::{JsonRpcClient, JsonRpcTransport},
    transport::{WebSocketConnectOptions, WebSocketTransport},
};

pub struct HostControllerClient<T> {
    rpc: JsonRpcClient<T>,
}

impl<T> HostControllerClient<T>
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
        params: &ControllerInitializeParams,
    ) -> HostClientResult<ControllerInitializeResponse> {
        self.rpc.request(INITIALIZE_METHOD, params).await
    }

    pub async fn list_targets(
        &mut self,
        params: &ListTargetsParams,
    ) -> HostClientResult<ListTargetsResponse> {
        self.rpc.request(LIST_TARGETS_METHOD, params).await
    }

    pub async fn list_templates(
        &mut self,
        params: &ListTemplatesParams,
    ) -> HostClientResult<ListTemplatesResponse> {
        self.rpc.request(LIST_TEMPLATES_METHOD, params).await
    }

    pub async fn create_target(
        &mut self,
        params: &CreateTargetParams,
    ) -> HostClientResult<CreateTargetResponse> {
        self.rpc.request(CREATE_TARGET_METHOD, params).await
    }

    pub async fn get_target(
        &mut self,
        params: &GetTargetParams,
    ) -> HostClientResult<GetTargetResponse> {
        self.rpc.request(GET_TARGET_METHOD, params).await
    }

    pub async fn close_target(
        &mut self,
        params: &CloseTargetParams,
    ) -> HostClientResult<CloseTargetResponse> {
        self.rpc.request(CLOSE_TARGET_METHOD, params).await
    }

    pub async fn ensure_ingress(
        &mut self,
        params: &EnsureIngressParams,
    ) -> HostClientResult<IngressResponse> {
        self.rpc.request(ENSURE_INGRESS_METHOD, params).await
    }

    pub async fn remove_ingress(
        &mut self,
        params: &RemoveIngressParams,
    ) -> HostClientResult<IngressResponse> {
        self.rpc.request(REMOVE_INGRESS_METHOD, params).await
    }
}

impl HostControllerClient<WebSocketTransport> {
    pub async fn connect(
        endpoint: &str,
        options: WebSocketConnectOptions,
    ) -> HostClientResult<Self> {
        Ok(Self::new(
            WebSocketTransport::connect(endpoint, options).await?,
        ))
    }
}
