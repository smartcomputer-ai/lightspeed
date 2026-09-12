use std::{path::PathBuf, sync::Arc, time::Duration};

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
        completions::{
            Client as CompletionsClient, Completion, Config as CompletionsConfig,
            CreateCompletionRequest,
        },
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
pub fn openai_completions_live_model() -> String {
    env_or_dotenv_var("OPENAI_COMPLETIONS_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_owned())
}

#[allow(dead_code)]
pub fn openai_completions_live_client() -> CompletionsClient {
    let api_key = env_or_dotenv_var("OPENAI_COMPLETIONS_API_KEY")
        .or_else(|_| env_or_dotenv_var("OPENAI_API_KEY"))
        .expect(
            "OPENAI_COMPLETIONS_API_KEY or OPENAI_API_KEY must be set in env or root .env to run openai:completions live tests",
        );
    assert!(!api_key.trim().is_empty(), "OpenAI API key is empty");
    let mut config = CompletionsConfig::new(api_key);
    if let Ok(base_url) = env_or_dotenv_var("OPENAI_COMPLETIONS_BASE_URL")
        .or_else(|_| env_or_dotenv_var("OPENAI_BASE_URL"))
    {
        config.base_url = base_url;
    }
    if let Ok(organization) = env_or_dotenv_var("OPENAI_ORG_ID") {
        config.organization = Some(organization);
    }
    if let Ok(project) = env_or_dotenv_var("OPENAI_PROJECT_ID") {
        config.project = Some(project);
    }
    CompletionsClient::new(config).expect("OpenAI Completions client")
}

#[allow(dead_code)]
pub fn deepseek_completions_live_model() -> String {
    env_or_dotenv_var("DEEPSEEK_COMPLETIONS_MODEL").unwrap_or_else(|_| "deepseek-v4-pro".to_owned())
}

#[allow(dead_code)]
pub fn env_or_dotenv_var(name: &str) -> Result<String, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(env_error) => dotenv_var(name).ok_or(env_error),
    }
}

fn dotenv_var(name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(root_dotenv_path()).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == name {
            return Some(unquote_dotenv_value(value.trim()));
        }
    }
    None
}

fn root_dotenv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .join(".env")
}

fn unquote_dotenv_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_owned();
        }
    }
    value.to_owned()
}

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
