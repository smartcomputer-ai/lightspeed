#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use llm_clients::{
    ApiResponse, LlmApiError,
    openai::{
        audio as audio_api, completions as completions_api,
        responses::{
            self as responses_api, CompactResponse, CompactResponseRequest,
            CountInputTokensRequest, DeletedResponse, ListInputItemsRequest, Response,
            ResponseItemList, ResponseStream, RetrieveResponseRequest,
        },
    },
};

const MAX_LIVE_ATTEMPTS: usize = 3;

pub fn env_or_dotenv_var(name: &str) -> Result<String, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(env_error) => dotenv_var(name).ok_or(env_error),
    }
}

pub fn required_env_or_dotenv_var(name: &str, missing_message: &str) -> String {
    let value = env_or_dotenv_var(name).expect(missing_message);
    assert!(!value.trim().is_empty(), "{name} is set but empty");
    value
}

pub fn required_first_env_or_dotenv_var(names: &[&str], missing_message: &str) -> String {
    for name in names {
        if let Ok(value) = env_or_dotenv_var(name) {
            assert!(!value.trim().is_empty(), "{name} is set but empty");
            return value;
        }
    }
    panic!("{missing_message}");
}

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

pub fn repo_relative_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root().join(path)
    }
}

fn dotenv_var(name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(repo_root().join(".env")).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        if key.trim() == name {
            return Some(unquote_dotenv_value(value.trim()));
        }
    }
    None
}

fn unquote_dotenv_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

pub async fn openai_responses_create(
    client: &responses_api::Client,
    request: responses_api::CreateResponseRequest,
) -> Result<ApiResponse<Response>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.create(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "responses create")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_responses_stream(
    client: &responses_api::Client,
    request: responses_api::CreateResponseRequest,
) -> Result<ResponseStream, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.stream(request.clone()).await {
            Ok(stream) => return Ok(stream),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "responses stream")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_responses_retrieve(
    client: &responses_api::Client,
    response_id: &str,
    request: RetrieveResponseRequest,
) -> Result<ApiResponse<Response>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.retrieve(response_id, request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "responses retrieve")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_responses_list_input_items(
    client: &responses_api::Client,
    response_id: &str,
    request: ListInputItemsRequest,
) -> Result<ApiResponse<ResponseItemList>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.list_input_items(response_id, request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "responses list_input_items")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_responses_count_input_tokens(
    client: &responses_api::Client,
    request: CountInputTokensRequest,
) -> Result<ApiResponse<responses_api::InputTokens>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.count_input_tokens(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "responses count_input_tokens")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_responses_delete(
    client: &responses_api::Client,
    response_id: &str,
) -> Result<ApiResponse<DeletedResponse>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.delete(response_id).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "responses delete")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_responses_cancel(
    client: &responses_api::Client,
    response_id: &str,
) -> Result<ApiResponse<Response>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.cancel(response_id).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "responses cancel")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_responses_compact(
    client: &responses_api::Client,
    request: CompactResponseRequest,
) -> Result<ApiResponse<CompactResponse>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.compact(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "responses compact")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_completions_create(
    client: &completions_api::Client,
    request: completions_api::CreateCompletionRequest,
) -> Result<ApiResponse<completions_api::Completion>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.create(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "completions create")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_completions_stream(
    client: &completions_api::Client,
    request: completions_api::CreateCompletionRequest,
) -> Result<completions_api::CompletionStream, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.stream(request.clone()).await {
            Ok(stream) => return Ok(stream),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "completions stream")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

pub async fn openai_audio_transcription_create(
    client: &audio_api::Client,
    request: audio_api::CreateTranscriptionRequest,
) -> Result<ApiResponse<audio_api::Transcription>, LlmApiError> {
    let mut attempt = 0;
    loop {
        match client.create_transcription(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry(&error, attempt) => {
                retry(&error, attempt, "audio transcription create")
            }
            Err(error) => return Err(error),
        }
        attempt += 1;
    }
}

fn should_retry(error: &LlmApiError, attempt: usize) -> bool {
    attempt + 1 < MAX_LIVE_ATTEMPTS
        && match error {
            LlmApiError::HttpStatus(error) => error.retryable,
            LlmApiError::Transport(error) => error.retryable,
            LlmApiError::Stream(error) => error.retryable,
            _ => false,
        }
}

fn retry(error: &LlmApiError, attempt: usize, operation: &str) {
    let delay = retry_delay(error, attempt);
    eprintln!(
        "retrying OpenAI live {operation} after retryable error \
         (attempt {}/{}): {error}",
        attempt + 1,
        MAX_LIVE_ATTEMPTS
    );
    thread::sleep(delay);
}

fn retry_delay(error: &LlmApiError, attempt: usize) -> Duration {
    if let LlmApiError::HttpStatus(error) = error
        && let Some(retry_after) = error.retry_after
    {
        return retry_after.min(Duration::from_secs(5));
    }
    Duration::from_millis(750 * (1 << attempt.min(2)))
}
