mod config;
#[allow(unused_imports)]
pub use config::{
    anthropic_messages_live_client, anthropic_messages_live_config, anthropic_messages_live_model,
    deepseek_completions_live_model, env_or_dotenv_var, openai_completions_live_client,
    openai_completions_live_model, openai_responses_live_client, openai_responses_live_model,
};

use std::{sync::Arc, time::Duration};

#[allow(dead_code)]
pub mod caching;
#[allow(dead_code)]
pub mod tool_media;

use async_trait::async_trait;
use engine::{ProviderApiKind, ProviderParams};
use llm_clients::{
    ApiResponse, LlmApiError,
    anthropic::messages::{self as am},
    openai::{
        completions::{Client as CompletionsClient, Completion, CreateCompletionRequest},
        responses::{
            Client, CompactResponse, CompactResponseRequest, CreateResponseRequest, Response,
        },
    },
};
use llm_runtime::{
    AnthropicMessagesApi, AnthropicMessagesParams, OpenAiCompletionsApi, OpenAiCompletionsParams,
    OpenAiResponsesApi, OpenAiResponsesParams,
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
pub fn openai_completions_params(params: &OpenAiCompletionsParams) -> ProviderParams {
    ProviderParams::new(
        ProviderApiKind::OpenAiCompletions,
        serde_json::to_value(params).expect("serialize params"),
    )
}

#[allow(dead_code)]
pub fn retrying_openai_completions_client(
    client: CompletionsClient,
) -> Arc<dyn OpenAiCompletionsApi> {
    Arc::new(RetryingOpenAiCompletionsClient { client })
}

struct RetryingOpenAiCompletionsClient {
    client: CompletionsClient,
}

#[async_trait]
impl OpenAiCompletionsApi for RetryingOpenAiCompletionsClient {
    async fn create(
        &self,
        request: CreateCompletionRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<Completion>, LlmApiError> {
        let mut attempt = 0;
        loop {
            match self
                .client
                .create_with_transport(request.clone(), auth, endpoint)
                .await
            {
                Ok(response) => return Ok(response),
                Err(error) if should_retry(&error, attempt) => {
                    sleep_before_retry(&error, attempt, "openai:completions create");
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
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
        auth: Option<llm_clients::RequestAuth<'_>>,
    ) -> Result<ApiResponse<am::Message>, LlmApiError> {
        let mut attempt = 0;
        loop {
            match self.client.create_with_auth(request.clone(), auth).await {
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
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<Response>, LlmApiError> {
        let mut attempt = 0;
        loop {
            match self
                .client
                .create_with_transport(request.clone(), auth, endpoint)
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
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<CompactResponse>, LlmApiError> {
        let mut attempt = 0;
        loop {
            match self
                .client
                .compact_with_transport(request.clone(), auth, endpoint)
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

#[allow(dead_code)]
pub async fn content_text(
    blobs: &dyn engine::storage::BlobStore,
    content: &engine::ContentRef,
) -> String {
    api_projection::project_content_text(blobs, content)
        .await
        .expect("project assistant content")
        .expect("assistant text")
}
