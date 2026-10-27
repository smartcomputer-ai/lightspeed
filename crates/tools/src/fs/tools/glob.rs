//! Canonical glob operation.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    fs::{FsGlobLimits, FsGlobRequest, FsPath, FsSearchStop, FsToolContext, pattern::PathPattern},
};

use super::{
    collect_file_paths_bounded, invalid_request, relative_path_string, resolve_path,
    root_relative_pattern,
};

pub const DEFAULT_GLOB_LIMIT: usize = 1_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GlobArgs {
    pub pattern: String,
    pub path: Option<FsPath>,
    pub max_depth: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
    let wire_pattern = root_relative_pattern(&args.pattern, &root);
    let pattern = PathPattern::new(&wire_pattern, false)
        .map_err(|error| invalid_request(error.to_string()))?;

    // A backend with a native enumeration (e.g. a remote host) walks the
    // tree locally under the same bounds and returns only matching paths.
    let native_request = FsGlobRequest {
        root: root.clone(),
        pattern: wire_pattern,
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
        if pattern.matches(&relative_path_string(&path, &root), false) {
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
    async fn generic_glob_keeps_star_within_one_segment() {
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/workspace/src/deep").expect("deep"),
            CreateDirectoryOptions::recursive(),
        )
        .await
        .expect("create deep");
        for path in ["/workspace/src/lib.rs", "/workspace/src/deep/nested.rs"] {
            fs.write_file(&FsPath::new(path).unwrap(), Vec::new())
                .await
                .expect("write");
        }
        let ctx = context(Arc::new(fs)).with_cwd(FsPath::new("/workspace").expect("cwd"));
        let glob = |pattern: &str| GlobArgs {
            pattern: pattern.to_owned(),
            path: None,
            max_depth: None,
            limit: None,
        };

        let shallow = invoke_glob(&ctx, glob("src/*.rs")).await.expect("glob");
        assert_eq!(
            shallow.matches,
            vec![FsPath::new("/workspace/src/lib.rs").unwrap()]
        );
        let named = invoke_glob(&ctx, glob("*.rs")).await.expect("glob");
        assert_eq!(named.matches.len(), 2, "a bare name matches at any depth");
        let anchored = invoke_glob(&ctx, glob("/workspace/*.rs"))
            .await
            .expect("glob");
        assert!(
            anchored.matches.is_empty(),
            "an absolute pattern below the root anchors to it"
        );
        let absolute = invoke_glob(&ctx, glob("/workspace/src/**/*.rs"))
            .await
            .expect("glob");
        assert_eq!(absolute.matches.len(), 2);
        let rooted = invoke_glob(&ctx, glob("/src/*.rs")).await.expect("glob");
        assert_eq!(
            rooted.matches,
            vec![FsPath::new("/workspace/src/lib.rs").unwrap()],
            "a leading '/' anchors to the root, as in gitignore"
        );
        let elsewhere = invoke_glob(&ctx, glob("/elsewhere/*.rs"))
            .await
            .expect("glob");
        assert!(
            elsewhere.matches.is_empty(),
            "any other leading '/' pattern anchors to the root too"
        );
        let direct = invoke_glob(
            &ctx,
            GlobArgs {
                max_depth: Some(2),
                ..glob("**/*.rs")
            },
        )
        .await
        .expect("glob");
        assert_eq!(
            direct.matches,
            vec![FsPath::new("/workspace/src/lib.rs").unwrap()],
            "depth 1 is the root's direct children"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generic_glob_applies_the_environment_walk_defaults() {
        let fs = InMemoryFileSystem::full_access();
        for directory in [
            "/repo/.git",
            "/repo/.cache",
            "/repo/src/gen",
            "/repo/target",
        ] {
            fs.create_directory(
                &FsPath::new(directory).unwrap(),
                CreateDirectoryOptions::recursive(),
            )
            .await
            .expect("create directory");
        }
        for (path, contents) in [
            ("/.gitignore", "*.log\n"),
            ("/repo/.gitignore", "target/\n"),
            ("/repo/src/.ignore", "gen/\n!keep.log\n"),
            ("/repo/.git/HEAD.rs", ""),
            ("/repo/.cache/hit.rs", ""),
            ("/repo/.env.rs", ""),
            ("/repo/target/out.rs", ""),
            ("/repo/src/gen/made.rs", ""),
            ("/repo/src/lib.rs", ""),
            ("/repo/src/debug.log", ""),
            ("/repo/src/keep.log", ""),
        ] {
            fs.write_file(&FsPath::new(path).unwrap(), contents.as_bytes().to_vec())
                .await
                .expect("write");
        }
        let ctx = context(Arc::new(fs)).with_cwd(FsPath::new("/repo").expect("cwd"));
        let glob = |pattern: &str| GlobArgs {
            pattern: pattern.to_owned(),
            path: None,
            max_depth: None,
            limit: None,
        };

        let sources = invoke_glob(&ctx, glob("*.rs")).await.expect("glob");
        assert_eq!(
            sources.matches,
            vec![FsPath::new("/repo/src/lib.rs").unwrap()],
            "hidden names, VCS directories and ignored paths are left out"
        );
        let logs = invoke_glob(&ctx, glob("*.log")).await.expect("glob");
        assert_eq!(
            logs.matches,
            vec![FsPath::new("/repo/src/keep.log").unwrap()],
            "an ancestor's rule applies and a deeper negation overrides it"
        );
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
