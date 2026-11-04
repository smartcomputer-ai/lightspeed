use harness::{BlobRef, ContentRef, ContextEntryInput, ContextEntryKind};

/// A decision on a provider-hosted MCP call, in the provider's own shape: the bytes
/// the caller stores, and the context entry that carries them into the next request.
/// The entry's ref is the digest of those bytes, so storing them is all that is left.
pub fn provider_mcp_approval_response(
    provider_request_id: &str,
    approve: bool,
    preview: &str,
) -> (Vec<u8>, ContextEntryInput) {
    let bytes = serde_json::json!({
        "type": "mcp_approval_response",
        "approval_request_id": provider_request_id,
        "approve": approve,
    })
    .to_string()
    .into_bytes();
    let entry = ContextEntryInput {
        kind: ContextEntryKind::McpApprovalResponse {
            approval_request_id: provider_request_id.to_owned(),
            approve,
        },
        content: ContentRef {
            content_ref: BlobRef::from_bytes(&bytes),
            media_type: Some("application/json".to_owned()),
            provider_kind: Some("openai.responses.mcp_approval_response".to_owned()),
        },
        preview: Some(preview.to_owned()),
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    };
    (bytes, entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_points_at_the_bytes_and_says_the_same_decision() {
        let (bytes, entry) = provider_mcp_approval_response("mcpr_1", false, "rejected");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({
                "type": "mcp_approval_response",
                "approval_request_id": "mcpr_1",
                "approve": false,
            })
        );
        assert_eq!(entry.content.content_ref, BlobRef::from_bytes(&bytes));
        assert_eq!(
            entry.kind,
            ContextEntryKind::McpApprovalResponse {
                approval_request_id: "mcpr_1".to_owned(),
                approve: false,
            }
        );
        assert_eq!(entry.preview.as_deref(), Some("rejected"));
    }
}
