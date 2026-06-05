use super::*;

pub(super) async fn run_input_from_api(
    store: &dyn BlobStore,
    input: &[InputItem],
) -> Result<Vec<ContextEntryInput>, AgentApiError> {
    let mut entries = Vec::new();
    for item in input {
        match item {
            InputItem::Text { text } => {
                let text = text.trim();
                if !text.is_empty() {
                    let content_ref = store
                        .put_bytes(text.as_bytes().to_vec())
                        .await
                        .map_err(map_blob_store_error)?;
                    entries.push(user_message_input(content_ref));
                }
            }
            InputItem::TextRef { blob_ref } => {
                let blob_ref = parse_blob_ref(blob_ref)?;
                let text = store
                    .read_text(&blob_ref)
                    .await
                    .map_err(map_input_blob_store_error)?;
                let text = text.trim();
                if !text.is_empty() {
                    entries.push(user_message_input(blob_ref));
                }
            }
        }
    }

    if entries.is_empty() {
        return Err(empty_run_input_error());
    }
    Ok(entries)
}

pub(super) fn user_message_input(content_ref: BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref,
        media_type: Some("text/plain".to_owned()),
        preview: None,
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    }
}
pub(super) fn empty_run_input_error() -> AgentApiError {
    AgentApiError::invalid_request("run/start input must contain at least one non-empty text item")
}
