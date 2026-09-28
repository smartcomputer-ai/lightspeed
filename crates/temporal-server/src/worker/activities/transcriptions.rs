use super::{audio, state::ActivityState};
use api::{TranscriptionFailure, TranscriptionFailureKind as Kind};
use engine::{BlobRef, storage::BlobStore};
use temporal_workflow::{TranscriptionActivityResult, TranscriptionWorkflowArgs};
use temporalio_sdk::activities::ActivityError;

fn activity_error(error: impl Into<anyhow::Error>) -> ActivityError {
    ActivityError::from(error.into())
}
fn failure(kind: Kind, message: impl Into<String>) -> TranscriptionFailure {
    TranscriptionFailure {
        kind,
        message: message.into(),
    }
}

pub(super) async fn execute(
    state: &ActivityState,
    args: TranscriptionWorkflowArgs,
) -> Result<TranscriptionActivityResult, ActivityError> {
    match transcribe(state, &args).await {
        Ok(transcript) => Ok(TranscriptionActivityResult::Succeeded {
            transcript_ref: transcript.to_string(),
        }),
        Err(ExecutionError::Terminal(failure)) => {
            Ok(TranscriptionActivityResult::Failed { failure })
        }
        Err(ExecutionError::Retry(error)) => Err(activity_error(anyhow::anyhow!(error))),
    }
}

enum ExecutionError {
    Terminal(TranscriptionFailure),
    Retry(String),
}
impl From<engine::storage::BlobStoreError> for ExecutionError {
    fn from(error: engine::storage::BlobStoreError) -> Self {
        if matches!(error, engine::storage::BlobStoreError::NotFound { .. }) {
            Self::Terminal(failure(
                Kind::InvalidAudio,
                "Audio is unavailable; upload it again.",
            ))
        } else {
            Self::Retry("Transcription content storage is unavailable.".into())
        }
    }
}

async fn transcribe(
    state: &ActivityState,
    record: &TranscriptionWorkflowArgs,
) -> Result<BlobRef, ExecutionError> {
    let deps = state.audio();
    let request = &record.request;
    let source = BlobRef::parse(&request.audio.blob_ref).map_err(|_| {
        ExecutionError::Terminal(failure(Kind::InvalidAudio, "Invalid audio reference."))
    })?;
    let mime = audio::normalized_mime(Some(&request.audio.mime));
    if !audio::PROVIDER_ACCEPTED_AUDIO_MIMES.contains(&mime.as_str())
        && !audio::TRANSCODABLE_AUDIO_MIMES.contains(&mime.as_str())
    {
        return Err(ExecutionError::Terminal(failure(
            Kind::InvalidAudio,
            "Unsupported audio MIME type.",
        )));
    }
    if deps.blobs.stat_blob(&source).await?.byte_len > audio::MAX_AUDIO_BYTES {
        return Err(ExecutionError::Terminal(failure(
            Kind::InvalidAudio,
            "Audio exceeds the 25 MiB limit.",
        )));
    }
    let bytes = deps.blobs.read_bytes(&source).await?;
    check_duration(&mime, &bytes)?;
    let prepared = audio::prepare_audio_for_transcription(
        bytes,
        &mime,
        &request.audio.name,
        deps.transcoder.as_deref(),
    )
    .await
    .map_err(|e| ExecutionError::Terminal(failure(Kind::InvalidAudio, e.to_string())))?;
    check_duration(&prepared.mime, &prepared.bytes)?;
    let result = deps
        .transcriber
        .transcribe(audio::AudioTranscriptionRequest {
            bytes: prepared.bytes,
            mime: prepared.mime,
            name: prepared.name,
            model: record.model.clone(),
            language: request.language.clone(),
            prompt: request.prompt.clone(),
        })
        .await
        .map_err(|e| {
            if e.retryable {
                ExecutionError::Retry(e.message)
            } else {
                ExecutionError::Terminal(failure(
                    if e.configuration {
                        Kind::Configuration
                    } else {
                        Kind::Provider
                    },
                    e.message,
                ))
            }
        })?;
    if result.text.len() > 1024 * 1024 {
        return Err(ExecutionError::Terminal(failure(
            Kind::Provider,
            "Transcript exceeds the 1 MiB limit.",
        )));
    }
    let result_ref = deps
        .blobs
        .put_bytes(result.text.trim().as_bytes().to_vec())
        .await?;
    Ok(result_ref)
}

fn check_duration(mime: &str, bytes: &[u8]) -> Result<(), ExecutionError> {
    if audio::audio_duration_ms(mime, bytes)
        .is_some_and(|duration| duration > audio::MAX_AUDIO_DURATION_MS)
    {
        return Err(ExecutionError::Terminal(failure(
            Kind::InvalidAudio,
            "Audio exceeds the ten-minute duration limit.",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_limit_is_enforced_before_provider_call() {
        // One complete Ogg page with a one-byte body and a 48 kHz granule clock.
        let mut page = b"OggS".to_vec();
        page.resize(29, 0);
        page[26] = 1;
        page[27] = 1;
        page[6..14].copy_from_slice(&(600_u64 * 48_000).to_le_bytes());
        assert!(check_duration("audio/ogg", &page).is_ok());
        page[6..14].copy_from_slice(&(601_u64 * 48_000).to_le_bytes());
        assert!(matches!(
            check_duration("audio/ogg", &page),
            Err(ExecutionError::Terminal(TranscriptionFailure {
                kind: Kind::InvalidAudio,
                ..
            }))
        ));
    }
}
