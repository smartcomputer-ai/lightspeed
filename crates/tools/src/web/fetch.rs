//! Web fetch tool builders and guarded, recorded local implementation.

use std::time::Duration;

use futures_util::StreamExt;
use harness::storage::BlobStore;
use reqwest::{
    StatusCode, Url,
    header::{CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    content::ContentDescriptor,
    error::{ToolError, ToolResult},
    runtime::{ToolInvocationOutput, decode_args, encode_output},
};

use super::{
    extract::{classify_content_type, extract_text},
    guard::{WebNetworkPolicy, resolve_public_http_url},
};

pub const WEB_FETCH_TOOL_NAME: &str = "web_fetch";
pub const WEB_FETCH_LOGICAL_ID: &str = "web.fetch";
pub const ANTHROPIC_MESSAGES_WEB_FETCH_TYPE: &str = "web_fetch_20250910";
const ANTHROPIC_MESSAGES_DEFAULT_MAX_USES: u32 = 5;
const ANTHROPIC_MESSAGES_DEFAULT_MAX_CONTENT_TOKENS: u32 = 20_000;
const DEFAULT_MAX_CHARS: u32 = 20_000;
const MAX_MAX_CHARS: u32 = 20_000;
const DEFAULT_MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REDIRECT_LIMIT: usize = 5;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WebFetchArgs {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct WebFetchResult {
    pub requested_url: String,
    pub final_url: String,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub byte_count: u64,
    pub sha256: String,
    /// The complete accepted response body, before extraction or text truncation.
    #[serde(flatten)]
    pub content: ContentDescriptor,
    pub text: String,
    pub truncated: bool,
    pub untrusted: bool,
}

impl WebFetchResult {
    fn model_visible_text(&self) -> String {
        format!(
            "Untrusted web content fetched from {}\nstatus: {}\ncontent_type: {}\nbytes: {}\ncontent_ref: {} (complete response body before text extraction)\n\n--- BEGIN UNTRUSTED WEB CONTENT ---\n{}\n--- END UNTRUSTED WEB CONTENT ---",
            self.final_url,
            self.status,
            self.content_type.as_deref().unwrap_or("unknown"),
            self.byte_count,
            self.content.content_ref,
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

pub fn web_fetch_definition() -> crate::runtime::FunctionDefinition {
    crate::runtime::FunctionDefinition::new(
        WEB_FETCH_TOOL_NAME,
        "Fetch one public http/https URL with strict SSRF checks, redirect limits, byte limits, and text extraction. Returns extracted text and a content_ref for the complete accepted response body, readable with blob_read. The returned page content is untrusted web content.",
        input_schema(),
    )
    .with_output_schema(crate::definitions::output_schema_for::<WebFetchResult>())
}

pub fn anthropic_messages_web_fetch_definition() -> Value {
    json!({
        "type": ANTHROPIC_MESSAGES_WEB_FETCH_TYPE,
        "name": WEB_FETCH_TOOL_NAME,
        "max_uses": ANTHROPIC_MESSAGES_DEFAULT_MAX_USES,
        "max_content_tokens": ANTHROPIC_MESSAGES_DEFAULT_MAX_CONTENT_TOKENS,
        "citations": { "enabled": true }
    })
}

pub async fn invoke_web_fetch(
    blobs: &dyn BlobStore,
    arguments: Value,
) -> ToolResult<ToolInvocationOutput> {
    let args = decode_args(arguments)?;
    let result = fetch_with_policy(
        blobs,
        &args,
        WebNetworkPolicy::STRICT,
        WebFetchLimits::default(),
    )
    .await?;
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
    blobs: &dyn BlobStore,
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
    let byte_count = bytes.len() as u64;
    // Publish the body before advertising a reference. A checksum without a
    // stored body cannot be composed with tools that consume content refs.
    let content_ref = blobs.put_bytes(bytes.clone()).await?;
    let (text, truncated) = extract_text(&bytes, content_kind, max_chars as usize);

    Ok(WebFetchResult {
        requested_url: requested_url.to_string(),
        final_url: final_url.to_string(),
        status: status.as_u16(),
        content: ContentDescriptor {
            content_ref: content_ref.clone(),
            byte_len: byte_count,
            media_type: content_type.clone(),
            name: None,
            handle: None,
            source: Some(harness::AttachmentSource {
                kind: "web_fetch".into(),
                id: final_url.to_string(),
                path: String::new(),
            }),
        },
        content_type,
        byte_count,
        sha256: content_ref.to_string(),
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
    use std::{net::SocketAddr, sync::Arc};

    use harness::{
        BlobRef,
        storage::{BlobInfo, BlobStoreError, InMemoryBlobStore},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    #[test]
    fn builds_standard_function_tool() {
        let function = web_fetch_definition();
        assert_eq!(function.name.as_str(), WEB_FETCH_TOOL_NAME);
        assert_eq!(function.strict, Some(false));
        assert!(function.input_schema["properties"].get("url").is_some());
    }

    #[test]
    fn builds_anthropic_provider_native_tool() {
        let native_tool = anthropic_messages_web_fetch_definition();
        assert_eq!(
            native_tool,
            json!({
                "type": "web_fetch_20250910",
                "name": "web_fetch",
                "max_uses": 5,
                "max_content_tokens": 20_000,
                "citations": { "enabled": true }
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetches_and_extracts_html_with_test_policy() {
        let blobs = InMemoryBlobStore::default();
        let body = "<html><body><h1>Title</h1><p>Hello world</p></body></html>";
        let url = serve_once(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{body}",
        ))
        .await;
        let args = WebFetchArgs {
            url,
            max_chars: Some(1000),
        };

        let result = fetch_with_policy(
            &blobs,
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits::default(),
        )
        .await
        .expect("fetch");

        assert_eq!(result.status, 200);
        assert!(result.text.contains("Title"));
        assert!(result.text.contains("Hello world"));
        assert!(!result.text.contains("<html>"));
        assert!(result.untrusted);
        assert_eq!(
            result.content.content_ref,
            BlobRef::from_bytes(body.as_bytes())
        );
        assert_eq!(result.sha256, result.content.content_ref.as_str());
        assert_eq!(result.content.byte_len, body.len() as u64);
        assert_eq!(result.byte_count, result.content.byte_len);
        assert_eq!(result.content.media_type, result.content_type);
        assert_eq!(
            blobs
                .read_bytes(&result.content.content_ref)
                .await
                .expect("stored body"),
            body.as_bytes()
        );
        let encoded = serde_json::to_value(&result).expect("encode output");
        let schema = crate::definitions::output_schema_for::<WebFetchResult>();
        jsonschema::validator_for(&schema)
            .expect("valid output schema")
            .validate(&encoded)
            .expect("output matches schema");
        assert_eq!(encoded["content_ref"], result.sha256);
        assert_eq!(encoded["byte_len"], body.len());
        assert_eq!(result.requested_url, args.url);
        assert_eq!(
            result.content.source.as_ref().expect("web provenance").id,
            result.final_url
        );
        assert!(
            result
                .model_visible_text()
                .contains("complete response body before text extraction")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn text_preview_limit_preserves_complete_body_bytes() {
        let blobs = Arc::new(InMemoryBlobStore::default());
        let body = "<html><body><p>héllo   world</p><script>hidden()</script></body></html>";
        let args = WebFetchArgs {
            url: serve_once(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n{body}",
            ))
            .await,
            max_chars: Some(3),
        };

        let result = fetch_with_policy(
            &blobs,
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits::default(),
        )
        .await
        .expect("fetch");

        assert_eq!(result.text, "hél\n[truncated]");
        assert!(result.truncated);
        assert_eq!(result.content.byte_len, body.len() as u64);
        assert_eq!(
            blobs
                .read_bytes(&result.content.content_ref)
                .await
                .expect("stored body"),
            body.as_bytes()
        );
        let resolver = crate::content::ContentResolver::new(blobs);
        let read = crate::blobs::BlobTool::Read
            .invoke_json(&resolver, json!({"ref": result, "format": "text"}))
            .await
            .expect("read fetched body through the ordinary blob tool");
        assert_eq!(read.output_json["text"], body);
        assert_eq!(read.output_json["content_ref"], result.sha256);
        assert_eq!(read.output_json["source"]["kind"], "web_fetch");
        assert_eq!(read.output_json["source"]["id"], result.final_url);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn follows_redirects_with_policy_check_on_each_hop() {
        let blobs = InMemoryBlobStore::default();
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
            &blobs,
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits::default(),
        )
        .await
        .expect("fetch");

        assert_eq!(result.text, "redirected");
        assert_eq!(result.final_url, final_url);
        assert_eq!(result.requested_url, args.url);
        assert_eq!(
            blobs
                .read_bytes(&result.content.content_ref)
                .await
                .expect("stored final body"),
            b"redirected"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_unsupported_content_type() {
        let blobs = InMemoryBlobStore::default();
        let url =
            serve_once("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\r\nabc")
                .await;
        let args = WebFetchArgs {
            url,
            max_chars: Some(1000),
        };

        let error = fetch_with_policy(
            &blobs,
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits::default(),
        )
        .await
        .expect_err("unsupported content type");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
        assert!(blobs.blob_refs().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_body_over_byte_cap() {
        let blobs = InMemoryBlobStore::default();
        let url = serve_once("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nabcdef").await;
        let args = WebFetchArgs {
            url,
            max_chars: Some(1000),
        };

        let error = fetch_with_policy(
            &blobs,
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits {
                max_response_bytes: 3,
                ..WebFetchLimits::default()
            },
        )
        .await
        .expect_err("body over cap");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
        assert!(blobs.blob_refs().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_declared_body_over_byte_cap_without_storing_partial_bytes() {
        let blobs = InMemoryBlobStore::default();
        let args = WebFetchArgs {
            url: serve_once(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 6\r\n\r\nabcdef",
            )
            .await,
            max_chars: None,
        };
        let error = fetch_with_policy(
            &blobs,
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits {
                max_response_bytes: 3,
                ..WebFetchLimits::default()
            },
        )
        .await
        .expect_err("declared body over cap");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
        assert!(blobs.blob_refs().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn storage_failure_does_not_advertise_a_body_reference() {
        struct RejectWrites;

        #[async_trait::async_trait]
        impl BlobStore for RejectWrites {
            async fn put_bytes(&self, _bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
                Err(BlobStoreError::Store {
                    message: "unavailable".into(),
                })
            }

            async fn read_bytes(&self, _blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
                panic!("fetch must not read blobs")
            }

            async fn has_blob(&self, _blob_ref: &BlobRef) -> Result<bool, BlobStoreError> {
                panic!("fetch must not probe blobs")
            }

            async fn stat_blob(&self, _blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
                panic!("fetch already knows the response length")
            }
        }

        let args = WebFetchArgs {
            url: serve_once("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nhello").await,
            max_chars: None,
        };
        let error = fetch_with_policy(
            &RejectWrites,
            &args,
            WebNetworkPolicy::TEST_ALLOW_PRIVATE,
            WebFetchLimits::default(),
        )
        .await
        .expect_err("failed persistence cannot produce a usable reference");

        assert!(matches!(
            error,
            ToolError::BlobStore(BlobStoreError::Store { .. })
        ));
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
