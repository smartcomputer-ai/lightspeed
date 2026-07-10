use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use auth::{
    AuthGrantStore, AuthProviderKind, AuthProviderStore, AuthTokenBroker, GitHubAppRuntime,
    GrantRefreshLock, OAuthClientStore, OAuthRefreshRuntime, RegistryTokenBroker, SecretStore,
};
use llm_clients::{LlmApiError, anthropic::messages as anthropic, openai::responses as openai};
use llm_runtime::{ProviderKeyResolver, provider_keys::ProviderKeyError};
use store_pg::PgStore;

use super::*;

const OPENAI_PROVIDER_ID: &str = "openai";
const ANTHROPIC_PROVIDER_ID: &str = "anthropic";
const OPENAI_RESPONSES_API_KIND: &str = "openai:responses";
const ANTHROPIC_MESSAGES_API_KIND: &str = "anthropic:messages";

/// The whole P97 route set. This remains code-local deliberately: provider
/// discovery is direct, not a registry or persisted catalog.
pub(super) struct ModelDiscoveryService {
    openai: Arc<openai::Client>,
    anthropic: Arc<anthropic::Client>,
    provider_keys: Arc<dyn ProviderKeyResolver>,
}

impl ModelDiscoveryService {
    pub(super) fn new(
        openai: Arc<openai::Client>,
        anthropic: Arc<anthropic::Client>,
        provider_keys: Arc<dyn ProviderKeyResolver>,
    ) -> Self {
        Self {
            openai,
            anthropic,
            provider_keys,
        }
    }

    pub(super) async fn list(&self, selectable_only: bool) -> ModelListResponse {
        let (openai, anthropic) = tokio::join!(self.list_openai(), self.list_anthropic());
        let (mut models, providers) = match (openai, anthropic) {
            ((models_a, provider_a), (models_b, provider_b)) => {
                let mut models = models_a;
                models.extend(models_b);
                (models, vec![provider_a, provider_b])
            }
        };
        models.sort_by(|left, right| {
            (
                &left.provider_id,
                &left.api_kind,
                &left.display_name,
                &left.model,
            )
                .cmp(&(
                    &right.provider_id,
                    &right.api_kind,
                    &right.display_name,
                    &right.model,
                ))
        });
        if selectable_only {
            models.retain(|model| {
                model.provider_id != OPENAI_PROVIDER_ID || is_openai_selectable_model(&model.model)
            });
        }
        ModelListResponse { models, providers }
    }

    async fn list_openai(&self) -> (Vec<ModelView>, ModelProviderDiscoveryView) {
        let result = async {
            let auth = self
                .provider_keys
                .resolve_provider_key(OPENAI_PROVIDER_ID)
                .await
                .map_err(DiscoveryError::ProviderKey)?;
            self.openai
                .list_models_with_auth(auth.as_ref().map(|auth| auth.as_request_auth()))
                .await
                .map_err(DiscoveryError::Provider)
        }
        .await;
        match result {
            Ok(response) => {
                let fetched_at_ms = discovery_now_ms();
                let models = response
                    .parsed
                    .data
                    .into_iter()
                    .map(|model| ModelView {
                        provider_id: OPENAI_PROVIDER_ID.to_owned(),
                        api_kind: OPENAI_RESPONSES_API_KIND.to_owned(),
                        display_name: model.id.clone(),
                        model: model.id,
                        capabilities: ModelCapabilitiesView::default(),
                        source: ModelSource::Provider,
                        fetched_at_ms,
                    })
                    .collect();
                (
                    models,
                    provider_success(
                        OPENAI_PROVIDER_ID,
                        &[OPENAI_RESPONSES_API_KIND],
                        fetched_at_ms,
                    ),
                )
            }
            Err(error) => (
                Vec::new(),
                provider_failure(
                    OPENAI_PROVIDER_ID,
                    &[OPENAI_RESPONSES_API_KIND],
                    error.sanitized_message(),
                ),
            ),
        }
    }

    async fn list_anthropic(&self) -> (Vec<ModelView>, ModelProviderDiscoveryView) {
        let result = async {
            let auth = self
                .provider_keys
                .resolve_provider_key(ANTHROPIC_PROVIDER_ID)
                .await
                .map_err(DiscoveryError::ProviderKey)?;
            self.anthropic
                .list_models_with_auth(auth.as_ref().map(|auth| auth.as_request_auth()))
                .await
                .map_err(DiscoveryError::Provider)
        }
        .await;
        match result {
            Ok(models) => {
                let fetched_at_ms = discovery_now_ms();
                let models = models
                    .into_iter()
                    .map(|model| ModelView {
                        provider_id: ANTHROPIC_PROVIDER_ID.to_owned(),
                        api_kind: ANTHROPIC_MESSAGES_API_KIND.to_owned(),
                        display_name: model.display_name.unwrap_or_else(|| model.id.clone()),
                        capabilities: ModelCapabilitiesView {
                            reasoning_efforts: anthropic_reasoning_efforts(
                                model.capabilities.as_ref(),
                            ),
                            parallel_tool_use: None,
                            max_output_tokens: model.max_tokens,
                            max_input_tokens: model.max_input_tokens,
                        },
                        model: model.id,
                        source: ModelSource::Provider,
                        fetched_at_ms,
                    })
                    .collect();
                (
                    models,
                    provider_success(
                        ANTHROPIC_PROVIDER_ID,
                        &[ANTHROPIC_MESSAGES_API_KIND],
                        fetched_at_ms,
                    ),
                )
            }
            Err(error) => (
                Vec::new(),
                provider_failure(
                    ANTHROPIC_PROVIDER_ID,
                    &[ANTHROPIC_MESSAGES_API_KIND],
                    error.sanitized_message(),
                ),
            ),
        }
    }
}

/// `GET /v1/models` does not say which endpoint a model supports. This is
/// intentionally only a small exclusion policy for model-id families that
/// cannot be Lightspeed's text-generation route; it is not a capability
/// catalog or a positive compatibility claim.
fn is_openai_selectable_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    ![
        "text-embedding-",
        "text-moderation-",
        "omni-moderation-",
        "dall-e-",
        "gpt-image-",
        "sora-",
        "whisper-",
        "tts-",
        "gpt-realtime",
        "realtime-",
        "gpt-audio",
        "audio-",
        "gpt-4o-transcribe",
        "gpt-4o-mini-transcribe",
        "gpt-4o-mini-tts",
    ]
    .iter()
    .any(|prefix| model.starts_with(prefix))
}

fn anthropic_reasoning_efforts(
    capabilities: Option<&anthropic::ModelCapabilities>,
) -> Option<Vec<String>> {
    let effort = capabilities?.effort.as_ref()?;
    let efforts = [
        ("low", effort.low.as_ref()),
        ("medium", effort.medium.as_ref()),
        ("high", effort.high.as_ref()),
        ("max", effort.max.as_ref()),
        ("xhigh", effort.xhigh.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, support)| support.filter(|support| support.supported).map(|_| name))
    .map(str::to_owned)
    .collect::<Vec<_>>();
    Some(efforts)
}

fn provider_success(
    provider_id: &str,
    api_kinds: &[&str],
    fetched_at_ms: i64,
) -> ModelProviderDiscoveryView {
    ModelProviderDiscoveryView {
        provider_id: provider_id.to_owned(),
        api_kinds: api_kinds.iter().map(|kind| (*kind).to_owned()).collect(),
        fetched_at_ms: Some(fetched_at_ms),
        error: None,
    }
}

fn discovery_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn provider_failure(
    provider_id: &str,
    api_kinds: &[&str],
    error: String,
) -> ModelProviderDiscoveryView {
    ModelProviderDiscoveryView {
        provider_id: provider_id.to_owned(),
        api_kinds: api_kinds.iter().map(|kind| (*kind).to_owned()).collect(),
        fetched_at_ms: None,
        error: Some(error),
    }
}

enum DiscoveryError {
    ProviderKey(ProviderKeyError),
    Provider(LlmApiError),
}

impl DiscoveryError {
    fn sanitized_message(&self) -> String {
        match self {
            Self::ProviderKey(ProviderKeyError::NotUsable { .. }) => {
                "provider credential is not usable".to_owned()
            }
            Self::ProviderKey(ProviderKeyError::Backend { .. }) => {
                "provider credential lookup failed".to_owned()
            }
            Self::Provider(LlmApiError::Configuration(_)) => {
                "provider credential is not configured".to_owned()
            }
            Self::Provider(LlmApiError::Transport(_)) => "provider request failed".to_owned(),
            Self::Provider(LlmApiError::HttpStatus(error)) => {
                format!("provider returned HTTP {}", error.status)
            }
            Self::Provider(LlmApiError::Decode(_)) => {
                "provider returned an invalid model list".to_owned()
            }
            Self::Provider(LlmApiError::Stream(_))
            | Self::Provider(LlmApiError::Unsupported(_)) => {
                "provider model discovery is unavailable".to_owned()
            }
        }
    }
}

pub(super) fn stored_provider_key_resolver(
    store: Arc<PgStore>,
    token_client: Arc<dyn auth::OAuthTokenClient>,
    github_api: Arc<dyn auth::GitHubApiClient>,
) -> Arc<dyn ProviderKeyResolver> {
    let grants: Arc<dyn AuthGrantStore> = store.clone();
    let secrets: Arc<dyn SecretStore> = store.clone();
    let clients: Arc<dyn OAuthClientStore> = store.clone();
    let providers: Arc<dyn AuthProviderStore> = store.clone();
    let locks: Arc<dyn GrantRefreshLock> = store.clone();
    let broker: Arc<dyn AuthTokenBroker> = Arc::new(
        RegistryTokenBroker::new(grants.clone(), secrets.clone(), locks)
            .with_oauth_refresh(OAuthRefreshRuntime::new(clients, token_client))
            .with_token_source(
                AuthProviderKind::GitHubApp,
                Arc::new(GitHubAppRuntime::new(
                    providers.clone(),
                    github_api,
                    grants,
                    secrets.clone(),
                )),
            ),
    );
    Arc::new(crate::worker::StoredProviderKeyResolver::new(
        providers, secrets, broker,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_effort_normalization_keeps_provider_vocabulary() {
        let capabilities = anthropic::ModelCapabilities {
            effort: Some(anthropic::EffortCapability {
                low: Some(anthropic::CapabilitySupport { supported: true }),
                medium: Some(anthropic::CapabilitySupport { supported: false }),
                high: Some(anthropic::CapabilitySupport { supported: true }),
                max: Some(anthropic::CapabilitySupport { supported: true }),
                xhigh: None,
            }),
        };
        assert_eq!(
            anthropic_reasoning_efforts(Some(&capabilities)),
            Some(vec!["low".to_owned(), "high".to_owned(), "max".to_owned()])
        );
    }

    #[test]
    fn provider_errors_do_not_expose_raw_upstream_messages() {
        let error = DiscoveryError::Provider(LlmApiError::Decode(
            llm_clients::DecodeError::with_raw("invalid", "secret upstream body"),
        ));
        assert_eq!(
            error.sanitized_message(),
            "provider returned an invalid model list"
        );
    }

    #[test]
    fn selectable_policy_removes_only_clearly_non_generation_openai_families() {
        for model in [
            "text-embedding-3-large",
            "omni-moderation-latest",
            "gpt-image-1",
            "sora-2",
            "whisper-1",
            "tts-1",
            "gpt-realtime",
            "gpt-4o-mini-transcribe",
        ] {
            assert!(!is_openai_selectable_model(model), "{model}");
        }
        for model in ["gpt-5", "gpt-4o-mini", "o4-mini", "custom-fine-tune"] {
            assert!(is_openai_selectable_model(model), "{model}");
        }
    }
}
