use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use engine::{ProviderApiKind, ProviderParams};
use llm_clients::{
    ApiResponse, LlmApiError,
    anthropic::messages::{self as am},
    openai::responses::{
        Client, CompactResponse, CompactResponseRequest, CreateResponseRequest, Response,
    },
};
use llm_runtime::{
    AnthropicMessagesApi, AnthropicMessagesParams, OpenAiResponsesApi, OpenAiResponsesParams,
};

const MAX_LIVE_ATTEMPTS: usize = 3;

#[allow(dead_code)]
pub fn openai_params(params: &OpenAiResponsesParams) -> ProviderParams {
    ProviderParams::new(
        ProviderApiKind::OpenAiResponses,
        serde_json::to_value(params).expect("serialize params"),
    )
}

#[allow(dead_code)]
pub fn anthropic_params(params: &AnthropicMessagesParams) -> ProviderParams {
    ProviderParams::new(
        ProviderApiKind::AnthropicMessages,
        serde_json::to_value(params).expect("serialize params"),
    )
}

#[allow(dead_code)]
pub fn retrying_anthropic_messages_client(client: am::Client) -> Arc<dyn AnthropicMessagesApi> {
    Arc::new(RetryingAnthropicMessagesClient { client })
}

struct RetryingAnthropicMessagesClient {
    client: am::Client,
}

#[async_trait]
impl AnthropicMessagesApi for RetryingAnthropicMessagesClient {
    async fn create(
        &self,
        request: am::CreateMessageRequest,
        api_key: Option<&str>,
    ) -> Result<ApiResponse<am::Message>, LlmApiError> {
        let mut attempt = 0;
        loop {
            match self
                .client
                .create_with_api_key(request.clone(), api_key)
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) if should_retry(&error, attempt) => {
                    sleep_before_retry(&error, attempt, "anthropic:messages create");
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[allow(dead_code)]
pub fn retrying_openai_responses_client(client: Client) -> Arc<dyn OpenAiResponsesApi> {
    Arc::new(RetryingOpenAiResponsesClient { client })
}

struct RetryingOpenAiResponsesClient {
    client: Client,
}

#[async_trait]
impl OpenAiResponsesApi for RetryingOpenAiResponsesClient {
    async fn create(
        &self,
        request: CreateResponseRequest,
        api_key: Option<&str>,
    ) -> Result<ApiResponse<Response>, LlmApiError> {
        let mut attempt = 0;
        loop {
            match self
                .client
                .create_with_api_key(request.clone(), api_key)
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) if should_retry(&error, attempt) => {
                    sleep_before_retry(&error, attempt, "openai:responses create");
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn compact(
        &self,
        request: CompactResponseRequest,
        api_key: Option<&str>,
    ) -> Result<ApiResponse<CompactResponse>, LlmApiError> {
        let mut attempt = 0;
        loop {
            match self
                .client
                .compact_with_api_key(request.clone(), api_key)
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) if should_retry(&error, attempt) => {
                    sleep_before_retry(&error, attempt, "openai:responses compact");
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
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

fn sleep_before_retry(error: &LlmApiError, attempt: usize, operation: &str) {
    let delay = retry_delay(error, attempt);
    eprintln!(
        "retrying live {operation} after retryable error (attempt {}/{}): {error}",
        attempt + 1,
        MAX_LIVE_ATTEMPTS
    );
    std::thread::sleep(delay);
}

fn retry_delay(error: &LlmApiError, attempt: usize) -> Duration {
    if let LlmApiError::HttpStatus(error) = error
        && let Some(retry_after) = error.retry_after
    {
        return retry_after.min(Duration::from_secs(5));
    }
    Duration::from_millis(750 * (1 << attempt.min(2)))
}
