//! Outbound registration: dial the environment gateway, prove the daemon
//! identity, and serve reverse-dialed data connections for as long as the
//! control connection lives.
//!
//! The daemon never chooses an identity mode and never retries a terminal
//! rejection: a closed environment, a refused key, or a bad signature ends
//! the process with a non-zero exit so an orchestrator sees it.

use std::{
    path::Path,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use environment_protocol::{
    registration::{
        DaemonControlMessage, GatewayControlMessage, RegisterParams, RegistrationReceipt,
        RegistrationRejectionCode, decode_hex,
    },
    shared::{CURRENT_PROTOCOL_VERSION, SecretString},
};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest as _,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};

use crate::{
    DaemonRuntime,
    config::{REEXEC_REGISTRATION_KEY_ENV, RegistrationConfig},
    identity::DaemonIdentity,
    server::{run_data_connection, tracing_line},
};

const AUTO_UPGRADE_ATTEMPTED_ENV: &str = "LIGHTSPEED_ENVD_AUTO_UPGRADE_ATTEMPTED";

/// How long the daemon waits for the gateway's challenge and verdict.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A control session that lasted this long resets the reconnect backoff.
const STABLE_SESSION: Duration = Duration::from_secs(30);

/// Why one control session ended.
enum SessionEnd {
    /// The gateway or network went away; reconnect with backoff.
    Disconnected(String),
    /// The gateway refused the daemon for a transient reason.
    Retryable(RegistrationRejectionCode, String),
}

/// Registration failure the daemon must not retry.
#[derive(Debug, thiserror::Error)]
#[error("registration rejected ({code:?}): {message}")]
pub struct TerminalRejection {
    pub code: RegistrationRejectionCode,
    pub message: String,
}

pub async fn run_outbound(runtime: DaemonRuntime, registration: RegistrationConfig) -> Result<()> {
    let identity = DaemonIdentity::load_or_create(&runtime.config().state_dir)
        .context("load daemon identity")?;
    tracing_line(&format!(
        "registering with {} as daemon public key {}",
        registration.gateway_url,
        identity.public_key_hex()
    ));
    let tls = TlsSettings::from_config(registration.ca_file.as_deref())?;
    let mut backoff = INITIAL_BACKOFF;
    loop {
        let started = Instant::now();
        match control_session(&runtime, &registration, &identity, &tls).await {
            Ok(SessionEnd::Disconnected(reason)) => {
                tracing_line(&format!("control connection ended: {reason}"));
            }
            Ok(SessionEnd::Retryable(code, message)) => {
                tracing_line(&format!("registration deferred ({code:?}): {message}"));
            }
            Err(error) => {
                if let Some(rejection) = error.downcast_ref::<TerminalRejection>() {
                    tracing_line(&format!("{rejection}; not retrying"));
                    return Err(error);
                }
                tracing_line(&format!("control connection failed: {error:#}"));
            }
        }
        if started.elapsed() >= STABLE_SESSION {
            backoff = INITIAL_BACKOFF;
        }
        tokio::time::sleep(jittered(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn control_session(
    runtime: &DaemonRuntime,
    registration: &RegistrationConfig,
    identity: &DaemonIdentity,
    tls: &TlsSettings,
) -> Result<SessionEnd> {
    let request = registration
        .gateway_url
        .as_str()
        .into_client_request()
        .context("gateway URL")?;
    let (socket, _) = connect_async_tls_with_config(request, None, false, Some(tls.connector()))
        .await
        .map_err(|error| describe_connect_error(&registration.gateway_url, error))?;
    let (mut writer, mut reader) = socket.split();

    let challenge = tokio::time::timeout(HANDSHAKE_TIMEOUT, next_control_message(&mut reader))
        .await
        .context("waiting for gateway challenge")??;
    let nonce = match challenge {
        Some(GatewayControlMessage::Challenge {
            protocol_version,
            nonce,
        }) => {
            if protocol_version != CURRENT_PROTOCOL_VERSION {
                return Err(protocol_mismatch_error(registration, protocol_version).await);
            }
            decode_hex(&nonce).ok_or_else(|| anyhow!("gateway challenge nonce is not hex"))?
        }
        Some(GatewayControlMessage::Rejected { code, message }) => {
            return rejection(code, message);
        }
        Some(other) => bail!("unexpected gateway message before challenge: {other:?}"),
        None => {
            return Ok(SessionEnd::Disconnected(
                "closed before challenge".to_owned(),
            ));
        }
    };

    let register = DaemonControlMessage::Register(RegisterParams {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        registration_key: registration.registration_key.clone(),
        daemon_public_key: identity.public_key_hex(),
        signature: identity.sign_challenge(&nonce),
        display_name: registration.display_name.clone(),
        metadata: registration.metadata.clone(),
        implementation: runtime.implementation(),
    });
    writer
        .send(Message::Text(serde_json::to_string(&register)?.into()))
        .await
        .context("send register")?;

    let verdict = tokio::time::timeout(HANDSHAKE_TIMEOUT, next_control_message(&mut reader))
        .await
        .context("waiting for gateway verdict")??;
    let (receipt, heartbeat_interval_ms) = match verdict {
        Some(GatewayControlMessage::Accepted {
            receipt,
            heartbeat_interval_ms,
        }) => (receipt, heartbeat_interval_ms),
        Some(GatewayControlMessage::Rejected { code, message }) => {
            return rejection(code, message);
        }
        Some(other) => bail!("unexpected gateway message after register: {other:?}"),
        None => return Ok(SessionEnd::Disconnected("closed before verdict".to_owned())),
    };
    announce_receipt(&receipt, registration.receipt_path.as_deref())?;
    let _ = heartbeat_interval_ms;

    loop {
        let message = match reader.next().await {
            Some(Ok(message)) => message,
            Some(Err(error)) => return Ok(SessionEnd::Disconnected(error.to_string())),
            None => return Ok(SessionEnd::Disconnected("gateway closed".to_owned())),
        };
        match message {
            Message::Text(text) => match serde_json::from_str::<GatewayControlMessage>(&text) {
                Ok(GatewayControlMessage::OpenData { token, data_url }) => {
                    let runtime = runtime.clone();
                    let tls = tls.clone();
                    tokio::spawn(async move {
                        if let Err(error) =
                            serve_data_connection(runtime, &data_url, token, &tls).await
                        {
                            tracing_line(&format!("data connection failed: {error:#}"));
                        }
                    });
                }
                Ok(GatewayControlMessage::Rejected { code, message }) => {
                    return rejection(code, message);
                }
                Ok(other) => tracing_line(&format!("ignoring gateway message {other:?}")),
                Err(error) => tracing_line(&format!("ignoring undecodable gateway frame: {error}")),
            },
            Message::Ping(bytes) => {
                if writer.send(Message::Pong(bytes)).await.is_err() {
                    return Ok(SessionEnd::Disconnected("pong failed".to_owned()));
                }
            }
            Message::Close(frame) => {
                let _ = writer.flush().await;
                return Ok(SessionEnd::Disconnected(
                    frame
                        .map(|frame| frame.reason.to_string())
                        .unwrap_or_else(|| "gateway closed".to_owned()),
                ));
            }
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn protocol_mismatch_error(
    registration: &RegistrationConfig,
    gateway_protocol: u32,
) -> anyhow::Error {
    let base = format!(
        "gateway speaks protocol {gateway_protocol}; this daemon speaks {CURRENT_PROTOCOL_VERSION}; discovery document: {}",
        registration.discovery_url
    );
    let Some(restart) = &registration.auto_upgrade else {
        return TerminalRejection {
            code: RegistrationRejectionCode::UnsupportedProtocol,
            message: format!(
                "{base}; run `lightspeed-envd upgrade` or set LIGHTSPEED_ENVD_AUTO_UPGRADE=1"
            ),
        }
        .into();
    };
    if std::env::var_os(AUTO_UPGRADE_ATTEMPTED_ENV).is_some() {
        return TerminalRejection {
            code: RegistrationRejectionCode::UnsupportedProtocol,
            message: format!(
                "{base}; automatic upgrade was already attempted by this process lineage"
            ),
        }
        .into();
    }

    tracing_line(&format!(
        "{base}; automatic upgrade enabled, installing the gateway's envd build"
    ));
    let request = crate::upgrade::UpgradeRequest {
        discovery_url: registration.discovery_url.clone(),
        ca_file: registration.ca_file.clone(),
        install_path: restart.executable.clone(),
        target: release_info::TARGET.to_owned(),
        expected_protocol: Some(gateway_protocol),
    };
    let installed = match crate::upgrade::install(request).await {
        Ok(installed) => installed,
        Err(error) => {
            return TerminalRejection {
                code: RegistrationRejectionCode::UnsupportedProtocol,
                message: format!("{base}; automatic upgrade failed: {error:#}"),
            }
            .into();
        }
    };
    tracing_line(&format!(
        "installed lightspeed-envd {} ({}, protocol {}); re-executing {}",
        installed.version,
        installed.git_sha,
        installed.protocol_version,
        restart.executable.display()
    ));
    let error = exec_replacement(
        &restart.executable,
        &restart.arg0,
        &restart.args,
        registration.registration_key.as_ref(),
    );
    TerminalRejection {
        code: RegistrationRejectionCode::UnsupportedProtocol,
        message: format!("{base}; installed the replacement but could not re-execute it: {error}"),
    }
    .into()
}

#[cfg(unix)]
fn exec_replacement(
    executable: &Path,
    arg0: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    registration_key: Option<&SecretString>,
) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;

    let mut command = Command::new(executable);
    command
        .arg0(arg0)
        .args(args)
        .env(AUTO_UPGRADE_ATTEMPTED_ENV, "1");
    // Configuration parsing deliberately removes key variables from the
    // current process. Hand the already-held value across exactly this exec
    // so a first-seen daemon can still register even if its key file was
    // removed; the replacement consumes and scrubs this internal variable.
    if let Some(registration_key) = registration_key {
        command
            .env(REEXEC_REGISTRATION_KEY_ENV, registration_key.expose())
            .env_remove("LIGHTSPEED_ENVD_REGISTRATION_KEY")
            .env_remove("LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE");
    }
    command.exec()
}

#[cfg(not(unix))]
fn exec_replacement(
    _executable: &Path,
    _arg0: &std::ffi::OsStr,
    _args: &[std::ffi::OsString],
    _registration_key: Option<&SecretString>,
) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "envd automatic upgrade is supported only on Unix",
    )
}

/// A gateway that answers the WebSocket upgrade with an HTTP status is
/// reachable but not serving registration; say so instead of leaking a raw
/// handshake error. Still retried, because a deploy can produce the same
/// answer for a moment.
fn describe_connect_error(
    gateway_url: &str,
    error: tokio_tungstenite::tungstenite::Error,
) -> anyhow::Error {
    if let tokio_tungstenite::tungstenite::Error::Http(response) = &error {
        let status = response.status();
        if status == 404 {
            return anyhow!(
                "{gateway_url} does not serve environment registration (HTTP 404): point LIGHTSPEED_ENVD_GATEWAY_URL at a Lightspeed process running the environment-gateway role"
            );
        }
        return anyhow!(
            "environment gateway {gateway_url} answered HTTP {status} instead of upgrading"
        );
    }
    anyhow::Error::new(error).context(format!("connect to environment gateway {gateway_url}"))
}

fn rejection(code: RegistrationRejectionCode, message: String) -> Result<SessionEnd> {
    if code.is_terminal() {
        Err(TerminalRejection { code, message }.into())
    } else {
        Ok(SessionEnd::Retryable(code, message))
    }
}

async fn next_control_message<S>(
    reader: &mut futures_util::stream::SplitStream<WebSocketStream<S>>,
) -> Result<Option<GatewayControlMessage>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match reader.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text)
                    .map(Some)
                    .context("decode gateway control message");
            }
            Some(Ok(Message::Binary(bytes))) => {
                return serde_json::from_slice(&bytes)
                    .map(Some)
                    .context("decode gateway control message");
            }
            Some(Ok(Message::Close(_))) | None => return Ok(None),
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(error.into()),
        }
    }
}

/// Write the receipt atomically (when a path is configured) and log it once.
/// It carries ids only; never the registration key or the private key.
fn announce_receipt(receipt: &RegistrationReceipt, path: Option<&Path>) -> Result<()> {
    let json = serde_json::to_string(receipt)?;
    if let Some(path) = path {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create receipt dir {}", parent.display()))?;
        }
        let temp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&temp, format!("{json}\n"))
            .with_context(|| format!("write receipt {}", temp.display()))?;
        std::fs::rename(&temp, path)
            .with_context(|| format!("install receipt {}", path.display()))?;
    }
    tracing_line(&format!("registered {json}"));
    Ok(())
}

/// Dial the gateway data route for one waiting worker and run the ordinary
/// data protocol on it. The token is a one-time pairing credential issued
/// over the authenticated control connection.
async fn serve_data_connection(
    runtime: DaemonRuntime,
    data_url: &str,
    token: SecretString,
    tls: &TlsSettings,
) -> Result<()> {
    let mut request = data_url.into_client_request().context("data URL")?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.expose())).context("token header")?,
    );
    let (socket, _) = connect_async_tls_with_config(request, None, false, Some(tls.connector()))
        .await
        .context("connect data route")?;
    run_data_connection(&runtime, socket).await
}

/// TLS trust for the gateway: the bundled WebPKI roots, plus an operator
/// supplied PEM bundle for gateways behind a private CA.
///
/// The client config always names its crypto provider. A workspace build
/// links two rustls providers, so a config that relied on the process
/// default would panic on the first dial; the daemon never hands the
/// WebSocket client a `None` connector that would build such a config.
#[derive(Clone)]
struct TlsSettings {
    config: Arc<rustls::ClientConfig>,
}

impl TlsSettings {
    fn from_config(ca_file: Option<&Path>) -> Result<Self> {
        use rustls_pki_types::{CertificateDer, pem::PemObject as _};
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(ca_file) = ca_file {
            let mut added = 0usize;
            for certificate in CertificateDer::pem_file_iter(ca_file)
                .with_context(|| format!("read CA file {}", ca_file.display()))?
            {
                let certificate =
                    certificate.with_context(|| format!("parse CA file {}", ca_file.display()))?;
                roots
                    .add(certificate)
                    .with_context(|| format!("add trust anchor from {}", ca_file.display()))?;
                added += 1;
            }
            if added == 0 {
                bail!("CA file {} holds no certificates", ca_file.display());
            }
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .context("TLS protocol versions")?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
        })
    }

    fn connector(&self) -> Connector {
        Connector::Rustls(self.config.clone())
    }
}

fn jittered(base: Duration) -> Duration {
    use rand::RngExt as _;
    let factor = rand::rng().random_range(0.75_f64..=1.25_f64);
    base.mul_f64(factor)
}

#[allow(dead_code)]
type ControlSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_are_written_atomically_and_contain_only_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("out").join("registration.json");
        let receipt = RegistrationReceipt {
            environment_id: "environment_a".to_owned(),
            incarnation_id: "incarnation_a".to_owned(),
            daemon_id: "daemon_a".to_owned(),
            connection_id: "connection_a".to_owned(),
            identity_mode: "ephemeral".to_owned(),
        };
        announce_receipt(&receipt, Some(&path)).expect("write");
        let written: RegistrationReceipt =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(written, receipt);
        assert!(std::fs::read_dir(path.parent().unwrap()).unwrap().count() == 1);
    }

    #[test]
    fn terminal_rejections_stop_and_transient_ones_retry() {
        assert!(rejection(RegistrationRejectionCode::EnvironmentClosed, "x".to_owned()).is_err());
        assert!(matches!(
            rejection(RegistrationRejectionCode::CapacityExhausted, "x".to_owned()),
            Ok(SessionEnd::Retryable(
                RegistrationRejectionCode::CapacityExhausted,
                _
            ))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn protocol_mismatch_without_auto_upgrade_names_discovery_and_command() {
        let registration = RegistrationConfig {
            gateway_url: "wss://gateway.example/environment-gateway/connect".to_owned(),
            registration_key: None,
            display_name: None,
            metadata: Default::default(),
            receipt_path: None,
            ca_file: None,
            discovery_url: "https://gateway.example/.well-known/lightspeed-envd".to_owned(),
            auto_upgrade: None,
        };
        let error = protocol_mismatch_error(&registration, CURRENT_PROTOCOL_VERSION + 1).await;
        let rejection = error
            .downcast_ref::<TerminalRejection>()
            .expect("terminal rejection");
        assert_eq!(
            rejection.code,
            RegistrationRejectionCode::UnsupportedProtocol
        );
        assert!(rejection.message.contains(&registration.discovery_url));
        assert!(rejection.message.contains("lightspeed-envd upgrade"));
    }

    /// A self-signed EC certificate, valid 2026-2036, used only as PEM input.
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBmTCCAT+gAwIBAgIUGlIKCYXc2/JUu1OUJBgh5a7gJ4cwCgYIKoZIzj0EAwIw
IjEgMB4GA1UEAwwXbGlnaHRzcGVlZC1lbnZkLXRlc3QtY2EwHhcNMjYwOTAzMTA1
MjUwWhcNMzYwODMxMTA1MjUwWjAiMSAwHgYDVQQDDBdsaWdodHNwZWVkLWVudmQt
dGVzdC1jYTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABHdyQgLSBwg3QZhQHj3S
AnnUyqMnO2ZIP80Q4un3Fns7yFLOhs6YD5qu8hVSutuxSAbCAlM3Uxujy83VYJNe
cxajUzBRMB0GA1UdDgQWBBRs3ltP/Ch8oC/RpiwqRAEBpGeuhzAfBgNVHSMEGDAW
gBRs3ltP/Ch8oC/RpiwqRAEBpGeuhzAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49
BAMCA0gAMEUCICgTw9frlQLXbAvhAXlMwBH6gCkV3ypx4U93yaMHghBDAiEAi4tZ
fIEYbSNa76Ox9es7E5q8U7/XDqkEOn/kC4eSA5Q=
-----END CERTIFICATE-----
";

    #[test]
    fn missing_ca_file_and_empty_bundle_fail_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(TlsSettings::from_config(Some(&temp.path().join("missing.pem"))).is_err());
        let empty = temp.path().join("empty.pem");
        std::fs::write(&empty, "").expect("write");
        assert!(TlsSettings::from_config(Some(&empty)).is_err());
    }

    /// Both trust configurations hand the WebSocket client an explicit
    /// rustls connector. This is what keeps a workspace build, which links
    /// two crypto providers, from reaching tungstenite's default connector
    /// and panicking on the first dial.
    #[test]
    fn every_trust_configuration_carries_an_explicit_connector() {
        let bundled = TlsSettings::from_config(None).expect("bundled roots");
        assert!(matches!(bundled.connector(), Connector::Rustls(_)));

        let temp = tempfile::tempdir().expect("tempdir");
        let ca_file = temp.path().join("ca.pem");
        std::fs::write(&ca_file, TEST_CA_PEM).expect("write");
        let extended = TlsSettings::from_config(Some(&ca_file)).expect("operator roots");
        assert!(matches!(extended.connector(), Connector::Rustls(_)));
    }
}
