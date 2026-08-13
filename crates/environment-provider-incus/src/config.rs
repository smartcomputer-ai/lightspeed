use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context as _, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Parser, Debug)]
#[command(name = "lightspeed-provider-incus", version)]
pub struct ProviderArgs {
    #[arg(long, env = "LIGHTSPEED_INCUS_PROVIDER_CONFIG")]
    pub config: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawConfig {
    controller_listen: SocketAddr,
    incus: IncusConfig,
    templates: Vec<TemplatePolicy>,
    #[serde(default)]
    network: NetworkPolicy,
    relay_idle_seconds: Option<u64>,
    dial_timeout_seconds: Option<u64>,
    envd_port: Option<u16>,
    ingress: Option<IngressConfig>,
}

#[derive(Clone, Debug)]
pub struct Config(Arc<ConfigInner>);

#[derive(Debug)]
pub struct ConfigInner {
    pub controller_listen: SocketAddr,
    pub incus: IncusConfig,
    pub templates: BTreeMap<String, TemplatePolicy>,
    pub network: NetworkPolicy,
    pub relay_idle_seconds: u64,
    pub dial_timeout_seconds: u64,
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
    /// per-binding OVN networks in cluster mode.
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
    pub template_id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub image_fingerprint: String,
    pub cpu: u32,
    pub memory: String,
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
        let mut templates = BTreeMap::new();
        for template in raw.templates {
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
            if let Some(port) = template.ingress_port {
                if port == 0
                    || port == raw.envd_port.unwrap_or(19091)
                    || matches!(port, 22 | 2375 | 2376)
                {
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
            incus: raw.incus,
            templates,
            network: raw.network,
            relay_idle_seconds: raw.relay_idle_seconds.unwrap_or(60).max(1),
            dial_timeout_seconds: raw.dial_timeout_seconds.unwrap_or(10).max(1),
            envd_port: raw.envd_port.unwrap_or(19091),
            ingress: raw.ingress,
        })))
    }
    pub fn config_path(&self) -> &PathBuf {
        &self.config
    }
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
        bail!("incus cluster mode requires clusterNetworkUplink for OVN binding networks")
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
    pub fn template(&self, template_id: &str) -> anyhow::Result<&TemplatePolicy> {
        self.templates
            .get(template_id)
            .filter(|template| !template.deprecated)
            .ok_or_else(|| anyhow::anyhow!("template is not offered by this provider"))
    }

    pub fn incus_mode_name(&self) -> &'static str {
        match self.incus.mode {
            IncusMode::Single => "single",
            IncusMode::Cluster => "cluster",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn config_loads_provider_wide_templates_without_universe_bindings() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("provider.json");
        let document = serde_json::json!({
            "controllerListen":"127.0.0.1:0",
            "incus":{"mode":"single","endpoints":["https://incus.test"],"clientCertificatePem":"cert","clientPrivateKeyPem":"key","serverCaPem":"ca","storagePool":"default"},
            "network":{"deniedEgressCidrs":["100.64.0.0/10"]},
            "templates":[{"templateId":"dev-small-v1","displayName":"Small","imageFingerprint":"abc","cpu":2,"memory":"4GiB","disk":"40GiB"}]
        });
        tokio::fs::write(&path, serde_json::to_vec(&document).unwrap())
            .await
            .unwrap();
        let config = ProviderArgs {
            config: path.clone(),
        }
        .load()
        .await
        .expect("config");
        assert!(config.template("dev-small-v1").is_ok());
        assert_eq!(config.network.denied_egress_cidrs, ["100.64.0.0/10"]);
        let mut duplicate = document;
        let first_template = duplicate["templates"][0].clone();
        duplicate["templates"]
            .as_array_mut()
            .unwrap()
            .push(first_template);
        tokio::fs::write(&path, serde_json::to_vec(&duplicate).unwrap())
            .await
            .unwrap();
        assert!(ProviderArgs { config: path }.load().await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn config_rejects_invalid_provider_network_cidr() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("provider.json");
        let document = serde_json::json!({
            "controllerListen":"127.0.0.1:0",
            "incus":{"mode":"single","endpoints":["https://incus.test"],"clientCertificatePem":"cert","clientPrivateKeyPem":"key","serverCaPem":"ca","storagePool":"default"},
            "network":{"deniedEgressCidrs":["not-a-cidr"]},
            "templates":[{"templateId":"dev-small-v1","displayName":"Small","imageFingerprint":"abc","cpu":2,"memory":"4GiB","disk":"40GiB"}]
        });
        tokio::fs::write(&path, serde_json::to_vec(&document).unwrap())
            .await
            .unwrap();
        assert!(ProviderArgs { config: path }.load().await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn config_requires_explicit_valid_mode_topology() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("provider.json");
        let base = serde_json::json!({
            "controllerListen":"127.0.0.1:0",
            "incus":{"mode":"single","endpoints":["https://one.test","https://two.test"],"clientCertificatePem":"cert","clientPrivateKeyPem":"key","serverCaPem":"ca","storagePool":"default"},
            "network":{"deniedEgressCidrs":[]},
            "templates":[{"templateId":"t","displayName":"T","imageFingerprint":"abc","cpu":1,"memory":"1GiB","disk":"10GiB"}]
        });
        tokio::fs::write(&path, serde_json::to_vec(&base).unwrap())
            .await
            .unwrap();
        assert!(
            ProviderArgs {
                config: path.clone()
            }
            .load()
            .await
            .is_err()
        );

        let mut clustered = base;
        clustered["incus"]["mode"] = serde_json::json!("cluster");
        clustered["incus"]["clusterNetworkUplink"] = serde_json::json!("UPLINK");
        clustered["templates"][0]["clusterGroup"] = serde_json::json!("x86-production");
        tokio::fs::write(&path, serde_json::to_vec(&clustered).unwrap())
            .await
            .unwrap();
        let config = ProviderArgs { config: path }
            .load()
            .await
            .expect("cluster config");
        assert_eq!(config.incus_mode_name(), "cluster");
    }
}
