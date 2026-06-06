#![allow(dead_code)]

use std::{thread, time::Duration};

use llm_clients::{
    ApiResponse, LlmApiError,
    openai::{
        completions as completions_api,
        responses::{
            self as responses_api, CompactResponse, CompactResponseRequest,
            CountInputTokensRequest, DeletedResponse, ListInputItemsRequest, Response,
            ResponseItemList, ResponseStream, RetrieveResponseRequest,
        },
    },
};

const MAX_LIVE_ATTEMPTS: usize = 3;

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
