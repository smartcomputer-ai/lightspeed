//! Canonical glob operation.

use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    host::{context::HostToolContext, fs::FsPath},
};

use super::{collect_file_paths, invalid_request, relative_path_string, resolve_path};

pub const DEFAULT_GLOB_LIMIT: usize = 1_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
    pub path: Option<FsPath>,
    pub max_depth: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GlobResult {
    pub path: FsPath,
    pub pattern: String,
    pub matches: Vec<FsPath>,
    pub truncated: bool,
}

pub async fn invoke_glob(ctx: &HostToolContext, args: GlobArgs) -> ToolResult<GlobResult> {
    if args.pattern.is_empty() {
        return Err(invalid_request("glob pattern must not be empty"));
    }
    let pattern = Pattern::new(&args.pattern)
        .map_err(|error| invalid_request(format!("invalid glob pattern: {error}")))?;
    let limit = args.limit.unwrap_or(DEFAULT_GLOB_LIMIT);
    if limit == 0 {
        return Err(invalid_request("glob limit must be 1 or greater"));
    }

    let root = match args.path {
        Some(path) => resolve_path(ctx, &path)?,
        None => ctx.cwd.clone().unwrap_or_else(FsPath::current_dir),
    };
    let paths = collect_file_paths(ctx, root.clone(), args.max_depth).await?;
    let mut matches = Vec::new();

    for path in paths {
        if glob_matches(&pattern, &args.pattern, &path, &root) {
            matches.push(path);
            if matches.len() > limit {
                matches.truncate(limit);
                return Ok(GlobResult {
                    path: root,
                    pattern: args.pattern,
                    matches,
                    truncated: true,
                });
            }
        }
    }

    Ok(GlobResult {
        path: root,
        pattern: args.pattern,
        matches,
        truncated: false,
    })
}

fn glob_matches(pattern: &Pattern, pattern_text: &str, path: &FsPath, root: &FsPath) -> bool {
    if pattern_text.starts_with('/') {
        return pattern.matches(path.as_str());
    }

    let relative = relative_path_string(path, root);
    pattern.matches(&relative)
        || (!pattern_text.contains('/')
            && path
                .file_name()
                .is_some_and(|file_name| pattern.matches(file_name)))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::storage::InMemoryBlobStore;

    use super::*;
    use crate::host::fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem};

    fn context(fs: Arc<dyn FileSystem>) -> HostToolContext {
        HostToolContext::new(fs, None, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_glob_finds_files_relative_to_root() {
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/workspace/src").expect("src"),
            CreateDirectoryOptions::recursive(),
        )
        .await
        .expect("create src");
        fs.write_file(&FsPath::new("/workspace/src/lib.rs").unwrap(), Vec::new())
            .await
            .expect("write lib");
        fs.write_file(&FsPath::new("/workspace/README.md").unwrap(), Vec::new())
            .await
            .expect("write readme");
        let ctx = context(Arc::new(fs)).with_cwd(FsPath::new("/workspace").expect("cwd"));

        let result = invoke_glob(
            &ctx,
            GlobArgs {
                pattern: "**/*.rs".to_string(),
                path: None,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("glob");

        assert_eq!(
            result.matches,
            vec![FsPath::new("/workspace/src/lib.rs").unwrap()]
        );
        assert!(!result.truncated);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_glob_applies_limit() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/a.txt").unwrap(), Vec::new())
            .await
            .expect("write a");
        fs.write_file(&FsPath::new("/b.txt").unwrap(), Vec::new())
            .await
            .expect("write b");
        let ctx = context(Arc::new(fs));

        let result = invoke_glob(
            &ctx,
            GlobArgs {
                pattern: "*.txt".to_string(),
                path: Some(FsPath::root()),
                max_depth: None,
                limit: Some(1),
            },
        )
        .await
        .expect("glob");

        assert_eq!(result.matches.len(), 1);
        assert!(result.truncated);
    }
}
