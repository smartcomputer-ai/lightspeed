use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context as _, bail};
use clap::Parser;
use environment_protocol::{
    bytes::SecretString,
    control::template::TemplateResources,
    idle::IdlePolicy,
    ids::TemplateId,
    limits::MIN_INTERMEDIARY_IDLE_MS,
    metadata::{DescriptiveText, DisplayName},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::incus::unsupported_idle_stage;

/// Shortest provider credential accepted, in bytes.
const MIN_CREDENTIAL_BYTES: usize = 32;

/// Default pause between two passes of the idle loop, in seconds.
const DEFAULT_IDLE_CHECK_SECONDS: u64 = 60;

#[derive(Parser, Debug)]
#[command(name = "lightspeed-provider-incus", version = release_info::LONG_VERSION)]
pub struct ProviderArgs {
    #[arg(long, env = "LIGHTSPEED_INCUS_PROVIDER_CONFIG")]
    pub config: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawConfig {
    controller_listen: SocketAddr,
    /// File holding the provider credential Lightspeed sends as
    /// `Authorization: Bearer` on `/control` and every data route. Without
    /// one the listener checks no credential.
    credential_file: Option<PathBuf>,
    incus: IncusConfig,
    templates: Vec<TemplatePolicy>,
    #[serde(default)]
    network: NetworkPolicy,
    relay_idle_seconds: Option<u64>,
    dial_timeout_seconds: Option<u64>,
    idle_check_seconds: Option<u64>,
    envd_port: Option<u16>,
    ingress: Option<IngressConfig>,
}

#[derive(Clone, Debug)]
pub struct Config(pub(crate) Arc<ConfigInner>);

#[derive(Debug)]
pub struct ConfigInner {
    pub controller_listen: SocketAddr,
    /// `None`: requests are not authenticated.
    pub credential: Option<ProviderCredential>,
    pub incus: IncusConfig,
    pub templates: BTreeMap<TemplateId, TemplatePolicy>,
    pub network: NetworkPolicy,
    pub relay_idle_seconds: u64,
    pub dial_timeout_seconds: u64,
    /// Pause between two passes of the idle loop.
    pub idle_check_seconds: u64,
    pub envd_port: u16,
    pub ingress: Option<IngressConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IncusConfig {
    pub mode: IncusMode,
    pub endpoints: Vec<String>,
    pub client_certificate_pem: PathBuf,
    pub client_private_key_pem: PathBuf,
    pub server_ca_pem: PathBuf,
    pub storage_pool: String,
    /// Existing physical/bridge uplink used when the provider creates its
    /// per-universe OVN networks in cluster mode.
    pub cluster_network_uplink: Option<String>,
    /// Trusted Incus image server used to lazily cache missing immutable
    /// fingerprints on the target node.
    pub image_server_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum IncusMode {
    Single,
    Cluster,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkPolicy {
    #[serde(default)]
    pub denied_egress_cidrs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplatePolicy {
    pub template_id: TemplateId,
    pub display_name: DisplayName,
    pub description: Option<DescriptiveText>,
    pub image_fingerprint: String,
    pub cpu: u32,
    /// Incus size string, such as `4GiB`.
    pub memory: String,
    /// Incus size string, such as `40GiB`.
    pub disk: String,
    /// Optional native Incus cluster group used as the hard placement set.
    #[serde(default)]
    pub cluster_group: Option<String>,
    #[serde(default)]
    pub public_ingress: bool,
    #[serde(default)]
    pub ingress_port: Option<u16>,
    #[serde(default)]
    pub deprecated: bool,
    /// Idle policy of targets whose `target/create` names none. Only stages
    /// this provider can carry out (pause, stop, close) are accepted.
    #[serde(default)]
    pub idle: Option<IdlePolicy>,
    /// `cpu`, `memory` and `disk` in provider-neutral units, derived at load.
    #[serde(skip)]
    pub resources: TemplateResources,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressConfig {
    pub public_base_url: String,
    pub listen: SocketAddr,
}

impl ProviderArgs {
    pub async fn load(self) -> anyhow::Result<Config> {
        let bytes = tokio::fs::read(&self.config)
            .await
            .with_context(|| format!("read {}", self.config.display()))?;
        let raw: RawConfig =
            serde_json::from_slice(&bytes).context("decode provider config JSON")?;
        validate_incus(&raw.incus)?;
        for cidr in &raw.network.denied_egress_cidrs {
            validate_ipv4_cidr(cidr)
                .with_context(|| format!("network deniedEgressCidrs entry {cidr}"))?;
        }
        if raw.templates.is_empty() {
            bail!("provider must configure at least one template")
        }
        let envd_port = raw.envd_port.unwrap_or(19091);
        let relay_idle_seconds = raw
            .relay_idle_seconds
            .unwrap_or(MIN_INTERMEDIARY_IDLE_MS / 1000);
        if relay_idle_seconds * 1000 < MIN_INTERMEDIARY_IDLE_MS {
            bail!(
                "relayIdleSeconds must be at least {}: relays never idle-close a data connection sooner",
                MIN_INTERMEDIARY_IDLE_MS / 1000
            )
        }
        let idle_check_seconds = raw.idle_check_seconds.unwrap_or(DEFAULT_IDLE_CHECK_SECONDS);
        if idle_check_seconds == 0 {
            bail!("idleCheckSeconds must be positive")
        }
        let credential = match &raw.credential_file {
            Some(path) => Some(ProviderCredential::load(path).await?),
            None => None,
        };
        let mut templates = BTreeMap::new();
        for mut template in raw.templates {
            if template.cluster_group.is_some() && raw.incus.mode != IncusMode::Cluster {
                bail!(
                    "template {} selects a clusterGroup in single mode",
                    template.template_id
                )
            }
            if let Some(group) = &template.cluster_group {
                validate_cluster_group(group)
                    .with_context(|| format!("template {} clusterGroup", template.template_id))?;
            }
            if template.public_ingress != template.ingress_port.is_some() {
                bail!(
                    "template {} must set both publicIngress=true and ingressPort, or neither",
                    template.template_id
                )
            }
            if let Some(idle) = &template.idle {
                validate_template_idle(idle)
                    .with_context(|| format!("template {} idle", template.template_id))?;
            }
            if let Some(port) = template.ingress_port {
                if port == 0 || port == envd_port || matches!(port, 22 | 2375 | 2376) {
                    bail!(
                        "template {} uses a reserved management port for ingress",
                        template.template_id
                    )
                }
                if raw.ingress.is_none() {
                    bail!(
                        "template {} permits ingress but provider ingress is not configured",
                        template.template_id
                    )
                }
            }
            template.resources = TemplateResources {
                vcpus: Some(template.cpu),
                memory_bytes: Some(
                    parse_incus_size(&template.memory)
                        .with_context(|| format!("template {} memory", template.template_id))?,
                ),
                disk_bytes: Some(
                    parse_incus_size(&template.disk)
                        .with_context(|| format!("template {} disk", template.template_id))?,
                ),
            };
            let template_id = template.template_id.clone();
            if templates.insert(template_id.clone(), template).is_some() {
                bail!("duplicate provider template: {template_id}")
            }
        }
        if let Some(ingress) = &raw.ingress {
            let url = reqwest::Url::parse(&ingress.public_base_url)
                .context("parse ingress publicBaseUrl")?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                bail!(
                    "ingress publicBaseUrl must be an HTTPS origin without path, query, or fragment"
                )
            }
        }
        Ok(Config(Arc::new(ConfigInner {
            controller_listen: raw.controller_listen,
            credential,
            incus: raw.incus,
            templates,
            network: raw.network,
            relay_idle_seconds,
            dial_timeout_seconds: raw.dial_timeout_seconds.unwrap_or(10).max(1),
            idle_check_seconds,
            envd_port,
            ingress: raw.ingress,
        })))
    }
    pub fn config_path(&self) -> &PathBuf {
        &self.config
    }
}

/// The provider credential: the bearer token Lightspeed presents on
/// `/control` and on every data route. `Debug` is redacted.
#[derive(Clone)]
pub struct ProviderCredential(SecretString);

impl ProviderCredential {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        if value.len() < MIN_CREDENTIAL_BYTES
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            bail!(
                "the provider credential must be at least {MIN_CREDENTIAL_BYTES} bytes without whitespace"
            )
        }
        Ok(Self(SecretString::new(value)))
    }

    async fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let contents = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("read credentialFile {}", path.display()))?;
        Self::new(contents.trim_end_matches(['\r', '\n']))
            .with_context(|| format!("credentialFile {}", path.display()))
    }

    pub fn expose(&self) -> &str {
        self.0.expose()
    }

    /// True when `presented` is this credential. Compares fixed-size digests
    /// so the time taken reveals neither the credential's length nor how
    /// much of it matched.
    pub fn matches(&self, presented: &str) -> bool {
        let expected = Sha256::digest(self.0.expose().as_bytes());
        let presented = Sha256::digest(presented.as_bytes());
        expected
            .iter()
            .zip(presented.iter())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
    }
}

impl std::fmt::Debug for ProviderCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderCredential(<redacted>)")
    }
}

/// A template's default idle policy: valid, and every power stage it sets
/// one this provider can move a target to.
fn validate_template_idle(idle: &IdlePolicy) -> anyhow::Result<()> {
    idle.validate()?;
    if let Some(action) = unsupported_idle_stage(idle) {
        bail!("the Incus provider cannot carry out the {action} stage")
    }
    Ok(())
}

/// Bytes of an Incus size string: a whole number with an optional unit
/// (`B`, decimal `kB` to `EB`, binary `KiB` to `EiB`), as Incus accepts
/// for `limits.memory` and disk `size`.
fn parse_incus_size(value: &str) -> anyhow::Result<u64> {
    const UNITS: [(&str, u64); 13] = [
        ("EiB", 1 << 60),
        ("PiB", 1 << 50),
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
        ("EB", 1_000_000_000_000_000_000),
        ("PB", 1_000_000_000_000_000),
        ("TB", 1_000_000_000_000),
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("kB", 1_000),
        ("B", 1),
    ];
    let (number, multiplier) = UNITS
        .iter()
        .find_map(|(unit, multiplier)| value.strip_suffix(unit).map(|number| (number, *multiplier)))
        .unwrap_or((value, 1));
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("must be a whole number of bytes with an optional Incus unit such as GiB: {value}")
    }
    number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .filter(|bytes| *bytes > 0 && *bytes < (1 << 53))
        .ok_or_else(|| anyhow::anyhow!("size is out of range: {value}"))
}

fn validate_incus(incus: &IncusConfig) -> anyhow::Result<()> {
    if incus.endpoints.is_empty() {
        bail!("incus.endpoints must contain at least one HTTPS endpoint")
    }
    if incus.mode == IncusMode::Single && incus.endpoints.len() != 1 {
        bail!("incus single mode requires exactly one endpoint")
    }
    let mut unique_endpoints = std::collections::BTreeSet::new();
    for endpoint in &incus.endpoints {
        let url = reqwest::Url::parse(endpoint).context("parse Incus endpoint")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("Incus endpoints must be HTTPS origins without path, query, or fragment")
        }
        if !unique_endpoints.insert(url.to_string()) {
            bail!("incus.endpoints must not contain duplicates")
        }
    }
    if incus.mode == IncusMode::Cluster
        && incus
            .cluster_network_uplink
            .as_deref()
            .is_none_or(str::is_empty)
    {
        bail!("incus cluster mode requires clusterNetworkUplink for OVN universe networks")
    }
    if incus.mode == IncusMode::Single && incus.cluster_network_uplink.is_some() {
        bail!("incus clusterNetworkUplink is only valid in cluster mode")
    }
    Ok(())
}

fn validate_cluster_group(group: &str) -> anyhow::Result<()> {
    if group.is_empty()
        || group.len() > 63
        || group
            .chars()
            .any(|character| character.is_whitespace() || "/?#&@".contains(character))
    {
        bail!("must be a non-empty Incus group name without URL delimiters")
    }
    Ok(())
}

fn validate_ipv4_cidr(cidr: &str) -> anyhow::Result<()> {
    let (address, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("must be an IPv4 CIDR"))?;
    address
        .parse::<std::net::Ipv4Addr>()
        .context("must contain a valid IPv4 address")?;
    let prefix = prefix
        .parse::<u8>()
        .context("must contain a numeric prefix")?;
    if prefix > 32 {
        bail!("IPv4 prefix must be between 0 and 32")
    }
    Ok(())
}

impl std::ops::Deref for Config {
    type Target = ConfigInner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Config {
    pub fn incus_mode_name(&self) -> &'static str {
        match self.incus.mode {
            IncusMode::Single => "single",
            IncusMode::Cluster => "cluster",
        }
    }
}

#[cfg(test)]
mod tests {
    use environment_protocol::idle::IdlePolicyError;

    use super::*;

    const CREDENTIAL: &str = "0123456789abcdef0123456789abcdef";

    fn base() -> serde_json::Value {
        serde_json::json!({
            "controllerListen":"127.0.0.1:0",
            "incus":{"mode":"single","endpoints":["https://incus.test"],"clientCertificatePem":"cert","clientPrivateKeyPem":"key","serverCaPem":"ca","storagePool":"default"},
            "network":{"deniedEgressCidrs":["100.64.0.0/10"]},
            "templates":[{"templateId":"dev-small-v1","displayName":"Small","imageFingerprint":"abc","cpu":2,"memory":"4GiB","disk":"40GiB"}]
        })
    }

    /// Write `document` and load it. With `credential`, `credentialFile`
    /// points at a fresh file holding it.
    async fn load(
        directory: &tempfile::TempDir,
        mut document: serde_json::Value,
        credential: Option<&str>,
    ) -> anyhow::Result<Config> {
        if let Some(credential) = credential {
            let credential_path = directory.path().join("credential");
            tokio::fs::write(&credential_path, format!("{credential}\n"))
                .await
                .unwrap();
            document["credentialFile"] = serde_json::json!(credential_path);
        }
        let path = directory.path().join("provider.json");
        tokio::fs::write(&path, serde_json::to_vec(&document).unwrap())
            .await
            .unwrap();
        ProviderArgs { config: path }.load().await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn config_loads_provider_wide_templates_without_universe_lists() {
        let directory = tempfile::tempdir().expect("tempdir");
        let config = load(&directory, base(), Some(CREDENTIAL))
            .await
            .expect("config");
        let template = &config.templates[&TemplateId::new("dev-small-v1").unwrap()];
        assert_eq!(
            template.resources,
            TemplateResources {
                vcpus: Some(2),
                memory_bytes: Some(4 << 30),
                disk_bytes: Some(40 << 30),
            }
        );
        assert_eq!(template.idle, None);
        assert_eq!(config.network.denied_egress_cidrs, ["100.64.0.0/10"]);
        assert_eq!(config.relay_idle_seconds, 60);
        assert_eq!(config.idle_check_seconds, 60);
        assert!(config.credential.is_some());

        let mut duplicate = base();
        let first_template = duplicate["templates"][0].clone();
        duplicate["templates"]
            .as_array_mut()
            .unwrap()
            .push(first_template);
        assert!(load(&directory, duplicate, Some(CREDENTIAL)).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn config_rejects_invalid_provider_network_cidr() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut document = base();
        document["network"]["deniedEgressCidrs"] = serde_json::json!(["not-a-cidr"]);
        assert!(load(&directory, document, None).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn config_requires_explicit_valid_mode_topology() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut document = base();
        document["incus"]["endpoints"] =
            serde_json::json!(["https://one.test", "https://two.test"]);
        assert!(load(&directory, document.clone(), None).await.is_err());

        document["incus"]["mode"] = serde_json::json!("cluster");
        document["incus"]["clusterNetworkUplink"] = serde_json::json!("UPLINK");
        document["templates"][0]["clusterGroup"] = serde_json::json!("x86-production");
        let config = load(&directory, document, None)
            .await
            .expect("cluster config");
        assert_eq!(config.incus_mode_name(), "cluster");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_configured_credential_must_be_strong() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(load(&directory, base(), Some("short")).await.is_err());
        assert!(
            load(
                &directory,
                base(),
                Some("0123456789abcdef 0123456789abcdef")
            )
            .await
            .is_err()
        );
        let mut missing_file = base();
        missing_file["credentialFile"] = serde_json::json!(directory.path().join("absent"));
        assert!(load(&directory, missing_file, None).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_credential_is_optional() {
        let directory = tempfile::tempdir().expect("tempdir");
        let config = load(&directory, base(), None)
            .await
            .expect("config without a credential");
        assert!(config.credential.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_data_route_base_is_not_provider_config() {
        // Lightspeed derives the data route base from the controller URL or
        // takes it from the provider record; the provider has no say.
        let directory = tempfile::tempdir().expect("tempdir");
        let mut document = base();
        document["dataRouteBase"] = serde_json::json!("ws://127.0.0.1:19090/routes");
        assert!(load(&directory, document, None).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn relays_never_idle_close_below_the_protocol_minimum() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut document = base();
        document["relayIdleSeconds"] = serde_json::json!(59);
        assert!(load(&directory, document.clone(), None).await.is_err());
        document["relayIdleSeconds"] = serde_json::json!(1900);
        let config = load(&directory, document, None).await.expect("config");
        assert_eq!(config.relay_idle_seconds, 1900);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_idle_check_interval_is_positive() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut document = base();
        document["idleCheckSeconds"] = serde_json::json!(0);
        assert!(load(&directory, document.clone(), None).await.is_err());
        document["idleCheckSeconds"] = serde_json::json!(15);
        let config = load(&directory, document, None).await.expect("config");
        assert_eq!(config.idle_check_seconds, 15);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn template_idle_policies_are_validated_at_load() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut document = base();
        document["templates"][0]["idle"] =
            serde_json::json!({"pauseAfterMs": 600000, "stopAfterMs": 3600000});
        let config = load(&directory, document, None).await.expect("config");
        assert_eq!(
            config.templates[&TemplateId::new("dev-small-v1").unwrap()].idle,
            Some(IdlePolicy {
                pause_after_ms: Some(600_000),
                stop_after_ms: Some(3_600_000),
                ..IdlePolicy::default()
            })
        );

        for idle in [
            serde_json::json!({}),
            serde_json::json!({"pauseAfterMs": 0}),
            serde_json::json!({"pauseAfterMs": 600000, "stopAfterMs": 60000}),
            // Incus targets cannot be suspended.
            serde_json::json!({"suspendAfterMs": 600000}),
            serde_json::json!({"pauseAfterMs": 600000, "unknown": 1}),
        ] {
            let mut document = base();
            document["templates"][0]["idle"] = idle.clone();
            assert!(load(&directory, document, None).await.is_err(), "{idle}");
        }
        assert_eq!(
            validate_template_idle(&IdlePolicy::default())
                .unwrap_err()
                .downcast::<IdlePolicyError>()
                .unwrap(),
            IdlePolicyError::Empty
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn template_sizes_and_descriptions_are_validated_at_load() {
        let directory = tempfile::tempdir().expect("tempdir");
        for (field, value) in [
            ("memory", serde_json::json!("50%")),
            ("disk", serde_json::json!("lots")),
            ("displayName", serde_json::json!("")),
            ("description", serde_json::json!("a\u{0}b")),
            ("templateId", serde_json::json!("")),
        ] {
            let mut document = base();
            document["templates"][0][field] = value;
            assert!(load(&directory, document, None).await.is_err(), "{field}");
        }
    }

    #[test]
    fn incus_sizes_parse_decimal_and_binary_units() {
        assert_eq!(parse_incus_size("4GiB").unwrap(), 4 << 30);
        assert_eq!(parse_incus_size("512MB").unwrap(), 512_000_000);
        assert_eq!(parse_incus_size("1024").unwrap(), 1024);
        assert!(parse_incus_size("GiB").is_err());
        assert!(parse_incus_size("1.5GiB").is_err());
        assert!(parse_incus_size("0").is_err());
    }

    #[test]
    fn credential_comparison_needs_the_exact_value() {
        let credential = ProviderCredential::new(CREDENTIAL).unwrap();
        assert!(credential.matches(CREDENTIAL));
        assert!(!credential.matches(&CREDENTIAL[1..]));
        assert!(!credential.matches(""));
        assert!(!format!("{credential:?}").contains(CREDENTIAL));
    }
}
