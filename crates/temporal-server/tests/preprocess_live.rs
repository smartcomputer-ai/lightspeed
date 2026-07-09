mod support;

use std::sync::{Arc, Mutex};

use api::{
    AgentApiService, BlobPutItem, BlobPutParams, ContextEntryKindView, ContextMessageRoleView,
    InputItem, MediaKind, RunStartParams, RunStartSource, RunStatus, SessionConfig,
    SessionStartParams,
};
use api_projection::model_to_api;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use engine::SessionId;
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities_with_audio_preprocessors,
    fake_worker_activities_with_audio_transcriber, final_assistant_text, live_workflow_handle,
    require_storage_live_env, run_with_live_worker, wait_for_terminal_run,
};
use temporal_server::{
    default_model_from_env,
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
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn preprocess_live_audio_input_is_transcribed_before_admission() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let transcriber = Arc::new(RecordingAudioTranscriber::new(TRANSCRIPT_TEXT));
    let activities = fake_worker_activities_with_audio_transcriber(transcriber.clone()).await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_audio_preprocess_live_client(client, task_queue, session_id, transcriber)
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires local/up.sh or compatible Temporal + Postgres env"]
async fn preprocess_live_transcodable_audio_is_transcoded_before_admission() -> anyhow::Result<()> {
    let _lock = LIVE_TEST_LOCK.lock().expect("live test lock");
    let _ = dotenvy::dotenv();
    require_storage_live_env()?;

    let transcriber = Arc::new(RecordingAudioTranscriber::new(TRANSCRIPT_TEXT));
    let transcoded_bytes = tiny_wav_bytes();
    let transcoder = Arc::new(RecordingAudioTranscoder::new(
        transcoded_bytes.clone(),
        "audio/wav",
        "voice-note.wav",
    ));
    let activities = fake_worker_activities_with_audio_preprocessors(
        transcriber.clone(),
        Some(transcoder.clone()),
    )
    .await?;
    run_with_live_worker(activities, move |client, task_queue, session_id| {
        run_transcodable_audio_preprocess_live_client(
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

async fn run_audio_preprocess_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    transcriber: Arc<RecordingAudioTranscriber>,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(128)
        .build();

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
    })
    .await?;

    let audio = api
        .put_blobs(BlobPutParams {
            blobs: vec![BlobPutItem {
                bytes_base64: BASE64.encode(AUDIO_BYTES),
            }],
        })
        .await?;
    let started = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Media {
                    blob_ref: audio.result.blobs[0].blob_ref.clone(),
                    mime: "audio/ogg".to_owned(),
                    kind: MediaKind::Audio,
                    name: Some("voice-note.ogg".to_owned()),
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
    assert!(transcript.contains("[audio transcript: voice-note.ogg]"));
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
                .reason("agent audio preprocess live test cleanup")
                .build(),
        )
        .await;
    Ok(())
}

async fn run_transcodable_audio_preprocess_live_client(
    client: Client,
    task_queue: String,
    session_id: SessionId,
    transcriber: Arc<RecordingAudioTranscriber>,
    transcoder: Arc<RecordingAudioTranscoder>,
    transcoded_bytes: Vec<u8>,
) -> anyhow::Result<()> {
    let store = pg_store_from_env().await?;
    let model = default_model_from_env();
    let api = GatewayAgentApi::builder(client.clone(), store)
        .with_task_queue(task_queue)
        .with_default_model(model.clone())
        .with_max_steps_per_input(128)
        .build();

    api.start_session(SessionStartParams {
        session_id: Some(session_id.as_str().to_owned()),
        display_name: None,
        config: Some(SessionConfig {
            model: Some(model_to_api(&model)),
            ..SessionConfig::default()
        }),
        profile: None,
    })
    .await?;

    let audio = api
        .put_blobs(BlobPutParams {
            blobs: vec![BlobPutItem {
                bytes_base64: BASE64.encode(TRANSCODABLE_AUDIO_BYTES),
            }],
        })
        .await?;
    let started = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            source: RunStartSource::Input {
                items: vec![InputItem::Media {
                    blob_ref: audio.result.blobs[0].blob_ref.clone(),
                    mime: "audio/x-aac".to_owned(),
                    kind: MediaKind::Audio,
                    name: Some("voice-note.aac".to_owned()),
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
    assert!(transcript.contains("[audio transcript: voice-note.aac]"));
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
                .reason("agent audio transcode preprocess live test cleanup")
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
