use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Json, Router,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{StreamExt as _, stream};
use host_protocol::{
    control::{
        handshake::{
            ControllerCapabilities, ControllerInitializeParams, ControllerInitializeResponse,
        },
        ingress::{
            EnsureIngressParams, IngressResponse, ProviderIngressStatus, RemoveIngressParams,
        },
        methods::*,
        targets::*,
    },
    error::{HostError, HostErrorCode},
    shared::{CURRENT_PROTOCOL_VERSION, HostCapabilities, HostScope, ImplementationInfo},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{Config, IncusBackend, incus::OwnedTarget, ingress, policy, relay};

#[derive(Clone)]
struct App<B> {
    config: Config,
    backend: B,
    binding_locks: Arc<tokio::sync::Mutex<BTreeMap<(String, String), Arc<tokio::sync::Mutex<()>>>>>,
}

pub async fn serve<B: IncusBackend>(config: Config, backend: B) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(config.controller_listen).await?;
    let app = Router::new()
        .route("/health", get(health::<B>))
        .route("/control", get(upgrade::<B>))
        .route(
            "/routes/:universe/:binding/:environment/:incarnation/:target",
            get(data_upgrade::<B>),
        )
        .with_state(App {
            config,
            backend,
            binding_locks: Arc::default(),
        });
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health<B: IncusBackend>(State(app): State<App<B>>) -> Response {
    match app.backend.topology().await {
        Ok(topology) => Json(topology).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn upgrade<B: IncusBackend>(
    State(app): State<App<B>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade
        .on_upgrade(move |socket| connection(app, socket))
        .into_response()
}

async fn data_upgrade<B: IncusBackend>(
    State(app): State<App<B>>,
    Path((universe, binding_id, environment, incarnation, target_id)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let binding = ProviderBindingContext {
        universe_id: universe.clone(),
        binding_id: binding_id.clone(),
    };
    let expected = policy::instance_name(&universe, &binding_id, &environment, &incarnation);
    if expected != target_id {
        return StatusCode::CONFLICT.into_response();
    }
    let target_id = host_protocol::shared::HostTargetId::new(target_id);
    let target = match app.backend.get_owned(&binding, &target_id).await {
        Ok(Some(target)) => target,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    if verify_binding(&target, &binding).is_err()
        || target.environment_id != environment
        || target.incarnation_id != incarnation
    {
        return StatusCode::CONFLICT.into_response();
    }
    let guest = match relay::dial_guest(&app.config, &target).await {
        Ok(guest) => guest,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let config = app.config.clone();
    upgrade
        .max_message_size(64 * 1024 * 1024)
        .on_upgrade(move |socket| relay::proxy(config, socket, guest))
        .into_response()
}

async fn connection<B: IncusBackend>(app: App<B>, mut socket: WebSocket) {
    while let Some(Ok(message)) = socket.recv().await {
        match message {
            Message::Text(text) => {
                let response = dispatch(&app, &text).await;
                if socket
                    .send(Message::Text(response.to_string()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Message::Ping(bytes) => {
                let _ = socket.send(Message::Pong(bytes)).await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

async fn dispatch<B: IncusBackend>(app: &App<B>, text: &str) -> Value {
    let request: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(decode_error) => {
            return error(
                Value::Null,
                HostErrorCode::InvalidRequest,
                decode_error.to_string(),
            );
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        INITIALIZE_METHOD => decode::<ControllerInitializeParams>(params)
            .and_then(|params| {
                if params.protocol_version != CURRENT_PROTOCOL_VERSION {
                    anyhow::bail!("unsupported protocol version")
                }
                Ok(ControllerInitializeResponse {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    capabilities: ControllerCapabilities {
                        list_templates: true,
                        list_targets: true,
                        create_target: true,
                        get_target: true,
                        close_target: true,
                        ingress: app.config.ingress.is_some(),
                    },
                    implementation: ImplementationInfo {
                        name: "lightspeed-provider-incus".to_owned(),
                        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                    },
                })
            })
            .and_then(encode),
        LIST_TEMPLATES_METHOD => list_templates(app, decode(params)).await.and_then(encode),
        LIST_TARGETS_METHOD => list_targets(app, decode(params)).await.and_then(encode),
        CREATE_TARGET_METHOD => create_target(app, decode(params)).await.and_then(encode),
        GET_TARGET_METHOD => get_target(app, decode(params)).await.and_then(encode),
        CLOSE_TARGET_METHOD => close_target(app, decode(params)).await.and_then(encode),
        ENSURE_INGRESS_METHOD => ensure_ingress(app, decode(params)).await.and_then(encode),
        REMOVE_INGRESS_METHOD => remove_ingress(app, decode(params)).await.and_then(encode),
        _ => Err(anyhow::anyhow!("unknown controller method: {method}")),
    };
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(error_value) => error(id, HostErrorCode::Internal, error_value.to_string()),
    }
}

async fn list_templates<B: IncusBackend>(
    app: &App<B>,
    params: anyhow::Result<ListTemplatesParams>,
) -> anyhow::Result<ListTemplatesResponse> {
    let _params = params?;
    Ok(ListTemplatesResponse {
        templates: app
            .config
            .templates
            .values()
            .map(|template| EnvironmentTemplate {
                template_id: template.template_id.clone(),
                display_name: template.display_name.clone(),
                description: template.description.clone(),
                public_ingress: template.public_ingress,
                deprecated: template.deprecated,
                metadata: BTreeMap::from([
                    (
                        "imageFingerprint".to_owned(),
                        template.image_fingerprint.clone(),
                    ),
                    ("cpu".to_owned(), template.cpu.to_string()),
                    ("memory".to_owned(), template.memory.clone()),
                    ("disk".to_owned(), template.disk.clone()),
                ]),
            })
            .collect(),
    })
}

async fn list_targets<B: IncusBackend>(
    app: &App<B>,
    params: anyhow::Result<ListTargetsParams>,
) -> anyhow::Result<ListTargetsResponse> {
    let params = params?;
    let binding = &params.binding;
    let config = app.config.clone();
    let mut targets = stream::iter(app.backend.list_owned(binding).await?)
        .map(move |mut target| {
            let config = config.clone();
            async move {
                observe_daemon_readiness(&config, &mut target).await;
                target
            }
        })
        .buffer_unordered(16)
        .collect::<Vec<_>>()
        .await;
    if let Some(status) = params.status {
        targets.retain(|target| target.status == status);
    }
    Ok(ListTargetsResponse {
        targets: targets.into_iter().map(summary).collect(),
    })
}

async fn get_target<B: IncusBackend>(
    app: &App<B>,
    params: anyhow::Result<GetTargetParams>,
) -> anyhow::Result<GetTargetResponse> {
    let params = params?;
    let binding = &params.binding;
    let mut target = app
        .backend
        .get_owned(binding, &params.target_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("target not found"))?;
    verify_binding(&target, binding)?;
    observe_daemon_readiness(&app.config, &mut target).await;
    Ok(GetTargetResponse {
        target: summary(target),
    })
}

async fn create_target<B: IncusBackend>(
    app: &App<B>,
    params: anyhow::Result<CreateTargetParams>,
) -> anyhow::Result<CreateTargetResponse> {
    let params = params?;
    let binding = params.binding.clone();
    let binding_lock = {
        let mut locks = app.binding_locks.lock().await;
        locks
            .entry((
                params.binding.universe_id.clone(),
                params.binding.binding_id.clone(),
            ))
            .or_default()
            .clone()
    };
    let _binding_guard = binding_lock.lock().await;
    let template = app.config.template(&params.template_id)?;
    app.backend.reconcile_binding(&binding).await?;
    app.backend
        .ensure_image(&template.image_fingerprint)
        .await?;
    let expected_target_id = policy::instance_name(
        &params.binding.universe_id,
        &params.binding.binding_id,
        &params.environment_id,
        &params.incarnation_id,
    );
    if let Some(mut target) = app
        .backend
        .get_owned(&binding, &expected_target_id.clone().into())
        .await?
    {
        verify_create(&target, &params, template)?;
        observe_daemon_readiness(&app.config, &mut target).await;
        return Ok(CreateTargetResponse {
            target: summary(target),
        });
    }
    let existing = app.backend.list_owned(&binding).await?;
    if let Some(target) = existing
        .iter()
        .find(|target| target.request_id == params.request_id)
    {
        verify_create(target, &params, template)?;
        let mut target = target.clone();
        observe_daemon_readiness(&app.config, &mut target).await;
        return Ok(CreateTargetResponse {
            target: summary(target),
        });
    }
    let mut target = app.backend.create_vm(&binding, template, &params).await?;
    verify_create(&target, &params, template)?;
    observe_daemon_readiness(&app.config, &mut target).await;
    Ok(CreateTargetResponse {
        target: summary(target),
    })
}

async fn close_target<B: IncusBackend>(
    app: &App<B>,
    params: anyhow::Result<CloseTargetParams>,
) -> anyhow::Result<CloseTargetResponse> {
    let params = params?;
    let binding = &params.binding;
    let expected_target_id = policy::instance_name(
        &params.binding.universe_id,
        &params.binding.binding_id,
        &params.environment_id,
        &params.incarnation_id,
    );
    if params.target_id.as_str() != expected_target_id {
        anyhow::bail!("target handle does not match deterministic ownership identity")
    }
    let Some(target) = app.backend.get_owned(binding, &params.target_id).await? else {
        return Ok(CloseTargetResponse {
            target_id: params.target_id,
            status: HostTargetStatus::Closed,
        });
    };
    verify_binding(&target, binding)?;
    if target.environment_id != params.environment_id
        || target.incarnation_id != params.incarnation_id
    {
        anyhow::bail!("target ownership does not match close request")
    }
    app.backend
        .delete_vm(binding, &target, params.force)
        .await?;
    Ok(CloseTargetResponse {
        target_id: params.target_id,
        status: HostTargetStatus::Closed,
    })
}

async fn ensure_ingress<B: IncusBackend>(
    app: &App<B>,
    params: anyhow::Result<EnsureIngressParams>,
) -> anyhow::Result<IngressResponse> {
    let params = params?;
    let binding = &params.binding;
    let mut target = app
        .backend
        .get_owned(binding, &params.target_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("target not found"))?;
    verify_ingress_target(
        &target,
        binding,
        &params.environment_id,
        &params.incarnation_id,
    )?;
    let template = app
        .config
        .templates
        .get(&target.template_id)
        .ok_or_else(|| anyhow::anyhow!("target template is no longer configured"))?;
    let port = template
        .ingress_port
        .ok_or_else(|| anyhow::anyhow!("template does not permit public ingress"))?;
    let (hostname, endpoint) = ingress::endpoint(&app.config, &target)?;
    target = app
        .backend
        .set_ingress(binding, &target, Some(&hostname), Some(port))
        .await?;
    verify_ingress_target(
        &target,
        binding,
        &params.environment_id,
        &params.incarnation_id,
    )?;
    Ok(IngressResponse {
        status: ProviderIngressStatus::Ready,
        public_endpoint: Some(endpoint),
    })
}

async fn remove_ingress<B: IncusBackend>(
    app: &App<B>,
    params: anyhow::Result<RemoveIngressParams>,
) -> anyhow::Result<IngressResponse> {
    let params = params?;
    let binding = &params.binding;
    let Some(target) = app.backend.get_owned(binding, &params.target_id).await? else {
        return Ok(IngressResponse {
            status: ProviderIngressStatus::Disabled,
            public_endpoint: None,
        });
    };
    verify_ingress_target(
        &target,
        binding,
        &params.environment_id,
        &params.incarnation_id,
    )?;
    app.backend
        .set_ingress(binding, &target, None, None)
        .await?;
    Ok(IngressResponse {
        status: ProviderIngressStatus::Disabled,
        public_endpoint: None,
    })
}

fn verify_ingress_target(
    target: &OwnedTarget,
    binding: &ProviderBindingContext,
    environment: &str,
    incarnation: &str,
) -> anyhow::Result<()> {
    verify_binding(target, binding)?;
    if target.environment_id != environment || target.incarnation_id != incarnation {
        anyhow::bail!("target ownership does not match ingress request")
    }
    Ok(())
}

fn verify_binding(target: &OwnedTarget, binding: &ProviderBindingContext) -> anyhow::Result<()> {
    if target.universe_id != binding.universe_id || target.binding_id != binding.binding_id {
        anyhow::bail!("target belongs to another binding")
    }
    Ok(())
}
fn verify_create(
    target: &OwnedTarget,
    params: &CreateTargetParams,
    template: &crate::config::TemplatePolicy,
) -> anyhow::Result<()> {
    if target.universe_id != params.binding.universe_id
        || target.binding_id != params.binding.binding_id
        || target.environment_id != params.environment_id
        || target.incarnation_id != params.incarnation_id
        || target.request_id != params.request_id
        || target.template_id != template.template_id
        || target.image_fingerprint != template.image_fingerprint
    {
        anyhow::bail!("existing target metadata conflicts with provision request")
    }
    Ok(())
}
async fn observe_daemon_readiness(config: &Config, target: &mut OwnedTarget) {
    if target.status == HostTargetStatus::Ready && !relay::probe_guest(config, target).await {
        target.status = HostTargetStatus::Starting;
    }
}
fn summary(target: OwnedTarget) -> HostTargetSummary {
    let mut metadata = BTreeMap::from([
        ("templateId".to_owned(), target.template_id),
        ("imageFingerprint".to_owned(), target.image_fingerprint),
    ]);
    if let Some(location) = target.location {
        metadata.insert("incusMember".to_owned(), location);
    }
    HostTargetSummary {
        target_id: target.target_id,
        display_name: Some(target.environment_id),
        status: target.status,
        scope: HostScope::Default,
        capabilities: HostCapabilities::filesystem(true, true)
            .with_filesystem_search()
            .with_process()
            .with_jobs(),
        default_cwd: None,
        metadata,
    }
}
fn decode<T: DeserializeOwned>(value: Value) -> anyhow::Result<T> {
    Ok(serde_json::from_value(value)?)
}
fn encode<T: serde::Serialize>(value: T) -> anyhow::Result<Value> {
    Ok(serde_json::to_value(value)?)
}
fn error(id: Value, code: HostErrorCode, message: String) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":HostError::new(code,message)})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> OwnedTarget {
        OwnedTarget {
            target_id: "target-a".into(),
            name: "target-a".to_owned(),
            universe_id: "u-a".to_owned(),
            binding_id: "b-a".to_owned(),
            environment_id: "e-a".to_owned(),
            incarnation_id: "i-a".to_owned(),
            request_id: "r-a".to_owned(),
            template_id: "dev-small-v1".to_owned(),
            image_fingerprint: "fingerprint-a".to_owned(),
            status: HostTargetStatus::Ready,
            ipv4_address: Some("10.0.0.2".to_owned()),
            ingress_hostname: None,
            ingress_port: None,
            location: Some("member-a".to_owned()),
        }
    }

    #[test]
    fn ownership_checks_fence_cross_binding_and_changed_image_adoption() {
        let binding = ProviderBindingContext {
            universe_id: "u-a".to_owned(),
            binding_id: "b-a".to_owned(),
        };
        assert!(verify_binding(&target(), &binding).is_ok());
        let other = ProviderBindingContext {
            universe_id: "u-b".to_owned(),
            ..binding.clone()
        };
        assert!(verify_binding(&target(), &other).is_err());
        let params = CreateTargetParams {
            request_id: "r-a".to_owned(),
            environment_id: "e-a".to_owned(),
            incarnation_id: "i-a".to_owned(),
            binding: ProviderBindingContext {
                universe_id: "u-a".to_owned(),
                binding_id: "b-a".to_owned(),
            },
            template_id: "dev-small-v1".to_owned(),
        };
        let mut template = crate::config::TemplatePolicy {
            template_id: "dev-small-v1".to_owned(),
            display_name: "Small".to_owned(),
            description: None,
            image_fingerprint: "fingerprint-a".to_owned(),
            cpu: 2,
            memory: "4GiB".to_owned(),
            disk: "40GiB".to_owned(),
            cluster_group: None,
            public_ingress: false,
            ingress_port: None,
            deprecated: false,
        };
        assert!(verify_create(&target(), &params, &template).is_ok());
        template.image_fingerprint = "fingerprint-b".to_owned();
        assert!(verify_create(&target(), &params, &template).is_err());
    }

    #[test]
    fn summary_reports_member_without_changing_target_identity() {
        let summary = summary(target());
        assert_eq!(summary.target_id.as_str(), "target-a");
        assert_eq!(
            summary.metadata.get("incusMember").map(String::as_str),
            Some("member-a")
        );
    }
}
