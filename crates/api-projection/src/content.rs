//! Read-only views of durable content. Provider payloads remain authoritative.

use api::{AgentApiError, ContentRefView};
use engine::{ContentRef, storage::BlobStore};

pub fn content_ref_to_api(content: &ContentRef) -> ContentRefView {
    ContentRefView {
        content_ref: content.content_ref.as_str().to_owned(),
        media_type: content.media_type.clone(),
        provider_kind: content.provider_kind.clone(),
        media_handle: engine::media::is_media_content(content)
            .then(|| engine::media::media_handle(&content.content_ref)),
    }
}

/// Returns no text for binary media. Native assistant JSON is decoded, never
/// exposed as display text; malformed recognized payloads fail explicitly.
pub async fn project_content_text(
    blobs: &dyn BlobStore,
    content: &ContentRef,
) -> Result<Option<String>, AgentApiError> {
    if !super::is_text_message_media_type(content.media_type.as_deref()) {
        return Ok(None);
    }
    let text = blobs
        .read_text(&content.content_ref)
        .await
        .map_err(super::map_blob_store_error)?;
    if content.media_type.as_deref() != Some("application/json") {
        return Ok(Some(text));
    }
    let project = match content.provider_kind.as_deref() {
        Some(engine::ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND) => {
            llm_clients::content::anthropic_text_blocks
        }
        Some(engine::OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND) => {
            llm_clients::content::openai_response_message
        }
        Some(llm_clients::content::OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND) => {
            llm_clients::content::openai_completion_message
        }
        Some(llm_clients::content::OPENAI_COMPLETIONS_REASONING_PROVIDER_KIND) => {
            llm_clients::content::openai_completion_reasoning
        }
        Some(llm_clients::content::OPENAI_RESPONSES_REASONING_PROVIDER_KIND) => {
            llm_clients::content::openai_response_reasoning
        }
        Some(llm_clients::content::ANTHROPIC_THINKING_PROVIDER_KIND) => {
            llm_clients::content::anthropic_thinking
        }
        Some(llm_clients::content::AUDIO_TRANSCRIPT_PROVIDER_KIND) => {
            llm_clients::content::audio_transcript
        }
        Some(_) => {
            return Err(AgentApiError::invalid_request(
                "unsupported native content text projection",
            ));
        }
        None => return Ok(Some(text)),
    };
    let raw = serde_json::from_str(&text).map_err(|error| {
        AgentApiError::internal(format!("invalid native content JSON: {error}"))
    })?;
    project(&raw)
        .map(Some)
        .ok_or_else(|| AgentApiError::internal("invalid native content"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{
        ContextEntry, ContextEntryId, ContextEntryKind, ContextEntrySource,
        storage::InMemoryBlobStore,
    };
    use llm_clients::content::*;
    use serde_json::json;

    fn context_entry(input: engine::ContextEntryInput) -> ContextEntry {
        ContextEntry {
            entry_id: ContextEntryId::new(1),
            key: None,
            kind: input.kind,
            source: ContextEntrySource::ContextEdit,
            content: input.content,
            preview: input.preview,
            origin: input.origin,
            provenance_ref: input.provenance_ref,
            token_estimate: input.token_estimate,
            supersedes: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn audio_run_summary_previews_project_text_before_applying_the_limit() {
        let blobs = InMemoryBlobStore::new();
        let projector = crate::CoreAgentProjector::new(&blobs);
        let boundary = format!("{}é", "🦀".repeat(127));
        for (text, expected_preview, truncated) in [
            (
                "please summarize this".to_owned(),
                "please summarize this".to_owned(),
                false,
            ),
            (
                format!("{boundary}🦀 more text"),
                format!("{boundary}…"),
                true,
            ),
        ] {
            let bytes = serde_json::to_vec(&AudioTranscript {
                filename: "voice.ogg".to_owned(),
                text,
            })
            .unwrap();
            let reference = blobs.put_bytes(bytes.clone()).await.unwrap();
            let source = engine::RunSource::Input {
                input: vec![engine::ContextEntryInput {
                    kind: ContextEntryKind::Message {
                        role: engine::ContextMessageRole::User,
                    },
                    content: ContentRef {
                        content_ref: reference.clone(),
                        media_type: Some("application/json".into()),
                        provider_kind: Some(AUDIO_TRANSCRIPT_PROVIDER_KIND.into()),
                    },
                    preview: Some("short preprocessing preview".into()),
                    origin: None,
                    provenance_ref: None,
                    token_estimate: None,
                }],
            };
            let summary = projector
                .project_run_summary_source(Some(&source))
                .await
                .unwrap();
            assert_eq!(
                summary,
                api::RunSummarySourceView::Input {
                    content_ref: Some(reference.as_str().to_owned()),
                    preview: Some(expected_preview),
                    preview_truncated: truncated,
                }
            );
            assert_eq!(blobs.read_bytes(&reference).await.unwrap(), bytes);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_and_run_views_return_full_text_without_context_or_payload_rewriting() {
        let blobs = InMemoryBlobStore::new();
        let full = "héllo 🦀\n".repeat(2000);
        for (kind, bytes) in [
            (None, full.as_bytes().to_vec()),
            (Some(engine::ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND),
                serde_json::to_vec(&json!([{"type":"text","text":full,"citations":[]}])).unwrap()),
            (Some(engine::OPENAI_RESPONSES_MESSAGE_PROVIDER_KIND),
                serde_json::to_vec(&json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":full,"annotations":[]}]})).unwrap()),
            (Some(OPENAI_COMPLETIONS_MESSAGE_PROVIDER_KIND),
                serde_json::to_vec(&json!({"role":"assistant","content":full,"annotations":[]})).unwrap()),
            (Some(AUDIO_TRANSCRIPT_PROVIDER_KIND),
                serde_json::to_vec(&json!({"filename":"note.ogg","text":full})).unwrap()),
        ] {
            let content = ContentRef {
                content_ref: blobs.put_bytes(bytes.clone()).await.unwrap(),
                media_type: Some(if kind.is_some() { "application/json" } else { "text/plain" }.into()),
                provider_kind: kind.map(str::to_owned),
            };
            let input = engine::ContextEntryInput {
                kind: ContextEntryKind::Message { role: engine::ContextMessageRole::User },
                content: content.clone(),
                preview: Some("short preview".into()),
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            };
            let entry = context_entry(input.clone());
            let projector = crate::CoreAgentProjector::new(&blobs);
            let view = projector.project_context_entry(&entry, None).await.unwrap();
            assert_eq!(view.text.as_deref(), Some(full.as_str()));
            assert!(!view.text_truncated);
            assert_eq!(projector.project_input_entries(&[input]).await.unwrap(),
                vec![api::InputItem::Text { origin: None,  text: full.clone() }]);

            // The retained terminal output still resolves after its context is gone.
            let source = engine::RunSource::Input { input: Vec::new() };
            let run = projector.project_run_with_metadata(crate::ProjectRun {
                entries: &[], run_id: engine::RunId::new(1), status: api::RunStatus::Completed,
                output: Some(&content), source: &source, started_at_ms: None,
                completed_at_ms: Some(1), usage: None,
            }).await.unwrap();
            assert!(run.entries.is_empty());
            assert_eq!(run.output, Some(content_ref_to_api(&content)));
            assert_eq!(run.output_text.as_deref(), Some(full.as_str()));
            assert_eq!(blobs.read_bytes(&content.content_ref).await.unwrap(), bytes);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn binary_run_outputs_keep_their_reference_without_text() {
        let blobs = InMemoryBlobStore::new();
        let content = ContentRef {
            content_ref: blobs.put_bytes(vec![0xff, 0x00]).await.unwrap(),
            media_type: Some("image/png".into()),
            provider_kind: None,
        };
        let source = engine::RunSource::Input { input: Vec::new() };
        let projector = crate::CoreAgentProjector::new(&blobs);
        for output in [None, Some(&content)] {
            let run = projector
                .project_run_with_metadata(crate::ProjectRun {
                    entries: &[],
                    run_id: engine::RunId::new(1),
                    status: api::RunStatus::Completed,
                    output,
                    source: &source,
                    started_at_ms: None,
                    completed_at_ms: Some(1),
                    usage: None,
                })
                .await
                .unwrap();
            assert_eq!(run.output, output.map(content_ref_to_api));
            assert!(run.output_text.is_none());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_payloads_keep_bounded_previews_and_readable_original_blobs() {
        let blobs = InMemoryBlobStore::new();
        let full = "tool output 🦀\n".repeat(2000);
        let input = engine::ContextEntryInput {
            kind: ContextEntryKind::ToolResult {
                call_id: engine::ToolCallId::new("call"),
                is_error: false,
            },
            content: ContentRef::text(blobs.insert_text(&full).await),
            preview: None,
            origin: None,
            provenance_ref: None,
            token_estimate: None,
        };
        let entry = context_entry(input);
        let view = crate::CoreAgentProjector::new(&blobs)
            .project_context_entry(&entry, None)
            .await
            .unwrap();
        assert!(view.text_truncated);
        assert!(view.text.as_ref().unwrap().len() <= 4099);
        assert!(full.starts_with(view.text.as_ref().unwrap().trim_end_matches('…')));
        assert_eq!(
            blobs.read_text(&entry.content.content_ref).await.unwrap(),
            full
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reasoning_views_include_full_exposed_text_from_unchanged_native_content() {
        let blobs = InMemoryBlobStore::new();
        let text = "Inspecting 🦀 the source.\n".repeat(500);
        for (kind, raw) in [
            (
                ANTHROPIC_THINKING_PROVIDER_KIND,
                json!({"type":"thinking","thinking":text,"signature":"do not display"}),
            ),
            (
                OPENAI_RESPONSES_REASONING_PROVIDER_KIND,
                json!({"id":"rs_1","type":"reasoning","summary":[{"type":"summary_text","text":text}],"encrypted_content":"do not display"}),
            ),
            (
                OPENAI_COMPLETIONS_REASONING_PROVIDER_KIND,
                json!({"reasoning_content":text,"reasoning_details":[{"type":"reasoning.encrypted","data":"do not display"}]}),
            ),
        ] {
            let bytes = serde_json::to_vec(&raw).unwrap();
            let entry = ContextEntry {
                entry_id: ContextEntryId::new(1),
                key: None,
                kind: ContextEntryKind::ReasoningState,
                source: ContextEntrySource::ContextEdit,
                content: ContentRef {
                    content_ref: blobs.put_bytes(bytes.clone()).await.unwrap(),
                    media_type: Some("application/json".into()),
                    provider_kind: Some(kind.into()),
                },
                preview: Some(text.chars().take(256).collect()),
                origin: None,
                provenance_ref: None,
                token_estimate: None,
                supersedes: None,
            };
            let view = crate::CoreAgentProjector::new(&blobs)
                .project_context_entry(&entry, None)
                .await
                .unwrap();
            assert!(!view.text_truncated);
            assert_eq!(view.text.as_deref(), Some(text.as_str()));
            assert!(!view.text.as_ref().unwrap().contains("do not display"));
            assert_eq!(
                project_content_text(&blobs, &entry.content)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(text.as_str())
            );
            assert_eq!(
                blobs.read_bytes(&entry.content.content_ref).await.unwrap(),
                bytes
            );
        }
    }
}
