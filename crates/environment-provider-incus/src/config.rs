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
    controller_token: Option<String>,
    daemon_token_key: String,
    incus: IncusConfig,
    bindings: Vec<BindingPolicy>,
    relay_idle_seconds: Option<u64>,
    dial_timeout_seconds: Option<u64>,
    envd_port: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct Config(Arc<ConfigInner>);

#[derive(Debug)]
pub struct ConfigInner {
    pub controller_listen: SocketAddr,
    pub controller_token: Option<String>,
    pub daemon_token_key: String,
    pub incus: IncusConfig,
    pub bindings: BTreeMap<(String, String), BindingPolicy>,
    pub relay_idle_seconds: u64,
    pub dial_timeout_seconds: u64,
    pub envd_port: u16,
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
    pub deprecated: bool,
}

impl ProviderArgs {
    pub async fn load(self) -> anyhow::Result<Config> {
        let bytes = tokio::fs::read(&self.config)
            .await
            .with_context(|| format!("read {}", self.config.display()))?;
        let raw: RawConfig =
            serde_json::from_slice(&bytes).context("decode provider config JSON")?;
        if raw.daemon_token_key.is_empty() {
            bail!("daemonTokenKey must not be empty")
        }
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
        Ok(Config(Arc::new(ConfigInner {
            controller_listen: raw.controller_listen,
            controller_token: raw.controller_token.filter(|value| !value.is_empty()),
            daemon_token_key: raw.daemon_token_key,
            incus: raw.incus,
            bindings,
            relay_idle_seconds: raw.relay_idle_seconds.unwrap_or(60).max(1),
            dial_timeout_seconds: raw.dial_timeout_seconds.unwrap_or(10).max(1),
            envd_port: raw.envd_port.unwrap_or(19091),
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
            "controllerListen":"127.0.0.1:0","daemonTokenKey":"secret",
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
        assert!(config.controller_token.is_none());
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
