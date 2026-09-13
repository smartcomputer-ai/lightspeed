//! Configuration policy shared by the native OpenAI clients.

use crate::error::ConfigurationError;
use reqwest::header::{HeaderMap, HeaderValue};

pub(super) fn apply_env_overrides(
    base_url: &mut String,
    organization: &mut Option<String>,
    project: &mut Option<String>,
    mut lookup: impl FnMut(&str) -> Option<String>,
) {
    if let Some(value) = lookup("OPENAI_BASE_URL") {
        *base_url = value;
    }
    if let Some(value) = lookup("OPENAI_ORG_ID") {
        *organization = Some(value);
    }
    if let Some(value) = lookup("OPENAI_PROJECT_ID") {
        *project = Some(value);
    }
}

pub(super) fn default_headers(
    organization: Option<&str>,
    project: Option<&str>,
) -> Result<HeaderMap, ConfigurationError> {
    let mut headers = HeaderMap::new();
    if let Some(organization) = organization {
        headers.insert(
            "OpenAI-Organization",
            HeaderValue::from_str(organization).map_err(|err| {
                ConfigurationError::new(format!("invalid OpenAI organization header: {err}"))
            })?,
        );
    }
    if let Some(project) = project {
        headers.insert(
            "OpenAI-Project",
            HeaderValue::from_str(project).map_err(|err| {
                ConfigurationError::new(format!("invalid OpenAI project header: {err}"))
            })?,
        );
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_overrides_preserve_missing_values_and_apply_empty_values() {
        let mut base_url = "https://configured.example/v1".to_owned();
        let mut organization = Some("configured-org".to_owned());
        let mut project = Some("configured-project".to_owned());
        apply_env_overrides(
            &mut base_url,
            &mut organization,
            &mut project,
            |name| match name {
                "OPENAI_BASE_URL" => Some("https://override.example/v1".to_owned()),
                "OPENAI_PROJECT_ID" => Some(String::new()),
                _ => None,
            },
        );
        assert_eq!(base_url, "https://override.example/v1");
        assert_eq!(organization.as_deref(), Some("configured-org"));
        assert_eq!(project.as_deref(), Some(""));

        apply_env_overrides(
            &mut base_url,
            &mut organization,
            &mut project,
            |name| match name {
                "OPENAI_BASE_URL" => Some(String::new()),
                "OPENAI_ORG_ID" => Some("override-org".to_owned()),
                _ => None,
            },
        );
        assert_eq!(base_url, "");
        assert_eq!(organization.as_deref(), Some("override-org"));
        assert_eq!(project.as_deref(), Some(""));
    }

    #[test]
    fn optional_headers_preserve_empty_values_without_adding_auth_or_content_type() {
        assert!(default_headers(None, None).unwrap().is_empty());
        let headers = default_headers(Some(""), Some("project-test")).unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers["openai-organization"], "");
        assert_eq!(headers["openai-project"], "project-test");
    }

    #[test]
    fn invalid_headers_report_the_field_and_preserve_validation_order() {
        for (organization, project, field) in [
            (Some("bad\norg"), Some("bad\nproject"), "organization"),
            (Some("valid-org"), Some("bad\nproject"), "project"),
        ] {
            let error: ConfigurationError = default_headers(organization, project).unwrap_err();
            assert_eq!(
                error.message,
                format!("invalid OpenAI {field} header: failed to parse header value")
            );
        }
    }
}
