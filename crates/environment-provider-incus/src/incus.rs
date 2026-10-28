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

use environment_protocol::{
    control::target::{PowerState, TargetOwner},
    idle::{IdleAction, IdlePolicy},
    ids::{EnvironmentId, ProviderTargetId, UniverseId},
};

use crate::{
    config::{Config, IncusMode, TemplatePolicy},
    policy,
};

/// A Lightspeed-managed Incus instance, read from its ownership metadata.
#[derive(Clone, Debug)]
pub struct OwnedTarget {
    /// The instance name, which is the deterministic name of `owner`.
    pub target_id: ProviderTargetId,
    pub owner: TargetOwner,
    pub template_id: String,
    /// The idle policy the target was created with: the `target/create`
    /// override or the template's default. `None` keeps it awake.
    pub idle: Option<IdlePolicy>,
    pub status: InstanceStatus,
    pub ipv4_address: Option<String>,
    pub ingress_hostname: Option<String>,
    pub ingress_port: Option<u16>,
    /// Current Incus cluster member. Diagnostic only; target identity remains
    /// the cluster-wide instance name.
    pub location: Option<String>,
}

impl OwnedTarget {
    pub fn name(&self) -> &str {
        self.target_id.as_str()
    }

    pub fn universe(&self) -> &UniverseId {
        &self.owner.universe_id
    }
}

/// Incus instance status, reduced to what the provider acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceStatus {
    Running,
    Starting,
    Stopping,
    Stopped,
    /// `Frozen` or `Freezing`: a paused VM.
    Frozen,
    Error,
    /// Any other status Incus reports.
    Other,
}

impl InstanceStatus {
    fn from_incus(status: &str) -> Self {
        match status {
            "Running" | "Thawed" => Self::Running,
            "Starting" => Self::Starting,
            "Stopping" => Self::Stopping,
            "Stopped" => Self::Stopped,
            "Frozen" | "Freezing" => Self::Frozen,
            "Error" => Self::Error,
            _ => Self::Other,
        }
    }
}

/// The Incus operations the controller needs. Mutations are slow (image
/// pulls, boots, graceful shutdowns); the controller runs them in the
/// background and observes the result with [`IncusBackend::get_owned`].
#[async_trait]
pub trait IncusBackend: Clone + Send + Sync + 'static {
    async fn topology(&self) -> anyhow::Result<IncusTopology>;
    /// Ensure the universe's project, network, ACL and profile exist.
    async fn reconcile_universe(&self, universe_id: &UniverseId) -> anyhow::Result<()>;
    async fn ensure_image(&self, fingerprint: &str) -> anyhow::Result<()>;
    async fn list_all_owned(&self) -> anyhow::Result<Vec<OwnedTarget>>;
    /// The managed instance `name` in the universe's project. `None` when
    /// the project or instance does not exist, when `name` is not an Incus
    /// instance name, or when the instance carries no valid ownership
    /// metadata.
    async fn get_owned(
        &self,
        universe_id: &UniverseId,
        name: &str,
    ) -> anyhow::Result<Option<OwnedTarget>>;
    /// Create the owner's instance from the template, recording `idle` as
    /// its idle policy, and start it.
    async fn create_vm(
        &self,
        owner: &TargetOwner,
        template: &TemplatePolicy,
        idle: Option<&IdlePolicy>,
    ) -> anyhow::Result<()>;
    /// Destroy the instance: a graceful stop within 30 s, then a forced stop,
    /// then delete. Its ingress metadata goes with it.
    async fn delete_vm(&self, target: &OwnedTarget) -> anyhow::Result<()>;
    /// Record (or with `None` clear) the realized ingress hostname and port.
    async fn set_ingress(
        &self,
        target: &OwnedTarget,
        route: Option<(&str, u16)>,
    ) -> anyhow::Result<()>;
    /// Converge the instance toward `power`. A stopped VM asked to pause is
    /// started; a later call freezes it.
    async fn set_power(&self, target: &OwnedTarget, power: PowerState) -> anyhow::Result<()>;
}

/// Power states the Incus provider can move a VM to. Stateful stop
/// (`suspended`) is deliberately not advertised until it is wired up.
pub const SUPPORTED_POWER_STATES: [PowerState; 3] =
    [PowerState::Running, PowerState::Paused, PowerState::Stopped];

/// The first stage of `idle` that moves a target to a power state this
/// provider cannot reach; such a stage would never fire.
pub fn unsupported_idle_stage(idle: &IdlePolicy) -> Option<IdleAction> {
    idle.stages()
        .into_iter()
        .map(|(action, _)| action)
        .find(|action| {
            action
                .power_state()
                .is_some_and(|state| !SUPPORTED_POWER_STATES.contains(&state))
        })
}

/// envd's state directory inside every VM: outside the agent's working tree
/// and writable by the service (`ReadWritePaths` in the image's unit).
pub const ENVD_STATE_DIR: &str = "/var/lib/lightspeed-envd";

/// Bound on one graceful Incus state action, in seconds.
const STATE_ACTION_TIMEOUT_SECONDS: i64 = 30;

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
    reconcile_lock: Arc<tokio::sync::Mutex<()>>,
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
            reconcile_lock: Arc::new(tokio::sync::Mutex::new(())),
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
        // Incus includes `"operation": ""` in synchronous responses. Only
        // non-empty references identify an asynchronous operation to wait on.
        let operation = pending_operation(envelope.operation);
        let had_operation = operation.is_some();
        if let Some(operation) = operation {
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
        if !self.exists(path, project).await?
            && let Err(error) = self
                .request_unit(Method::POST, collection, project, Some(body))
                .await
        {
            if self.exists(path, project).await? {
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }

    async fn instance(&self, project: &str, name: &str) -> anyhow::Result<Option<OwnedTarget>> {
        if !policy::is_instance_name(name) {
            return Ok(None);
        }
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
        Ok(owned_from_instance(instance, state))
    }

    async fn instance_state_action(
        &self,
        project: &str,
        name: &str,
        action: &str,
        timeout: i64,
        force: bool,
    ) -> anyhow::Result<()> {
        self.request_unit(
            Method::PUT,
            &format!("/instances/{name}/state"),
            Some(project),
            Some(json!({"action":action,"timeout":timeout,"force":force})),
        )
        .await
    }

    /// Run a state action; when it fails, accept it anyway if the instance
    /// already reached a status in `reached` (a stale read, a transport
    /// failure after Incus accepted the action, or a concurrent change).
    async fn converge_state(
        &self,
        project: &str,
        name: &str,
        action: &str,
        force: bool,
        reached: &[InstanceStatus],
    ) -> anyhow::Result<()> {
        let Err(error) = self
            .instance_state_action(project, name, action, STATE_ACTION_TIMEOUT_SECONDS, force)
            .await
        else {
            return Ok(());
        };
        match self.instance(project, name).await? {
            Some(target) if reached.contains(&target.status) => Ok(()),
            None if action == "stop" => Ok(()),
            _ => Err(error),
        }
    }

    /// Incus can briefly report the pre-operation `Stopped` status right
    /// after a start operation succeeds. Wait a few seconds for the first
    /// post-start status so the next observation is not stale.
    async fn settle_after_start(&self, project: &str, name: &str) -> anyhow::Result<()> {
        for _ in 0..10 {
            match self.instance(project, name).await? {
                Some(target) if target.status != InstanceStatus::Stopped => return Ok(()),
                None => anyhow::bail!("the Incus instance disappeared while starting"),
                Some(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
        Ok(())
    }

    async fn reconcile_network_acls(&self) -> anyhow::Result<()> {
        let projects: Vec<Project> = self
            .request(Method::GET, "/projects?recursion=1", None, None)
            .await?;
        let mut networks = Vec::new();
        for project in projects {
            let Some(universe_id) = project.universe() else {
                continue;
            };
            let network_name = policy::network_name(&universe_id);
            let network_project = network_resource_project(self.config.incus.mode, &project.name);
            if !self
                .exists(&format!("/networks/{network_name}"), network_project)
                .await?
            {
                continue;
            }
            let network: Network = self
                .request(
                    Method::GET,
                    &format!("/networks/{network_name}"),
                    network_project,
                    None,
                )
                .await?;
            let Some(cidr) = network
                .config
                .get("ipv4.address")
                .filter(|value| value.as_str() != "none" && value.as_str() != "auto")
                .cloned()
            else {
                continue;
            };
            networks.push((project.name, cidr));
        }

        for (_, own_cidr) in &networks {
            if let Some(denied) = self
                .config
                .network
                .denied_egress_cidrs
                .iter()
                .find(|denied| ipv4_cidrs_overlap(own_cidr, denied))
            {
                anyhow::bail!(
                    "provider denied egress CIDR {denied} overlaps Incus-assigned universe network {own_cidr}"
                )
            }
        }

        for (project, own_cidr) in &networks {
            let network_project = network_resource_project(self.config.incus.mode, project);
            let siblings = networks
                .iter()
                .filter(|(_, cidr)| cidr != own_cidr)
                .map(|(_, cidr)| cidr);
            let ingress: Vec<Value> = siblings
                .clone()
                .map(|source| {
                    json!({
                        "action":"reject",
                        "state":"enabled",
                        "source":source,
                        "description":"block sibling Lightspeed universe network"
                    })
                })
                .collect();
            let egress: Vec<Value> = self
                .config
                .network
                .denied_egress_cidrs
                .iter()
                .map(|destination| {
                    json!({
                        "action":"reject",
                        "state":"enabled",
                        "destination":destination,
                        "description":"block provider control/trusted network"
                    })
                })
                .chain(siblings.map(|destination| {
                    json!({
                        "action":"reject",
                        "state":"enabled",
                        "destination":destination,
                        "description":"block sibling Lightspeed universe network"
                    })
                }))
                .collect();
            self.request_unit(
                Method::PUT,
                &format!("/network-acls/{project}-acl"),
                network_project,
                Some(json!({
                    "description":"Lightspeed universe baseline",
                    "ingress":ingress,
                    "egress":egress,
                    "config":{}
                })),
            )
            .await?;
        }
        Ok(())
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

    async fn reconcile_universe(&self, universe_id: &UniverseId) -> anyhow::Result<()> {
        let _reconcile_guard = self.reconcile_lock.lock().await;
        let project = policy::project_name(universe_id);
        let isolated_project_networks = self.config.incus.mode == IncusMode::Cluster;
        let network_project = network_resource_project(self.config.incus.mode, &project);
        self.ensure_resource(&format!("/projects/{project}"), "/projects", None, json!({"name":project,"description":"Lightspeed managed universe","config":{"features.images":"false","features.networks":isolated_project_networks.to_string(),"features.profiles":"true","restricted":"true","restricted.devices.nic":"managed","restricted.devices.disk":"managed","restricted.networks.access":policy::network_name(universe_id),"user.lightspeed.managed":"true","user.lightspeed.universe":universe_id.as_str()}})).await?;
        self.ensure_resource(&format!("/network-acls/{}", policy::acl_name(universe_id)), "/network-acls", network_project, json!({"name":policy::acl_name(universe_id),"description":"Lightspeed universe baseline","ingress":[],"egress":[],"config":{}})).await?;
        let network = match self.config.incus.mode {
            IncusMode::Single => {
                json!({"name":policy::network_name(universe_id),"type":"bridge","config":{"ipv4.address":"auto","ipv4.nat":"true","ipv6.address":"none","security.acls":policy::acl_name(universe_id),"security.acls.default.ingress.action":"allow","security.acls.default.egress.action":"allow"}})
            }
            IncusMode::Cluster => {
                json!({"name":policy::network_name(universe_id),"type":"ovn","config":{"network":self.config.incus.cluster_network_uplink.as_deref().expect("validated cluster uplink"),"ipv4.address":"auto","ipv4.nat":"true","ipv6.address":"none","security.acls":policy::acl_name(universe_id),"security.acls.default.ingress.action":"allow","security.acls.default.egress.action":"allow"}})
            }
        };
        self.ensure_resource(
            &format!("/networks/{}", policy::network_name(universe_id)),
            "/networks",
            network_project,
            network,
        )
        .await?;
        self.reconcile_network_acls().await?;
        self.ensure_resource(&format!("/profiles/{}", policy::profile_name(universe_id)), "/profiles", Some(&project), json!({"name":policy::profile_name(universe_id),"description":"Lightspeed VM baseline","config":{},"devices":{"eth0":{"type":"nic","network":policy::network_name(universe_id),"security.port_isolation":"true","security.ipv4_filtering":"true"},"root":{"type":"disk","pool":self.config.incus.storage_pool,"path":"/"}}})).await
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

    async fn list_all_owned(&self) -> anyhow::Result<Vec<OwnedTarget>> {
        let projects: Vec<Project> = self
            .request(Method::GET, "/projects?recursion=1", None, None)
            .await?;
        let mut targets = Vec::new();
        for project in projects.into_iter().filter(Project::is_lightspeed_managed) {
            let instances: Vec<Instance> = self
                .request(
                    Method::GET,
                    "/instances?recursion=1",
                    Some(&project.name),
                    None,
                )
                .await?;
            for instance in instances {
                let state = self
                    .request(
                        Method::GET,
                        &format!("/instances/{}/state", instance.name),
                        Some(&project.name),
                        None,
                    )
                    .await
                    .unwrap_or_default();
                if let Some(target) = owned_from_instance(instance, state)
                    && project.name == policy::project_name(target.universe())
                {
                    targets.push(target);
                }
            }
        }
        Ok(targets)
    }

    async fn get_owned(
        &self,
        universe_id: &UniverseId,
        name: &str,
    ) -> anyhow::Result<Option<OwnedTarget>> {
        if !policy::is_instance_name(name) {
            return Ok(None);
        }
        let project = policy::project_name(universe_id);
        if !self.exists(&format!("/projects/{project}"), None).await? {
            return Ok(None);
        }
        Ok(self
            .instance(&project, name)
            .await?
            .filter(|target| target.universe() == universe_id))
    }

    async fn create_vm(
        &self,
        owner: &TargetOwner,
        template: &TemplatePolicy,
        idle: Option<&IdlePolicy>,
    ) -> anyhow::Result<()> {
        let project = policy::project_name(&owner.universe_id);
        let name = policy::instance_name(owner);
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .to_string();
        let cloud_init = format!(
            "#cloud-config\nwrite_files:\n  - path: /etc/lightspeed-envd/provider.env\n    permissions: '0600'\n    content: |\n      LIGHTSPEED_ENVD_LISTEN=0.0.0.0:{}\n      LIGHTSPEED_ENVD_CWD=/workspace\n      LIGHTSPEED_ENVD_FS_ROOT=/\n      LIGHTSPEED_ENVD_STATE_DIR={}\nruncmd:\n  - [mkdir, -p, /workspace]\n  - [systemctl, enable, --now, lightspeed-envd]\n",
            self.config.envd_port, ENVD_STATE_DIR,
        );
        let mut config = ownership_config(owner);
        config.extend([
            ("limits.cpu".to_owned(), template.cpu.to_string()),
            ("limits.memory".to_owned(), template.memory.clone()),
            ("cloud-init.user-data".to_owned(), cloud_init),
            (
                format!("{}template", policy::META_PREFIX),
                template.template_id.to_string(),
            ),
            (
                format!("{}image", policy::META_PREFIX),
                template.image_fingerprint.clone(),
            ),
            (format!("{}created_at_ms", policy::META_PREFIX), created_at),
        ]);
        if let Some(idle) = idle {
            config.insert(
                format!("{}idle", policy::META_PREFIX),
                serde_json::to_string(idle).context("encode the idle policy")?,
            );
        }
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
        if let Err(create_error) = self.request_unit(Method::POST, &path, Some(&project), Some(json!({"name":name,"type":"virtual-machine","start":false,"profiles":[policy::profile_name(&owner.universe_id)],"source":{"type":"image","fingerprint":template.image_fingerprint},"config":config,"devices":{"root":{"type":"disk","pool":self.config.incus.storage_pool,"path":"/","size":template.disk}}}))).await
            // A transport can fail after Incus accepted the deterministic
            // create. Resolve that uncertainty before returning an error.
            && self.instance(&project, &name).await?.is_none()
        {
            return Err(create_error);
        }
        start_instance(self, &project, &name).await
    }

    async fn delete_vm(&self, target: &OwnedTarget) -> anyhow::Result<()> {
        let project = policy::project_name(target.universe());
        let name = target.name();
        if target.status != InstanceStatus::Stopped {
            // Graceful first; a guest that ignores ACPI power-down (a hung
            // kernel, an emergency shell) is then stopped by force, since its
            // disk is destroyed either way.
            let graceful = self
                .converge_state(&project, name, "stop", false, &[InstanceStatus::Stopped])
                .await;
            if graceful.is_err() {
                self.converge_state(&project, name, "stop", true, &[InstanceStatus::Stopped])
                    .await
                    .context("force-stop Incus instance before delete")?;
            }
        }
        if let Err(error) = self
            .request_unit(
                Method::DELETE,
                &format!("/instances/{name}"),
                Some(&project),
                None,
            )
            .await
            && self.instance(&project, name).await?.is_some()
        {
            return Err(error);
        }
        Ok(())
    }

    async fn set_power(&self, target: &OwnedTarget, power: PowerState) -> anyhow::Result<()> {
        if !SUPPORTED_POWER_STATES.contains(&power) {
            anyhow::bail!("Incus provider does not support power state {power}")
        }
        let project = policy::project_name(target.universe());
        let name = target.name();
        match (target.status, power) {
            // Already there or already on the way.
            (InstanceStatus::Running | InstanceStatus::Starting, PowerState::Running)
            | (InstanceStatus::Frozen, PowerState::Paused)
            | (InstanceStatus::Stopped | InstanceStatus::Stopping, PowerState::Stopped) => Ok(()),
            (InstanceStatus::Frozen, PowerState::Running) => {
                self.converge_state(
                    &project,
                    name,
                    "unfreeze",
                    false,
                    &[InstanceStatus::Running],
                )
                .await
            }
            (InstanceStatus::Stopped, PowerState::Running | PowerState::Paused) => {
                // A stopped VM cannot be frozen directly; converge through
                // running and let a later call freeze it.
                start_instance(self, &project, name).await
            }
            (InstanceStatus::Running, PowerState::Paused) => {
                self.converge_state(&project, name, "freeze", false, &[InstanceStatus::Frozen])
                    .await
            }
            (InstanceStatus::Running | InstanceStatus::Frozen, PowerState::Stopped) => {
                if target.status == InstanceStatus::Frozen {
                    self.converge_state(
                        &project,
                        name,
                        "unfreeze",
                        false,
                        &[InstanceStatus::Running],
                    )
                    .await?;
                }
                self.converge_state(&project, name, "stop", false, &[InstanceStatus::Stopped])
                    .await
            }
            // Transitional, failed or unknown: nothing safe to do until the
            // instance settles; the caller re-observes later.
            _ => Ok(()),
        }
    }

    async fn set_ingress(
        &self,
        target: &OwnedTarget,
        route: Option<(&str, u16)>,
    ) -> anyhow::Result<()> {
        let project = policy::project_name(target.universe());
        let name = target.name();
        let instance: Instance = self
            .request(
                Method::GET,
                &format!("/instances/{name}"),
                Some(&project),
                None,
            )
            .await?;
        let mut config = instance.config;
        let hostname_key = format!("{}ingress_hostname", policy::META_PREFIX);
        let port_key = format!("{}ingress_port", policy::META_PREFIX);
        match route {
            Some((hostname, port)) => {
                config.insert(hostname_key, hostname.to_owned());
                config.insert(port_key, port.to_string());
            }
            None => {
                config.remove(&hostname_key);
                config.remove(&port_key);
            }
        }
        let update = self.request_unit(Method::PUT, &format!("/instances/{name}"), Some(&project), Some(json!({"config":config,"description":instance.description,"profiles":instance.profiles,"devices":instance.devices,"ephemeral":instance.ephemeral}))).await;
        let observed = self
            .instance(&project, name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("target disappeared while updating ingress"))?;
        let realized = observed
            .ingress_hostname
            .as_deref()
            .zip(observed.ingress_port);
        if realized != route {
            return Err(update.err().unwrap_or_else(|| {
                anyhow::anyhow!("Incus did not realize the requested ingress metadata")
            }));
        }
        Ok(())
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

#[derive(Deserialize)]
struct Project {
    name: String,
    #[serde(default)]
    config: BTreeMap<String, String>,
}

impl Project {
    /// The universe a provider-managed project isolates, or `None` for any
    /// other project, including one whose name does not match its metadata.
    fn universe(&self) -> Option<UniverseId> {
        if self
            .config
            .get("user.lightspeed.managed")
            .is_none_or(|value| value != "true")
        {
            return None;
        }
        let universe_id =
            UniverseId::new(self.config.get("user.lightspeed.universe")?.as_str()).ok()?;
        (self.name == policy::project_name(&universe_id)).then_some(universe_id)
    }

    fn is_lightspeed_managed(&self) -> bool {
        self.universe().is_some()
    }
}

#[derive(Deserialize)]
struct Network {
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
    host_name: String,
    #[serde(default)]
    addresses: Vec<Address>,
}
#[derive(Deserialize)]
struct Address {
    family: String,
    scope: String,
    address: String,
}

fn ipv4_cidrs_overlap(left: &str, right: &str) -> bool {
    fn range(cidr: &str) -> Option<(u32, u32)> {
        let (address, prefix) = cidr.split_once('/')?;
        let address = u32::from(address.parse::<std::net::Ipv4Addr>().ok()?);
        let prefix = prefix.parse::<u32>().ok()?;
        if prefix > 32 {
            return None;
        }
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        let start = address & mask;
        Some((start, start | !mask))
    }
    let Some((left_start, left_end)) = range(left) else {
        return false;
    };
    let Some((right_start, right_end)) = range(right) else {
        return false;
    };
    left_start <= right_end && right_start <= left_end
}

fn pending_operation(operation: Option<String>) -> Option<String> {
    operation.filter(|operation| !operation.is_empty())
}

fn managed_ipv4_address(state: &InstanceState) -> Option<String> {
    state
        .network
        .values()
        // Incus reports a host-side interface for managed instance NICs.
        // Guest-created networks such as Docker's bridge have no host_name.
        .filter(|network| !network.host_name.is_empty())
        .flat_map(|network| &network.addresses)
        .find(|address| address.family == "inet" && address.scope == "global")
        .map(|address| address.address.clone())
}

/// OVN networks are project-local, while standalone bridge networks are
/// global Incus resources that restricted projects are allowed to reference.
fn network_resource_project(mode: IncusMode, project: &str) -> Option<&str> {
    match mode {
        IncusMode::Single => None,
        IncusMode::Cluster => Some(project),
    }
}

/// Start a stopped instance or unfreeze a frozen one; a running or starting
/// one needs nothing.
async fn start_instance(client: &IncusClient, project: &str, name: &str) -> anyhow::Result<()> {
    let target = client
        .instance(project, name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("the Incus instance disappeared before it started"))?;
    match target.status {
        InstanceStatus::Running | InstanceStatus::Starting => Ok(()),
        InstanceStatus::Frozen => {
            client
                .converge_state(project, name, "unfreeze", false, &[InstanceStatus::Running])
                .await
        }
        _ => {
            client
                .converge_state(
                    project,
                    name,
                    "start",
                    false,
                    &[InstanceStatus::Running, InstanceStatus::Starting],
                )
                .await?;
            client.settle_after_start(project, name).await
        }
    }
}

/// The `user.lightspeed.*` keys that make an instance the owner's.
fn ownership_config(owner: &TargetOwner) -> BTreeMap<String, String> {
    BTreeMap::from([
        (format!("{}managed", policy::META_PREFIX), "true".to_owned()),
        (
            format!("{}universe", policy::META_PREFIX),
            owner.universe_id.to_string(),
        ),
        (
            format!("{}environment", policy::META_PREFIX),
            owner.environment_id.to_string(),
        ),
    ])
}

/// The owned target an instance describes, or `None` when its ownership
/// metadata is missing, invalid or does not match its deterministic name.
fn owned_from_instance(instance: Instance, state: InstanceState) -> Option<OwnedTarget> {
    let get = |key: &str| {
        instance
            .config
            .get(&format!("{}{}", policy::META_PREFIX, key))
            .cloned()
    };
    if get("managed")? != "true" {
        return None;
    }
    let owner = TargetOwner {
        universe_id: UniverseId::new(get("universe")?).ok()?,
        environment_id: EnvironmentId::new(get("environment")?).ok()?,
    };
    if instance.name != policy::instance_name(&owner) {
        return None;
    }
    Some(OwnedTarget {
        target_id: ProviderTargetId::new(instance.name.clone()).ok()?,
        owner,
        template_id: get("template")?,
        // The provider writes this key itself; a value that does not decode
        // leaves the target without an idle policy rather than unowned.
        idle: get("idle").and_then(|value| serde_json::from_str(&value).ok()),
        status: InstanceStatus::from_incus(&instance.status),
        ipv4_address: managed_ipv4_address(&state),
        ingress_hostname: get("ingress_hostname"),
        ingress_port: get("ingress_port").and_then(|value| value.parse().ok()),
        location: (!instance.location.is_empty()).then_some(instance.location),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_overlap_handles_host_and_network_notation() {
        assert!(ipv4_cidrs_overlap("10.42.7.1/24", "10.0.0.0/8"));
        assert!(ipv4_cidrs_overlap("169.254.169.0/24", "169.254.169.254/32"));
        assert!(!ipv4_cidrs_overlap("10.42.7.1/24", "100.64.0.0/10"));
    }

    #[test]
    fn synchronous_incus_operation_reference_is_not_pending() {
        assert_eq!(pending_operation(None), None);
        assert_eq!(pending_operation(Some(String::new())), None);
        assert_eq!(
            pending_operation(Some("/1.0/operations/op-1".to_owned())),
            Some("/1.0/operations/op-1".to_owned())
        );
    }

    #[test]
    fn managed_projects_require_matching_universe_metadata() {
        let universe_id = UniverseId::new("u").unwrap();
        let project = Project {
            name: policy::project_name(&universe_id),
            config: BTreeMap::from([
                ("user.lightspeed.managed".to_owned(), "true".to_owned()),
                ("user.lightspeed.universe".to_owned(), "u".to_owned()),
            ]),
        };
        assert!(project.is_lightspeed_managed());
        assert_eq!(project.universe(), Some(universe_id));
        let spoofed = Project {
            name: "ls-spoofed".to_owned(),
            ..project
        };
        assert!(!spoofed.is_lightspeed_managed());
    }

    fn instance(name: &str, config: BTreeMap<String, String>) -> Instance {
        Instance {
            name: name.to_owned(),
            description: String::new(),
            profiles: Vec::new(),
            devices: BTreeMap::new(),
            ephemeral: false,
            status: "Running".to_owned(),
            config,
            location: String::new(),
        }
    }

    #[test]
    fn ownership_metadata_must_match_the_deterministic_name() {
        let owner = policy::tests::owner("u", "e");
        let mut config = ownership_config(&owner);
        config.insert("user.lightspeed.template".to_owned(), "t".to_owned());
        config.insert("user.lightspeed.image".to_owned(), "f".to_owned());
        let name = policy::instance_name(&owner);
        let owned = owned_from_instance(instance(&name, config.clone()), InstanceState::default())
            .expect("owned");
        assert_eq!(owned.owner, owner);
        assert_eq!(owned.status, InstanceStatus::Running);
        assert_eq!(owned.idle, None);
        assert!(
            owned_from_instance(
                instance("ls-other", config.clone()),
                InstanceState::default()
            )
            .is_none()
        );
        config.insert("user.lightspeed.universe".to_owned(), "bad id!".to_owned());
        assert!(owned_from_instance(instance(&name, config), InstanceState::default()).is_none());
    }

    #[test]
    fn standalone_network_resources_are_global_but_cluster_networks_are_project_local() {
        assert_eq!(
            network_resource_project(IncusMode::Single, "universe-a"),
            None
        );
        assert_eq!(
            network_resource_project(IncusMode::Cluster, "universe-a"),
            Some("universe-a")
        );
    }

    #[test]
    fn the_recorded_idle_policy_is_read_back() {
        let owner = policy::tests::owner("u", "e");
        let mut config = ownership_config(&owner);
        config.insert("user.lightspeed.template".to_owned(), "t".to_owned());
        let idle = IdlePolicy {
            pause_after_ms: Some(600_000),
            close_after_ms: Some(86_400_000),
            ..IdlePolicy::default()
        };
        config.insert(
            "user.lightspeed.idle".to_owned(),
            serde_json::to_string(&idle).unwrap(),
        );
        let name = policy::instance_name(&owner);
        let owned = owned_from_instance(instance(&name, config.clone()), InstanceState::default())
            .expect("owned");
        assert_eq!(owned.idle, Some(idle));

        config.insert("user.lightspeed.idle".to_owned(), "not json".to_owned());
        let owned = owned_from_instance(instance(&name, config), InstanceState::default())
            .expect("an undecodable idle policy keeps ownership");
        assert_eq!(owned.idle, None);
    }

    #[test]
    fn managed_ipv4_ignores_guest_created_bridges() {
        let state = InstanceState {
            network: BTreeMap::from([
                (
                    "docker0".to_owned(),
                    NetworkState {
                        host_name: String::new(),
                        addresses: vec![Address {
                            family: "inet".to_owned(),
                            scope: "global".to_owned(),
                            address: "172.17.0.1".to_owned(),
                        }],
                    },
                ),
                (
                    "enp5s0".to_owned(),
                    NetworkState {
                        host_name: "tap123".to_owned(),
                        addresses: vec![Address {
                            family: "inet".to_owned(),
                            scope: "global".to_owned(),
                            address: "10.42.0.2".to_owned(),
                        }],
                    },
                ),
            ]),
        };
        assert_eq!(managed_ipv4_address(&state).as_deref(), Some("10.42.0.2"));
    }

    #[test]
    fn incus_statuses_reduce_to_provider_statuses() {
        assert_eq!(
            InstanceStatus::from_incus("Running"),
            InstanceStatus::Running
        );
        assert_eq!(
            InstanceStatus::from_incus("Freezing"),
            InstanceStatus::Frozen
        );
        assert_eq!(InstanceStatus::from_incus("Error"), InstanceStatus::Error);
        assert_eq!(InstanceStatus::from_incus("Pending"), InstanceStatus::Other);
        assert!(SUPPORTED_POWER_STATES.contains(&PowerState::Paused));
        assert!(!SUPPORTED_POWER_STATES.contains(&PowerState::Suspended));
    }
}
