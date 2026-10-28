//! Verified installation of the envd build published by a deployment.
//!
//! Discovery is deliberately outside the authenticated Runtime API. The
//! deployment document names one archive per target and pins it by SHA-256;
//! the candidate binary must then report the same build and protocol facts
//! before it can replace the running executable.

use std::{
    io::{Read as _, Seek as _, Write as _},
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, anyhow};
use environment_protocol::{
    bootstrap::{ArtifactSha256, BuildFacts, DiscoveryArtifact, DiscoveryDocument},
    routes::{DISCOVERY_PATH, canonical_origin},
};
use flate2::read::GzDecoder;
use futures_util::StreamExt as _;
use reqwest::Url;
use sha2::{Digest as _, Sha256};

const MAX_DISCOVERY_BYTES: usize = 1_048_576;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;
const CANDIDATE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct UpgradeRequest {
    pub discovery_url: String,
    pub ca_file: Option<PathBuf>,
    pub install_path: PathBuf,
    pub target: String,
    /// Automatic upgrades pin discovery to the protocol in the gateway's
    /// challenge. Manual upgrades install whichever protocol the deployment
    /// currently publishes.
    pub expected_protocol: Option<u32>,
    /// A protocol that installing would not change, refused as terminal:
    /// an automatic upgrade after an unreadable first frame installs any
    /// protocol but the daemon's own.
    pub other_than: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledBuild {
    pub version: String,
    pub git_sha: String,
    pub protocol_version: u32,
    pub path: PathBuf,
}

/// Why an upgrade did not install anything.
#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    /// A network failure, timeout, HTTP 5xx or 429 while fetching the
    /// discovery document or the archive: retry later.
    #[error("{0:#}")]
    Transient(anyhow::Error),
    /// Everything else: an integrity failure (checksum, candidate build
    /// facts, a discovery protocol other than the challenge's), a document
    /// without this target, or a local install failure. Retrying cannot
    /// help.
    #[error("{0:#}")]
    Terminal(anyhow::Error),
}

impl UpgradeError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

type UpgradeResult<T> = Result<T, UpgradeError>;

fn terminal(error: impl Into<anyhow::Error>) -> UpgradeError {
    UpgradeError::Terminal(error.into())
}

/// Classify a failed HTTP exchange: transport trouble and server-side
/// statuses are transient, a client-side status is terminal.
fn http_failure(error: reqwest::Error, context: String) -> UpgradeError {
    let transient = match error.status() {
        Some(status) => status.is_server_error() || status.as_u16() == 429,
        None => true,
    };
    let error = anyhow::Error::new(error).context(context);
    if transient {
        UpgradeError::Transient(error)
    } else {
        UpgradeError::Terminal(error)
    }
}

macro_rules! fail {
    ($($arg:tt)*) => {
        return Err(terminal(anyhow!($($arg)*)))
    };
}

/// Resolve an explicit override or derive the deployment discovery endpoint
/// from an outbound WebSocket gateway URL: its canonical origin, as HTTP,
/// with `DISCOVERY_PATH`.
pub fn resolve_discovery_url(
    gateway_url: Option<&str>,
    override_url: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(url) = override_url {
        validate_discovery_url(url)?;
        return Ok(url.to_owned());
    }
    let gateway_url = gateway_url.ok_or_else(|| {
        anyhow!("upgrade needs LIGHTSPEED_ENVD_GATEWAY_URL or LIGHTSPEED_ENVD_DISCOVERY_URL")
    })?;
    let resolved = derive_discovery_url(gateway_url)?;
    validate_discovery_url(&resolved)?;
    Ok(resolved)
}

/// The discovery endpoint a gateway URL implies, not yet checked for
/// transport security: `https` for `wss://`, plain `http` for `ws://`.
pub fn derive_discovery_url(gateway_url: &str) -> anyhow::Result<String> {
    if !gateway_url.starts_with("ws://") && !gateway_url.starts_with("wss://") {
        anyhow::bail!("cannot derive a discovery URL from gateway URL {gateway_url}");
    }
    let origin = canonical_origin(gateway_url).context("parse environment gateway URL")?;
    Ok(match origin.as_str().strip_prefix("wss://") {
        Some(authority) => format!("https://{authority}{DISCOVERY_PATH}"),
        None => format!(
            "http://{}{DISCOVERY_PATH}",
            origin.as_str().trim_start_matches("ws://")
        ),
    })
}

/// Discovery and archive downloads require HTTPS. Plain HTTP exists only for
/// loopback development and the local automatic-upgrade acceptance fixture.
pub fn validate_discovery_url(value: &str) -> anyhow::Result<()> {
    validate_download_url(value, "discovery URL").map(|_| ())
}

pub async fn install(request: UpgradeRequest) -> UpgradeResult<InstalledBuild> {
    crate::install_crypto_provider();
    validate_discovery_url(&request.discovery_url).map_err(terminal)?;
    let client = http_client(request.ca_file.as_deref())
        .await
        .map_err(terminal)?;
    let discovery = fetch_discovery(&client, &request.discovery_url).await?;
    if let Some(expected) = request.expected_protocol
        && discovery.protocol_version != expected
    {
        fail!(
            "discovery document {} advertises protocol {}, but the gateway challenged with protocol {expected}",
            request.discovery_url,
            discovery.protocol_version
        );
    }
    if request.other_than == Some(discovery.protocol_version) {
        fail!(
            "discovery document {} advertises protocol {}, which this daemon already speaks",
            request.discovery_url,
            discovery.protocol_version
        );
    }
    validate_git_sha(&discovery.git_sha, "discovery gitSha").map_err(terminal)?;
    let Some(artifact) = discovery.artifacts.get(&request.target) else {
        fail!(
            "discovery document {} has no envd artifact for target {}",
            request.discovery_url,
            request.target
        );
    };
    validate_artifact(artifact).map_err(terminal)?;
    let Some(artifact_url) = artifact.url.as_deref() else {
        fail!(
            "deployment discovery document {} has no public URL for target {}",
            request.discovery_url,
            request.target
        );
    };
    let artifact_url =
        validate_download_url(artifact_url, "envd artifact URL").map_err(terminal)?;
    if !artifact_url
        .path()
        .ends_with(&format!("/{}", artifact.file))
    {
        fail!("envd artifact URL does not end with its declared filename");
    }

    let (mut archive_file, actual_sha) =
        download_archive(&client, &artifact_url, MAX_ARCHIVE_BYTES).await?;
    if actual_sha != artifact.sha256 {
        fail!(
            "envd archive checksum mismatch: expected {}, downloaded {actual_sha}",
            artifact.sha256
        );
    }

    let manual = || {
        manual_install_command(
            artifact_url.as_str(),
            &artifact.sha256.to_hex(),
            &request.install_path,
        )
    };
    let Some(parent) = request.install_path.parent() else {
        fail!(
            "envd executable has no parent directory: {}",
            request.install_path.display()
        );
    };
    let mut candidate = tempfile::Builder::new()
        .prefix(".lightspeed-envd-upgrade-")
        .tempfile_in(parent)
        .map_err(|error| {
            terminal(anyhow!(
                "cannot write next to {}: {error}\n{}",
                request.install_path.display(),
                manual()
            ))
        })?;
    extract_binary(&mut archive_file, candidate.as_file_mut()).map_err(terminal)?;
    make_executable(candidate.path()).map_err(terminal)?;
    candidate
        .as_file_mut()
        .sync_all()
        .context("sync candidate envd executable")
        .map_err(terminal)?;
    // Close the write handle before executing the candidate: Linux refuses
    // to run a file that is still open for writing (ETXTBSY). The path stays
    // reserved and is removed on every failure path.
    let candidate = candidate.into_temp_path();

    let build = inspect_candidate(&candidate).await.map_err(terminal)?;
    verify_candidate(&build, &discovery, &request.target).map_err(terminal)?;
    candidate.persist(&request.install_path).map_err(|error| {
        terminal(anyhow!(
            "replace {} atomically: {}\n{}",
            request.install_path.display(),
            error.error,
            manual()
        ))
    })?;
    std::fs::File::open(&request.install_path)
        .and_then(|installed| installed.sync_all())
        .context("sync installed envd executable")
        .map_err(terminal)?;

    Ok(InstalledBuild {
        version: build.version,
        git_sha: build.git_sha,
        protocol_version: build.protocol_version,
        path: request.install_path,
    })
}

async fn http_client(ca_file: Option<&Path>) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .user_agent(concat!("lightspeed-envd/", env!("CARGO_PKG_VERSION")));
    if let Some(ca_file) = ca_file {
        let pem = tokio::fs::read(ca_file)
            .await
            .with_context(|| format!("read CA file {}", ca_file.display()))?;
        let certificates = reqwest::tls::Certificate::from_pem_bundle(&pem)
            .with_context(|| format!("parse CA file {}", ca_file.display()))?;
        if certificates.is_empty() {
            anyhow::bail!("CA file {} holds no certificates", ca_file.display());
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder.build().context("build envd upgrade HTTP client")
}

/// Fetch and decode the discovery document. It is part of the frozen
/// bootstrap surface: members this daemon does not know are ignored.
async fn fetch_discovery(client: &reqwest::Client, url: &str) -> UpgradeResult<DiscoveryDocument> {
    let context = || format!("fetch envd discovery document {url}");
    let response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| http_failure(error, context()))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DISCOVERY_BYTES as u64)
    {
        fail!("envd discovery document exceeds {MAX_DISCOVERY_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| http_failure(error, context()))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_DISCOVERY_BYTES {
            fail!("envd discovery document exceeds {MAX_DISCOVERY_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .context("decode envd discovery document")
        .map_err(terminal)
}

async fn download_archive(
    client: &reqwest::Client,
    url: &Url,
    max_bytes: u64,
) -> UpgradeResult<(tempfile::NamedTempFile, ArtifactSha256)> {
    let context = || format!("download envd archive {url}");
    let response = client
        .get(url.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| http_failure(error, context()))?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        fail!("envd archive exceeds {max_bytes} bytes");
    }
    let mut file = tempfile::NamedTempFile::new()
        .context("create temporary envd archive")
        .map_err(terminal)?;
    let mut stream = response.bytes_stream();
    let mut size = 0_u64;
    let mut sha = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| http_failure(error, context()))?;
        size = size.saturating_add(chunk.len() as u64);
        if size > max_bytes {
            fail!("envd archive exceeds {max_bytes} bytes");
        }
        sha.update(&chunk);
        file.write_all(&chunk)
            .context("write temporary envd archive")
            .map_err(terminal)?;
    }
    file.as_file_mut()
        .sync_all()
        .context("sync temporary envd archive")
        .map_err(terminal)?;
    file.as_file_mut()
        .rewind()
        .context("rewind temporary envd archive")
        .map_err(terminal)?;
    let digest: [u8; 32] = sha
        .finalize()
        .as_slice()
        .try_into()
        .expect("sha256 is 32 bytes");
    Ok((file, ArtifactSha256::from_bytes(digest)))
}

fn extract_binary(
    archive_file: &mut tempfile::NamedTempFile,
    destination: &mut std::fs::File,
) -> anyhow::Result<()> {
    archive_file
        .as_file_mut()
        .rewind()
        .context("rewind envd archive")?;
    let decoder = GzDecoder::new(archive_file.as_file_mut());
    let mut archive = tar::Archive::new(decoder);
    let mut found = false;
    for entry in archive.entries().context("read envd archive")? {
        let mut entry = entry.context("read envd archive entry")?;
        let path = entry.path().context("read envd archive path")?;
        if path.as_ref() != Path::new("lightspeed-envd") {
            anyhow::bail!("envd archive contains unexpected entry {}", path.display());
        }
        if found {
            anyhow::bail!("envd archive contains lightspeed-envd more than once");
        }
        if !entry.header().entry_type().is_file() {
            anyhow::bail!("envd archive entry lightspeed-envd is not a regular file");
        }
        let size = entry.size();
        if size == 0 || size > MAX_BINARY_BYTES {
            anyhow::bail!("envd executable has invalid size {size}");
        }
        let copied = std::io::copy(&mut entry.by_ref().take(MAX_BINARY_BYTES + 1), destination)
            .context("extract envd executable")?;
        if copied != size {
            anyhow::bail!("envd executable size differs from its archive header");
        }
        found = true;
    }
    if !found {
        anyhow::bail!("envd archive does not contain lightspeed-envd");
    }
    destination
        .flush()
        .context("flush candidate envd executable")
}

#[cfg(unix)]
fn make_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("make candidate executable {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> anyhow::Result<()> {
    anyhow::bail!("envd self-upgrade is supported only on Unix")
}

/// Run the candidate's `--print-build`, decoded as the frozen
/// [`BuildFacts`].
async fn inspect_candidate(path: &Path) -> anyhow::Result<BuildFacts> {
    let output = tokio::time::timeout(
        CANDIDATE_TIMEOUT,
        tokio::process::Command::new(path)
            .arg("--print-build")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("candidate envd --print-build timed out")?
    .with_context(|| format!("run candidate envd {}", path.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "candidate envd --print-build exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("decode candidate envd --print-build")
}

fn verify_candidate(
    build: &BuildFacts,
    discovery: &DiscoveryDocument,
    target: &str,
) -> anyhow::Result<()> {
    if build.name != crate::DAEMON_NAME {
        anyhow::bail!(
            "candidate build name is {}, expected {}",
            build.name,
            crate::DAEMON_NAME
        );
    }
    if build.version != discovery.version {
        anyhow::bail!(
            "candidate version {} differs from discovery version {}",
            build.version,
            discovery.version
        );
    }
    if build.git_sha != discovery.git_sha {
        anyhow::bail!(
            "candidate gitSha {} differs from discovery gitSha {}",
            build.git_sha,
            discovery.git_sha
        );
    }
    if build.target != target {
        anyhow::bail!(
            "candidate target {} differs from requested target {target}",
            build.target
        );
    }
    if build.protocol_version != discovery.protocol_version {
        anyhow::bail!(
            "candidate protocol {} differs from discovery protocol {}",
            build.protocol_version,
            discovery.protocol_version
        );
    }
    Ok(())
}

fn validate_artifact(artifact: &DiscoveryArtifact) -> anyhow::Result<()> {
    if artifact.file.is_empty()
        || Path::new(&artifact.file)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(artifact.file.as_str())
    {
        anyhow::bail!("envd artifact has an unsafe filename");
    }
    if !artifact.file.ends_with(".tar.gz") {
        anyhow::bail!("envd artifact is not a .tar.gz archive");
    }
    Ok(())
}

fn validate_git_sha(value: &str, label: &str) -> anyhow::Result<()> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        anyhow::bail!("{label} must be a full lowercase hexadecimal commit");
    }
    Ok(())
}

fn validate_download_url(value: &str, label: &str) -> anyhow::Result<Url> {
    let url = Url::parse(value).with_context(|| format!("parse {label}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("{label} must not contain credentials");
    }
    if url.fragment().is_some() {
        anyhow::bail!("{label} must not contain a fragment");
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&url) => {}
        _ => anyhow::bail!("{label} must use https (or http toward loopback)"),
    }
    if url.host_str().is_none() {
        anyhow::bail!("{label} has no host");
    }
    Ok(url)
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn manual_install_command(url: &str, sha256: &str, install_path: &Path) -> String {
    let url = shell_quote(url);
    let path = shell_quote(&install_path.to_string_lossy());
    format!(
        "install manually:\n  tmp=$(mktemp -d)\n  curl --fail --location --output \"$tmp/envd.tar.gz\" {url}\n  printf '%s  %s\\n' {sha256} \"$tmp/envd.tar.gz\" | sha256sum --check\n  tar -xzf \"$tmp/envd.tar.gz\" -C \"$tmp\" lightspeed-envd\n  sudo install -m 0755 \"$tmp/lightspeed-envd\" {path}"
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_url_is_derived_from_the_gateway_origin() {
        assert_eq!(
            resolve_discovery_url(
                Some("wss://gateway.example/environment-gateway/connect?old=1"),
                None,
            )
            .expect("derive"),
            "https://gateway.example/.well-known/lightspeed-envd"
        );
        assert_eq!(
            resolve_discovery_url(Some("ws://127.0.0.1:18080/connect"), None).expect("derive"),
            "http://127.0.0.1:18080/.well-known/lightspeed-envd"
        );
        assert_eq!(
            resolve_discovery_url(None, Some("https://downloads.example/current-envd.json"))
                .expect("override"),
            "https://downloads.example/current-envd.json"
        );
    }

    #[test]
    fn download_urls_require_tls_except_on_loopback() {
        assert!(validate_discovery_url("https://gateway.example/doc").is_ok());
        assert!(validate_discovery_url("http://localhost:8000/doc").is_ok());
        assert!(validate_discovery_url("http://127.0.0.1:8000/doc").is_ok());
        assert!(validate_discovery_url("http://gateway.example/doc").is_err());
        assert!(validate_discovery_url("https://user:secret@gateway.example/doc").is_err());
        assert!(validate_discovery_url("file:///tmp/envd.json").is_err());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn installs_only_an_archive_and_binary_matching_the_document() {
        let temp = tempfile::tempdir().expect("tempdir");
        let install_path = temp.path().join("lightspeed-envd");
        std::fs::write(&install_path, b"old envd").expect("old executable");
        let git_sha = "a".repeat(40);
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' '{{\"name\":\"lightspeed-envd\",\"version\":\"0.2.0\",\"gitSha\":\"{git_sha}\",\"target\":\"{}\",\"protocolVersion\":3}}'\n",
            release_info::TARGET
        );
        let archive = envd_archive(script.as_bytes());
        let archive_sha = environment_protocol::bytes::encode_hex(&Sha256::digest(&archive));
        let (origin, server) = serve_upgrade_with_document(archive, |origin| {
            discovery_json(origin, &archive_sha, &git_sha, 3)
        })
        .await;

        let installed = install(UpgradeRequest {
            discovery_url: format!("{origin}/envd.json"),
            ca_file: None,
            install_path: install_path.clone(),
            target: release_info::TARGET.to_owned(),
            expected_protocol: Some(3),
            other_than: None,
        })
        .await
        .expect("install");
        server.await.expect("server");
        assert_eq!(installed.git_sha, git_sha);
        assert_eq!(installed.protocol_version, 3);
        assert_eq!(
            std::fs::read_to_string(install_path).expect("installed"),
            script
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn gateway_protocol_mismatch_leaves_the_executable_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let install_path = temp.path().join("lightspeed-envd");
        std::fs::write(&install_path, b"old envd").expect("old executable");
        let git_sha = "b".repeat(40);
        let archive = envd_archive(b"#!/bin/sh\nexit 1\n");
        let (origin, server) = serve_upgrade_with_document(archive, |origin| {
            discovery_json(origin, &"0".repeat(64), &git_sha, 4)
        })
        .await;
        let error = install(UpgradeRequest {
            discovery_url: format!("{origin}/envd.json"),
            ca_file: None,
            install_path: install_path.clone(),
            target: release_info::TARGET.to_owned(),
            expected_protocol: Some(3),
            other_than: None,
        })
        .await
        .expect_err("protocol mismatch");
        server.abort();
        assert!(!error.is_transient());
        assert!(
            error
                .to_string()
                .contains("gateway challenged with protocol 3")
        );
        assert_eq!(
            std::fs::read(&install_path).expect("old executable"),
            b"old envd"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn an_upgrade_to_the_daemons_own_protocol_is_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let install_path = temp.path().join("lightspeed-envd");
        std::fs::write(&install_path, b"old envd").expect("old executable");
        let git_sha = "d".repeat(40);
        let archive = envd_archive(b"#!/bin/sh\nexit 1\n");
        let (origin, server) = serve_upgrade_with_document(archive, |origin| {
            discovery_json(origin, &"0".repeat(64), &git_sha, 3)
        })
        .await;
        let error = install(UpgradeRequest {
            discovery_url: format!("{origin}/envd.json"),
            ca_file: None,
            install_path: install_path.clone(),
            target: release_info::TARGET.to_owned(),
            expected_protocol: None,
            other_than: Some(3),
        })
        .await
        .expect_err("own protocol");
        server.abort();
        assert!(!error.is_transient(), "reinstalling cannot help");
        assert_eq!(
            std::fs::read(&install_path).expect("old executable"),
            b"old envd"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn checksum_mismatch_leaves_the_executable_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let install_path = temp.path().join("lightspeed-envd");
        std::fs::write(&install_path, b"old envd").expect("old executable");
        let git_sha = "c".repeat(40);
        let archive = envd_archive(b"#!/bin/sh\nexit 1\n");
        let (origin, server) = serve_upgrade_with_document(archive, |origin| {
            discovery_json(origin, &"0".repeat(64), &git_sha, 4)
        })
        .await;
        let error = install(UpgradeRequest {
            discovery_url: format!("{origin}/envd.json"),
            ca_file: None,
            install_path: install_path.clone(),
            target: release_info::TARGET.to_owned(),
            expected_protocol: Some(4),
            other_than: None,
        })
        .await
        .expect_err("checksum mismatch");
        server.await.expect("server");
        assert!(!error.is_transient(), "integrity failures are terminal");
        assert!(error.to_string().contains("archive checksum mismatch"));
        assert_eq!(
            std::fs::read(&install_path).expect("old executable"),
            b"old envd"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_errors_are_transient_and_client_errors_terminal() {
        for (status, transient) in [(503, true), (429, true), (404, false)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listener");
            let origin = format!("http://{}", listener.local_addr().expect("address"));
            let server = tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut request = vec![0_u8; 4096];
                let _ = socket.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 {status} Status\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                socket.write_all(response.as_bytes()).await.expect("write");
            });
            let temp = tempfile::tempdir().expect("tempdir");
            let error = install(UpgradeRequest {
                discovery_url: format!("{origin}/envd.json"),
                ca_file: None,
                install_path: temp.path().join("lightspeed-envd"),
                target: release_info::TARGET.to_owned(),
                expected_protocol: Some(3),
                other_than: None,
            })
            .await
            .expect_err("no document");
            server.await.expect("server");
            assert_eq!(error.is_transient(), transient, "HTTP {status}: {error}");
        }

        let unreachable = install(UpgradeRequest {
            discovery_url: "http://127.0.0.1:9/envd.json".to_owned(),
            ca_file: None,
            install_path: PathBuf::from("/nonexistent/lightspeed-envd"),
            target: release_info::TARGET.to_owned(),
            expected_protocol: None,
            other_than: None,
        })
        .await
        .expect_err("nothing listens");
        assert!(unreachable.is_transient(), "{unreachable}");
    }

    #[cfg(unix)]
    fn envd_archive(bytes: &[u8]) -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};

        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, "lightspeed-envd", bytes)
            .expect("append envd");
        let encoder = archive.into_inner().expect("finish tar");
        encoder.finish().expect("finish gzip")
    }

    #[cfg(unix)]
    fn discovery_json(origin: &str, sha256: &str, git_sha: &str, protocol: u32) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": "0.2.0",
            "gitSha": git_sha,
            "channel": "main",
            "protocolVersion": protocol,
            "builtAtMs": 1,
            "artifacts": {
                (release_info::TARGET): {
                    "file": "lightspeed-envd-0.2.0-test.tar.gz",
                    "sha256": sha256,
                    "url": format!("{origin}/lightspeed-envd-0.2.0-test.tar.gz")
                }
            }
        }))
        .expect("discovery JSON")
    }

    #[cfg(unix)]
    async fn serve_upgrade_with_document(
        archive: Vec<u8>,
        document: impl FnOnce(&str) -> Vec<u8>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let document = document(&origin);
        let task = tokio::spawn(serve_responses(listener, document, archive));
        (origin, task)
    }

    #[cfg(unix)]
    async fn serve_responses(
        listener: tokio::net::TcpListener,
        document: Vec<u8>,
        archive: Vec<u8>,
    ) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = vec![0_u8; 4096];
            let read = socket.read(&mut request).await.expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            let body = if request.starts_with("GET /envd.json ") {
                &document
            } else {
                &archive
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write headers");
            socket.write_all(body).await.expect("write body");
        }
    }
}
