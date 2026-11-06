//! Windows transfers remain unavailable until journal recovery and atomic
//! publication have native implementations. No journal or staging side effect
//! is performed by this boundary.

use super::Confinement;
use environment_protocol::{
    data::{handshake::TransferCeilings, transfer::*},
    error::{Capability, ProtocolError},
};
use std::sync::Arc;

pub(super) struct Transfers;

macro_rules! unavailable {
    ($($name:ident($params:ty) -> $response:ty;)*) => {
        $(pub fn $name(&self, _params: $params) -> Result<$response, ProtocolError> {
            Err(ProtocolError::capability_unavailable(Capability::Filesystem,
                "Windows transfer publication and journal recovery are unavailable"))
        })*
    };
}

impl Transfers {
    pub fn new(_: Arc<Confinement>, _: TransferCeilings, _: bool) -> Result<Self, ProtocolError> {
        Ok(Self)
    }
    unavailable! {
        begin(BeginTransferParams) -> TransferStatus;
        advance(TransferOperationParams) -> TransferStatus;
        get_status(TransferOperationParams) -> TransferStatus;
        read_inventory(ReadTransferInventoryParams) -> ReadTransferInventoryResponse;
        append(AppendTransferInventoryParams) -> TransferStatus;
        list_missing(ListTransferMissingParams) -> ListTransferMissingResponse;
        read(ReadTransferContentParams) -> ReadTransferContentResponse;
        write(WriteTransferContentParams) -> WriteTransferContentResponse;
        commit(TransferOperationParams) -> TransferStatus;
        abort(TransferOperationParams) -> TransferStatus;
    }
}
