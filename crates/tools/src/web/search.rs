//! OpenAI Responses hosted web search tool builder.

use engine::{
    OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE, OpenAiResponsesRequestDefaults, ProviderApiKind,
    ProviderNativeToolExecution, ProviderNativeToolSpec, ToolKind, ToolName, ToolParallelism,
    ToolSpec, ToolTargetRequirement,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    error::{ToolError, ToolResult},
    runtime::{ToolDocument, ToolSpecBundle},
};

pub const WEB_SEARCH_TOOL_NAME: &str = "web_search";

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenAiResponsesWebSearchConfig {
    #[serde(default)]
    pub mode: WebSearchMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_context_size: Option<WebSearchContextSize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_location: Option<OpenAiApproximateUserLocation>,
    #[serde(default)]
    pub include_sources: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchMode {
    #[default]
    Disabled,
    Cached,
    Live,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchContextSize {
    Low,
    Medium,
    High,
}

impl WebSearchContextSize {
    fn as_openai_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenAiApproximateUserLocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

impl OpenAiResponsesWebSearchConfig {
    pub fn cached() -> Self {
        Self {
            mode: WebSearchMode::Cached,
            ..Self::default()
        }
    }

    pub fn live() -> Self {
        Self {
            mode: WebSearchMode::Live,
            ..Self::default()
        }
    }

    pub fn enabled(&self) -> bool {
        self.mode != WebSearchMode::Disabled
    }

    pub fn validate(&self) -> ToolResult<()> {
        validate_domains("allowed_domains", &self.allowed_domains)?;
        validate_domains("blocked_domains", &self.blocked_domains)?;
        if let Some(location) = &self.user_location {
            location.validate()?;
        }
        Ok(())
    }

    pub fn native_tool_json(&self) -> ToolResult<Option<Value>> {
        self.validate()?;
        let Some(external_web_access) = self.mode.external_web_access() else {
            return Ok(None);
        };

        let mut tool = Map::new();
        tool.insert("type".to_string(), json!("web_search"));
        tool.insert(
            "external_web_access".to_string(),
            json!(external_web_access),
        );
        if let Some(search_context_size) = self.search_context_size {
            tool.insert(
                "search_context_size".to_string(),
                json!(search_context_size.as_openai_str()),
            );
        }
        let filters = self.filters_json();
        if !filters.is_empty() {
            tool.insert("filters".to_string(), Value::Object(filters));
        }
        if let Some(location) = &self.user_location {
            tool.insert("user_location".to_string(), location.to_openai_json());
        }
        Ok(Some(Value::Object(tool)))
    }

    fn filters_json(&self) -> Map<String, Value> {
        let mut filters = Map::new();
        if !self.allowed_domains.is_empty() {
            filters.insert("allowed_domains".to_string(), json!(self.allowed_domains));
        }
        if !self.blocked_domains.is_empty() {
            filters.insert("blocked_domains".to_string(), json!(self.blocked_domains));
        }
        filters
    }
}

impl WebSearchMode {
    fn external_web_access(self) -> Option<bool> {
        match self {
            Self::Disabled => None,
            Self::Cached => Some(false),
            Self::Live => Some(true),
        }
    }
}

impl OpenAiApproximateUserLocation {
    pub fn validate(&self) -> ToolResult<()> {
        if let Some(country) = &self.country {
            let valid = country.len() == 2 && country.chars().all(|ch| ch.is_ascii_alphabetic());
            if !valid {
                return Err(invalid_request(
                    "user_location.country must be a two-letter country code",
                ));
            }
        }
        Ok(())
    }

    fn to_openai_json(&self) -> Value {
        let mut location = Map::new();
        location.insert("type".to_string(), json!("approximate"));
        if let Some(country) = &self.country {
            location.insert("country".to_string(), json!(country));
        }
        if let Some(city) = &self.city {
            location.insert("city".to_string(), json!(city));
        }
        if let Some(region) = &self.region {
            location.insert("region".to_string(), json!(region));
        }
        if let Some(timezone) = &self.timezone {
            location.insert("timezone".to_string(), json!(timezone));
        }
        Value::Object(location)
    }
}

pub fn openai_responses_web_search_tool_bundle(
    config: &OpenAiResponsesWebSearchConfig,
) -> ToolResult<Option<ToolSpecBundle>> {
    let Some(native_tool) = config.native_tool_json()? else {
        return Ok(None);
    };
    let native_tool = ToolDocument::text(
        "application/json",
        serde_json::to_string(&native_tool).map_err(|error| ToolError::InvalidRequest {
            message: format!("failed to encode OpenAI Responses web search tool: {error}"),
        })?,
    );
    Ok(Some(ToolSpecBundle {
        spec: ToolSpec {
            name: ToolName::new(WEB_SEARCH_TOOL_NAME),
            kind: ToolKind::ProviderNative(ProviderNativeToolSpec {
                api_kind: ProviderApiKind::OpenAiResponses,
                native_tool_ref: native_tool.blob_ref.clone(),
                execution: ProviderNativeToolExecution::ProviderHosted,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: ToolTargetRequirement::None,
        },
        documents: vec![native_tool],
    }))
}

pub fn apply_openai_responses_web_search_defaults(
    defaults: &mut OpenAiResponsesRequestDefaults,
    config: &OpenAiResponsesWebSearchConfig,
) {
    if config.enabled() && config.include_sources {
        ensure_include(
            &mut defaults.include,
            OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE,
        );
    }
}

fn ensure_include(include: &mut Vec<String>, value: &str) {
    if !include.iter().any(|existing| existing == value) {
        include.push(value.to_string());
    }
}

fn validate_domains(label: &'static str, domains: &[String]) -> ToolResult<()> {
    if domains.len() > 100 {
        return Err(invalid_request(format!(
            "{label} supports at most 100 domains"
        )));
    }
    for domain in domains {
        validate_domain(label, domain)?;
    }
    Ok(())
}

fn validate_domain(label: &'static str, domain: &str) -> ToolResult<()> {
    if domain.is_empty() {
        return Err(invalid_request(format!(
            "{label} must not contain empty domains"
        )));
    }
    if domain.contains("://") || domain.contains('/') {
        return Err(invalid_request(format!(
            "{label} domain {domain:?} must omit scheme and path"
        )));
    }
    if domain.len() > 253 {
        return Err(invalid_request(format!(
            "{label} domain {domain:?} is too long"
        )));
    }
    let valid = domain
        .split('.')
        .all(|part| !part.is_empty() && part.chars().all(valid_domain_char));
    if !valid {
        return Err(invalid_request(format!(
            "{label} domain {domain:?} contains invalid characters"
        )));
    }
    Ok(())
}

fn valid_domain_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '-'
}

fn invalid_request(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_web_search_has_no_tool() {
        let bundle =
            openai_responses_web_search_tool_bundle(&OpenAiResponsesWebSearchConfig::default())
                .expect("bundle");

        assert!(bundle.is_none());
    }

    #[test]
    fn cached_web_search_builds_provider_native_tool() {
        let bundle = openai_responses_web_search_tool_bundle(&OpenAiResponsesWebSearchConfig {
            mode: WebSearchMode::Cached,
            search_context_size: Some(WebSearchContextSize::Medium),
            allowed_domains: vec!["docs.rs".to_string()],
            blocked_domains: vec!["reddit.com".to_string()],
            user_location: Some(OpenAiApproximateUserLocation {
                country: Some("US".to_string()),
                city: None,
                region: None,
                timezone: None,
            }),
            include_sources: true,
        })
        .expect("bundle")
        .expect("enabled");

        assert_eq!(bundle.spec.name.as_str(), WEB_SEARCH_TOOL_NAME);
        assert_eq!(bundle.spec.parallelism, ToolParallelism::ParallelSafe);
        assert_eq!(bundle.spec.target_requirement, ToolTargetRequirement::None);
        let ToolKind::ProviderNative(native) = &bundle.spec.kind else {
            panic!("expected provider-native tool");
        };
        assert_eq!(native.api_kind, ProviderApiKind::OpenAiResponses);
        assert_eq!(
            native.execution,
            ProviderNativeToolExecution::ProviderHosted
        );
        assert_eq!(native.native_tool_ref, bundle.documents[0].blob_ref);
        let native_tool: Value =
            serde_json::from_slice(&bundle.documents[0].bytes).expect("native tool");
        assert_eq!(
            native_tool,
            json!({
                "type": "web_search",
                "external_web_access": false,
                "search_context_size": "medium",
                "filters": {
                    "allowed_domains": ["docs.rs"],
                    "blocked_domains": ["reddit.com"]
                },
                "user_location": {
                    "type": "approximate",
                    "country": "US"
                }
            })
        );
    }

    #[test]
    fn live_web_search_sets_external_access_true() {
        let native_tool = OpenAiResponsesWebSearchConfig::live()
            .native_tool_json()
            .expect("tool json")
            .expect("enabled");

        assert_eq!(native_tool["external_web_access"], json!(true));
    }

    #[test]
    fn include_sources_preserves_existing_defaults() {
        let mut defaults = OpenAiResponsesRequestDefaults::default();
        apply_openai_responses_web_search_defaults(
            &mut defaults,
            &OpenAiResponsesWebSearchConfig {
                mode: WebSearchMode::Live,
                include_sources: true,
                ..OpenAiResponsesWebSearchConfig::default()
            },
        );
        apply_openai_responses_web_search_defaults(
            &mut defaults,
            &OpenAiResponsesWebSearchConfig {
                mode: WebSearchMode::Live,
                include_sources: true,
                ..OpenAiResponsesWebSearchConfig::default()
            },
        );

        assert!(defaults.include.iter().any(|value| {
            value == engine::OPENAI_RESPONSES_REASONING_ENCRYPTED_CONTENT_INCLUDE
        }));
        assert_eq!(
            defaults
                .include
                .iter()
                .filter(|value| *value == OPENAI_RESPONSES_WEB_SEARCH_SOURCES_INCLUDE)
                .count(),
            1
        );
    }

    #[test]
    fn domain_filters_reject_schemes_and_paths() {
        let err = OpenAiResponsesWebSearchConfig {
            mode: WebSearchMode::Cached,
            allowed_domains: vec!["https://example.com/path".to_string()],
            ..OpenAiResponsesWebSearchConfig::default()
        }
        .native_tool_json()
        .expect_err("invalid domain");

        assert!(err.to_string().contains("must omit scheme and path"));
    }

    #[test]
    fn user_location_rejects_invalid_country_code() {
        let err = OpenAiResponsesWebSearchConfig {
            mode: WebSearchMode::Cached,
            user_location: Some(OpenAiApproximateUserLocation {
                country: Some("USA".to_string()),
                city: None,
                region: None,
                timezone: None,
            }),
            ..OpenAiResponsesWebSearchConfig::default()
        }
        .native_tool_json()
        .expect_err("invalid country");

        assert!(err.to_string().contains("two-letter country code"));
    }
}
