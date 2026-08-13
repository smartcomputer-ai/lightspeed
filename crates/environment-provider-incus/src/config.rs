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
    bindings: Vec<BindingPolicy>,
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
    pub bindings: BTreeMap<(String, String), BindingPolicy>,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingPolicy {
    pub universe_id: String,
    pub binding_id: String,
    pub ipv4_cidr: String,
    #[serde(default)]
    pub denied_egress_cidrs: Vec<String>,
    pub max_environments: Option<u32>,
    pub templates: Vec<TemplatePolicy>,
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
        let mut bindings = BTreeMap::new();
        for binding in raw.bindings {
            if binding.templates.is_empty() {
                bail!("binding {} has no templates", binding.binding_id)
            }
            let key = (binding.universe_id.clone(), binding.binding_id.clone());
            if bindings.insert(key, binding).is_some() {
                bail!("duplicate universe/binding policy")
            }
        }
        for binding in bindings.values() {
            for template in &binding.templates {
                if template.cluster_group.is_some() && raw.incus.mode != IncusMode::Cluster {
                    bail!(
                        "template {} selects a clusterGroup in single mode",
                        template.template_id
                    )
                }
                if let Some(group) = &template.cluster_group {
                    validate_cluster_group(group).with_context(|| {
                        format!("template {} clusterGroup", template.template_id)
                    })?;
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
            bindings,
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

impl std::ops::Deref for Config {
    type Target = ConfigInner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Config {
    pub fn binding(&self, universe: &str, binding: &str) -> anyhow::Result<&BindingPolicy> {
        self.bindings
            .get(&(universe.to_owned(), binding.to_owned()))
            .ok_or_else(|| anyhow::anyhow!("binding is not admitted by this provider"))
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
    async fn config_rejects_duplicate_bindings_and_preserves_nullable_quota() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("provider.json");
        let binding = serde_json::json!({
            "universeId":"00000000-0000-0000-0000-000000000001",
            "bindingId":"primary",
            "ipv4Cidr":"10.1.0.1/24",
            "templates":[{"templateId":"dev-small-v1","displayName":"Small","imageFingerprint":"abc","cpu":2,"memory":"4GiB","disk":"40GiB"}]
        });
        let document = serde_json::json!({
            "controllerListen":"127.0.0.1:0",
            "incus":{"mode":"single","endpoints":["https://incus.test"],"clientCertificatePem":"cert","clientPrivateKeyPem":"key","serverCaPem":"ca","storagePool":"default"},
            "bindings":[binding]
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
        assert_eq!(
            config
                .binding("00000000-0000-0000-0000-000000000001", "primary")
                .unwrap()
                .max_environments,
            None
        );
        let mut duplicate = document;
        let first_binding = duplicate["bindings"][0].clone();
        duplicate["bindings"]
            .as_array_mut()
            .unwrap()
            .push(first_binding);
        tokio::fs::write(&path, serde_json::to_vec(&duplicate).unwrap())
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
            "bindings":[{"universeId":"u","bindingId":"b","ipv4Cidr":"10.1.0.1/24","templates":[{"templateId":"t","displayName":"T","imageFingerprint":"abc","cpu":1,"memory":"1GiB","disk":"10GiB"}]}]
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
        clustered["bindings"][0]["templates"][0]["clusterGroup"] =
            serde_json::json!("x86-production");
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
