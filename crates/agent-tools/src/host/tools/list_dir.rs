//! Canonical list-directory operation.

use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    host::{context::HostToolContext, fs::FsPath},
};

use super::resolve_path;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListDirArgs {
    #[serde(default = "FsPath::root")]
    pub path: FsPath,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListDirEntry {
    pub file_name: String,
    pub is_directory: bool,
    pub is_file: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListDirResult {
    pub path: FsPath,
    pub resolved_path: FsPath,
    pub entries: Vec<ListDirEntry>,
}

pub async fn invoke_list_dir(
    ctx: &HostToolContext,
    args: ListDirArgs,
) -> ToolResult<ListDirResult> {
    let resolved_path = resolve_path(ctx, &args.path)?;
    let mut entries = ctx
        .fs
        .read_directory(&resolved_path)
        .await?
        .into_iter()
        .map(|entry| ListDirEntry {
            file_name: entry.file_name,
            is_directory: entry.is_directory,
            is_file: entry.is_file,
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.file_name.cmp(&right.file_name));

    Ok(ListDirResult {
        path: args.path,
        resolved_path,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agent_core::storage::InMemoryBlobStore;

    use super::*;
    use crate::host::fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem};

    fn context(fs: Arc<dyn FileSystem>) -> HostToolContext {
        HostToolContext::new(fs, None, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_list_dir_resolves_relative_paths_and_sorts_entries() {
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/workspace").expect("workspace"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create workspace");
        fs.create_directory(
            &FsPath::new("/workspace/zeta").expect("dir"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create dir");
        fs.write_file(
            &FsPath::new("/workspace/alpha.txt").expect("file"),
            b"hello".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs)).with_cwd(FsPath::new("/workspace").expect("cwd"));

        let result = invoke_list_dir(
            &ctx,
            ListDirArgs {
                path: FsPath::current_dir(),
            },
        )
        .await
        .expect("list dir");

        assert_eq!(result.resolved_path, FsPath::new("/workspace").unwrap());
        assert_eq!(
            result.entries,
            vec![
                ListDirEntry {
                    file_name: "alpha.txt".to_string(),
                    is_directory: false,
                    is_file: true,
                },
                ListDirEntry {
                    file_name: "zeta".to_string(),
                    is_directory: true,
                    is_file: false,
                },
            ]
        );
    }
}
