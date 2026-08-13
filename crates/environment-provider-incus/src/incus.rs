use std::{collections::BTreeMap, time::Duration};

use async_trait::async_trait;
use reqwest::{Certificate, Client, Identity, Method, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use host_protocol::{
    control::targets::{CreateTargetParams, HostTargetStatus},
    shared::HostTargetId,
};

use crate::{
    config::{BindingPolicy, Config, TemplatePolicy},
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
}

#[async_trait]
pub trait IncusBackend: Clone + Send + Sync + 'static {
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
}

#[derive(Clone)]
pub struct IncusClient {
    config: Config,
    http: Client,
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
        Ok(Self { config, http })
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        project: Option<&str>,
        body: Option<Value>,
    ) -> anyhow::Result<T> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let suffix = project
            .map(|project| format!("{separator}project={project}"))
            .unwrap_or_default();
        let url = format!(
            "{}/1.0{}{}",
            self.config.incus.base_url.trim_end_matches('/'),
            path,
            suffix
        );
        let mut request = self.http.request(method, url);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let envelope: Envelope<T> = response.json().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Incus request failed ({status}): {}",
                envelope
                    .error
                    .unwrap_or_else(|| envelope.status.unwrap_or_default())
            );
        }
        if let Some(operation) = envelope.operation {
            self.wait_operation(&operation, project).await?;
        }
        envelope
            .metadata
            .ok_or_else(|| anyhow::anyhow!("Incus response has no metadata"))
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
        let separator = if path.contains('?') { '&' } else { '?' };
        let suffix = project
            .map(|project| format!("{separator}project={project}"))
            .unwrap_or_default();
        let url = format!(
            "{}/1.0{}{}",
            self.config.incus.base_url.trim_end_matches('/'),
            path,
            suffix
        );
        let status = self.http.get(url).send().await?.status();
        if status == StatusCode::NOT_FOUND {
            Ok(false)
        } else if status.is_success() {
            Ok(true)
        } else {
            anyhow::bail!("Incus existence check failed: {status}")
        }
    }

    async fn wait_operation(&self, operation: &str, project: Option<&str>) -> anyhow::Result<()> {
        let operation = operation.strip_prefix("/1.0").unwrap_or(operation);
        let project = project
            .map(|project| format!("&project={project}"))
            .unwrap_or_default();
        let url = format!(
            "{}/1.0{operation}/wait?timeout=60{project}",
            self.config.incus.base_url.trim_end_matches('/')
        );
        let response = self.http.get(url).send().await?;
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
        Ok(())
    }

    async fn ensure_resource(
        &self,
        path: &str,
        collection: &str,
        project: Option<&str>,
        body: Value,
    ) -> anyhow::Result<()> {
        if !self.exists(path, project).await? {
            self.request_unit(Method::POST, collection, project, Some(body))
                .await?;
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
        self.ensure_resource(&format!("/networks/{}", policy::network_name(binding)), "/networks", Some(&project), json!({"name":policy::network_name(binding),"type":"bridge","config":{"ipv4.address":binding.ipv4_cidr,"ipv4.nat":"true","ipv6.address":"none","security.acls":policy::acl_name(binding),"security.acls.default.ingress.action":"allow","security.acls.default.egress.action":"allow"}})).await?;
        self.ensure_resource(&format!("/profiles/{}", policy::profile_name(binding)), "/profiles", Some(&project), json!({"name":policy::profile_name(binding),"description":"Lightspeed VM baseline","config":{},"devices":{"eth0":{"type":"nic","network":policy::network_name(binding),"security.port_isolation":"true","security.ipv4_filtering":"true"},"root":{"type":"disk","pool":self.config.incus.storage_pool,"path":"/"}}})).await
    }

    async fn ensure_image(&self, fingerprint: &str) -> anyhow::Result<()> {
        if self.exists(&format!("/images/{fingerprint}"), None).await? {
            return Ok(());
        }
        let server = self.config.incus.image_server_url.as_deref().ok_or_else(|| anyhow::anyhow!("immutable Incus image fingerprint is not cached and no imageServerUrl is configured: {fingerprint}"))?;
        self.request_unit(Method::POST,"/images",None,Some(json!({"source":{"type":"image","mode":"pull","protocol":"incus","server":server,"fingerprint":fingerprint},"public":false,"auto_update":false}))).await?;
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
        self.request_unit(Method::POST, "/instances", Some(&project), Some(json!({"name":name,"type":"virtual-machine","start":false,"profiles":[policy::profile_name(binding)],"source":{"type":"image","fingerprint":template.image_fingerprint},"config":config,"devices":{"root":{"type":"disk","pool":self.config.incus.storage_pool,"path":"/","size":template.disk}}}))).await?;
        self.request_unit(
            Method::PUT,
            &format!("/instances/{name}/state"),
            Some(&project),
            Some(json!({"action":"start","timeout":30,"force":false})),
        )
        .await?;
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
        self.request_unit(
            Method::DELETE,
            &format!("/instances/{}", target.name),
            Some(&project),
            None,
        )
        .await
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
    status: String,
    #[serde(default)]
    config: BTreeMap<String, String>,
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
    })
}
