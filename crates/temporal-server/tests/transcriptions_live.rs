mod support;

use std::sync::{Arc, Mutex};

use api::{
    AgentApiService, BlobPutItem, BlobPutParams, ContextEntryKindView, ContextMessageRoleView,
    InputItem, RunStartParams, RunStartSource, RunStatus, SessionConfig, SessionStartParams,
};
use api_projection::model_to_api;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use engine::SessionId;
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities_with_audio_processing,
    fake_worker_activities_with_audio_transcriber, final_assistant_text, live_workflow_handle,
    require_storage_live_env, run_with_live_worker, wait_for_terminal_run,
};
use temporal_server::{
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{
        AudioTranscodeError, AudioTranscodeOutput, AudioTranscodeRequest, AudioTranscoder,
        AudioTranscriber, AudioTranscription, AudioTranscriptionError, AudioTranscriptionRequest,
    },
};
use temporalio_client::{Client, WorkflowTerminateOptions};

const AUDIO_BYTES: &[u8] = b"OggS fake voice note";
const TRANSCODABLE_AUDIO_BYTES: &[u8] = b"AAC fake voice note";
const TRANSCRIPT_TEXT: &str = "please file the deployment note from this audio";

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn transcription_live_prepared_input_retains_source_audio() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let transcriber = Arc::new(RecordingAudioTranscriber::new(TRANSCRIPT_TEXT));
    let activities = fake_worker_activities_with_audio_transcriber(transcriber.clone()).await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_audio_transcription_live_client(client, task_queue, session_id, transcriber)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn transcription_live_transcodable_audio_is_prepared_outside_session() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let transcriber = Arc::new(RecordingAudioTranscriber::new(TRANSCRIPT_TEXT));
    let transcoded_bytes = tiny_wav_bytes();
    let transcoder = Arc::new(RecordingAudioTranscoder::new(
        transcoded_bytes.clone(),
        "audio/wav",
        "voice-note.wav",
    ));
    let activities =
        fake_worker_activities_with_audio_processing(transcriber.clone(), Some(transcoder.clone()))
            .await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_transcodable_audio_transcription_live_client(
            client,
            task_queue,
            session_id,
            transcriber,
            transcoder,
            transcoded_bytes,
        )
    })
    .await
}

async fn run_audio_transcription_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    transcriber: Arc<RecordingAudioTranscriber>,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = support::live::openai_live_model();
    support::live::seed_agent_default(&store, &model).await?;
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .build();

    api.start_session(SessionStartParams {
        access: None,
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
        delete_after_close_ms: None,
    })
    .await?;

    let audio = api
        .put_blobs(BlobPutParams {
            blobs: vec![BlobPutItem {
                bytes_base64: BASE64.encode(AUDIO_BYTES),
            }],
        })
        .await?;
    let transcript_ref = transcribe(
        &api,
        audio.result.blobs[0].blob_ref.clone(),
        "audio/ogg",
        "voice-note.ogg",
    )
    .await?;
    let started = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::TextRef {
                    origin: None,
                    blob_ref: transcript_ref,
                    provenance_ref: Some(audio.result.blobs[0].blob_ref.clone()),
                }],
            },
            config: None,
        })
        .await?;

    let run = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    assert_eq!(run.status, RunStatus::Completed);
    let output = final_assistant_text(&run).expect("assistant output");
    assert!(output.contains("Fake agent completed run"));

    let user_messages: Vec<&str> = run
        .entries
        .iter()
        .filter_map(|entry| match entry.kind {
            ContextEntryKindView::Message {
                role: ContextMessageRoleView::User,
            } => entry.text.as_deref(),
            _ => None,
        })
        .collect();
    let transcript = user_messages
        .iter()
        .find(|text| text.contains(TRANSCRIPT_TEXT))
        .copied()
        .expect("run items should include transcribed audio text");
    assert_eq!(transcript, TRANSCRIPT_TEXT);
    let transcript_entry = run
        .entries
        .iter()
        .find(|entry| {
            entry.provenance_ref.as_deref() == Some(audio.result.blobs[0].blob_ref.as_str())
        })
        .expect("text with source provenance");
    assert_eq!(
        transcript_entry.content.media_type.as_deref(),
        Some("text/plain")
    );
    assert_eq!(
        transcript_entry.provenance_ref.as_deref(),
        Some(audio.result.blobs[0].blob_ref.as_str())
    );
    assert!(transcript_entry.content.provider_kind.is_none());
    assert_eq!(transcript_entry.text.as_deref(), Some(TRANSCRIPT_TEXT));
    assert!(!transcript_entry.text_truncated);
    assert!(
        user_messages
            .iter()
            .all(|text| *text != "[audio: voice-note.ogg]")
    );

    let requests = transcriber.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].mime.as_str(), "audio/ogg");
    assert_eq!(requests[0].name.as_str(), "voice-note.ogg");
    assert_eq!(requests[0].bytes.as_slice(), AUDIO_BYTES);

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent audio transcription live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_transcodable_audio_transcription_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    transcriber: Arc<RecordingAudioTranscriber>,
    transcoder: Arc<RecordingAudioTranscoder>,
    transcoded_bytes: Vec<u8>,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = support::live::openai_live_model();
    support::live::seed_agent_default(&store, &model).await?;
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .build();

    api.start_session(SessionStartParams {
        access: None,
        metadata: Default::default(),
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
        delete_after_close_ms: None,
    })
    .await?;

    let audio = api
        .put_blobs(BlobPutParams {
            blobs: vec![BlobPutItem {
                bytes_base64: BASE64.encode(TRANSCODABLE_AUDIO_BYTES),
            }],
        })
        .await?;
    let transcript_ref = transcribe(
        &api,
        audio.result.blobs[0].blob_ref.clone(),
        "audio/x-aac",
        "voice-note.aac",
    )
    .await?;
    let started = api
        .start_run(RunStartParams {
            notify_on_terminal: None,
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::TextRef {
                    origin: None,
                    blob_ref: transcript_ref,
                    provenance_ref: Some(audio.result.blobs[0].blob_ref.clone()),
                }],
            },
            config: None,
        })
        .await?;

    let run = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    assert_eq!(run.status, RunStatus::Completed);

    let user_messages: Vec<&str> = run
        .entries
        .iter()
        .filter_map(|entry| match entry.kind {
            ContextEntryKindView::Message {
                role: ContextMessageRoleView::User,
            } => entry.text.as_deref(),
            _ => None,
        })
        .collect();
    let transcript = user_messages
        .iter()
        .find(|text| text.contains(TRANSCRIPT_TEXT))
        .copied()
        .expect("run items should include transcribed audio text");
    assert_eq!(transcript, TRANSCRIPT_TEXT);
    let transcript_entry = run
        .entries
        .iter()
        .find(|entry| {
            entry.provenance_ref.as_deref() == Some(audio.result.blobs[0].blob_ref.as_str())
        })
        .expect("text with source provenance");
    assert_eq!(
        transcript_entry.content.media_type.as_deref(),
        Some("text/plain")
    );
    assert_eq!(
        transcript_entry.provenance_ref.as_deref(),
        Some(audio.result.blobs[0].blob_ref.as_str())
    );
    assert!(transcript_entry.content.provider_kind.is_none());
    assert_eq!(transcript_entry.text.as_deref(), Some(TRANSCRIPT_TEXT));
    assert!(!transcript_entry.text_truncated);
    assert!(
        user_messages
            .iter()
            .all(|text| *text != "[audio: voice-note.aac]")
    );

    let transcode_requests = transcoder.requests();
    assert_eq!(transcode_requests.len(), 1);
    assert_eq!(transcode_requests[0].mime.as_str(), "audio/aac");
    assert_eq!(transcode_requests[0].name.as_str(), "voice-note.aac");
    assert_eq!(
        transcode_requests[0].bytes.as_slice(),
        TRANSCODABLE_AUDIO_BYTES
    );

    let transcription_requests = transcriber.requests();
    assert_eq!(transcription_requests.len(), 1);
    assert_eq!(transcription_requests[0].mime.as_str(), "audio/wav");
    assert_eq!(transcription_requests[0].name.as_str(), "voice-note.wav");
    assert_eq!(transcription_requests[0].bytes, transcoded_bytes);

    let handle = live_workflow_handle(&client, &session_id)?;
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("audio transcription live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

struct RecordingAudioTranscriber {
    text: String,
    requests: Mutex<Vec<AudioTranscriptionRequest>>,
}

impl RecordingAudioTranscriber {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<AudioTranscriptionRequest> {
        self.requests
            .lock()
            .expect("recording transcriber requests")
            .clone()
    }
}

#[async_trait]
impl AudioTranscriber for RecordingAudioTranscriber {
    async fn transcribe(
        &self,
        request: AudioTranscriptionRequest,
    ) -> Result<AudioTranscription, AudioTranscriptionError> {
        self.requests
            .lock()
            .expect("recording transcriber requests")
            .push(request);
        Ok(AudioTranscription {
            text: self.text.clone(),
        })
    }
}

struct RecordingAudioTranscoder {
    bytes: Vec<u8>,
    mime: String,
    name: String,
    requests: Mutex<Vec<AudioTranscodeRequest>>,
}

impl RecordingAudioTranscoder {
    fn new(bytes: Vec<u8>, mime: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            bytes,
            mime: mime.into(),
            name: name.into(),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<AudioTranscodeRequest> {
        self.requests
            .lock()
            .expect("recording transcoder requests")
            .clone()
    }
}

#[async_trait]
impl AudioTranscoder for RecordingAudioTranscoder {
    async fn transcode(
        &self,
        request: AudioTranscodeRequest,
    ) -> Result<AudioTranscodeOutput, AudioTranscodeError> {
        self.requests
            .lock()
            .expect("recording transcoder requests")
            .push(request);
        Ok(AudioTranscodeOutput {
            bytes: self.bytes.clone(),
            mime: self.mime.clone(),
            name: self.name.clone(),
        })
    }
}

fn tiny_wav_bytes() -> Vec<u8> {
    let sample_rate = 8_000u32;
    let channels = 1u16;
    let bits_per_sample = 16u16;
    let sample_count = sample_rate as usize;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_len = sample_count * block_align as usize;

    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
    bytes.resize(44 + data_len, 0);
    bytes
}

async fn transcribe(
    api: &GatewayAgentApi,
    blob_ref: String,
    mime: &str,
    name: &str,
) -> anyhow::Result<String> {
    let model = api::ModelConfig {
        provider_id: "test-speech".into(),
        api_kind: "openai:audio-transcriptions".into(),
        model: "pinned-speech-model".into(),
    };
    let before = api
        .read_model_defaults(api::ModelDefaultsReadParams {})
        .await?
        .result
        .defaults;
    let updated = api
        .put_model_defaults(api::ModelDefaultsPutParams {
            slot: api::ModelDefaultSlot::SpeechToText,
            model: Some(model.clone()),
            expected_revision: before.revision,
        })
        .await?
        .result
        .defaults;
    let request = api::TranscriptionStartParams {
        idempotency_key: uuid::Uuid::new_v4().to_string(),
        audio: api::TranscriptionAudio {
            blob_ref,
            mime: mime.into(),
            name: name.into(),
        },
        model: None,
        language: Some("en".into()),
        prompt: None,
    };
    let initial = api
        .start_transcription(request.clone())
        .await?
        .result
        .transcription;
    api.put_model_defaults(api::ModelDefaultsPutParams {
        slot: api::ModelDefaultSlot::SpeechToText,
        model: before.speech_to_text,
        expected_revision: updated.revision,
    })
    .await?;
    let terminal = loop {
        let view = api
            .read_transcription(api::TranscriptionReadParams {
                transcription_id: initial.transcription_id.clone(),
            })
            .await?
            .result
            .transcription;
        if view.status.is_terminal() {
            break view;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        terminal.status,
        api::TranscriptionStatus::Succeeded,
        "{:?}",
        terminal.failure
    );
    assert_eq!(terminal.model, model);
    assert_eq!(terminal.text.as_deref(), Some(TRANSCRIPT_TEXT));
    let mut other = support::live::local_request_context().await?;
    other.actor = Some("another-person".into());
    let denied = temporal_server::gateway::request_context::with_request_context(
        other,
        api.read_transcription(api::TranscriptionReadParams {
            transcription_id: initial.transcription_id.clone(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(denied.kind, api::AgentApiErrorKind::Forbidden);
    let retried = api
        .start_transcription(request.clone())
        .await?
        .result
        .transcription;
    assert_eq!(retried.transcript_ref, terminal.transcript_ref);
    assert_eq!(terminal.audio, request.audio);
    assert_eq!(retried.model, model);
    let mut conflicting = request;
    conflicting.prompt = Some("different options".into());
    assert_eq!(
        api.start_transcription(conflicting).await.unwrap_err().kind,
        api::AgentApiErrorKind::Conflict
    );
    let cancelled = api
        .cancel_transcription(api::TranscriptionCancelParams {
            transcription_id: initial.transcription_id,
        })
        .await?
        .result
        .transcription;
    assert_eq!(cancelled.status, api::TranscriptionStatus::Succeeded);
    Ok(terminal.transcript_ref.unwrap())
}

struct ControlledTranscriber {
    text: String,
    attempts: std::sync::atomic::AtomicUsize,
    block: bool,
}

#[async_trait]
impl AudioTranscriber for ControlledTranscriber {
    async fn transcribe(
        &self,
        _request: AudioTranscriptionRequest,
    ) -> Result<AudioTranscription, AudioTranscriptionError> {
        let attempt = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.block {
            return std::future::pending().await;
        }
        if attempt == 0 {
            return Err(AudioTranscriptionError {
                message: "temporary failure".into(),
                retryable: true,
                configuration: false,
            });
        }
        Ok(AudioTranscription {
            text: self.text.clone(),
        })
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires ./dev.sh infra or compatible Temporal + Postgres env"]
async fn transcription_live_retries_transient_failure_and_cancels_running_activity()
-> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().await;
    require_storage_live_env()?;
    for block in [false, true] {
        let transcriber = Arc::new(ControlledTranscriber {
            // A plain text result can be shared with sessions from other jobs.
            // The expiry assertion needs a unique, unsubmitted result blob.
            text: format!("unsubmitted transcript {}", uuid::Uuid::new_v4()),
            attempts: Default::default(),
            block,
        });
        let activities = fake_worker_activities_with_audio_transcriber(transcriber.clone()).await?;
        run_with_live_worker(activities, move |client, queue, _| async move {
            let store = pg_store_from_env().await?;
            store.ensure_universe().await?;
            let api = GatewayAgentApi::builder(client, store).with_task_queue(queue).build();
            let blob = api.put_blobs(BlobPutParams { blobs: vec![BlobPutItem { bytes_base64: BASE64.encode(AUDIO_BYTES) }] }).await?.result.blobs.remove(0).blob_ref;
            let request = api::TranscriptionStartParams {
                idempotency_key: uuid::Uuid::new_v4().to_string(),
                audio: api::TranscriptionAudio { blob_ref: blob, mime: "audio/ogg".into(), name: "voice.ogg".into() },
                model: Some(api::ModelConfig { provider_id: "fake".into(), api_kind: "openai:audio-transcriptions".into(), model: "speech".into() }),
                language: None, prompt: None,
            };
            let started = api.start_transcription(request).await?.result.transcription;
            while transcriber.attempts.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            if block {
                api.cancel_transcription(api::TranscriptionCancelParams { transcription_id: started.transcription_id.clone() }).await?;
            }
            let terminal = loop {
                let view = api.read_transcription(api::TranscriptionReadParams { transcription_id: started.transcription_id.clone() }).await?.result.transcription;
                if view.status.is_terminal() { break view; }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            };
            assert_eq!(terminal.status, if block { api::TranscriptionStatus::Cancelled } else { api::TranscriptionStatus::Succeeded });
            assert_eq!(transcriber.attempts.load(std::sync::atomic::Ordering::SeqCst), if block { 1 } else { 2 });
            if !block {
                let store = pg_store_from_env().await?;
                let reference = engine::BlobRef::parse(terminal.transcript_ref.as_ref().unwrap())?;
                sqlx::query("UPDATE cas_blobs SET created_at_ms = 1, touched_at_ms = 1 WHERE universe_id = $1 AND digest = $2")
                    .bind(store.config().universe_id).bind(reference.as_str().trim_start_matches("sha256:")).execute(store.pool()).await?;
                assert_eq!(store.delete_dead_blobs(&[reference], 2, &[]).await?.len(), 1, "unsubmitted workflows do not create retention roots");
                let expired = api.read_transcription(api::TranscriptionReadParams { transcription_id: started.transcription_id }).await?.result.transcription;
                assert_eq!(expired.status, api::TranscriptionStatus::Expired);
            }

            Ok(())
        }).await?;
    }
    Ok(())
}
