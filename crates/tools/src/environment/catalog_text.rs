//! Catalog text rendered once by the publisher.

use crate::environment::projection::{FsRoute, FsRouteAccess, FsRouteSource, VfsCatalog};

pub(crate) fn vfs_catalog_text(catalog: &VfsCatalog) -> String {
    let mut text = String::new();
    if catalog.routes.is_empty() {
        text.push_str("  No VFS routes are currently mounted.\n");
    } else {
        for route in &catalog.routes {
            text.push_str(&format!("  {}\n", route_line(route)));
        }
    }
    text.push_str(
        "\nUse vfs_* tools for these paths. VFS files are not visible to environment file tools or commands. Ordinary file and command tools operate only on the active environment.",
    );
    text
}

fn route_line(route: &FsRoute) -> String {
    format!(
        "{:<12} {} - {}",
        route.path,
        route_access(route.access),
        route_source(&route.source)
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
        FsRouteSource::VfsSnapshot { .. } => "VFS snapshot".to_owned(),
        FsRouteSource::VfsWorkspace { .. } => "VFS workspace".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use crate::environment::projection::{
        FsRoute, FsRouteAccess, FsRouteAvailability, FsRouteSource, VfsCatalog,
    };

    use super::*;

    #[test]
    fn vfs_catalog_text_says_no_shell() {
        let catalog = VfsCatalog::new(
            0,
            vec![FsRoute {
                path: crate::fs::FsPath::new("/workspace").unwrap(),
                source_path: None,
                access: FsRouteAccess::ReadWrite,
                source: FsRouteSource::VfsWorkspace {
                    workspace_id: "workspace_1".to_owned(),
                },
                availability: FsRouteAvailability::Available,
            }],
        );

        let text = vfs_catalog_text(&catalog);

        assert!(text.contains("/workspace"));
        assert!(text.contains("Use vfs_* tools"));
        assert!(text.contains("not visible to environment file tools"));
        assert!(!text.contains("workspace_1"));
        let mut first = catalog.clone();
        first.routes[0].source = FsRouteSource::VfsSnapshot {
            snapshot_ref: harness::BlobRef::from_bytes(b"first snapshot"),
        };
        let mut second = first.clone();
        second.routes[0].source = FsRouteSource::VfsSnapshot {
            snapshot_ref: harness::BlobRef::from_bytes(b"second snapshot"),
        };
        assert_eq!(vfs_catalog_text(&first), vfs_catalog_text(&second));
    }
}
