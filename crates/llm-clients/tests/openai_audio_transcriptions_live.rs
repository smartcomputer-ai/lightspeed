use llm_clients::openai::audio::{
    AudioFile, Client, Config, CreateTranscriptionRequest, DEFAULT_TRANSCRIPTION_MODEL,
};
use std::path::PathBuf;
use std::time::Duration;

mod support;

use support::{
    env_or_dotenv_var, openai_audio_transcription_create, repo_relative_path,
    required_env_or_dotenv_var,
};

const DEFAULT_AUDIO_FIXTURE_URL: &str =
    "https://commons.wikimedia.org/wiki/Special:Redirect/file/Kennedy_berliner.ogg";
const DEFAULT_AUDIO_FIXTURE_EXPECT: &str = "berliner";

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

async fn live_fixture() -> AudioFixture {
    if let Ok(value) = env_or_dotenv_var("OPENAI_AUDIO_TRANSCRIPTION_FIXTURE") {
        assert!(
            !value.trim().is_empty(),
            "OPENAI_AUDIO_TRANSCRIPTION_FIXTURE is set but empty"
        );
        return read_fixture_path(repo_relative_path(value));
    }

    let url = env_or_dotenv_var("OPENAI_AUDIO_TRANSCRIPTION_FIXTURE_URL")
        .unwrap_or_else(|_| DEFAULT_AUDIO_FIXTURE_URL.to_owned());
    read_fixture_url(&url).await
}

fn read_fixture_path(path: PathBuf) -> AudioFixture {
    let bytes = std::fs::read(&path).expect("read audio fixture");
    assert!(!bytes.is_empty(), "audio fixture is empty");
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("fixture path must have a UTF-8 file name")
        .to_owned();
    let mime = mime_for_name(&filename, "OPENAI_AUDIO_TRANSCRIPTION_FIXTURE");
    AudioFixture {
        bytes,
        filename,
        mime: mime.to_owned(),
        default_expected_text: None,
    }
}

async fn read_fixture_url(url: &str) -> AudioFixture {
    let parsed_url = reqwest::Url::parse(url).expect("audio fixture URL must be valid");
    let filename = parsed_url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .unwrap_or("audio.ogg")
        .to_owned();
    let mime = mime_for_name(&filename, "OPENAI_AUDIO_TRANSCRIPTION_FIXTURE_URL");
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("fixture download client")
        .get(parsed_url)
        .send()
        .await
        .expect("download audio fixture")
        .error_for_status()
        .expect("audio fixture download returned an error status");
    let bytes = response
        .bytes()
        .await
        .expect("read downloaded audio fixture")
        .to_vec();
    assert!(!bytes.is_empty(), "downloaded audio fixture is empty");
    AudioFixture {
        bytes,
        filename,
        mime: mime.to_owned(),
        default_expected_text: (url == DEFAULT_AUDIO_FIXTURE_URL)
            .then_some(DEFAULT_AUDIO_FIXTURE_EXPECT),
    }
}

fn mime_for_name(name: &str, source: &str) -> &'static str {
    match PathBuf::from(name)
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
            "{source} has unsupported extension {other:?}; use mp3, mp4, m4a, wav, webm, ogg, opus, or flac"
        ),
    }
}

struct AudioFixture {
    bytes: Vec<u8>,
    filename: String,
    mime: String,
    default_expected_text: Option<&'static str>,
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires OPENAI_API_KEY and network access or OPENAI_AUDIO_TRANSCRIPTION_FIXTURE (costs real money)"]
async fn openai_audio_transcriptions_live_create() {
    let client = live_client();
    let fixture = live_fixture().await;
    let expected_text = env_or_dotenv_var("OPENAI_AUDIO_TRANSCRIPTION_EXPECT")
        .ok()
        .or_else(|| fixture.default_expected_text.map(str::to_owned));
    let mut request = CreateTranscriptionRequest::new(AudioFile {
        bytes: fixture.bytes,
        filename: fixture.filename,
        mime: fixture.mime,
    });
    request.model = live_model();

    let response = openai_audio_transcription_create(&client, request)
        .await
        .expect("create transcription");

    assert_eq!(response.status, 200);
    let text = response.parsed.text.trim();
    assert!(!text.is_empty(), "expected non-empty transcription text");
    if let Some(expected) = expected_text {
        assert!(
            text.to_lowercase().contains(&expected.to_lowercase()),
            "expected transcription to contain {expected:?}, got {text:?}"
        );
    }
}
