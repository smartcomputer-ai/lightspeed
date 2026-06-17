//! Provider-neutral prompt text for environment projection context entries.

use engine::{BlobRef, ToolExecutionTarget, storage::BlobStore};
use tools::environment::projection::{
    EnvironmentActive, EnvironmentCapabilities, EnvironmentCatalogSnapshot, EnvironmentKind,
    EnvironmentStatus, FsRoute, FsRouteAccess, FsRouteSource, VfsCatalog,
};

use crate::error::{LlmAdapterError, LlmAdapterResult};

pub(crate) async fn read_vfs_catalog(
    blobs: &dyn BlobStore,
    blob_ref: &BlobRef,
) -> LlmAdapterResult<VfsCatalog> {
    read_projection(blobs, blob_ref).await
}

pub(crate) async fn read_environment_catalog(
    blobs: &dyn BlobStore,
    blob_ref: &BlobRef,
) -> LlmAdapterResult<EnvironmentCatalogSnapshot> {
    read_projection(blobs, blob_ref).await
}

pub(crate) async fn read_environment_active(
    blobs: &dyn BlobStore,
    blob_ref: &BlobRef,
) -> LlmAdapterResult<EnvironmentActive> {
    read_projection(blobs, blob_ref).await
}

pub(crate) fn vfs_catalog_text(catalog: &VfsCatalog) -> String {
    let mut text = String::from("Filesystem (virtual: file tools only, no shell):\n");
    if catalog.routes.is_empty() {
        text.push_str("  No VFS routes are currently mounted.\n");
    } else {
        for route in &catalog.routes {
            text.push_str(&format!("  {}\n", route_line(route)));
        }
    }
    text.push_str("\nThe VFS is not an execution environment. Commands cannot run in VFS paths.");
    text
}

pub(crate) fn environment_catalog_text(catalog: &EnvironmentCatalogSnapshot) -> String {
    let mut text = String::from("Execution environments:\n");
    if catalog.environments.is_empty() {
        text.push_str(
            "  No execution environments are configured. File tools may still work through fs:session; process tools require an active env target.",
        );
        return text;
    }

    for environment in &catalog.environments {
        text.push_str(&format!(
            "  {}{}\n",
            environment.env_id,
            if catalog.active_env_id.as_deref() == Some(environment.env_id.as_str()) {
                " [ACTIVE]"
            } else {
                ""
            }
        ));
        text.push_str(&format!(
            "    kind: {}, status: {}, capabilities: {}\n",
            environment_kind(environment.kind),
            environment_status(environment.status),
            capabilities(&environment.capabilities)
        ));
        if let Some(cwd) = &environment.cwd {
            text.push_str(&format!("    cwd: {cwd}\n"));
        }
        if let Some(target) = &environment.exec_target {
            text.push_str(&format!("    exec_target: {}\n", target_text(target)));
        }
    }
    if let Some(active_env_id) = &catalog.active_env_id {
        text.push_str(&format!(
            "\nCommands run in active environment {active_env_id}."
        ));
    } else {
        text.push_str("\nNo execution environment is active; commands cannot run.");
    }
    text
}

pub(crate) fn environment_active_text(active: &EnvironmentActive) -> String {
    let mut text = format!("Active execution environment: {}\n", active.env_id);
    if active.fs_routes.is_empty() {
        text.push_str(
            "\nNo filesystem routes are declared as the same underlying state as this environment.",
        );
        return text;
    }

    text.push_str("\nFilesystem routes for the active environment:\n");
    for route in &active.fs_routes {
        text.push_str(&format!("  {}\n", route_line(route)));
    }
    text
}

async fn read_projection<T>(blobs: &dyn BlobStore, blob_ref: &BlobRef) -> LlmAdapterResult<T>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = blobs.read_bytes(blob_ref).await?;
    serde_json::from_slice(&bytes).map_err(|error| LlmAdapterError::InvalidJson {
        blob_ref: blob_ref.clone(),
        message: error.to_string(),
    })
}

fn route_line(route: &FsRoute) -> String {
    let same_state = match &route.same_state_as_active_env {
        Some(env_id) => format!("; same files as shell in {env_id}"),
        None => "; no shell access".to_owned(),
    };
    format!(
        "{:<12} {} - {}{}",
        route.path,
        route_access(route.access),
        route_source(&route.source),
        same_state
    )
}

fn route_access(access: FsRouteAccess) -> &'static str {
    match access {
        FsRouteAccess::ReadOnly => "read-only",
        FsRouteAccess::ReadWrite => "read/write",
    }
}

fn route_source(source: &FsRouteSource) -> String {
    match source {
        FsRouteSource::VfsSnapshot { snapshot_ref } => {
            format!("VFS snapshot {snapshot_ref}")
        }
        FsRouteSource::VfsWorkspace { workspace_id } => {
            format!("VFS workspace {workspace_id}")
        }
        FsRouteSource::HostFilesystem { target } => {
            format!("environment filesystem {}", target_text(target))
        }
        FsRouteSource::FusedWorkspace { env_id } => {
            format!("fused workspace for {env_id}")
        }
    }
}

fn target_text(target: &ToolExecutionTarget) -> String {
    format!("{}:{}", target.namespace, target.id)
}

fn environment_kind(kind: EnvironmentKind) -> &'static str {
    match kind {
        EnvironmentKind::Sandbox => "sandbox",
        EnvironmentKind::RemoteHost => "remote_host",
        EnvironmentKind::AttachedHost => "attached_host",
        EnvironmentKind::Connector => "connector",
        EnvironmentKind::Browser => "browser",
    }
}

fn environment_status(status: EnvironmentStatus) -> &'static str {
    match status {
        EnvironmentStatus::Attaching => "attaching",
        EnvironmentStatus::Ready => "ready",
        EnvironmentStatus::Degraded => "degraded",
        EnvironmentStatus::Detached => "detached",
    }
}

fn capabilities(capabilities: &EnvironmentCapabilities) -> String {
    let mut names = Vec::new();
    if capabilities.fs_read {
        names.push("fs_read");
    }
    if capabilities.fs_write {
        names.push("fs_write");
    }
    if capabilities.process_exec {
        names.push("process_exec");
    }
    if capabilities.process_stdin {
        names.push("process_stdin");
    }
    if capabilities.network {
        names.push("network");
    }
    if capabilities.persistent {
        names.push("persistent");
    }
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use tools::environment::projection::{
        EnvironmentCatalogSnapshot, FsRoute, FsRouteAccess, FsRouteSource, VfsCatalog,
    };

    use super::*;

    #[test]
    fn vfs_catalog_text_says_no_shell() {
        let catalog = VfsCatalog::new(
            0,
            vec![FsRoute {
                path: tools::fs::FsPath::new("/workspace").unwrap(),
                access: FsRouteAccess::ReadWrite,
                source: FsRouteSource::VfsWorkspace {
                    workspace_id: "workspace_1".to_owned(),
                },
                same_state_as_active_env: None,
            }],
        );

        let text = vfs_catalog_text(&catalog);

        assert!(text.contains("/workspace"));
        assert!(text.contains("no shell"));
        assert!(text.contains("Commands cannot run in VFS paths"));
    }

    #[test]
    fn empty_environment_catalog_text_is_instructive() {
        let catalog = EnvironmentCatalogSnapshot::empty(0);

        let text = environment_catalog_text(&catalog);

        assert!(text.contains("No execution environments are configured"));
        assert!(text.contains("process tools require an active env target"));
    }
}
