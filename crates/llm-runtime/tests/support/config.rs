//! Provider setup shared by live suites; pure builders also support offline checks.
#![allow(dead_code)]

use llm_clients::{
    anthropic::messages as am,
    openai::{
        completions::{Client as CompletionsClient, Config as CompletionsConfig},
        responses as oai,
    },
};
use std::{env::VarError, path::PathBuf};

pub fn openai_completions_live_model() -> String {
    env_or_dotenv_var("OPENAI_COMPLETIONS_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_owned())
}

pub fn openai_completions_live_client() -> CompletionsClient {
    CompletionsClient::new(openai_completions_config_with(env_or_dotenv_var))
        .expect("OpenAI Completions client")
}

pub(super) fn openai_completions_config_with(
    lookup: impl Fn(&str) -> Result<String, VarError>,
) -> CompletionsConfig {
    let api_key = lookup("OPENAI_COMPLETIONS_API_KEY")
        .or_else(|_| lookup("OPENAI_API_KEY"))
        .expect(
            "OPENAI_COMPLETIONS_API_KEY or OPENAI_API_KEY must be set in env or root .env to run openai:completions live tests",
        );
    assert!(!api_key.trim().is_empty(), "OpenAI API key is empty");
    let mut config = CompletionsConfig::new(api_key);
    if let Ok(base_url) =
        lookup("OPENAI_COMPLETIONS_BASE_URL").or_else(|_| lookup("OPENAI_BASE_URL"))
    {
        config.base_url = base_url;
    }
    if let Ok(organization) = lookup("OPENAI_ORG_ID") {
        config.organization = Some(organization);
    }
    if let Ok(project) = lookup("OPENAI_PROJECT_ID") {
        config.project = Some(project);
    }
    config
}

pub fn deepseek_completions_live_model() -> String {
    env_or_dotenv_var("DEEPSEEK_COMPLETIONS_MODEL").unwrap_or_else(|_| "deepseek-v4-pro".to_owned())
}

pub fn env_or_dotenv_var(name: &str) -> Result<String, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(env_error) => dotenv_var(name).ok_or(env_error),
    }
}

fn dotenv_var(name: &str) -> Option<String> {
    let contents = std::fs::read_to_string(root_dotenv_path()).ok()?;
    dotenv_value(&contents, name)
}

pub(super) fn dotenv_value(contents: &str, name: &str) -> Option<String> {
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

pub fn openai_responses_live_model() -> String {
    env_or_dotenv_var("OPENAI_RESPONSES_MODEL")
        .or_else(|_| env_or_dotenv_var("OPENAI_LIVE_MODEL"))
        .unwrap_or_else(|_| "gpt-5.5".to_string())
}

pub fn openai_responses_live_client() -> oai::Client {
    oai::Client::new(openai_responses_config_with(env_or_dotenv_var))
        .expect("OpenAI Responses client")
}

pub(super) fn openai_responses_config_with(
    lookup: impl Fn(&str) -> Result<String, VarError>,
) -> oai::Config {
    let api_key = lookup("OPENAI_API_KEY").expect(
        "OPENAI_API_KEY must be set in env or root .env to run llm-runtime openai:responses live tests",
    );
    assert!(
        !api_key.trim().is_empty(),
        "OPENAI_API_KEY is set but empty"
    );

    let mut config = oai::Config::new(api_key);
    if let Ok(base_url) = lookup("OPENAI_BASE_URL") {
        config.base_url = base_url;
    }
    if let Ok(org_id) = lookup("OPENAI_ORG_ID") {
        config.organization = Some(org_id);
    }
    if let Ok(project) = lookup("OPENAI_PROJECT_ID") {
        config.project = Some(project);
    }

    config
}

pub fn anthropic_messages_live_model() -> String {
    env_or_dotenv_var("ANTHROPIC_MESSAGES_MODEL")
        .or_else(|_| env_or_dotenv_var("ANTHROPIC_LIVE_MODEL"))
        .unwrap_or_else(|_| "claude-opus-5".to_string())
}

pub fn anthropic_messages_live_client() -> am::Client {
    am::Client::new(anthropic_messages_live_config()).expect("Anthropic Messages client")
}

pub fn anthropic_messages_live_config() -> am::Config {
    anthropic_messages_config_with(env_or_dotenv_var)
}

pub(super) fn anthropic_messages_config_with(
    lookup: impl Fn(&str) -> Result<String, VarError>,
) -> am::Config {
    let api_key = lookup("ANTHROPIC_API_KEY").expect(
        "ANTHROPIC_API_KEY must be set in env or root .env to run llm-runtime anthropic:messages live tests",
    );
    assert!(
        !api_key.trim().is_empty(),
        "ANTHROPIC_API_KEY is set but empty"
    );

    let mut config = am::Config::new(api_key);
    if let Ok(base_url) = lookup("ANTHROPIC_BASE_URL") {
        config.base_url = base_url;
    }
    config
}
