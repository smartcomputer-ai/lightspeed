//! Every provider client must read one transport failure the same way, so
//! these run each failure through each client's real call path.

use std::time::Duration;

use llm_clients::openai::{audio, completions, responses};
use llm_clients::{LlmApiError, anthropic, transport::HttpClientConfig};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy)]
enum AfterHeaders {
    /// Close the connection with the declared body unfinished.
    Close,
    /// Keep the connection open with the body unfinished until the client hangs up.
    Stall,
}

async fn read_request(stream: &mut TcpStream) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).await.expect("read");
        assert!(read > 0, "client hung up before finishing its request");
        bytes.extend_from_slice(&buffer[..read]);
        let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
        let size: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .map_or(0, |value| value.parse().expect("content length"));
        if bytes.len() >= end + 4 + size {
            return;
        }
    }
}

/// Answers one request with headers and part of the promised body.
async fn broken_body_server(after_headers: AfterHeaders) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        read_request(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                  content-length: 1000\r\n\r\n{\"partial\":",
            )
            .await
            .expect("write");
        stream.flush().await.expect("flush");
        if let AfterHeaders::Stall = after_headers {
            let mut buffer = [0_u8; 64];
            while stream.read(&mut buffer).await.is_ok_and(|read| read > 0) {}
        }
    });
    format!("http://{address}/v1")
}

/// A base URL with nothing listening on it.
async fn refused_base_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    format!("http://{address}/v1")
}

async fn call_every_client(
    base_url: impl AsyncFn() -> String,
    http: HttpClientConfig,
) -> Vec<(&'static str, LlmApiError)> {
    let mut failures = Vec::new();

    let mut config = anthropic::messages::Config::new("test-key");
    config.base_url = base_url().await;
    config.http = http;
    let error = anthropic::messages::Client::new(config)
        .expect("client")
        .create(anthropic::messages::CreateMessageRequest::user_text(
            "test", "hello", 16,
        ))
        .await
        .expect_err("anthropic must fail");
    failures.push(("anthropic", error));

    let mut config = completions::Config::new("test-key");
    config.base_url = base_url().await;
    config.http = http;
    let error = completions::Client::new(config)
        .expect("client")
        .create(completions::CreateCompletionRequest::user_text(
            "test", "hello",
        ))
        .await
        .expect_err("completions must fail");
    failures.push(("completions", error));

    let mut config = responses::Config::new("test-key");
    config.base_url = base_url().await;
    config.http = http;
    let error = responses::Client::new(config)
        .expect("client")
        .create(responses::CreateResponseRequest::text("test", "hello"))
        .await
        .expect_err("responses must fail");
    failures.push(("responses", error));

    let mut config = audio::Config::new("test-key");
    config.base_url = base_url().await;
    config.http = http;
    let error = audio::Client::new(config)
        .expect("client")
        .create_transcription(audio::CreateTranscriptionRequest::new(audio::AudioFile {
            filename: "test.wav".into(),
            mime: "audio/wav".into(),
            bytes: b"RIFF".to_vec(),
        }))
        .await
        .expect_err("audio must fail");
    failures.push(("audio", error));

    failures
}

#[tokio::test(flavor = "current_thread")]
async fn a_connection_closed_after_the_headers_is_retryable_on_every_client() {
    let failures = call_every_client(
        || broken_body_server(AfterHeaders::Close),
        HttpClientConfig::default(),
    )
    .await;
    for (client, error) in failures {
        assert!(matches!(error, LlmApiError::Transport(_)), "{client}");
        assert!(error.retryable(), "{client}: {error}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_deadline_hit_after_the_headers_is_retryable_on_every_client() {
    let failures = call_every_client(
        || broken_body_server(AfterHeaders::Stall),
        HttpClientConfig {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_millis(200),
        },
    )
    .await;
    for (client, error) in failures {
        assert!(matches!(error, LlmApiError::Transport(_)), "{client}");
        assert!(error.retryable(), "{client}: {error}");
        assert!(
            error.to_string().contains("timed out after 200ms"),
            "{client}: {error}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_refused_connection_is_retryable_on_every_client() {
    let failures = call_every_client(refused_base_url, HttpClientConfig::default()).await;
    for (client, error) in failures {
        assert!(matches!(error, LlmApiError::Transport(_)), "{client}");
        assert!(error.retryable(), "{client}: {error}");
    }
}
