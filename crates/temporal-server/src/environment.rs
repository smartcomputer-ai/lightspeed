//! Runtime resolution products for active universe environments.

use std::collections::BTreeMap;

use environments::EnvironmentRecord;
use tools::{environment::EnvironmentToolContext, fs::FsToolContext};

#[derive(Clone)]
pub struct RuntimeEnvironment {
    resource: EnvironmentRecord,
    tool_context: EnvironmentToolContext,
}

impl RuntimeEnvironment {
    pub fn from_resource(
        resource: EnvironmentRecord,
        mut tool_context: EnvironmentToolContext,
        fs_context: FsToolContext,
    ) -> Self {
        let environment_id = resource.environment_id.as_str().to_owned();
        tool_context = tool_context.with_environment_id(environment_id);
        let _ = fs_context;
        tool_context.filesystem = None;

        Self {
            resource,
            tool_context,
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

#[derive(Clone, Default)]
pub struct SessionEnvironmentManager {
    environments: BTreeMap<String, RuntimeEnvironment>,
}

impl SessionEnvironmentManager {
    pub fn new(_blobs: std::sync::Arc<dyn engine::storage::BlobStore>) -> Self {
        Self::default()
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
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use engine::storage::InMemoryBlobStore;
    use environments::{
        EnvironmentIncarnationId, EnvironmentIncarnationRecord, EnvironmentProviderBindingId,
        EnvironmentProviderId, EnvironmentProvisionRequestId, EnvironmentSource, EnvironmentStatus,
        EnvironmentTemplateId,
    };
    use host_protocol::shared::HostTargetId;
    use tools::fs::{FsPath, FsToolContext, InMemoryFileSystem};

    use super::*;

    fn resource() -> EnvironmentRecord {
        let target_id = HostTargetId::new("target-a");
        EnvironmentRecord {
            environment_id: engine::EnvironmentId::new("environment-a"),
            request_id: EnvironmentProvisionRequestId::new("request-a"),
            source: EnvironmentSource::Provisioned {
                provider_id: EnvironmentProviderId::new("provider-a"),
                binding_id: EnvironmentProviderBindingId::new("binding-a"),
            },
            display_name: None,
            status: EnvironmentStatus::Offline,
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: EnvironmentIncarnationId::new("incarnation-a"),
                provision_request_id: Some(EnvironmentProvisionRequestId::new("request-a")),
                provider_target_id: Some(target_id.clone()),
                template_id: Some(EnvironmentTemplateId::new("template-a")),
                adoption_source_target: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: BTreeMap::from([("fsRoot".to_owned(), "/sandbox".to_owned())]),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn environment_has_no_filesystem_before_p119_data_plane_admission() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = FsToolContext::new(Arc::new(InMemoryFileSystem::full_access()), blobs.clone())
            .with_cwd(FsPath::new("/sandbox/project").expect("cwd"));
        let context = EnvironmentToolContext::new(None, blobs);

        let environment = RuntimeEnvironment::from_resource(resource(), context, fs);
        assert!(environment.tool_context().filesystem.is_none());
    }
}
