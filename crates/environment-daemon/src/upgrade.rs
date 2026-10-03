//! Verified installation of the envd build published by a deployment.
//!
//! Discovery is deliberately outside the authenticated Runtime API. The
//! deployment document names one archive per target and pins it by SHA-256;
//! the candidate binary must then report the same build and protocol facts
//! before it can replace the running executable.

use std::{
    collections::BTreeMap,
    io::{Read as _, Seek as _, Write as _},
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use flate2::read::GzDecoder;
use futures_util::StreamExt as _;
use reqwest::Url;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const DISCOVERY_PATH: &str = "/.well-known/lightspeed-envd";
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledBuild {
    pub version: String,
    pub git_sha: String,
    pub protocol_version: u32,
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryDocument {
    version: String,
    git_sha: String,
    protocol_version: u32,
    artifacts: BTreeMap<String, DiscoveryArtifact>,
}

#[derive(Debug, Deserialize)]
struct DiscoveryArtifact {
    file: String,
    sha256: String,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CandidateBuild {
    name: String,
    version: String,
    git_sha: String,
    target: String,
    protocol_version: u32,
}

/// Resolve an explicit override or derive the deployment discovery endpoint
/// from an outbound WebSocket gateway URL.
pub fn resolve_discovery_url(
    gateway_url: Option<&str>,
    override_url: Option<&str>,
) -> Result<String> {
    if let Some(url) = override_url {
        validate_discovery_url(url)?;
        return Ok(url.to_owned());
    }
    let gateway_url = gateway_url.ok_or_else(|| {
        anyhow!("upgrade needs LIGHTSPEED_ENVD_GATEWAY_URL or LIGHTSPEED_ENVD_DISCOVERY_URL")
    })?;
    let mut url = Url::parse(gateway_url).context("parse environment gateway URL")?;
    let http_scheme = match url.scheme() {
        "wss" => "https",
        "ws" => "http",
        scheme => bail!("cannot derive discovery URL from gateway scheme {scheme}"),
    };
    url.set_scheme(http_scheme)
        .map_err(|_| anyhow!("derive discovery URL scheme"))?;
    url.set_path(DISCOVERY_PATH);
    url.set_query(None);
    url.set_fragment(None);
    let resolved = url.to_string();
    validate_discovery_url(&resolved)?;
    Ok(resolved)
}

/// Discovery and archive downloads require HTTPS. Plain HTTP exists only for
/// loopback development and the local automatic-upgrade acceptance fixture.
pub fn validate_discovery_url(value: &str) -> Result<()> {
    validate_download_url(value, "discovery URL").map(|_| ())
}

pub async fn install(request: UpgradeRequest) -> Result<InstalledBuild> {
    crate::install_crypto_provider();
    validate_discovery_url(&request.discovery_url)?;
    let client = http_client(request.ca_file.as_deref()).await?;
    let discovery = fetch_discovery(&client, &request.discovery_url).await?;
    if let Some(expected) = request.expected_protocol
        && discovery.protocol_version != expected
    {
        bail!(
            "discovery document {} advertises protocol {}, but the gateway challenged with protocol {expected}",
            request.discovery_url,
            discovery.protocol_version
        );
    }
    validate_git_sha(&discovery.git_sha, "discovery gitSha")?;
    let artifact = discovery.artifacts.get(&request.target).ok_or_else(|| {
        anyhow!(
            "discovery document {} has no envd artifact for target {}",
            request.discovery_url,
            request.target
        )
    })?;
    validate_artifact(artifact)?;
    let artifact_url = artifact.url.as_deref().ok_or_else(|| {
        anyhow!(
            "deployment discovery document {} has no public URL for target {}",
            request.discovery_url,
            request.target
        )
    })?;
    let artifact_url = validate_download_url(artifact_url, "envd artifact URL")?;
    if !artifact_url
        .path()
        .ends_with(&format!("/{}", artifact.file))
    {
        bail!("envd artifact URL does not end with its declared filename");
    }

    let (mut archive_file, actual_sha) =
        download_archive(&client, &artifact_url, MAX_ARCHIVE_BYTES).await?;
    if actual_sha != artifact.sha256 {
        bail!(
            "envd archive checksum mismatch: expected {}, downloaded {actual_sha}",
            artifact.sha256
        );
    }

    let parent = request.install_path.parent().ok_or_else(|| {
        anyhow!(
            "envd executable has no parent directory: {}",
            request.install_path.display()
        )
    })?;
    let mut candidate = tempfile::Builder::new()
        .prefix(".lightspeed-envd-upgrade-")
        .tempfile_in(parent)
        .map_err(|error| {
            anyhow!(
                "cannot write next to {}: {error}\n{}",
                request.install_path.display(),
                manual_install_command(
                    artifact_url.as_str(),
                    &artifact.sha256,
                    &request.install_path
                )
            )
        })?;
    extract_binary(&mut archive_file, candidate.as_file_mut())?;
    make_executable(candidate.path())?;
    candidate
        .as_file_mut()
        .sync_all()
        .context("sync candidate envd executable")?;
    // Close the write handle before executing the candidate: Linux refuses
    // to run a file that is still open for writing (ETXTBSY). The path stays
    // reserved and is removed on every failure path.
    let candidate = candidate.into_temp_path();

    let build = inspect_candidate(&candidate).await?;
    verify_candidate(&build, &discovery, &request.target)?;
    candidate.persist(&request.install_path).map_err(|error| {
        anyhow!(
            "replace {} atomically: {}\n{}",
            request.install_path.display(),
            error.error,
            manual_install_command(
                artifact_url.as_str(),
                &artifact.sha256,
                &request.install_path
            )
        )
    })?;
    std::fs::File::open(&request.install_path)
        .and_then(|installed| installed.sync_all())
        .context("sync installed envd executable")?;

    Ok(InstalledBuild {
        version: build.version,
        git_sha: build.git_sha,
        protocol_version: build.protocol_version,
        path: request.install_path,
    })
}

async fn http_client(ca_file: Option<&Path>) -> Result<reqwest::Client> {
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
            bail!("CA file {} holds no certificates", ca_file.display());
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder.build().context("build envd upgrade HTTP client")
}

async fn fetch_discovery(client: &reqwest::Client, url: &str) -> Result<DiscoveryDocument> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetch envd discovery document {url}"))?
        .error_for_status()
        .with_context(|| format!("fetch envd discovery document {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DISCOVERY_BYTES as u64)
    {
        bail!("envd discovery document exceeds {MAX_DISCOVERY_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read envd discovery document")?;
        if bytes.len().saturating_add(chunk.len()) > MAX_DISCOVERY_BYTES {
            bail!("envd discovery document exceeds {MAX_DISCOVERY_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("decode envd discovery document")
}

async fn download_archive(
    client: &reqwest::Client,
    url: &Url,
    max_bytes: u64,
) -> Result<(tempfile::NamedTempFile, String)> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("download envd archive {url}"))?
        .error_for_status()
        .with_context(|| format!("download envd archive {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        bail!("envd archive exceeds {max_bytes} bytes");
    }
    let mut file = tempfile::NamedTempFile::new().context("create temporary envd archive")?;
    let mut stream = response.bytes_stream();
    let mut size = 0_u64;
    let mut sha = Sha256::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read envd archive response")?;
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| anyhow!("envd archive size overflow"))?;
        if size > max_bytes {
            bail!("envd archive exceeds {max_bytes} bytes");
        }
        sha.update(&chunk);
        file.write_all(&chunk)
            .context("write temporary envd archive")?;
    }
    file.as_file_mut()
        .sync_all()
        .context("sync temporary envd archive")?;
    file.as_file_mut()
        .rewind()
        .context("rewind temporary envd archive")?;
    Ok((file, hex::encode(sha.finalize())))
}

fn extract_binary(
    archive_file: &mut tempfile::NamedTempFile,
    destination: &mut std::fs::File,
) -> Result<()> {
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
            bail!("envd archive contains unexpected entry {}", path.display());
        }
        if found {
            bail!("envd archive contains lightspeed-envd more than once");
        }
        if !entry.header().entry_type().is_file() {
            bail!("envd archive entry lightspeed-envd is not a regular file");
        }
        let size = entry.size();
        if size == 0 || size > MAX_BINARY_BYTES {
            bail!("envd executable has invalid size {size}");
        }
        let copied = std::io::copy(&mut entry.by_ref().take(MAX_BINARY_BYTES + 1), destination)
            .context("extract envd executable")?;
        if copied != size {
            bail!("envd executable size differs from its archive header");
        }
        found = true;
    }
    if !found {
        bail!("envd archive does not contain lightspeed-envd");
    }
    destination
        .flush()
        .context("flush candidate envd executable")
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("make candidate executable {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    bail!("envd self-upgrade is supported only on Unix")
}

async fn inspect_candidate(path: &Path) -> Result<CandidateBuild> {
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
        bail!(
            "candidate envd --print-build exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("decode candidate envd --print-build")
}

fn verify_candidate(
    build: &CandidateBuild,
    discovery: &DiscoveryDocument,
    target: &str,
) -> Result<()> {
    if build.name != "lightspeed-envd" {
        bail!(
            "candidate build name is {}, expected lightspeed-envd",
            build.name
        );
    }
    if build.version != discovery.version {
        bail!(
            "candidate version {} differs from discovery version {}",
            build.version,
            discovery.version
        );
    }
    if build.git_sha != discovery.git_sha {
        bail!(
            "candidate gitSha {} differs from discovery gitSha {}",
            build.git_sha,
            discovery.git_sha
        );
    }
    if build.target != target {
        bail!(
            "candidate target {} differs from requested target {target}",
            build.target
        );
    }
    if build.protocol_version != discovery.protocol_version {
        bail!(
            "candidate protocol {} differs from discovery protocol {}",
            build.protocol_version,
            discovery.protocol_version
        );
    }
    Ok(())
}

fn validate_artifact(artifact: &DiscoveryArtifact) -> Result<()> {
    if artifact.file.is_empty()
        || Path::new(&artifact.file)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(artifact.file.as_str())
    {
        bail!("envd artifact has an unsafe filename");
    }
    if !artifact.file.ends_with(".tar.gz") {
        bail!("envd artifact is not a .tar.gz archive");
    }
    if artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("envd artifact has an invalid SHA-256 checksum");
    }
    Ok(())
}

fn validate_git_sha(value: &str, label: &str) -> Result<()> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("{label} must be a full lowercase hexadecimal commit");
    }
    Ok(())
}

fn validate_download_url(value: &str, label: &str) -> Result<Url> {
    let url = Url::parse(value).with_context(|| format!("parse {label}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{label} must not contain credentials");
    }
    if url.fragment().is_some() {
        bail!("{label} must not contain a fragment");
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&url) => {}
        _ => bail!("{label} must use https (or http toward loopback)"),
    }
    if url.host_str().is_none() {
        bail!("{label} has no host");
    }
    Ok(url)
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
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
        let archive_sha = hex::encode(Sha256::digest(&archive));
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
        })
        .await
        .expect_err("protocol mismatch");
        server.abort();
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
        })
        .await
        .expect_err("checksum mismatch");
        server.await.expect("server");
        assert!(error.to_string().contains("archive checksum mismatch"));
        assert_eq!(
            std::fs::read(&install_path).expect("old executable"),
            b"old envd"
        );
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
