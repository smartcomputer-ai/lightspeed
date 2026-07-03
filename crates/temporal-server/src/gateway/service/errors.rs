use super::*;

pub(super) fn is_not_found(error: &AgentApiError) -> bool {
    matches!(error.kind, AgentApiErrorKind::NotFound)
}

pub(super) fn map_admission_failure_to_api_error(failure: &AgentAdmissionFailure) -> AgentApiError {
    match failure.kind {
        AgentAdmissionFailureKind::RejectedCommand => {
            AgentApiError::rejected(failure.message.clone())
        }
        AgentAdmissionFailureKind::UnsupportedAudioMime => {
            AgentApiError::unsupported_audio_mime(failure.message.clone())
        }
        AgentAdmissionFailureKind::AudioBlobMissing => {
            AgentApiError::invalid_request(failure.message.clone())
        }
        AgentAdmissionFailureKind::AudioBlobTooLarge => {
            AgentApiError::audio_blob_too_large(failure.message.clone())
        }
        AgentAdmissionFailureKind::AudioDurationTooLong => {
            AgentApiError::audio_duration_too_long(failure.message.clone())
        }
        AgentAdmissionFailureKind::TranscoderUnavailable => {
            AgentApiError::transcoder_unavailable(failure.message.clone())
        }
        AgentAdmissionFailureKind::TranscodeFailure => {
            AgentApiError::transcode_failure(failure.message.clone())
        }
        AgentAdmissionFailureKind::TranscriptionFailure => {
            AgentApiError::transcription_failure(failure.message.clone())
        }
    }
}

pub(super) fn map_blob_store_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => {
            AgentApiError::internal(format!("stored run input blob disappeared: {blob_ref}"))
        }
        BlobStoreError::Store { message } => AgentApiError::internal(message),
    }
}

pub(super) fn map_blob_read_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => {
            AgentApiError::not_found(format!("blob not found: {blob_ref}"))
        }
        BlobStoreError::Store { message } => AgentApiError::internal(message),
    }
}

pub(super) fn map_vfs_manifest_blob_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => AgentApiError::invalid_request(format!(
            "vfs manifest references missing blob: {blob_ref}"
        )),
        BlobStoreError::Store { message } => AgentApiError::internal(message),
    }
}

pub(super) fn map_vfs_commit_error(error: vfs::VfsError) -> AgentApiError {
    match error {
        vfs::VfsError::BlobStore(error) => map_blob_store_error(error),
        error => AgentApiError::invalid_request(error.to_string()),
    }
}

pub(super) fn map_vfs_read_error(error: vfs::VfsError) -> AgentApiError {
    match error {
        vfs::VfsError::BlobStore(error) => map_blob_read_error(error),
        error => AgentApiError::invalid_request(error.to_string()),
    }
}

pub(super) fn map_vfs_catalog_error(error: VfsCatalogError) -> AgentApiError {
    match error {
        VfsCatalogError::AlreadyExists { kind, id } => {
            AgentApiError::conflict(format!("vfs catalog {kind} already exists: {id}"))
        }
        VfsCatalogError::NotFound { kind, id } => {
            AgentApiError::not_found(format!("vfs catalog {kind} not found: {id}"))
        }
        VfsCatalogError::RevisionConflict { .. } => AgentApiError::conflict(error.to_string()),
        VfsCatalogError::InvalidInput { message } => AgentApiError::invalid_request(message),
        VfsCatalogError::Store { message } => AgentApiError::internal(message),
    }
}

pub(super) fn map_fs_error(error: tools::fs::FsError) -> AgentApiError {
    match error {
        tools::fs::FsError::InvalidPath(error) => AgentApiError::invalid_request(error.to_string()),
        tools::fs::FsError::InvalidInput { message } => AgentApiError::invalid_request(message),
        tools::fs::FsError::NotFound { path } => {
            AgentApiError::not_found(format!("vfs path not found: {path}"))
        }
        tools::fs::FsError::AlreadyExists { path } => {
            AgentApiError::conflict(format!("vfs path already exists: {path}"))
        }
        tools::fs::FsError::PermissionDenied { path } => {
            AgentApiError::rejected(format!("vfs permission denied: {path}"))
        }
        tools::fs::FsError::Unsupported { message }
        | tools::fs::FsError::InvalidData { message }
        | tools::fs::FsError::Failed { message } => AgentApiError::internal(message),
    }
}

pub(super) fn map_input_blob_store_error(error: BlobStoreError) -> AgentApiError {
    match error {
        BlobStoreError::NotFound { blob_ref } => {
            AgentApiError::invalid_request(format!("run/start input blob not found: {blob_ref}"))
        }
        BlobStoreError::Store { message } => AgentApiError::invalid_request(message),
    }
}

pub(super) fn map_workflow_start_error(error: WorkflowStartError) -> AgentApiError {
    match error {
        WorkflowStartError::AlreadyStarted { .. } => {
            AgentApiError::conflict("agent session workflow already exists")
        }
        WorkflowStartError::PayloadConversion(error) => AgentApiError::internal(error.to_string()),
        WorkflowStartError::Rpc(status) => AgentApiError::internal(status.to_string()),
        _ => AgentApiError::internal(error.to_string()),
    }
}

pub(super) fn map_workflow_query_error(error: WorkflowQueryError) -> AgentApiError {
    match error {
        WorkflowQueryError::NotFound(_) => AgentApiError::not_found("agent workflow not found"),
        WorkflowQueryError::Rejected(rejection) => {
            AgentApiError::internal(format!("{rejection:?}"))
        }
        WorkflowQueryError::PayloadConversion(error) => AgentApiError::internal(error.to_string()),
        WorkflowQueryError::Rpc(status) => AgentApiError::internal(status.to_string()),
        WorkflowQueryError::Other(error) => AgentApiError::internal(error.to_string()),
        _ => AgentApiError::internal(error.to_string()),
    }
}

pub(super) fn map_workflow_interaction_error(error: WorkflowInteractionError) -> AgentApiError {
    match error {
        WorkflowInteractionError::NotFound(_) => {
            AgentApiError::not_found("agent workflow not found")
        }
        WorkflowInteractionError::PayloadConversion(error) => {
            AgentApiError::internal(error.to_string())
        }
        WorkflowInteractionError::Rpc(status) => AgentApiError::internal(status.to_string()),
        WorkflowInteractionError::Other(error) => AgentApiError::internal(error.to_string()),
        _ => AgentApiError::internal(error.to_string()),
    }
}
