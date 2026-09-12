//! Environment source connections and directory validation; discovery never wakes a machine.
use crate::{
    environment_gateway::EnvironmentGatewayClientConfig, environment_resolver::EnvironmentResolver,
};
use engine::{
    ContextEntryInput, ContextEntryKey, CoreAgentCommand, EnvironmentId, EnvironmentsFeature,
    SessionId,
    storage::{BlobStore, BlobStoreError},
};
use environment_client::{EnvironmentDataClient, JsonRpcTransport, WebSocketTransport};
use environment_protocol::{
    data::{
        fs::GetMetadataParams,
        handshake::{InitializeParams, InitializeResponse, InitializedParams},
    },
    shared::{CURRENT_PROTOCOL_VERSION, EnvironmentPath},
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
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

pub(crate) struct Discovery<'a> {
    resolver: Option<&'a EnvironmentResolver>,
    gateway: Option<&'a EnvironmentGatewayClientConfig>,
    pub feature: Option<&'a EnvironmentsFeature>,
    pub environment_id: Option<&'a EnvironmentId>,
    connection: Option<SourceConnection>,
}

pub(crate) struct SourceConnection {
    pub client: EnvironmentDataClient<WebSocketTransport>,
    pub initialized: InitializeResponse,
    pub cwd: String,
    // Includes universe and incarnation so cached observations cannot cross routes.
    pub identity: String,
}

impl Discovery<'_> {
    pub async fn connection(&mut self) -> Result<&mut SourceConnection, String> {
        if self.connection.is_none() {
            self.connection = Some(
                connect(
                    self.resolver
                        .ok_or("environment discovery resolver unavailable")?,
                    self.gateway.ok_or("environment gateway unavailable")?,
                    self.feature.ok_or("environment feature unavailable")?,
                    self.environment_id.ok_or("no environment selected")?,
                )
                .await?,
            );
        }
        Ok(self
            .connection
            .as_mut()
            .expect("connected source discovery"))
    }

    pub fn discard_connection(&mut self) {
        // A cancelled RPC can leave an unread response. Never reuse that stream.
        self.connection = None;
    }
}

pub(crate) struct Publication {
    pub skill_command: Option<CoreAgentCommand>,
    pub prompt_entries: BTreeMap<ContextEntryKey, ContextEntryInput>,
}

#[tracing::instrument(skip_all, fields(%session_id, environment_id = ?environment_id))]
pub(crate) async fn refresh(
    blobs: &dyn BlobStore,
    resolver: Option<&EnvironmentResolver>,
    gateway: Option<&EnvironmentGatewayClientConfig>,
    session_id: &SessionId,
    feature: Option<&EnvironmentsFeature>,
    environment_id: Option<&EnvironmentId>,
    current_skills: Option<&ContextEntryInput>,
) -> Result<Publication, BlobStoreError> {
    let _total = PhaseTimer::new("total");
    let mut discovery = Discovery {
        resolver,
        gateway,
        feature,
        environment_id,
        connection: None,
    };
    let result = async {
        let skill_command =
            crate::environment_skills::refresh(blobs, &mut discovery, session_id, current_skills)
                .await?;
        let prompt_entries = crate::environment_prompts::refresh(blobs, &mut discovery).await?;
        Ok(Publication {
            skill_command,
            prompt_entries,
        })
    }
    .await;
    if let Some(mut connection) = discovery.connection.take() {
        let _close = PhaseTimer::new("close");
        let _ = tokio::time::timeout(Duration::from_secs(1), connection.client.close()).await;
    }
    result
}

// Drop also records elapsed time when a bounded attempt is cancelled.
pub(crate) struct PhaseTimer {
    phase: &'static str,
    started: Instant,
}
impl PhaseTimer {
    pub fn new(phase: &'static str) -> Self {
        Self {
            phase,
            started: Instant::now(),
        }
    }
}
impl Drop for PhaseTimer {
    fn drop(&mut self) {
        tracing::debug!(
            phase = self.phase,
            elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0,
            "environment source discovery timing"
        );
    }
}

async fn connect(
    resolver: &EnvironmentResolver,
    gateway: &EnvironmentGatewayClientConfig,
    feature: &EnvironmentsFeature,
    id: &EnvironmentId,
) -> Result<SourceConnection, String> {
    let policy = environments::EnvironmentAccessPolicy::new(
        feature.providers.clone(),
        feature.registration_keys.clone(),
    );
    let environment = {
        let _timer = PhaseTimer::new("registry");
        resolver
            .read_allowed(id, &policy)
            .await
            .map_err(|e| e.to_string())?
    };
    if environment.status != environments::EnvironmentStatus::Ready
        || environment.desired_power != environments::PowerState::Running
    {
        return Err("environment is not accessible; discovery does not wake it".into());
    }
    let connection = gateway.connection_for(resolver.universe_id(), &environment);
    let identity =
        serde_json::to_string(&(resolver.universe_id(), &connection)).map_err(|e| e.to_string())?;
    let mut client = {
        let _timer = PhaseTimer::new("connect");
        EnvironmentDataClient::connect(
            &connection.endpoint,
            gateway.connect_options("lightspeed-source-discovery"),
        )
        .await
        .map_err(|e| e.to_string())?
    };
    let initialized = {
        let _timer = PhaseTimer::new("initialize");
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
        initialized
    };
    if initialized.protocol_version != CURRENT_PROTOCOL_VERSION
        || !initialized.capabilities.filesystem_read
        || !initialized.capabilities.filesystem_scan
    {
        return Err("endpoint does not support filesystem source discovery".into());
    }
    let cwd = {
        let _timer = PhaseTimer::new("working_directory");
        working_directory(
            &mut client,
            feature.working_directory.as_deref(),
            initialized.default_cwd.as_deref(),
        )
        .await?
    };
    Ok(SourceConnection {
        client,
        initialized,
        cwd,
        identity,
    })
}
