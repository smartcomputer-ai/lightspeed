use std::sync::Arc;

use async_trait::async_trait;
use engine::{
    ContextEntryInput, ContextEntryKind, ContextMessageRole,
    storage::{BlobStore, BlobStoreError},
};
use llm_clients::{LlmApiError, ProviderFailureKind, openai::audio as oai};
use llm_runtime::ProviderKeyResolver;
use temporalio_sdk::activities::ActivityError;

use crate::worker::{PreprocessRunInputActivityRequest, PreprocessRunInputActivityResult};
use temporal_workflow::{
    PreprocessRunInputFailure, PreprocessRunInputFailureKind, PreprocessRunInputOutcome,
};

use super::state::PreprocessActivityDeps;

const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
const MAX_AUDIO_DURATION_MS: u64 = 10 * 60 * 1000;
const OPENAI_PROVIDER_ID: &str = "openai";
const PROVIDER_ACCEPTED_AUDIO_MIMES: &[&str] = &[
    "audio/mpeg",
    "audio/mp4",
    "audio/wav",
    "audio/webm",
    "audio/ogg",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTranscriptionRequest {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTranscription {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AudioTranscriptionError {
    #[error("audio transcription provider authentication failed: {message}")]
    ProviderAuthentication { message: String },
    #[error("audio transcription provider is not configured: {message}")]
    ProviderConfiguration { message: String },
    #[error("audio transcription provider failed: {message}")]
    ProviderTranscriptionFailure { message: String },
}

#[async_trait]
pub trait AudioTranscriber: Send + Sync {
    async fn transcribe(
        &self,
        request: AudioTranscriptionRequest,
    ) -> Result<AudioTranscription, AudioTranscriptionError>;
}

pub struct UnavailableAudioTranscriber;

#[async_trait]
impl AudioTranscriber for UnavailableAudioTranscriber {
    async fn transcribe(
        &self,
        _request: AudioTranscriptionRequest,
    ) -> Result<AudioTranscription, AudioTranscriptionError> {
        Err(AudioTranscriptionError::ProviderConfiguration {
            message: "audio transcriber is not configured".to_owned(),
        })
    }
}

pub struct OpenAiAudioTranscriber {
    client: Arc<oai::Client>,
    provider_keys: Arc<dyn ProviderKeyResolver>,
}

impl OpenAiAudioTranscriber {
    pub fn new(client: Arc<oai::Client>, provider_keys: Arc<dyn ProviderKeyResolver>) -> Self {
        Self {
            client,
            provider_keys,
        }
    }
}

#[async_trait]
impl AudioTranscriber for OpenAiAudioTranscriber {
    async fn transcribe(
        &self,
        request: AudioTranscriptionRequest,
    ) -> Result<AudioTranscription, AudioTranscriptionError> {
        let stored_key = self
            .provider_keys
            .resolve_provider_key(OPENAI_PROVIDER_ID)
            .await
            .map_err(|error| AudioTranscriptionError::ProviderConfiguration {
                message: error.to_string(),
            })?;
        let response = self
            .client
            .create_transcription_with_auth(
                oai::CreateTranscriptionRequest::new(oai::AudioFile {
                    bytes: request.bytes,
                    filename: request.name,
                    mime: request.mime,
                }),
                stored_key.as_ref().map(|auth| auth.as_request_auth()),
            )
            .await
            .map_err(map_openai_error)?;
        Ok(AudioTranscription {
            text: response.parsed.text,
        })
    }
}

pub(super) async fn preprocess_run_input(
    deps: &PreprocessActivityDeps,
    request: PreprocessRunInputActivityRequest,
) -> Result<PreprocessRunInputActivityResult, ActivityError> {
    let outcome = match rewrite_run_input(
        deps.blobs.as_ref(),
        deps.transcriber.as_ref(),
        request.input,
    )
    .await
    {
        Ok(input) => PreprocessRunInputOutcome::Succeeded { input },
        Err(failure) => PreprocessRunInputOutcome::Failed { failure },
    };
    Ok(PreprocessRunInputActivityResult { outcome })
}

async fn rewrite_run_input(
    blobs: &dyn BlobStore,
    transcriber: &dyn AudioTranscriber,
    input: Vec<ContextEntryInput>,
) -> Result<Vec<ContextEntryInput>, PreprocessRunInputFailure> {
    let mut rewritten = Vec::with_capacity(input.len());
    for entry in input {
        if !is_audio_entry(&entry) {
            rewritten.push(entry);
            continue;
        }
        rewritten.push(transcribe_entry(blobs, transcriber, entry).await?);
    }
    Ok(rewritten)
}

async fn transcribe_entry(
    blobs: &dyn BlobStore,
    transcriber: &dyn AudioTranscriber,
    entry: ContextEntryInput,
) -> Result<ContextEntryInput, PreprocessRunInputFailure> {
    let mime = normalized_mime(entry.media_type.as_deref());
    let name = audio_label(&entry);
    if !PROVIDER_ACCEPTED_AUDIO_MIMES.contains(&mime.as_str()) {
        return Err(failure(
            PreprocessRunInputFailureKind::UnsupportedAudioMime,
            format!(
                "unsupported audio mime type {mime} for {name}; accepted by transcription provider: {}",
                PROVIDER_ACCEPTED_AUDIO_MIMES.join(", ")
            ),
        ));
    }

    let info = blobs
        .stat_blob(&entry.content_ref)
        .await
        .map_err(map_audio_blob_error)?;
    if info.byte_len > MAX_AUDIO_BYTES {
        return Err(failure(
            PreprocessRunInputFailureKind::AudioBlobTooLarge,
            format!(
                "audio entry {name} blob {} is {} bytes; the limit is {MAX_AUDIO_BYTES} bytes",
                entry.content_ref, info.byte_len
            ),
        ));
    }

    let bytes = blobs
        .read_bytes(&entry.content_ref)
        .await
        .map_err(map_audio_blob_error)?;
    if let Some(duration_ms) = audio_duration_ms(&mime, &bytes)
        && duration_ms > MAX_AUDIO_DURATION_MS
    {
        return Err(failure(
            PreprocessRunInputFailureKind::AudioDurationTooLong,
            format!(
                "audio entry {name} duration is {}; the limit is {}",
                format_duration_ms(duration_ms),
                format_duration_ms(MAX_AUDIO_DURATION_MS)
            ),
        ));
    }
    let transcript = transcriber
        .transcribe(AudioTranscriptionRequest {
            bytes,
            mime,
            name: audio_filename(&name),
        })
        .await
        .map_err(map_transcription_error)?;
    let transcript_text = format!("[audio transcript: {name}]\n{}", transcript.text.trim());
    let transcript_ref = blobs
        .put_bytes(transcript_text.into_bytes())
        .await
        .map_err(|error| {
            failure(
                PreprocessRunInputFailureKind::ProviderTranscriptionFailure,
                format!("failed to store audio transcript: {error}"),
            )
        })?;

    Ok(ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content_ref: transcript_ref,
        media_type: Some("text/plain".to_owned()),
        preview: Some(format!("[audio transcript: {name}]")),
        provider_kind: None,
        provider_item_id: None,
        token_estimate: None,
    })
}

fn is_audio_entry(entry: &ContextEntryInput) -> bool {
    entry
        .media_type
        .as_deref()
        .map(|mime| mime.trim().to_ascii_lowercase().starts_with("audio/"))
        .unwrap_or(false)
}

fn normalized_mime(mime: Option<&str>) -> String {
    mime.unwrap_or_default().trim().to_ascii_lowercase()
}

fn audio_label(entry: &ContextEntryInput) -> String {
    let Some(preview) = entry.preview.as_deref().map(str::trim) else {
        return "audio".to_owned();
    };
    preview
        .strip_prefix("[audio: ")
        .and_then(|value| value.strip_suffix(']'))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("audio")
        .to_owned()
}

fn audio_filename(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed == "audio" {
        "audio.ogg".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn audio_duration_ms(mime: &str, bytes: &[u8]) -> Option<u64> {
    match mime {
        "audio/ogg" => ogg_opus_duration_ms(bytes),
        "audio/wav" => wav_duration_ms(bytes),
        _ => None,
    }
}

fn ogg_opus_duration_ms(bytes: &[u8]) -> Option<u64> {
    let mut offset = 0usize;
    let mut last_granule = None;
    while offset + 27 <= bytes.len() {
        if &bytes[offset..offset + 4] != b"OggS" {
            return None;
        }
        let granule = u64::from_le_bytes(bytes[offset + 6..offset + 14].try_into().ok()?);
        if granule != u64::MAX {
            last_granule = Some(granule);
        }
        let segments = bytes[offset + 26] as usize;
        let lacing_start = offset + 27;
        let lacing_end = lacing_start.checked_add(segments)?;
        if lacing_end > bytes.len() {
            return None;
        }
        let mut body_len = 0usize;
        for segment in &bytes[lacing_start..lacing_end] {
            body_len = body_len.checked_add(*segment as usize)?;
        }
        offset = lacing_end.checked_add(body_len)?;
    }
    last_granule.map(|samples| samples.saturating_mul(1000) / 48_000)
}

fn wav_duration_ms(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut offset = 12usize;
    let mut byte_rate = None;
    let mut data_len = None;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?) as usize;
        let data_start = offset + 8;
        let data_end = data_start.checked_add(chunk_len)?;
        if data_end > bytes.len() {
            return None;
        }
        if chunk_id == b"fmt " && chunk_len >= 16 {
            byte_rate = Some(u32::from_le_bytes(
                bytes[data_start + 8..data_start + 12].try_into().ok()?,
            ) as u64);
        } else if chunk_id == b"data" {
            data_len = Some(chunk_len as u64);
        }
        if byte_rate.is_some() && data_len.is_some() {
            break;
        }
        offset = data_end.checked_add(chunk_len % 2)?;
    }
    let byte_rate = byte_rate?;
    if byte_rate == 0 {
        return None;
    }
    Some(data_len?.saturating_mul(1000) / byte_rate)
}

fn format_duration_ms(duration_ms: u64) -> String {
    format!("{}s", duration_ms.div_ceil(1000))
}

fn map_audio_blob_error(error: BlobStoreError) -> PreprocessRunInputFailure {
    match error {
        BlobStoreError::NotFound { blob_ref } => failure(
            PreprocessRunInputFailureKind::AudioBlobMissing,
            format!("audio blob not found: {blob_ref}"),
        ),
        BlobStoreError::Store { message } => {
            failure(PreprocessRunInputFailureKind::AudioBlobMissing, message)
        }
    }
}

fn map_transcription_error(error: AudioTranscriptionError) -> PreprocessRunInputFailure {
    match error {
        AudioTranscriptionError::ProviderAuthentication { message } => failure(
            PreprocessRunInputFailureKind::ProviderAuthentication,
            message,
        ),
        AudioTranscriptionError::ProviderConfiguration { message } => failure(
            PreprocessRunInputFailureKind::ProviderConfiguration,
            message,
        ),
        AudioTranscriptionError::ProviderTranscriptionFailure { message } => failure(
            PreprocessRunInputFailureKind::ProviderTranscriptionFailure,
            message,
        ),
    }
}

fn map_openai_error(error: LlmApiError) -> AudioTranscriptionError {
    match error {
        LlmApiError::Configuration(error) => AudioTranscriptionError::ProviderConfiguration {
            message: error.to_string(),
        },
        LlmApiError::HttpStatus(error)
            if matches!(
                error.kind,
                ProviderFailureKind::Authentication | ProviderFailureKind::AccessDenied
            ) =>
        {
            AudioTranscriptionError::ProviderAuthentication {
                message: error.to_string(),
            }
        }
        other => AudioTranscriptionError::ProviderTranscriptionFailure {
            message: other.to_string(),
        },
    }
}

fn failure(
    kind: PreprocessRunInputFailureKind,
    message: impl Into<String>,
) -> PreprocessRunInputFailure {
    PreprocessRunInputFailure {
        kind,
        message: message.into(),
    }
}

pub(super) fn default_openai_audio_transcriber(
    provider_keys: Arc<dyn ProviderKeyResolver>,
) -> Result<Arc<dyn AudioTranscriber>, anyhow::Error> {
    let client = oai::Client::new(oai::Config::from_env_allow_missing_key())
        .map_err(|error| anyhow::anyhow!("construct OpenAI audio client: {error}"))?;
    Ok(Arc::new(OpenAiAudioTranscriber::new(
        Arc::new(client),
        provider_keys,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::storage::{BlobStore, InMemoryBlobStore};

    #[derive(Clone)]
    struct StaticTranscriber {
        text: String,
    }

    #[async_trait]
    impl AudioTranscriber for StaticTranscriber {
        async fn transcribe(
            &self,
            _request: AudioTranscriptionRequest,
        ) -> Result<AudioTranscription, AudioTranscriptionError> {
            Ok(AudioTranscription {
                text: self.text.clone(),
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn audio_entries_are_rewritten_to_transcript_text() {
        let blobs = InMemoryBlobStore::new();
        let audio_ref = blobs
            .put_bytes(b"OggS fake".to_vec())
            .await
            .expect("put audio");
        let input = vec![
            text_entry(blobs.put_bytes(b"before".to_vec()).await.expect("put text")),
            ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                content_ref: audio_ref,
                media_type: Some("audio/ogg".to_owned()),
                preview: Some("[audio: voice.ogg]".to_owned()),
                provider_kind: None,
                provider_item_id: None,
                token_estimate: None,
            },
        ];

        let rewritten = rewrite_run_input(
            &blobs,
            &StaticTranscriber {
                text: "please summarize this".to_owned(),
            },
            input,
        )
        .await
        .expect("rewrite");

        assert_eq!(rewritten.len(), 2);
        assert_eq!(rewritten[1].media_type.as_deref(), Some("text/plain"));
        assert_eq!(
            blobs
                .read_text(&rewritten[1].content_ref)
                .await
                .expect("read transcript"),
            "[audio transcript: voice.ogg]\nplease summarize this"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unsupported_audio_fails_whole_group() {
        let blobs = InMemoryBlobStore::new();
        let audio_ref = blobs
            .put_bytes(b"aac fake".to_vec())
            .await
            .expect("put audio");
        let input = vec![
            text_entry(blobs.put_bytes(b"before".to_vec()).await.expect("put text")),
            ContextEntryInput {
                kind: ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                content_ref: audio_ref,
                media_type: Some("audio/aac".to_owned()),
                preview: Some("[audio: voice.aac]".to_owned()),
                provider_kind: None,
                provider_item_id: None,
                token_estimate: None,
            },
        ];

        let failure = rewrite_run_input(
            &blobs,
            &StaticTranscriber {
                text: "unused".to_owned(),
            },
            input,
        )
        .await
        .expect_err("unsupported audio must fail group");

        assert_eq!(
            failure.kind,
            PreprocessRunInputFailureKind::UnsupportedAudioMime
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn long_ogg_audio_fails_duration_cap() {
        let blobs = InMemoryBlobStore::new();
        let audio_ref = blobs
            .put_bytes(ogg_page(((MAX_AUDIO_DURATION_MS / 1000) + 1) * 48_000))
            .await
            .expect("put audio");
        let input = vec![ContextEntryInput {
            kind: ContextEntryKind::Message {
                role: ContextMessageRole::User,
            },
            content_ref: audio_ref,
            media_type: Some("audio/ogg".to_owned()),
            preview: Some("[audio: long.ogg]".to_owned()),
            provider_kind: None,
            provider_item_id: None,
            token_estimate: None,
        }];

        let failure = rewrite_run_input(
            &blobs,
            &StaticTranscriber {
                text: "unused".to_owned(),
            },
            input,
        )
        .await
        .expect_err("long audio must fail group");

        assert_eq!(
            failure.kind,
            PreprocessRunInputFailureKind::AudioDurationTooLong
        );
        assert!(failure.message.contains("long.ogg"));
    }

    fn text_entry(content_ref: engine::BlobRef) -> ContextEntryInput {
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

    fn ogg_page(granule_position: u64) -> Vec<u8> {
        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(0);
        page.extend_from_slice(&granule_position.to_le_bytes());
        page.extend_from_slice(&1u32.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.push(1);
        page.push(1);
        page.push(0);
        page
    }
}
