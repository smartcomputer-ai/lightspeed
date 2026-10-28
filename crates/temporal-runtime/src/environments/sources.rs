//! Environment source connections and directory validation; discovery never wakes a machine.
use crate::{
    environments::gateway::EnvironmentGatewayClientConfig,
    environments::resolver::EnvironmentResolver,
};
use environment_client::DataClient;
use environment_protocol::{
    data::{
        handshake::InitializeResponse,
        scan::{ScanParams, ScanResponse, ScanRootResult},
    },
    paths::EnvironmentPath,
};
use harness::{
    ContextEntryInput, ContextEntryKey, CoreAgentCommand, EnvironmentId, EnvironmentsFeature,
    SessionId,
    storage::{BlobStore, BlobStoreError},
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// `clientInfo.name` of discovery connections.
const SOURCE_DISCOVERY_CLIENT: &str = "lightspeed-source-discovery";

/// The working directory discovery resolves sources against: the
/// attachment's configured directory, else the daemon's `defaultCwd`. It is
/// proven to exist by the probe root of each discovery scan, not by a
/// separate call.
pub(crate) fn working_directory(
    configured: Option<&str>,
    default: &EnvironmentPath,
) -> Result<EnvironmentPath, String> {
    let Some(value) = configured else {
        return Ok(default.clone());
    };
    if !value.starts_with('/') {
        return Err("environment working directory must be absolute".into());
    }
    let cwd = tools::environment::sources::absolute(std::path::Path::new("/"), value)?;
    EnvironmentPath::new(cwd.to_string_lossy()).map_err(|error| error.to_string())
}

pub(crate) struct Discovery<'a> {
    resolver: Option<&'a EnvironmentResolver>,
    gateway: Option<&'a EnvironmentGatewayClientConfig>,
    pub feature: Option<&'a EnvironmentsFeature>,
    pub environment_id: Option<&'a EnvironmentId>,
    connection: Option<SourceConnection>,
}

pub(crate) struct SourceConnection {
    pub client: DataClient,
    pub initialized: InitializeResponse,
    pub cwd: EnvironmentPath,
    // Includes the universe so cached observations cannot cross routes.
    pub identity: String,
}

impl SourceConnection {
    /// Run a discovery scan with the working-directory probe appended, check
    /// that probe, and return the response without it. `fs/scan` is an
    /// observation, so discovery never resets the idle clock.
    pub async fn scan(&self, mut query: ScanParams) -> Result<ScanResponse, String> {
        tools::environment::sources::with_working_directory_probe(&mut query, &self.cwd);
        let mut scan = self
            .client
            .scan(&query)
            .await
            .map_err(|error| error.to_string())?;
        match scan.roots.pop() {
            Some(ScanRootResult::Complete { .. } | ScanRootResult::Unchanged { .. }) => Ok(scan),
            Some(ScanRootResult::Missing { .. }) => {
                Err(format!("working directory {} does not exist", self.cwd))
            }
            Some(ScanRootResult::Failed { error, .. }) => {
                Err(format!("working directory {}: {error}", self.cwd))
            }
            None => Err("discovery scan answered without its working directory probe".into()),
        }
    }
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
        // Abandoned scans were cancelled by the multiplexed client; a fresh
        // connection keeps a failed attempt from affecting the next source.
        if let Some(connection) = self.connection.take() {
            tokio::spawn(async move {
                let _ = connection.client.close().await;
            });
        }
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
            crate::environments::skills::refresh(blobs, &mut discovery, session_id, current_skills)
                .await?;
        let prompt_entries = crate::environments::prompts::refresh(blobs, &mut discovery).await?;
        Ok(Publication {
            skill_command,
            prompt_entries,
        })
    }
    .await;
    if let Some(connection) = discovery.connection.take() {
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
    let Some(attachment) = feature.attachment(id.as_str()) else {
        return Err("active environment is not attached to this session".into());
    };
    let environment = {
        let _timer = PhaseTimer::new("registry");
        resolver.read(id).await.map_err(|e| e.to_string())?
    };
    // A data route dial wakes a powered-down target, and discovery must
    // never do that. A usable row whose last observation shows it powered
    // down (or otherwise not ready) is skipped. The observation can be
    // stale (providers power targets down on their own clock, and a dial
    // wakes them without a write), so the dial also asks the gateway to
    // check the provider first.
    let now_ms = crate::environments::lifecycle::clock_ms();
    if environment.use_gate(now_ms) != environments::EnvironmentUse::Use
        || environment.display_status() != environments::EnvironmentDisplayStatus::Ready
    {
        return Err("environment is not accessible; discovery does not wake it".into());
    }
    let endpoint = gateway.endpoint_for(resolver.universe_id(), &environment);
    let identity =
        serde_json::to_string(&(resolver.universe_id(), environment.environment_id.as_str()))
            .map_err(|e| e.to_string())?;
    let (client, initialized) = {
        let _timer = PhaseTimer::new("connect");
        DataClient::connect_initialized(
            &endpoint,
            gateway.no_wake_connect_options(SOURCE_DISCOVERY_CLIENT),
        )
        .await
        .map_err(|e| e.to_string())?
    };
    if initialized.capabilities.filesystem.is_none() {
        let _ = client.close().await;
        return Err("endpoint does not support filesystem source discovery".into());
    }
    let cwd = match working_directory(
        attachment.working_directory.as_deref(),
        &initialized.default_cwd,
    ) {
        Ok(cwd) => cwd,
        Err(error) => {
            let _ = client.close().await;
            return Err(error);
        }
    };
    Ok(SourceConnection {
        client,
        initialized,
        cwd,
        identity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_directory_is_configured_or_the_daemon_default() {
        let default = EnvironmentPath::new("/workspace").unwrap();
        assert_eq!(working_directory(None, &default).unwrap(), default);
        assert_eq!(
            working_directory(Some("/srv/app/./src/.."), &default)
                .unwrap()
                .as_str(),
            "/srv/app"
        );
        assert!(working_directory(Some("relative"), &default).is_err());
        assert!(working_directory(Some("/../escape"), &default).is_err());
    }
}
