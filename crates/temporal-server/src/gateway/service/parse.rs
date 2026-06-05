use super::*;

pub(super) fn parse_blob_ref(value: &str) -> Result<BlobRef, AgentApiError> {
    BlobRef::parse(value).map_err(|error| AgentApiError::invalid_request(error.to_string()))
}

pub(super) fn parse_vfs_workspace_id(value: String) -> Result<VfsWorkspaceId, AgentApiError> {
    VfsWorkspaceId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid vfs workspace id: {error}"))
    })
}
