use std::{
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use llm_clients::{LlmApiError, openai::audio as oai};
use llm_runtime::ModelProviderResolver;
pub(super) const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
pub(super) const MAX_AUDIO_DURATION_MS: u64 = 10 * 60 * 1000;
const OPENAI_PROVIDER_ID: &str = "openai";
pub(super) const PROVIDER_ACCEPTED_AUDIO_MIMES: &[&str] = &[
    "audio/mpeg",
    "audio/mp4",
    "audio/wav",
    "audio/webm",
    "audio/ogg",
];
pub(super) const TRANSCODABLE_AUDIO_MIMES: &[&str] =
    &["audio/aac", "audio/amr", "audio/3gpp", "audio/3gpp2"];
const TRANSCODED_AUDIO_MIME: &str = "audio/wav";
const DEFAULT_FFMPEG_PATH: &str = "ffmpeg";
const DEFAULT_TRANSCODE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTranscriptionRequest {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub name: String,
    pub model: api::ModelConfig,
    pub language: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTranscription {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("audio transcription failed: {message}")]
pub struct AudioTranscriptionError {
    pub message: String,
    pub retryable: bool,
    pub configuration: bool,
}

#[async_trait]
pub trait AudioTranscriber: Send + Sync {
    async fn transcribe(
        &self,
        request: AudioTranscriptionRequest,
    ) -> Result<AudioTranscription, AudioTranscriptionError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTranscodeRequest {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTranscodeOutput {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("audio transcode failed: {message}")]
pub struct AudioTranscodeError {
    pub message: String,
}

#[async_trait]
pub trait AudioTranscoder: Send + Sync {
    async fn transcode(
        &self,
        request: AudioTranscodeRequest,
    ) -> Result<AudioTranscodeOutput, AudioTranscodeError>;
}

pub struct UnavailableAudioTranscriber;

#[async_trait]
impl AudioTranscriber for UnavailableAudioTranscriber {
    async fn transcribe(
        &self,
        _request: AudioTranscriptionRequest,
    ) -> Result<AudioTranscription, AudioTranscriptionError> {
        Err(AudioTranscriptionError {
            message: "audio transcriber is not configured".to_owned(),
            retryable: false,
            configuration: true,
        })
    }
}

#[derive(Clone, Debug)]
pub struct FfmpegAudioTranscoder {
    ffmpeg_path: PathBuf,
    timeout: Duration,
    max_output_bytes: u64,
}

impl FfmpegAudioTranscoder {
    pub fn new(ffmpeg_path: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg_path: ffmpeg_path.into(),
            timeout: DEFAULT_TRANSCODE_TIMEOUT,
            max_output_bytes: MAX_AUDIO_BYTES,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn from_env() -> Self {
        let ffmpeg_path = env::var("LIGHTSPEED_FFMPEG_PATH")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_FFMPEG_PATH.to_owned());
        let timeout = env::var("LIGHTSPEED_AUDIO_TRANSCODE_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|millis| *millis > 0)
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TRANSCODE_TIMEOUT);
        Self::new(ffmpeg_path).with_timeout(timeout)
    }
}

#[async_trait]
impl AudioTranscoder for FfmpegAudioTranscoder {
    async fn transcode(
        &self,
        request: AudioTranscodeRequest,
    ) -> Result<AudioTranscodeOutput, AudioTranscodeError> {
        let temp_dir = tempfile::Builder::new()
            .prefix("lightspeed-audio-transcode-")
            .tempdir()
            .map_err(|error| AudioTranscodeError {
                message: format!("create transcode temp dir: {error}"),
            })?;
        let input_path = temp_dir
            .path()
            .join(format!("input.{}", extension_for_mime(&request.mime)));
        let output_path = temp_dir.path().join("output.wav");
        tokio::fs::write(&input_path, &request.bytes)
            .await
            .map_err(|error| AudioTranscodeError {
                message: format!("write transcode input: {error}"),
            })?;

        let mut command = tokio::process::Command::new(&self.ffmpeg_path);
        command
            .args(ffmpeg_args(&input_path, &output_path))
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let output = tokio::time::timeout(self.timeout, command.output())
            .await
            .map_err(|_| AudioTranscodeError {
                message: format!(
                    "ffmpeg did not finish within {}ms",
                    self.timeout.as_millis()
                ),
            })?
            .map_err(|error| AudioTranscodeError {
                message: format!("run ffmpeg: {error}"),
            })?;
        if !output.status.success() {
            return Err(AudioTranscodeError {
                message: ffmpeg_failure_message(&output.stderr),
            });
        }
        let metadata =
            tokio::fs::metadata(&output_path)
                .await
                .map_err(|error| AudioTranscodeError {
                    message: format!("read ffmpeg output metadata: {error}"),
                })?;
        if metadata.len() > self.max_output_bytes {
            return Err(AudioTranscodeError {
                message: format!(
                    "transcoded audio {} is {} bytes; the limit is {} bytes",
                    output_path.display(),
                    metadata.len(),
                    self.max_output_bytes
                ),
            });
        }
        let bytes = tokio::fs::read(&output_path)
            .await
            .map_err(|error| AudioTranscodeError {
                message: format!("read ffmpeg output: {error}"),
            })?;
        Ok(AudioTranscodeOutput {
            bytes,
            mime: TRANSCODED_AUDIO_MIME.to_owned(),
            name: transcoded_audio_filename(&request.name),
        })
    }
}

pub struct OpenAiAudioTranscriber {
    client: Arc<oai::Client>,
    provider_keys: Arc<dyn ModelProviderResolver>,
}

impl OpenAiAudioTranscriber {
    pub fn new(client: Arc<oai::Client>, provider_keys: Arc<dyn ModelProviderResolver>) -> Self {
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
        api::ModelDefaultSlot::SpeechToText
            .validate_model(&request.model)
            .map_err(|error| AudioTranscriptionError {
                message: error.to_string(),
                retryable: false,
                configuration: true,
            })?;
        let stored = llm_runtime::resolve_provider_route(
            self.provider_keys.as_ref(),
            &request.model.provider_id,
            &request.model.api_kind,
        )
        .await
        .map_err(|error| {
            let retryable = matches!(error, llm_runtime::ProviderKeyError::Backend { .. });
            AudioTranscriptionError {
                message: "Transcription provider configuration is missing or unusable.".into(),
                retryable,
                configuration: !retryable,
            }
        })?;
        if request.model.provider_id != OPENAI_PROVIDER_ID
            && stored.as_ref().and_then(|p| p.endpoint.as_ref()).is_none()
        {
            return Err(AudioTranscriptionError {
                message: "Transcription provider requires an explicit endpoint.".into(),
                retryable: false,
                configuration: true,
            });
        }
        let mut input = oai::CreateTranscriptionRequest::new(oai::AudioFile {
            bytes: request.bytes,
            filename: request.name,
            mime: request.mime,
        });
        input.model = request.model.model;
        input.language = request.language;
        input.prompt = request.prompt;
        let response = self
            .client
            .create_transcription_with_transport(
                input,
                stored.as_ref().map(|provider| provider.as_request_auth()),
                stored
                    .as_ref()
                    .and_then(|provider| provider.endpoint.as_ref())
                    .map(|endpoint| &endpoint.transport),
            )
            .await
            .map_err(map_openai_error)?;
        Ok(AudioTranscription {
            text: response.parsed.text,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AudioPreparationError {
    #[error("Audio requires a transcoder, but none is configured.")]
    TranscoderUnavailable,
    #[error(transparent)]
    Transcode(#[from] AudioTranscodeError),
    #[error("Transcoded audio exceeds the 25 MiB limit.")]
    OutputTooLarge,
    #[error("Audio transcoder returned unsupported MIME type {0}.")]
    UnsupportedOutputMime(String),
}

#[derive(Debug)]
pub(super) struct PreparedAudio {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub name: String,
}

pub(super) async fn prepare_audio_for_transcription(
    bytes: Vec<u8>,
    mime: &str,
    name: &str,
    transcoder: Option<&dyn AudioTranscoder>,
) -> Result<PreparedAudio, AudioPreparationError> {
    if PROVIDER_ACCEPTED_AUDIO_MIMES.contains(&mime) {
        return Ok(PreparedAudio {
            bytes,
            mime: mime.to_owned(),
            name: audio_filename(name),
        });
    }
    let Some(transcoder) = transcoder else {
        return Err(AudioPreparationError::TranscoderUnavailable);
    };
    let output = transcoder
        .transcode(AudioTranscodeRequest {
            bytes,
            mime: mime.to_owned(),
            name: audio_filename(name),
        })
        .await
        .map_err(AudioPreparationError::Transcode)?;
    if output.bytes.len() as u64 > MAX_AUDIO_BYTES {
        return Err(AudioPreparationError::OutputTooLarge);
    }
    if !PROVIDER_ACCEPTED_AUDIO_MIMES.contains(&output.mime.as_str()) {
        return Err(AudioPreparationError::UnsupportedOutputMime(output.mime));
    }
    Ok(PreparedAudio {
        bytes: output.bytes,
        mime: output.mime,
        name: output.name,
    })
}

pub(super) fn normalized_mime(mime: Option<&str>) -> String {
    let mime = mime
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "audio/mp3" => "audio/mpeg",
        "audio/x-m4a" | "audio/m4a" => "audio/mp4",
        "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "audio/wav",
        "audio/oga" | "audio/opus" => "audio/ogg",
        "audio/x-aac" => "audio/aac",
        "audio/3gp" => "audio/3gpp",
        "audio/3g2" => "audio/3gpp2",
        other => other,
    }
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

fn transcoded_audio_filename(label: &str) -> String {
    let filename = audio_filename(label);
    let stem = Path::new(&filename)
        .file_stem()
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("audio");
    format!("{stem}.wav")
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/wav" => "wav",
        "audio/webm" => "webm",
        "audio/ogg" => "ogg",
        "audio/aac" => "aac",
        "audio/amr" => "amr",
        "audio/3gpp" => "3gp",
        "audio/3gpp2" => "3g2",
        _ => "bin",
    }
}

fn ffmpeg_args(input_path: &Path, output_path: &Path) -> Vec<OsString> {
    [
        OsString::from("-nostdin"),
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-y"),
        OsString::from("-i"),
        input_path.as_os_str().to_owned(),
        OsString::from("-vn"),
        OsString::from("-ac"),
        OsString::from("1"),
        OsString::from("-ar"),
        OsString::from("16000"),
        OsString::from("-acodec"),
        OsString::from("pcm_s16le"),
        OsString::from("-f"),
        OsString::from("wav"),
        output_path.as_os_str().to_owned(),
    ]
    .into_iter()
    .collect()
}

fn ffmpeg_failure_message(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        "ffmpeg exited with a non-zero status".to_owned()
    } else {
        format!("ffmpeg exited with a non-zero status: {stderr}")
    }
}

pub(super) fn audio_duration_ms(mime: &str, bytes: &[u8]) -> Option<u64> {
    // Cheap duration enforcement is intentionally narrow for the first cut:
    // OGG/Opus and WAV are covered; MP3/M4A/WebM rely on the byte cap unless
    // a non-provider container is transcoded to WAV.
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

fn map_openai_error(error: LlmApiError) -> AudioTranscriptionError {
    let retryable = error.retryable();
    let configuration = matches!(error, LlmApiError::Configuration(_));
    AudioTranscriptionError {
        message: if configuration {
            "Transcription provider is not configured."
        } else {
            "Transcription provider request failed."
        }
        .into(),
        retryable,
        configuration,
    }
}

pub(super) fn default_openai_audio_transcriber(
    provider_keys: Arc<dyn ModelProviderResolver>,
) -> Result<Arc<dyn AudioTranscriber>, anyhow::Error> {
    let client = oai::Client::new(oai::Config::from_env_allow_missing_key())
        .map_err(|error| anyhow::anyhow!("construct OpenAI audio client: {error}"))?;
    Ok(Arc::new(OpenAiAudioTranscriber::new(
        Arc::new(client),
        provider_keys,
    )))
}

pub fn default_audio_transcoder_from_env() -> anyhow::Result<Option<Arc<dyn AudioTranscoder>>> {
    let Some(kind) = env::var("LIGHTSPEED_AUDIO_TRANSCODER")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "none")
    else {
        return Ok(None);
    };
    match kind.as_str() {
        "ffmpeg" => Ok(Some(Arc::new(FfmpegAudioTranscoder::from_env()))),
        other => anyhow::bail!(
            "unsupported LIGHTSPEED_AUDIO_TRANSCODER value {other:?}; expected \"ffmpeg\" or \"none\""
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct StaticTranscoder {
        result: Result<AudioTranscodeOutput, AudioTranscodeError>,
        requests: Mutex<Vec<AudioTranscodeRequest>>,
    }

    impl StaticTranscoder {
        fn new(result: Result<AudioTranscodeOutput, AudioTranscodeError>) -> Self {
            Self {
                result,
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<AudioTranscodeRequest> {
            self.requests
                .lock()
                .expect("static transcoder requests")
                .clone()
        }
    }

    #[async_trait]
    impl AudioTranscoder for StaticTranscoder {
        async fn transcode(
            &self,
            request: AudioTranscodeRequest,
        ) -> Result<AudioTranscodeOutput, AudioTranscodeError> {
            self.requests
                .lock()
                .expect("static transcoder requests")
                .push(request);
            self.result.clone()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn accepted_audio_bypasses_transcoding() {
        let transcoder = StaticTranscoder::new(Err(AudioTranscodeError {
            message: "must not run".into(),
        }));
        let audio = prepare_audio_for_transcription(
            b"audio".to_vec(),
            "audio/ogg",
            "voice.ogg",
            Some(&transcoder),
        )
        .await
        .unwrap();
        assert_eq!(audio.bytes, b"audio");
        assert_eq!(audio.mime, "audio/ogg");
        assert_eq!(audio.name, "voice.ogg");
        assert!(transcoder.requests().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transcodable_audio_requires_transcoder() {
        let error = prepare_audio_for_transcription(vec![], "audio/aac", "voice.aac", None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AudioPreparationError::TranscoderUnavailable
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transcodable_audio_is_prepared_for_provider() {
        let transcoder = StaticTranscoder::new(Ok(AudioTranscodeOutput {
            bytes: tiny_wav_bytes(),
            mime: "audio/wav".into(),
            name: "voice.wav".into(),
        }));
        let audio = prepare_audio_for_transcription(
            b"aac fake".to_vec(),
            "audio/aac",
            "voice.aac",
            Some(&transcoder),
        )
        .await
        .unwrap();
        assert_eq!(audio.bytes, tiny_wav_bytes());
        assert_eq!(audio.mime, "audio/wav");
        assert_eq!(audio.name, "voice.wav");
        assert_eq!(
            transcoder.requests(),
            vec![AudioTranscodeRequest {
                bytes: b"aac fake".to_vec(),
                mime: "audio/aac".into(),
                name: "voice.aac".into(),
            }]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_transcoder_output_is_rejected() {
        for (output, too_large) in [
            (
                AudioTranscodeOutput {
                    bytes: vec![0; MAX_AUDIO_BYTES as usize + 1],
                    mime: "audio/wav".into(),
                    name: "voice.wav".into(),
                },
                true,
            ),
            (
                AudioTranscodeOutput {
                    bytes: vec![],
                    mime: "audio/aac".into(),
                    name: "voice.aac".into(),
                },
                false,
            ),
        ] {
            let transcoder = StaticTranscoder::new(Ok(output));
            let error = prepare_audio_for_transcription(
                vec![],
                "audio/aac",
                "voice.aac",
                Some(&transcoder),
            )
            .await
            .unwrap_err();
            if too_large {
                assert!(matches!(error, AudioPreparationError::OutputTooLarge));
            } else {
                assert!(matches!(
                    error,
                    AudioPreparationError::UnsupportedOutputMime(_)
                ));
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transcode_failure_is_preserved() {
        let transcoder = StaticTranscoder::new(Err(AudioTranscodeError {
            message: "unsupported codec".into(),
        }));
        let error =
            prepare_audio_for_transcription(vec![], "audio/aac", "voice.aac", Some(&transcoder))
                .await
                .unwrap_err();
        assert!(matches!(error, AudioPreparationError::Transcode(_)));
    }

    #[test]
    fn duration_readers_measure_audio() {
        assert_eq!(
            audio_duration_ms("audio/wav", &tiny_wav_bytes()),
            Some(1000)
        );
        assert_eq!(
            audio_duration_ms("audio/ogg", &ogg_page(601 * 48_000)),
            Some(601_000)
        );
    }

    #[test]
    fn ffmpeg_command_args_normalize_audio_to_mono_wav() {
        let args = ffmpeg_args(Path::new("/tmp/in.aac"), Path::new("/tmp/out.wav"));
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            args,
            vec![
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-i",
                "/tmp/in.aac",
                "-vn",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-acodec",
                "pcm_s16le",
                "-f",
                "wav",
                "/tmp/out.wav"
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires ffmpeg on PATH or LIGHTSPEED_FFMPEG_PATH"]
    async fn ffmpeg_audio_transcoder_smoke_test() {
        let transcoder = FfmpegAudioTranscoder::from_env();

        let output = transcoder
            .transcode(AudioTranscodeRequest {
                bytes: tiny_wav_bytes(),
                mime: "audio/wav".to_owned(),
                name: "voice.wav".to_owned(),
            })
            .await
            .expect("ffmpeg should transcode tiny wav input");

        assert_eq!(output.mime, "audio/wav");
        assert_eq!(output.name, "voice.wav");
        assert!(output.bytes.starts_with(b"RIFF"));
        assert_eq!(audio_duration_ms(&output.mime, &output.bytes), Some(1000));
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
