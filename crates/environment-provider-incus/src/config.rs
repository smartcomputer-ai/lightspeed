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
    pub base_url: String,
    pub client_certificate_pem: PathBuf,
    pub client_private_key_pem: PathBuf,
    pub server_ca_pem: PathBuf,
    pub storage_pool: String,
    /// Trusted Incus image server used to lazily cache missing immutable
    /// fingerprints on the target node.
    pub image_server_url: Option<String>,
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
            "incus":{"baseUrl":"https://incus.test","clientCertificatePem":"cert","clientPrivateKeyPem":"key","serverCaPem":"ca","storagePool":"default"},
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
}
