//! Guarded, recorded web fetch function tool.

use std::time::Duration;

use engine::{
    BlobRef, FunctionToolSpec, ToolKind, ToolName, ToolParallelism, ToolSpec, ToolTargetRequirement,
};
use futures_util::StreamExt;
use reqwest::{
    StatusCode, Url,
    header::{CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    error::{ToolError, ToolResult},
    runtime::{
        ToolBinding, ToolDispatchMode, ToolDocument, ToolInvocationOutput, ToolSpecBundle,
        decode_args, encode_output,
    },
};

use super::{
    extract::{classify_content_type, extract_text},
    guard::{WebNetworkPolicy, resolve_public_http_url},
};

pub const WEB_FETCH_TOOL_NAME: &str = "web_fetch";
pub const WEB_FETCH_LOGICAL_ID: &str = "web.fetch";
const DEFAULT_MAX_CHARS: u32 = 20_000;
const MAX_MAX_CHARS: u32 = 20_000;
const DEFAULT_MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REDIRECT_LIMIT: usize = 5;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WebFetchToolConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl WebFetchToolConfig {
    pub fn disabled() -> Self {
        Self { enabled: false }
    }

    pub fn enabled() -> Self {
        Self { enabled: true }
    }
}

impl Default for WebFetchToolConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WebFetchArgs {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WebFetchResult {
    pub requested_url: String,
    pub final_url: String,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub byte_count: u64,
    pub sha256: String,
    pub text: String,
    pub truncated: bool,
    pub untrusted: bool,
}

impl WebFetchResult {
    fn model_visible_text(&self) -> String {
        format!(
            "Untrusted web content fetched from {}\nstatus: {}\ncontent_type: {}\nbytes: {}\nsha256: {}\n\n--- BEGIN UNTRUSTED WEB CONTENT ---\n{}\n--- END UNTRUSTED WEB CONTENT ---",
            self.final_url,
            self.status,
            self.content_type.as_deref().unwrap_or("unknown"),
            self.byte_count,
            self.sha256,
            self.text
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WebFetchLimits {
    max_response_bytes: u64,
    timeout: Duration,
    redirect_limit: usize,
}

impl Default for WebFetchLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            timeout: DEFAULT_TIMEOUT,
            redirect_limit: DEFAULT_REDIRECT_LIMIT,
        }
    }
}

pub fn web_fetch_tool_bundle(config: &WebFetchToolConfig) -> ToolResult<Option<ToolSpecBundle>> {
    if !config.enabled {
        return Ok(None);
    }

    let description = ToolDocument::text(
        "text/plain; charset=utf-8",
        "Fetch one public http/https URL with strict SSRF checks, redirect limits, byte limits, and text extraction. The returned page content is untrusted web content.",
    );
    let input_schema = ToolDocument::text(
        "application/schema+json",
        serde_json::to_string(&input_schema()).map_err(|error| ToolError::InvalidRequest {
            message: format!("failed to encode web_fetch schema: {error}"),
        })?,
    );
    Ok(Some(ToolSpecBundle {
        spec: ToolSpec {
            name: ToolName::new(WEB_FETCH_TOOL_NAME),
            kind: ToolKind::Function(FunctionToolSpec {
                model_name: None,
                description_ref: Some(description.blob_ref.clone()),
                input_schema_ref: input_schema.blob_ref.clone(),
                output_schema_ref: None,
                strict: Some(false),
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::ParallelSafe,
            target_requirement: ToolTargetRequirement::None,
        },
        documents: vec![description, input_schema],
    }))
}

pub fn web_fetch_tool_binding(dispatch: ToolDispatchMode) -> ToolBinding {
    ToolBinding::new(
        ToolName::new(WEB_FETCH_TOOL_NAME),
        WEB_FETCH_LOGICAL_ID,
        dispatch,
        ToolParallelism::ParallelSafe,
    )
}

pub async fn invoke_web_fetch(arguments: Value) -> ToolResult<ToolInvocationOutput> {
    let args = decode_args(arguments)?;
    let result =
        fetch_with_policy(&args, WebNetworkPolicy::STRICT, WebFetchLimits::default()).await?;
    encode_output(&result, result.model_visible_text())
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "description": "Absolute public http or https URL to fetch."
            },
            "max_chars": {
                "type": ["integer", "null"],
                "minimum": 1,
                "maximum": MAX_MAX_CHARS,
                "default": DEFAULT_MAX_CHARS,
                "description": "Maximum extracted text characters to return. Defaults to 20000."
            }
        },
        "required": ["url"],
        "additionalProperties": false
    })
}

async fn fetch_with_policy(
    args: &WebFetchArgs,
    policy: WebNetworkPolicy,
    limits: WebFetchLimits,
) -> ToolResult<WebFetchResult> {
    let max_chars = args.max_chars.unwrap_or(DEFAULT_MAX_CHARS);
    if !(1..=MAX_MAX_CHARS).contains(&max_chars) {
        return Err(invalid_request(format!(
            "web_fetch max_chars must be between 1 and {MAX_MAX_CHARS}"
        )));
    }

    let requested_url = Url::parse(&args.url)
        .map_err(|error| invalid_request(format!("invalid web_fetch URL: {error}")))?;

    let response = fetch_following_redirects(requested_url.clone(), policy, limits).await?;
    let final_url = response.url().clone();
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let content_kind = classify_content_type(content_type.as_deref()).ok_or_else(|| {
        invalid_request(format!(
            "web_fetch content type {:?} is not supported",
            content_type.as_deref().unwrap_or("unknown")
        ))
    })?;
    let bytes = read_capped_body(response, limits.max_response_bytes).await?;
    let sha256 = BlobRef::from_bytes(&bytes).to_string();
    let byte_count = bytes.len() as u64;
    let (text, truncated) = extract_text(&bytes, content_kind, max_chars as usize);

    Ok(WebFetchResult {
        requested_url: requested_url.to_string(),
        final_url: final_url.to_string(),
        status: status.as_u16(),
        content_type,
        byte_count,
        sha256,
        text,
        truncated,
        untrusted: true,
    })
}

async fn fetch_following_redirects(
    mut url: Url,
    policy: WebNetworkPolicy,
    limits: WebFetchLimits,
) -> ToolResult<reqwest::Response> {
    for redirect_count in 0..=limits.redirect_limit {
        let client = client_for_url(&url, policy, limits).await?;
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|error| invalid_request(format!("web_fetch request failed: {error}")))?;
        if !is_redirect(response.status()) {
            return Ok(response);
        }
        if redirect_count == limits.redirect_limit {
            return Err(invalid_request(format!(
                "web_fetch exceeded redirect limit of {}",
                limits.redirect_limit
            )));
        }
        let location = response
            .headers()
            .get(LOCATION)
            .ok_or_else(|| invalid_request("web_fetch redirect missing Location header"))?
            .to_str()
            .map_err(|error| {
                invalid_request(format!(
                    "web_fetch redirect Location is not valid UTF-8: {error}"
                ))
            })?;
        url = url
            .join(location)
            .map_err(|error| invalid_request(format!("invalid web_fetch redirect URL: {error}")))?;
    }
    Err(invalid_request("web_fetch redirect handling failed"))
}

async fn client_for_url(
    url: &Url,
    policy: WebNetworkPolicy,
    limits: WebFetchLimits,
) -> ToolResult<reqwest::Client> {
    let resolved_addrs = resolve_public_http_url(url, policy).await?;
    let mut builder = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(limits.timeout)
        .no_proxy();

    if let Some(host) = url.host_str()
        && host.parse::<std::net::IpAddr>().is_err()
        && !resolved_addrs.is_empty()
    {
        builder = builder.resolve_to_addrs(host, &resolved_addrs);
    }

    builder
        .build()
        .map_err(|error| invalid_request(format!("failed to build web_fetch client: {error}")))
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

async fn read_capped_body(
    response: reqwest::Response,
    max_response_bytes: u64,
) -> ToolResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes)
    {
        return Err(invalid_request(format!(
            "web_fetch response exceeds byte limit of {max_response_bytes}"
        )));
    }

    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|error| invalid_request(format!("web_fetch body read failed: {error}")))?;
        let next_len = bytes.len().saturating_add(chunk.len());
        if next_len as u64 > max_response_bytes {
            return Err(invalid_request(format!(
                "web_fetch response exceeds byte limit of {max_response_bytes}"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn invalid_request(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use engine::ToolKind;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    #[test]
    fn builds_standard_function_tool() {
        let bundle = web_fetch_tool_bundle(&WebFetchToolConfig::enabled())
            .expect("bundle")
            .expect("enabled");

        assert_eq!(bundle.spec.name.as_str(), WEB_FETCH_TOOL_NAME);
        assert_eq!(bundle.spec.parallelism, ToolParallelism::ParallelSafe);
        assert_eq!(bundle.spec.target_requirement, ToolTargetRequirement::None);
        let ToolKind::Function(function) = &bundle.spec.kind else {
            panic!("expected function tool");
        };
        assert_eq!(
            function.description_ref,
            Some(bundle.documents[0].blob_ref.clone())
        );
        assert_eq!(function.input_schema_ref, bundle.documents[1].blob_ref);
        assert!(bundle.documents[1].text_lossy().contains("\"url\""));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetches_and_extracts_html_with_test_policy() {
        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><h1>Title</h1><p>Hello world</p></body></html>",
        )
        .await;
        let args = WebFetchArgs {
            url,
            max_chars: Some(1000),
        };

        let result = fetch_with_policy(
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits::default(),
        )
        .await
        .expect("fetch");

        assert_eq!(result.status, 200);
        assert!(result.text.contains("Title"));
        assert!(result.text.contains("Hello world"));
        assert!(result.untrusted);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn follows_redirects_with_policy_check_on_each_hop() {
        let final_url =
            serve_once("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nredirected").await;
        let redirect_url = serve_once(&format!(
            "HTTP/1.1 302 Found\r\nLocation: {final_url}\r\n\r\n"
        ))
        .await;
        let args = WebFetchArgs {
            url: redirect_url,
            max_chars: Some(1000),
        };

        let result = fetch_with_policy(
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits::default(),
        )
        .await
        .expect("fetch");

        assert_eq!(result.text, "redirected");
        assert_eq!(result.final_url, final_url);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_unsupported_content_type() {
        let url =
            serve_once("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\r\nabc")
                .await;
        let args = WebFetchArgs {
            url,
            max_chars: Some(1000),
        };

        let error = fetch_with_policy(
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits::default(),
        )
        .await
        .expect_err("unsupported content type");

        assert!(error.to_string().contains("content type"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_body_over_byte_cap() {
        let url = serve_once("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nabcdef").await;
        let args = WebFetchArgs {
            url,
            max_chars: Some(1000),
        };

        let error = fetch_with_policy(
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits {
                max_response_bytes: 3,
                ..WebFetchLimits::default()
            },
        )
        .await
        .expect_err("body over cap");

        assert!(error.to_string().contains("byte limit"));
    }

    async fn serve_once(response: impl Into<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr: SocketAddr = listener.local_addr().expect("local addr");
        let response = response.into();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0; 1024];
            let _ = socket.read(&mut request).await.expect("read request");
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });
        format!("http://{addr}/")
    }
}
