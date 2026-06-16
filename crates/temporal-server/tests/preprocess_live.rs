mod support;

use std::sync::{Arc, Mutex};

use api::{
    AgentApiService, BlobPutParams, InputItem, MediaKind, RunStartParams, RunStatus,
    SessionConfigInput, SessionItemView, SessionStartParams,
};
use api_projection::model_to_api;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use engine::SessionId;
use support::live::{
    LIVE_TEST_LOCK, fake_worker_activities_with_audio_transcriber, final_assistant_text,
    require_storage_live_env, run_with_live_worker, wait_for_terminal_run,
};
use temporal_server::{
    default_model_from_env,
    gateway::GatewayAgentApi,
    pg_store_from_env,
    worker::{
        AudioTranscriber, AudioTranscription, AudioTranscriptionError, AudioTranscriptionRequest,
    },
};
use temporal_workflow::AgentSessionWorkflow;
use temporalio_client::{Client, WorkflowTerminateOptions};

const AUDIO_BYTES: &[u8] = b"OggS fake voice note";
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
        cwd: None,
        config: Some(SessionConfigInput {
            model: Some(model_to_api(&model)),
            ..SessionConfigInput::default()
        }),
    })
    .await?;

    let audio = api
        .put_blob(BlobPutParams {
            bytes_base64: BASE64.encode(AUDIO_BYTES),
        })
        .await?;
    let started = api
        .start_run(RunStartParams {
            submission_id: None,
            session_id: session_id.as_str().to_owned(),
            input: vec![InputItem::Media {
                blob_ref: audio.result.blob_ref,
                mime: "audio/ogg".to_owned(),
                kind: MediaKind::Audio,
                name: Some("voice-note.ogg".to_owned()),
            }],
            config: None,
        })
        .await?;

    let run = wait_for_terminal_run(&api, &session_id, &started.result.run.id).await?;
    assert_eq!(run.status, RunStatus::Completed);
    let output = final_assistant_text(&run).expect("assistant output");
    assert!(output.contains("Fake agent completed run"));

    let user_messages: Vec<&str> = run
        .items
        .iter()
        .filter_map(|item| match item {
            SessionItemView::UserMessage { text, .. } => Some(text.as_str()),
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

    let handle = client.get_workflow_handle::<AgentSessionWorkflow>(session_id.as_str());
    let _ = handle
        .terminate(
            WorkflowTerminateOptions::builder()
                .reason("agent audio preprocess live test cleanup")
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
