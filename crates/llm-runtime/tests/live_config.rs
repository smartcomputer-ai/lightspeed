//! Offline checks for shared live-test setup; never read credentials or call providers.
#[path = "support/config.rs"]
mod config;

use std::env::VarError;

fn lookup<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Result<String, VarError> + 'a {
    move |name| {
        values
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
            .ok_or(VarError::NotPresent)
    }
}

#[test]
fn dotenv_lookup_skips_malformed_lines_and_preserves_values() {
    let text = "# comment\n\nnot an assignment\n KEY = 'a=b'\nKEY=second\nEMPTY=\n";
    assert_eq!(config::dotenv_value(text, "KEY").as_deref(), Some("a=b"));
    assert_eq!(config::dotenv_value(text, "EMPTY").as_deref(), Some(""));
    assert_eq!(config::dotenv_value(text, "MISSING"), None);
}

#[test]
fn dotenv_lookup_removes_only_matching_outer_quotes() {
    for (input, expected) in [
        ("KEY=\"a'b\"", "a'b"),
        ("KEY='a\"b'", "a\"b"),
        ("KEY=\"unterminated", "\"unterminated"),
        ("KEY=plain'", "plain'"),
        ("KEY='\"nested\"'", "\"nested\""),
    ] {
        assert_eq!(
            config::dotenv_value(input, "KEY").as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn completions_specific_credentials_and_endpoint_take_precedence() {
    let config = config::openai_completions_config_with(lookup(&[
        ("OPENAI_API_KEY", "general"),
        ("OPENAI_COMPLETIONS_API_KEY", "specific"),
        ("OPENAI_BASE_URL", "https://general.example/v1"),
        ("OPENAI_COMPLETIONS_BASE_URL", "https://specific.example/v1"),
        ("OPENAI_ORG_ID", "org"),
        ("OPENAI_PROJECT_ID", "project"),
    ]));
    assert_eq!(config.api_key.as_deref(), Some("specific"));
    assert_eq!(config.base_url, "https://specific.example/v1");
    assert_eq!(config.organization.as_deref(), Some("org"));
    assert_eq!(config.project.as_deref(), Some("project"));
}

#[test]
fn completions_falls_back_to_shared_openai_settings() {
    let config = config::openai_completions_config_with(lookup(&[
        ("OPENAI_API_KEY", "general"),
        ("OPENAI_BASE_URL", "https://general.example/v1"),
    ]));
    assert_eq!(config.api_key.as_deref(), Some("general"));
    assert_eq!(config.base_url, "https://general.example/v1");
}

#[test]
fn responses_uses_shared_openai_settings_even_when_completions_overrides_exist() {
    let config = config::openai_responses_config_with(lookup(&[
        ("OPENAI_API_KEY", "general"),
        ("OPENAI_COMPLETIONS_API_KEY", "specific"),
        ("OPENAI_BASE_URL", "https://general.example/v1"),
        ("OPENAI_COMPLETIONS_BASE_URL", "https://specific.example/v1"),
        ("OPENAI_ORG_ID", "org"),
        ("OPENAI_PROJECT_ID", "project"),
    ]));
    assert_eq!(config.api_key.as_deref(), Some("general"));
    assert_eq!(config.base_url, "https://general.example/v1");
    assert_eq!(config.organization.as_deref(), Some("org"));
    assert_eq!(config.project.as_deref(), Some("project"));
}

#[test]
fn anthropic_preserves_endpoint_override_and_leaves_beta_selection_to_the_suite() {
    let config = config::anthropic_messages_config_with(lookup(&[
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("ANTHROPIC_BASE_URL", "https://anthropic.example"),
    ]));
    assert_eq!(config.base_url, "https://anthropic.example");
    assert!(config.beta_headers.is_empty());
}

#[test]
#[should_panic(expected = "OPENAI_API_KEY is set but empty")]
fn empty_credentials_fail_instead_of_silently_skipping_live_tests() {
    config::openai_responses_config_with(lookup(&[("OPENAI_API_KEY", "  ")]));
}
