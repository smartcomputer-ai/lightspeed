//! Canonical glob operation.

use std::time::{Duration, Instant};

use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    fs::{FsGlobLimits, FsGlobRequest, FsPath, FsSearchStop, FsToolContext},
};

use super::{collect_file_paths_bounded, invalid_request, relative_path_string, resolve_path};

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
    /// Why the enumeration stopped early; absent when it exhausted the tree
    /// within its bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<FsSearchStop>,
}

pub async fn invoke_glob(ctx: &FsToolContext, args: GlobArgs) -> ToolResult<GlobResult> {
    if args.pattern.is_empty() {
        return Err(invalid_request("glob pattern must not be empty"));
    }
    let pattern = Pattern::new(&args.pattern)
        .map_err(|error| invalid_request(format!("invalid glob pattern: {error}")))?;
    let requested_limit = args.limit.unwrap_or(DEFAULT_GLOB_LIMIT);
    if requested_limit == 0 {
        return Err(invalid_request("glob limit must be 1 or greater"));
    }
    // A caller may request fewer matches than the deployment maximum but can
    // never raise any traversal bound.
    let limits = FsGlobLimits {
        max_matches: (requested_limit as u64).min(ctx.limits.max_search_matches),
        max_entries: ctx.limits.max_search_files,
        max_duration_ms: ctx.limits.max_search_duration_ms,
    };

    let root = match args.path.as_ref() {
        Some(path) => resolve_path(ctx, path)?,
        None => ctx.fs_cwd.clone().unwrap_or_else(FsPath::current_dir),
    };

    // A backend with a native enumeration (e.g. a remote host) walks the
    // tree locally under the same bounds and returns only matching paths.
    let native_request = FsGlobRequest {
        root: root.clone(),
        pattern: args.pattern.clone(),
        max_depth: args.max_depth,
        limits,
    };
    if let Some(native) = ctx.fs.glob_files(&native_request).await? {
        return Ok(GlobResult {
            path: root,
            pattern: args.pattern,
            matches: native.matches,
            truncated: native.stopped.is_some(),
            stopped: native.stopped,
        });
    }

    let started = Instant::now();
    let deadline = Duration::from_millis(limits.max_duration_ms);
    let (paths, enumeration_stop) = collect_file_paths_bounded(
        ctx,
        root.clone(),
        args.max_depth,
        limits.max_entries,
        started,
        deadline,
    )
    .await?;
    let mut stopped = enumeration_stop;
    let mut matches = Vec::new();

    for path in paths {
        if glob_matches(&pattern, &args.pattern, &path, &root) {
            // Like the match budget elsewhere, truncation is reported only
            // when a match beyond the cap actually arrives.
            if matches.len() as u64 >= limits.max_matches {
                stopped = Some(FsSearchStop::MatchLimit);
                break;
            }
            matches.push(path);
        }
    }

    Ok(GlobResult {
        path: root,
        pattern: args.pattern,
        matches,
        truncated: stopped.is_some(),
        stopped,
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

    use harness::storage::InMemoryBlobStore;

    use super::*;
    use crate::fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem};

    fn context(fs: Arc<dyn FileSystem>) -> FsToolContext {
        FsToolContext::new(fs, Arc::new(InMemoryBlobStore::new()))
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
        assert_eq!(result.stopped, Some(FsSearchStop::MatchLimit));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_glob_stops_at_entry_budget_with_a_typed_reason() {
        let fs = InMemoryFileSystem::full_access();
        for index in 0..5 {
            fs.write_file(
                &FsPath::new(format!("/file_{index}.txt")).unwrap(),
                Vec::new(),
            )
            .await
            .expect("write file");
        }
        let ctx = context(Arc::new(fs)).with_limits(crate::limits::ToolLimits {
            max_search_files: 2,
            ..crate::limits::ToolLimits::default()
        });

        let result = invoke_glob(
            &ctx,
            GlobArgs {
                pattern: "*.txt".to_string(),
                path: Some(FsPath::root()),
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("glob");

        assert_eq!(result.stopped, Some(FsSearchStop::FileLimit));
        assert!(result.truncated);
        assert_eq!(result.matches.len(), 2);
    }

    struct NativeGlobFileSystem {
        inner: InMemoryFileSystem,
        response: crate::fs::FsGlobResponse,
    }

    #[async_trait::async_trait]
    impl FileSystem for NativeGlobFileSystem {
        fn access_policy(&self) -> crate::fs::FileAccessPolicy {
            self.inner.access_policy()
        }

        async fn read_file(&self, path: &FsPath) -> crate::fs::FsResult<Vec<u8>> {
            self.inner.read_file(path).await
        }

        async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> crate::fs::FsResult<()> {
            self.inner.write_file(path, contents).await
        }

        async fn create_directory(
            &self,
            path: &FsPath,
            options: CreateDirectoryOptions,
        ) -> crate::fs::FsResult<()> {
            self.inner.create_directory(path, options).await
        }

        async fn get_metadata(
            &self,
            _path: &FsPath,
        ) -> crate::fs::FsResult<crate::fs::FileMetadata> {
            panic!("native glob must not fall back to generic traversal");
        }

        async fn read_directory(
            &self,
            _path: &FsPath,
        ) -> crate::fs::FsResult<Vec<crate::fs::ReadDirectoryEntry>> {
            panic!("native glob must not fall back to per-directory reads");
        }

        async fn remove(
            &self,
            path: &FsPath,
            options: crate::fs::RemoveOptions,
        ) -> crate::fs::FsResult<()> {
            self.inner.remove(path, options).await
        }

        async fn copy(
            &self,
            source_path: &FsPath,
            destination_path: &FsPath,
            options: crate::fs::CopyOptions,
        ) -> crate::fs::FsResult<()> {
            self.inner
                .copy(source_path, destination_path, options)
                .await
        }

        async fn glob_files(
            &self,
            request: &FsGlobRequest,
        ) -> crate::fs::FsResult<Option<crate::fs::FsGlobResponse>> {
            assert_eq!(request.pattern, "**/*.rs");
            assert!(
                request.limits.max_matches
                    <= crate::limits::ToolLimits::default().max_search_matches
            );
            Ok(Some(self.response.clone()))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_glob_prefers_a_native_enumeration_backend() {
        let fs = NativeGlobFileSystem {
            inner: InMemoryFileSystem::full_access(),
            response: crate::fs::FsGlobResponse {
                matches: vec![FsPath::new("/remote/src/lib.rs").unwrap()],
                entries_visited: 40,
                stopped: Some(FsSearchStop::TimeLimit),
            },
        };
        let ctx = context(Arc::new(fs));

        let result = invoke_glob(
            &ctx,
            GlobArgs {
                pattern: "**/*.rs".to_string(),
                path: Some(FsPath::root()),
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("glob");

        assert_eq!(
            result.matches,
            vec![FsPath::new("/remote/src/lib.rs").unwrap()]
        );
        assert!(result.truncated);
        assert_eq!(result.stopped, Some(FsSearchStop::TimeLimit));
    }
}
