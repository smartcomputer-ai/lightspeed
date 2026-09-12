use super::*;

/// `blobs/put` is batch-native: pass one item to store a single blob. Results
/// come back in request order. Uploads are eligible for collection after the
/// deployment's grace period unless retained by a durable resource. Reads do
/// not extend retention; admitting an existing ref refreshes its grace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutParams {
    #[serde(default)]
    pub blobs: Vec<BlobPutItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutItem {
    pub bytes_base64: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutResult {
    pub blob_ref: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobPutResponse {
    #[serde(default)]
    pub blobs: Vec<BlobPutResult>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobReadParams {
    pub blob_ref: String,
}

/// Immutable content and its encoding. A reference can name plain text,
/// provider-native JSON, or media; it does not imply a UTF-8 text payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentRefView {
    pub content_ref: String,
    pub media_type: Option<String>,
    pub provider_kind: Option<String>,
    /// The model-facing name of provider-native media (an admitted image or
    /// PDF): `media:` plus the first twelve hex characters of `content_ref`.
    /// The model writes it as a URL (`![…](media:3f9a2c1d4e7b)`) to refer to
    /// media it was shown; resolve it against the media of the same session.
    /// Absent for text and other content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_handle: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobReadResponse {
    pub blob_ref: String,
    pub bytes_base64: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobHasParams {
    #[serde(default)]
    pub blob_refs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobHasItem {
    pub blob_ref: String,
    pub exists: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlobHasResponse {
    #[serde(default)]
    pub blobs: Vec<BlobHasItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsSnapshotCommitParams {
    pub manifest: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsSnapshotCommitResponse {
    pub snapshot_ref: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsSnapshotReadParams {
    pub snapshot_ref: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsSnapshotReadResponse {
    pub snapshot_ref: String,
    pub manifest: Value,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Snapshot to seed the workspace from. Absent starts the workspace from
    /// the empty snapshot, committed server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceCreateResponse {
    pub workspace: VfsWorkspaceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceReadParams {
    pub workspace_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceReadResponse {
    pub workspace: VfsWorkspaceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceUpdateParams {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    pub snapshot_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceUpdateResponse {
    pub workspace: VfsWorkspaceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceDeleteParams {
    pub workspace_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceDeleteResponse {
    pub workspace: VfsWorkspaceView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceView {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_snapshot_ref: Option<String>,
    pub head_snapshot_ref: String,
    /// File count of the head snapshot.
    pub files: u64,
    /// Total byte size of the head snapshot.
    pub bytes: u64,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VfsWorkspaceListResponse {
    #[serde(default)]
    pub workspaces: Vec<VfsWorkspaceView>,
}
