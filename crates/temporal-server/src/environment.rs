//! Runtime resolution products for active universe environments.

use std::collections::BTreeMap;

use environment_client::WebSocketTransport;
use environments::EnvironmentRecord;
use tools::{
    environment::EnvironmentToolContext, environment_protocol::RemoteEnvironmentConnection,
};

#[derive(Clone)]
pub struct RuntimeEnvironment {
    resource: EnvironmentRecord,
    tool_context: EnvironmentToolContext,
    remote_connection: Option<RemoteEnvironmentConnection<WebSocketTransport>>,
}

impl RuntimeEnvironment {
    /// Wraps a negotiated tool context for one registry environment. The
    /// filesystem, process, and job capabilities were already gated by the
    /// data-plane capability negotiation that produced `tool_context`; this
    /// only stamps the environment identity.
    pub fn from_resource(
        resource: EnvironmentRecord,
        tool_context: EnvironmentToolContext,
    ) -> Self {
        let environment_id = resource.environment_id.as_str().to_owned();
        let tool_context = tool_context.with_environment_id(environment_id);

        Self {
            resource,
            tool_context,
            remote_connection: None,
        }
    }

    pub fn with_remote_connection(
        mut self,
        connection: RemoteEnvironmentConnection<WebSocketTransport>,
    ) -> Self {
        self.remote_connection = Some(connection);
        self
    }

    pub async fn with_working_directory(
        mut self,
        cwd: tools::fs::FsPath,
    ) -> Result<Self, tools::fs::FsError> {
        if let Some(filesystem) = &mut self.tool_context.filesystem {
            let metadata = filesystem.fs.get_metadata(&cwd).await?;
            if !metadata.is_directory {
                return Err(tools::fs::FsError::InvalidInput {
                    message: format!("working directory is not a directory: {cwd}"),
                });
            }
            filesystem.fs_cwd = Some(cwd.clone());
        }
        self.tool_context.process_cwd = Some(cwd.clone());
        if let Some(connection) = self.remote_connection.take() {
            self.remote_connection = Some(connection.with_cwd(cwd));
        }
        Ok(self)
    }

    pub async fn close(&self) {
        if let Some(connection) = &self.remote_connection {
            let _ = connection.close().await;
        }
    }

    pub fn environment_id(&self) -> &str {
        self.resource.environment_id.as_str()
    }

    pub fn resource(&self) -> &EnvironmentRecord {
        &self.resource
    }

    pub fn tool_context(&self) -> &EnvironmentToolContext {
        &self.tool_context
    }
}

/// Why the session's active environment could not be resolved into a tool
/// context for this invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveEnvironmentBlocker {
    /// The environment exists and is provisioning or booting; the call must
    /// wait rather than fail.
    NotReady {
        environment_id: String,
        status: environments::EnvironmentStatus,
    },
    /// The environment cannot serve calls (failed, closed, unreachable, or
    /// not allowed); the call fails with this message.
    Unavailable { message: String },
}

#[derive(Clone, Default)]
pub struct SessionEnvironmentManager {
    environments: BTreeMap<String, RuntimeEnvironment>,
    active_blocker: Option<ActiveEnvironmentBlocker>,
}

impl SessionEnvironmentManager {
    pub fn new(_blobs: std::sync::Arc<dyn engine::storage::BlobStore>) -> Self {
        Self::default()
    }

    pub fn with_active_blocker(mut self, blocker: ActiveEnvironmentBlocker) -> Self {
        self.active_blocker = Some(blocker);
        self
    }

    /// The reason the active environment has no tool context in this manager,
    /// if resolution found one.
    pub fn active_blocker(&self) -> Option<&ActiveEnvironmentBlocker> {
        self.active_blocker.as_ref()
    }

    pub fn insert_environment(&mut self, environment: RuntimeEnvironment) {
        self.environments
            .insert(environment.environment_id().to_owned(), environment);
    }

    pub fn environment(&self, environment_id: &str) -> Option<&RuntimeEnvironment> {
        self.environments.get(environment_id)
    }

    pub fn active_tool_context(&self, environment_id: &str) -> Option<EnvironmentToolContext> {
        self.environment(environment_id)
            .map(|environment| environment.tool_context.clone())
    }

    pub async fn close(&self) {
        for environment in self.environments.values() {
            environment.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use engine::storage::InMemoryBlobStore;
    use environment_protocol::shared::ProviderTargetId;
    use environments::{
        EnvironmentIncarnationId, EnvironmentIncarnationRecord, EnvironmentProviderBindingId,
        EnvironmentProviderId, EnvironmentProvisionRequestId, EnvironmentSource, EnvironmentStatus,
        EnvironmentTemplateId,
    };
    use tools::fs::{FsPath, FsToolContext, InMemoryFileSystem};

    use super::*;

    fn resource() -> EnvironmentRecord {
        let target_id = ProviderTargetId::new("target-a");
        EnvironmentRecord {
            environment_id: engine::EnvironmentId::new("environment-a"),
            request_id: EnvironmentProvisionRequestId::new("request-a"),
            source: EnvironmentSource::Provisioned {
                provider_id: EnvironmentProviderId::new("provider-a"),
                binding_id: EnvironmentProviderBindingId::new("binding-a"),
            },
            display_name: None,
            status: EnvironmentStatus::Offline,
            desired_power: environments::PowerState::Running,
            idle_policy: None,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: EnvironmentIncarnationId::new("incarnation-a"),
                provision_request_id: Some(EnvironmentProvisionRequestId::new("request-a")),
                provider_target_id: Some(target_id.clone()),
                template_id: Some(EnvironmentTemplateId::new("template-a")),
                adoption_source_target: None,
                power_states: Vec::new(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: BTreeMap::from([("fsRoot".to_owned(), "/sandbox".to_owned())]),
            last_seen_at_ms: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn environment_keeps_negotiated_filesystem_context() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = FsToolContext::new(Arc::new(InMemoryFileSystem::full_access()), blobs.clone())
            .with_cwd(FsPath::new("/sandbox/project").expect("cwd"));
        let context = EnvironmentToolContext::new(None, blobs).with_filesystem(fs);

        let environment = RuntimeEnvironment::from_resource(resource(), context);
        let filesystem = environment
            .tool_context()
            .filesystem
            .as_ref()
            .expect("filesystem context survives runtime wrapping");
        assert_eq!(
            filesystem.fs_cwd,
            Some(FsPath::new("/sandbox/project").expect("cwd"))
        );
        assert_eq!(
            environment.tool_context().environment_id.as_deref(),
            Some("environment-a")
        );
    }

    #[test]
    fn environment_without_negotiated_filesystem_stays_without_one() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let context = EnvironmentToolContext::new(None, blobs);

        let environment = RuntimeEnvironment::from_resource(resource(), context);
        assert!(environment.tool_context().filesystem.is_none());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn directory_override_aligns_files_and_processes_without_mutating_the_template() {
        use tools::fs::{CreateDirectoryOptions, FileSystem};
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/project").unwrap(),
            CreateDirectoryOptions::recursive(),
        )
        .await
        .unwrap();
        fs.write_file(
            &FsPath::new("/project/note.txt").unwrap(),
            b"project file".to_vec(),
        )
        .await
        .unwrap();
        let blobs = Arc::new(InMemoryBlobStore::new());
        let context = EnvironmentToolContext::new(None, blobs.clone())
            .with_filesystem(FsToolContext::new(Arc::new(fs), blobs));
        let original = RuntimeEnvironment::from_resource(resource(), context);
        let configured = original
            .clone()
            .with_working_directory(FsPath::new("/project").unwrap())
            .await
            .unwrap();
        assert_eq!(
            configured
                .tool_context()
                .process_cwd
                .as_ref()
                .unwrap()
                .as_str(),
            "/project"
        );
        assert_eq!(
            configured
                .tool_context()
                .filesystem
                .as_ref()
                .unwrap()
                .fs_cwd
                .as_ref()
                .unwrap()
                .as_str(),
            "/project"
        );
        assert!(original.tool_context().process_cwd.is_none());
        assert!(
            original
                .clone()
                .with_working_directory(FsPath::new("/missing").unwrap())
                .await
                .is_err()
        );
        assert!(
            original
                .with_working_directory(FsPath::new("/project/note.txt").unwrap())
                .await
                .is_err()
        );
    }
}
