use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::Context as _;
use async_trait::async_trait;
use reqwest::{Certificate, Client, Identity, Method, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;

use host_protocol::{
    control::targets::{CreateTargetParams, HostTargetStatus},
    shared::HostTargetId,
};

use crate::{
    config::{BindingPolicy, Config, IncusMode, TemplatePolicy},
    policy,
};

#[derive(Clone, Debug)]
pub struct OwnedTarget {
    pub target_id: HostTargetId,
    pub name: String,
    pub universe_id: String,
    pub binding_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
    pub request_id: String,
    pub template_id: String,
    pub image_fingerprint: String,
    pub status: HostTargetStatus,
    pub ipv4_address: Option<String>,
    pub ingress_hostname: Option<String>,
    pub ingress_port: Option<u16>,
    /// Current Incus cluster member. Diagnostic only; target identity remains
    /// the cluster-wide instance name.
    pub location: Option<String>,
}

#[async_trait]
pub trait IncusBackend: Clone + Send + Sync + 'static {
    async fn topology(&self) -> anyhow::Result<IncusTopology>;
    async fn reconcile_binding(&self, binding: &BindingPolicy) -> anyhow::Result<()>;
    async fn ensure_image(&self, fingerprint: &str) -> anyhow::Result<()>;
    async fn list_owned(&self, binding: &BindingPolicy) -> anyhow::Result<Vec<OwnedTarget>>;
    async fn get_owned(
        &self,
        binding: &BindingPolicy,
        target_id: &HostTargetId,
    ) -> anyhow::Result<Option<OwnedTarget>>;
    async fn create_vm(
        &self,
        binding: &BindingPolicy,
        template: &TemplatePolicy,
        params: &CreateTargetParams,
    ) -> anyhow::Result<OwnedTarget>;
    async fn delete_vm(
        &self,
        binding: &BindingPolicy,
        target: &OwnedTarget,
        force: bool,
    ) -> anyhow::Result<()>;
    async fn set_ingress(
        &self,
        binding: &BindingPolicy,
        target: &OwnedTarget,
        hostname: Option<&str>,
        port: Option<u16>,
    ) -> anyhow::Result<OwnedTarget>;
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncusTopology {
    pub mode: &'static str,
    pub members: Vec<IncusMemberStatus>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncusMemberStatus {
    pub name: String,
    pub status: String,
    pub schedulable: bool,
}

#[derive(Clone)]
pub struct IncusClient {
    config: Config,
    http: Client,
    preferred_endpoint: Arc<AtomicUsize>,
}

impl IncusClient {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let cert = tokio::fs::read(&config.incus.client_certificate_pem).await?;
        let key = tokio::fs::read(&config.incus.client_private_key_pem).await?;
        let mut identity = cert;
        identity.extend_from_slice(&key);
        let ca = tokio::fs::read(&config.incus.server_ca_pem).await?;
        let http = Client::builder()
            .identity(Identity::from_pem(&identity)?)
            .add_root_certificate(Certificate::from_pem(&ca)?)
            .https_only(true)
            .timeout(Duration::from_secs(60))
            .build()?;
        let client = Self {
            config,
            http,
            preferred_endpoint: Arc::new(AtomicUsize::new(0)),
        };
        client.validate_topology().await?;
        Ok(client)
    }

    async fn validate_topology(&self) -> anyhow::Result<()> {
        let mut observed_members: Option<Vec<(String, String)>> = None;
        let mut reachable = 0usize;
        let mut errors = Vec::new();
        for endpoint in &self.config.incus.endpoints {
            let cluster = match self
                .request_at::<ClusterInfo>(endpoint, Method::GET, "/cluster", None, None)
                .await
            {
                Ok(cluster) => cluster,
                Err(error) if error.retryable => {
                    errors.push(format!("{endpoint}: {error}"));
                    continue;
                }
                Err(error) => return Err(error.source),
            };
            reachable += 1;
            match self.config.incus.mode {
                IncusMode::Single if cluster.enabled => {
                    anyhow::bail!("Incus single mode endpoint belongs to a cluster: {endpoint}")
                }
                IncusMode::Single => {}
                IncusMode::Cluster if !cluster.enabled => {
                    anyhow::bail!("Incus cluster mode endpoint is not clustered: {endpoint}")
                }
                IncusMode::Cluster => {
                    let mut members: Vec<(String, String)> = self
                        .request_at::<Vec<ClusterMember>>(
                            endpoint,
                            Method::GET,
                            "/cluster/members?recursion=1",
                            None,
                            None,
                        )
                        .await?
                        .into_iter()
                        .map(|member| (member.server_name, member.url))
                        .collect();
                    members.sort();
                    if members.is_empty() {
                        anyhow::bail!("Incus cluster endpoint reports no members: {endpoint}")
                    }
                    if let Some(expected) = &observed_members
                        && expected != &members
                    {
                        anyhow::bail!(
                            "configured Incus endpoints do not expose the same cluster membership"
                        )
                    }
                    observed_members = Some(members);
                }
            }
        }
        if reachable == 0 {
            anyhow::bail!(
                "no configured Incus endpoint is reachable: {}",
                errors.join("; ")
            )
        }
        if let Some(members) = observed_members
            && members.len() == 2
        {
            eprintln!(
                "warning: Incus cluster has two members; loss of one voter can remove quorum"
            );
        }
        Ok(())
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        project: Option<&str>,
        body: Option<Value>,
    ) -> anyhow::Result<T> {
        let mut errors = Vec::new();
        let endpoints = &self.config.incus.endpoints;
        let preferred = self.preferred_endpoint.load(Ordering::Relaxed) % endpoints.len();
        for offset in 0..endpoints.len() {
            let index = (preferred + offset) % endpoints.len();
            let endpoint = &endpoints[index];
            match self
                .request_at(endpoint, method.clone(), path, project, body.clone())
                .await
            {
                Ok(value) => {
                    self.preferred_endpoint.store(index, Ordering::Relaxed);
                    return Ok(value);
                }
                Err(error) if error.retryable => errors.push(format!("{endpoint}: {error}")),
                Err(error) => return Err(error.source),
            }
        }
        anyhow::bail!(
            "all configured Incus endpoints failed: {}",
            errors.join("; ")
        )
    }

    async fn request_at<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        method: Method,
        path: &str,
        project: Option<&str>,
        body: Option<Value>,
    ) -> Result<T, IncusRequestFailure> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let suffix = project
            .map(|project| format!("{separator}project={project}"))
            .unwrap_or_default();
        let url = format!("{}/1.0{}{}", endpoint.trim_end_matches('/'), path, suffix);
        let mut request = self.http.request(method, url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(IncusRequestFailure::reqwest)?;
        let status = response.status();
        let envelope: Envelope<T> = response
            .json()
            .await
            .map_err(IncusRequestFailure::reqwest)?;
        if !status.is_success() {
            return Err(IncusRequestFailure::response(
                status.is_server_error(),
                anyhow::anyhow!(
                    "Incus request failed ({status}): {}",
                    envelope
                        .error
                        .unwrap_or_else(|| envelope.status.unwrap_or_default())
                ),
            ));
        }
        let had_operation = envelope.operation.is_some();
        if let Some(operation) = envelope.operation {
            self.wait_operation(endpoint, &operation, project)
                .await
                .map_err(IncusRequestFailure::non_retryable)?;
        }
        match envelope.metadata {
            Some(metadata) => Ok(metadata),
            None if had_operation => serde_json::from_value(Value::Null)
                .context("decode empty successful Incus operation response")
                .map_err(IncusRequestFailure::non_retryable),
            None => Err(IncusRequestFailure::non_retryable(anyhow::anyhow!(
                "Incus response has no metadata"
            ))),
        }
    }

    async fn request_unit(
        &self,
        method: Method,
        path: &str,
        project: Option<&str>,
        body: Option<Value>,
    ) -> anyhow::Result<()> {
        let _: Value = self.request(method, path, project, body).await?;
        Ok(())
    }

    async fn exists(&self, path: &str, project: Option<&str>) -> anyhow::Result<bool> {
        let mut errors = Vec::new();
        let endpoints = &self.config.incus.endpoints;
        let preferred = self.preferred_endpoint.load(Ordering::Relaxed) % endpoints.len();
        for offset in 0..endpoints.len() {
            let index = (preferred + offset) % endpoints.len();
            let endpoint = &endpoints[index];
            let separator = if path.contains('?') { '&' } else { '?' };
            let suffix = project
                .map(|project| format!("{separator}project={project}"))
                .unwrap_or_default();
            let url = format!("{}/1.0{}{}", endpoint.trim_end_matches('/'), path, suffix);
            match self.http.get(url).send().await {
                Ok(response) if response.status() == StatusCode::NOT_FOUND => {
                    self.preferred_endpoint.store(index, Ordering::Relaxed);
                    return Ok(false);
                }
                Ok(response) if response.status().is_success() => {
                    self.preferred_endpoint.store(index, Ordering::Relaxed);
                    return Ok(true);
                }
                Ok(response) if response.status().is_server_error() => {
                    errors.push(format!("{endpoint}: HTTP {}", response.status()))
                }
                Ok(response) => {
                    anyhow::bail!("Incus existence check failed: {}", response.status())
                }
                Err(error) => errors.push(format!("{endpoint}: {error}")),
            }
        }
        anyhow::bail!(
            "all configured Incus endpoints failed: {}",
            errors.join("; ")
        )
    }

    async fn wait_operation(
        &self,
        initiating_endpoint: &str,
        operation: &str,
        project: Option<&str>,
    ) -> anyhow::Result<()> {
        let operation = operation.strip_prefix("/1.0").unwrap_or(operation);
        let project = project
            .map(|project| format!("&project={project}"))
            .unwrap_or_default();
        let mut endpoints = Vec::with_capacity(self.config.incus.endpoints.len());
        endpoints.push(initiating_endpoint);
        endpoints.extend(
            self.config
                .incus
                .endpoints
                .iter()
                .map(String::as_str)
                .filter(|endpoint| *endpoint != initiating_endpoint),
        );
        let mut errors = Vec::new();
        for endpoint in endpoints {
            let url = format!(
                "{}/1.0{operation}/wait?timeout=60{project}",
                endpoint.trim_end_matches('/')
            );
            let response = match self.http.get(url).send().await {
                Ok(response) => response,
                Err(error) if error.is_connect() || error.is_timeout() || error.is_body() => {
                    errors.push(format!("{endpoint}: {error}"));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if response.status().is_server_error() {
                errors.push(format!("{endpoint}: HTTP {}", response.status()));
                continue;
            }
            if !response.status().is_success() {
                anyhow::bail!("Incus operation failed: {}", response.status())
            }
            let envelope: Envelope<Value> = response.json().await?;
            if envelope
                .error
                .as_deref()
                .is_some_and(|error| !error.is_empty())
            {
                anyhow::bail!("Incus operation failed: {}", envelope.error.unwrap())
            }
            if let Some(metadata) = envelope.metadata {
                let failed = metadata
                    .get("status_code")
                    .and_then(Value::as_u64)
                    .is_some_and(|code| code >= 400)
                    || metadata
                        .get("err")
                        .and_then(Value::as_str)
                        .is_some_and(|error| !error.is_empty());
                if failed {
                    anyhow::bail!(
                        "Incus operation failed: {}",
                        metadata
                            .get("err")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown operation failure")
                    )
                }
            }
            return Ok(());
        }
        anyhow::bail!("all Incus operation waits failed: {}", errors.join("; "))
    }

    async fn ensure_resource(
        &self,
        path: &str,
        collection: &str,
        project: Option<&str>,
        body: Value,
    ) -> anyhow::Result<()> {
        if !self.exists(path, project).await? {
            if let Err(error) = self
                .request_unit(Method::POST, collection, project, Some(body))
                .await
            {
                if self.exists(path, project).await? {
                    return Ok(());
                }
                return Err(error);
            }
        }
        Ok(())
    }

    async fn instance(&self, project: &str, name: &str) -> anyhow::Result<Option<OwnedTarget>> {
        if !self
            .exists(&format!("/instances/{name}"), Some(project))
            .await?
        {
            return Ok(None);
        }
        let instance: Instance = self
            .request(
                Method::GET,
                &format!("/instances/{name}"),
                Some(project),
                None,
            )
            .await?;
        let state: InstanceState = self
            .request(
                Method::GET,
                &format!("/instances/{name}/state"),
                Some(project),
                None,
            )
            .await
            .unwrap_or_default();
        owned_from_instance(instance, state).map(Some)
    }
}

#[async_trait]
impl IncusBackend for IncusClient {
    async fn topology(&self) -> anyhow::Result<IncusTopology> {
        if self.config.incus.mode == IncusMode::Single {
            let _: Value = self.request(Method::GET, "", None, None).await?;
            return Ok(IncusTopology {
                mode: "single",
                members: vec![IncusMemberStatus {
                    name: "standalone".to_owned(),
                    status: "Online".to_owned(),
                    schedulable: true,
                }],
            });
        }
        let mut members: Vec<ClusterMember> = self
            .request(Method::GET, "/cluster/members?recursion=1", None, None)
            .await?;
        members.sort_by(|left, right| left.server_name.cmp(&right.server_name));
        Ok(IncusTopology {
            mode: "cluster",
            members: members
                .into_iter()
                .map(|member| IncusMemberStatus {
                    name: member.server_name,
                    schedulable: member.status.eq_ignore_ascii_case("online")
                        && member
                            .config
                            .get("scheduler.instance")
                            .is_none_or(|mode| mode != "manual"),
                    status: member.status,
                })
                .collect(),
        })
    }

    async fn reconcile_binding(&self, binding: &BindingPolicy) -> anyhow::Result<()> {
        let project = policy::project_name(binding);
        self.ensure_resource(&format!("/projects/{project}"), "/projects", None, json!({"name":project,"description":"Lightspeed managed binding","config":{"features.images":"true","features.networks":"true","features.profiles":"true","restricted":"true","restricted.devices.nic":"managed","restricted.devices.disk":"managed","restricted.networks.access":policy::network_name(binding)}})).await?;
        let denied = binding.denied_egress_cidrs.iter().chain(
            self.config
                .bindings
                .values()
                .filter(|other| {
                    other.universe_id != binding.universe_id
                        || other.binding_id != binding.binding_id
                })
                .map(|other| &other.ipv4_cidr),
        );
        let egress: Vec<Value> = denied.map(|destination| json!({"action":"reject","state":"enabled","destination":destination,"description":"block trusted/control or sibling binding network"})).collect();
        self.ensure_resource(&format!("/network-acls/{}", policy::acl_name(binding)), "/network-acls", Some(&project), json!({"name":policy::acl_name(binding),"description":"Lightspeed binding baseline","ingress":[],"egress":egress,"config":{}})).await?;
        let network = match self.config.incus.mode {
            IncusMode::Single => {
                json!({"name":policy::network_name(binding),"type":"bridge","config":{"ipv4.address":binding.ipv4_cidr,"ipv4.nat":"true","ipv6.address":"none","security.acls":policy::acl_name(binding),"security.acls.default.ingress.action":"allow","security.acls.default.egress.action":"allow"}})
            }
            IncusMode::Cluster => {
                json!({"name":policy::network_name(binding),"type":"ovn","config":{"network":self.config.incus.cluster_network_uplink.as_deref().expect("validated cluster uplink"),"ipv4.address":binding.ipv4_cidr,"ipv4.nat":"true","ipv6.address":"none","security.acls":policy::acl_name(binding),"security.acls.default.ingress.action":"allow","security.acls.default.egress.action":"allow"}})
            }
        };
        self.ensure_resource(
            &format!("/networks/{}", policy::network_name(binding)),
            "/networks",
            Some(&project),
            network,
        )
        .await?;
        self.ensure_resource(&format!("/profiles/{}", policy::profile_name(binding)), "/profiles", Some(&project), json!({"name":policy::profile_name(binding),"description":"Lightspeed VM baseline","config":{},"devices":{"eth0":{"type":"nic","network":policy::network_name(binding),"security.port_isolation":"true","security.ipv4_filtering":"true"},"root":{"type":"disk","pool":self.config.incus.storage_pool,"path":"/"}}})).await
    }

    async fn ensure_image(&self, fingerprint: &str) -> anyhow::Result<()> {
        if self.exists(&format!("/images/{fingerprint}"), None).await? {
            return Ok(());
        }
        let server = self.config.incus.image_server_url.as_deref().ok_or_else(|| anyhow::anyhow!("immutable Incus image fingerprint is not cached and no imageServerUrl is configured: {fingerprint}"))?;
        if let Err(error) = self.request_unit(Method::POST,"/images",None,Some(json!({"source":{"type":"image","mode":"pull","protocol":"incus","server":server,"fingerprint":fingerprint},"public":false,"auto_update":false}))).await
            && !self.exists(&format!("/images/{fingerprint}"), None).await?
        {
            return Err(error);
        }
        if !self.exists(&format!("/images/{fingerprint}"), None).await? {
            anyhow::bail!("cached Incus image does not match requested fingerprint: {fingerprint}")
        }
        Ok(())
    }

    async fn list_owned(&self, binding: &BindingPolicy) -> anyhow::Result<Vec<OwnedTarget>> {
        let project = policy::project_name(binding);
        let instances: Vec<Instance> = self
            .request(Method::GET, "/instances?recursion=1", Some(&project), None)
            .await?;
        let mut targets = Vec::new();
        for instance in instances {
            let state = self
                .request(
                    Method::GET,
                    &format!("/instances/{}/state", instance.name),
                    Some(&project),
                    None,
                )
                .await
                .unwrap_or_default();
            if let Ok(target) = owned_from_instance(instance, state) {
                targets.push(target);
            }
        }
        Ok(targets)
    }

    async fn get_owned(
        &self,
        binding: &BindingPolicy,
        target_id: &HostTargetId,
    ) -> anyhow::Result<Option<OwnedTarget>> {
        self.instance(&policy::project_name(binding), target_id.as_str())
            .await
    }

    async fn create_vm(
        &self,
        binding: &BindingPolicy,
        template: &TemplatePolicy,
        params: &CreateTargetParams,
    ) -> anyhow::Result<OwnedTarget> {
        let project = policy::project_name(binding);
        let name = policy::instance_name(
            &binding.universe_id,
            &binding.binding_id,
            &params.environment_id,
            &params.incarnation_id,
        );
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .to_string();
        let cloud_init = format!(
            "#cloud-config\nwrite_files:\n  - path: /etc/lightspeed-envd/provider.env\n    permissions: '0600'\n    content: |\n      LIGHTSPEED_ENVD_LISTEN=0.0.0.0:{}\n      LIGHTSPEED_ENVD_CWD=/workspace\n      LIGHTSPEED_ENVD_FS_ROOT=/\nruncmd:\n  - [mkdir, -p, /workspace]\n  - [systemctl, enable, --now, lightspeed-envd]\n",
            self.config.envd_port
        );
        let config = BTreeMap::from([
            ("limits.cpu".to_owned(), template.cpu.to_string()),
            ("limits.memory".to_owned(), template.memory.clone()),
            ("cloud-init.user-data".to_owned(), cloud_init),
            ("user.lightspeed.managed".to_owned(), "true".to_owned()),
            (
                "user.lightspeed.universe".to_owned(),
                params.binding.universe_id.clone(),
            ),
            (
                "user.lightspeed.binding".to_owned(),
                params.binding.binding_id.clone(),
            ),
            (
                "user.lightspeed.environment".to_owned(),
                params.environment_id.clone(),
            ),
            (
                "user.lightspeed.incarnation".to_owned(),
                params.incarnation_id.clone(),
            ),
            (
                "user.lightspeed.request".to_owned(),
                params.request_id.clone(),
            ),
            (
                "user.lightspeed.template".to_owned(),
                template.template_id.clone(),
            ),
            (
                "user.lightspeed.image".to_owned(),
                template.image_fingerprint.clone(),
            ),
            ("user.lightspeed.created_at_ms".to_owned(), created_at),
        ]);
        let path = if let Some(group) = &template.cluster_group {
            if !self
                .exists(&format!("/cluster/groups/{group}"), None)
                .await?
            {
                anyhow::bail!("configured Incus cluster group does not exist: {group}")
            }
            format!("/instances?target=@{group}")
        } else {
            "/instances".to_owned()
        };
        if let Err(create_error) = self.request_unit(Method::POST, &path, Some(&project), Some(json!({"name":name,"type":"virtual-machine","start":false,"profiles":[policy::profile_name(binding)],"source":{"type":"image","fingerprint":template.image_fingerprint},"config":config,"devices":{"root":{"type":"disk","pool":self.config.incus.storage_pool,"path":"/","size":template.disk}}}))).await {
            // A transport can fail after Incus accepted the deterministic
            // create. Resolve that uncertainty before returning an error.
            if let Some(target) = self.instance(&project, &name).await? {
                return Ok(target);
            }
            return Err(create_error);
        }
        if let Err(error) = self
            .request_unit(
                Method::PUT,
                &format!("/instances/{name}/state"),
                Some(&project),
                Some(json!({"action":"start","timeout":30,"force":false})),
            )
            .await
        {
            let observed = self.instance(&project, &name).await?;
            if !observed.as_ref().is_some_and(|target| {
                matches!(
                    target.status,
                    HostTargetStatus::Starting | HostTargetStatus::Ready
                )
            }) {
                return Err(error);
            }
        }
        self.instance(&project, &name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("created Incus instance disappeared"))
    }

    async fn delete_vm(
        &self,
        binding: &BindingPolicy,
        target: &OwnedTarget,
        force: bool,
    ) -> anyhow::Result<()> {
        let project = policy::project_name(binding);
        if force {
            let _ = self
                .request_unit(
                    Method::PUT,
                    &format!("/instances/{}/state", target.name),
                    Some(&project),
                    Some(json!({"action":"stop","timeout":0,"force":true})),
                )
                .await;
        }
        if let Err(error) = self
            .request_unit(
                Method::DELETE,
                &format!("/instances/{}", target.name),
                Some(&project),
                None,
            )
            .await
        {
            if self.instance(&project, &target.name).await?.is_some() {
                return Err(error);
            }
        }
        Ok(())
    }

    async fn set_ingress(
        &self,
        binding: &BindingPolicy,
        target: &OwnedTarget,
        hostname: Option<&str>,
        port: Option<u16>,
    ) -> anyhow::Result<OwnedTarget> {
        if hostname.is_some() != port.is_some() {
            anyhow::bail!("ingress hostname and port must be set together")
        }
        let project = policy::project_name(binding);
        let instance: Instance = self
            .request(
                Method::GET,
                &format!("/instances/{}", target.name),
                Some(&project),
                None,
            )
            .await?;
        let mut config = instance.config;
        match (hostname, port) {
            (Some(hostname), Some(port)) => {
                config.insert(
                    "user.lightspeed.ingress_hostname".to_owned(),
                    hostname.to_owned(),
                );
                config.insert("user.lightspeed.ingress_port".to_owned(), port.to_string());
            }
            _ => {
                config.remove("user.lightspeed.ingress_hostname");
                config.remove("user.lightspeed.ingress_port");
            }
        }
        let update = self.request_unit(Method::PUT, &format!("/instances/{}", target.name), Some(&project), Some(json!({"config":config,"description":instance.description,"profiles":instance.profiles,"devices":instance.devices,"ephemeral":instance.ephemeral}))).await;
        let observed = self
            .instance(&project, &target.name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("target disappeared while updating ingress"))?;
        if observed.ingress_hostname.as_deref() != hostname || observed.ingress_port != port {
            return Err(update.err().unwrap_or_else(|| {
                anyhow::anyhow!("Incus did not realize the requested ingress metadata")
            }));
        }
        Ok(observed)
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    metadata: Option<T>,
    operation: Option<String>,
    error: Option<String>,
    status: Option<String>,
}
#[derive(Deserialize)]
struct Instance {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    profiles: Vec<String>,
    #[serde(default)]
    devices: BTreeMap<String, Value>,
    #[serde(default)]
    ephemeral: bool,
    #[serde(default)]
    status: String,
    #[serde(default)]
    config: BTreeMap<String, String>,
    #[serde(default)]
    location: String,
}

#[derive(Deserialize)]
struct ClusterInfo {
    #[serde(default)]
    enabled: bool,
}

#[derive(Deserialize)]
struct ClusterMember {
    server_name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    config: BTreeMap<String, String>,
}

#[derive(Debug, Error)]
#[error("{source}")]
struct IncusRequestFailure {
    retryable: bool,
    #[source]
    source: anyhow::Error,
}

impl IncusRequestFailure {
    fn reqwest(error: reqwest::Error) -> Self {
        let retryable = error.is_connect() || error.is_timeout() || error.is_body();
        Self {
            retryable,
            source: error.into(),
        }
    }

    fn non_retryable(error: impl Into<anyhow::Error>) -> Self {
        Self {
            retryable: false,
            source: error.into(),
        }
    }

    fn response(retryable: bool, error: anyhow::Error) -> Self {
        Self {
            retryable,
            source: error,
        }
    }
}
#[derive(Default, Deserialize)]
struct InstanceState {
    #[serde(default)]
    network: BTreeMap<String, NetworkState>,
}
#[derive(Default, Deserialize)]
struct NetworkState {
    #[serde(default)]
    addresses: Vec<Address>,
}
#[derive(Deserialize)]
struct Address {
    family: String,
    scope: String,
    address: String,
}

fn owned_from_instance(instance: Instance, state: InstanceState) -> anyhow::Result<OwnedTarget> {
    let name = instance.name.clone();
    let get = |key: &str| {
        instance
            .config
            .get(&format!("{}{}", policy::META_PREFIX, key))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("instance {name} is not owned: missing {key}"))
    };
    if get("managed")? != "true" {
        anyhow::bail!("instance is not Lightspeed-managed")
    }
    let universe_id = get("universe")?;
    let binding_id = get("binding")?;
    let environment_id = get("environment")?;
    let incarnation_id = get("incarnation")?;
    if instance.name
        != policy::instance_name(&universe_id, &binding_id, &environment_id, &incarnation_id)
    {
        anyhow::bail!("managed instance name does not match immutable ownership metadata")
    }
    let ipv4_address = state
        .network
        .values()
        .flat_map(|network| &network.addresses)
        .find(|address| address.family == "inet" && address.scope == "global")
        .map(|address| address.address.clone());
    Ok(OwnedTarget {
        target_id: HostTargetId::new(instance.name.clone()),
        name: instance.name,
        universe_id,
        binding_id,
        environment_id,
        incarnation_id,
        request_id: get("request")?,
        template_id: get("template")?,
        image_fingerprint: get("image")?,
        status: match instance.status.as_str() {
            "Running" => HostTargetStatus::Ready,
            "Starting" => HostTargetStatus::Starting,
            "Stopped" => HostTargetStatus::Stopped,
            "Error" => HostTargetStatus::Failed,
            _ => HostTargetStatus::Unknown,
        },
        ipv4_address,
        ingress_hostname: instance
            .config
            .get(&format!("{}ingress_hostname", policy::META_PREFIX))
            .cloned(),
        ingress_port: instance
            .config
            .get(&format!("{}ingress_port", policy::META_PREFIX))
            .and_then(|value| value.parse().ok()),
        location: (!instance.location.is_empty()).then_some(instance.location),
    })
}
