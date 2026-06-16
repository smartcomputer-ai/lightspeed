use llm_clients::openai::audio::{
    AudioFile, Client, Config, CreateTranscriptionRequest, DEFAULT_TRANSCRIPTION_MODEL,
};
use std::path::{Path, PathBuf};

mod support;

use support::{
    env_or_dotenv_var, openai_audio_transcription_create, repo_relative_path,
    required_env_or_dotenv_var,
};

fn live_model() -> String {
    env_or_dotenv_var("OPENAI_AUDIO_TRANSCRIPTION_MODEL")
        .unwrap_or_else(|_| DEFAULT_TRANSCRIPTION_MODEL.to_owned())
}

fn live_client() -> Client {
    let api_key = required_env_or_dotenv_var(
        "OPENAI_API_KEY",
        "OPENAI_API_KEY must be set in env or root .env to run openai:audio-transcriptions live tests",
    );

    let mut config = Config::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("OPENAI_BASE_URL") {
        config.base_url = base_url;
    }
    if let Ok(org_id) = env_or_dotenv_var("OPENAI_ORG_ID") {
        config.organization = Some(org_id);
    }
    if let Ok(project) = env_or_dotenv_var("OPENAI_PROJECT_ID") {
        config.project = Some(project);
    }

    Client::new(config).expect("OpenAI Audio client")
}

fn live_fixture_path() -> PathBuf {
    let value = required_env_or_dotenv_var(
        "OPENAI_AUDIO_TRANSCRIPTION_FIXTURE",
        "OPENAI_AUDIO_TRANSCRIPTION_FIXTURE must point to an audio file to run openai:audio-transcriptions live tests",
    );
    repo_relative_path(value)
}

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp3" | "mpga" | "mpeg") => "audio/mpeg",
        Some("m4a" | "mp4") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("webm") => "audio/webm",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("flac") => "audio/flac",
        other => panic!(
            "OPENAI_AUDIO_TRANSCRIPTION_FIXTURE has unsupported extension {other:?}; use mp3, mp4, m4a, wav, webm, ogg, opus, or flac"
        ),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and OPENAI_AUDIO_TRANSCRIPTION_FIXTURE (costs real money)"]
async fn openai_audio_transcriptions_live_create() {
    let client = live_client();
    let fixture = live_fixture_path();
    let bytes = std::fs::read(&fixture).expect("read audio fixture");
    assert!(!bytes.is_empty(), "audio fixture is empty");
    let filename = fixture
        .file_name()
        .and_then(|name| name.to_str())
        .expect("fixture path must have a UTF-8 file name")
        .to_owned();
    let mut request = CreateTranscriptionRequest::new(AudioFile {
        bytes,
        filename,
        mime: mime_for_path(&fixture).to_owned(),
    });
    request.model = live_model();

    let response = openai_audio_transcription_create(&client, request)
        .await
        .expect("create transcription");

    assert_eq!(response.status, 200);
    let text = response.parsed.text.trim();
    assert!(!text.is_empty(), "expected non-empty transcription text");
    if let Ok(expected) = env_or_dotenv_var("OPENAI_AUDIO_TRANSCRIPTION_EXPECT") {
        assert!(
            text.to_lowercase().contains(&expected.to_lowercase()),
            "expected transcription to contain {expected:?}, got {text:?}"
        );
    }
}
