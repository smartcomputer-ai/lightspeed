//! Environment source connections and directory validation; discovery never wakes a machine.
use crate::{
    environment_gateway::EnvironmentGatewayClientConfig, environment_resolver::EnvironmentResolver,
};
use engine::{EnvironmentId, EnvironmentsFeature};
use environment_client::{EnvironmentDataClient, JsonRpcTransport, WebSocketTransport};
use environment_protocol::{
    data::{
        fs::GetMetadataParams,
        handshake::{InitializeParams, InitializeResponse, InitializedParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, EnvironmentPath},
};

pub(crate) async fn working_directory<T: JsonRpcTransport>(
    client: &mut EnvironmentDataClient<T>,
    configured: Option<&str>,
    default: Option<&str>,
) -> Result<String, String> {
    let value = configured
        .or(default)
        .ok_or("environment does not advertise a default working directory")?;
    if !value.starts_with('/') {
        return Err("environment working directory must be absolute".into());
    }
    let cwd = tools::environment::sources::absolute(std::path::Path::new("/"), value)?
        .to_string_lossy()
        .into_owned();
    let metadata = client
        .get_metadata(&GetMetadataParams {
            path: EnvironmentPath::new(&cwd).map_err(|e| e.to_string())?,
        })
        .await
        .map_err(|e| format!("working directory {cwd}: {e}"))?;
    if !metadata.is_directory {
        return Err(format!("working directory is not a directory: {cwd}"));
    }
    Ok(cwd)
}

pub(crate) async fn connect(
    resolver: &EnvironmentResolver,
    gateway: &EnvironmentGatewayClientConfig,
    feature: &EnvironmentsFeature,
    id: &EnvironmentId,
) -> Result<
    (
        EnvironmentDataClient<WebSocketTransport>,
        InitializeResponse,
        String,
    ),
    String,
> {
    let policy = environments::EnvironmentAccessPolicy::new(
        feature.providers.clone(),
        feature.registration_keys.clone(),
    );
    let environment = resolver
        .read_allowed(id, &policy)
        .await
        .map_err(|e| e.to_string())?;
    if environment.status != environments::EnvironmentStatus::Ready
        || environment.desired_power != environments::PowerState::Running
    {
        return Err("environment is not accessible; discovery does not wake it".into());
    }
    let connection = gateway.connection_for(resolver.universe_id(), &environment);
    let mut client = EnvironmentDataClient::connect(
        &connection.endpoint,
        gateway.connect_options("lightspeed-source-discovery"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let initialized = client
        .initialize(&InitializeParams {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            client_name: "lightspeed-source-discovery".into(),
            scope: connection.scope,
            resume_connection_id: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    client
        .initialized(&InitializedParams {})
        .await
        .map_err(|e| e.to_string())?;
    if initialized.protocol_version != CURRENT_PROTOCOL_VERSION
        || !initialized.capabilities.filesystem_read
        || !initialized.capabilities.filesystem_scan
    {
        let _ = client.close().await;
        return Err("endpoint does not support filesystem source discovery".into());
    }
    let cwd = match working_directory(
        &mut client,
        feature.working_directory.as_deref(),
        initialized.default_cwd.as_deref(),
    )
    .await
    {
        Ok(cwd) => cwd,
        Err(error) => {
            let _ = client.close().await;
            return Err(error);
        }
    };
    Ok((client, initialized, cwd))
}
