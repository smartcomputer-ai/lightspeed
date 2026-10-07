//! Canonical grep operation.

use std::time::{Duration, Instant};

use glob::Pattern;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    fs::{FsPath, FsSearchLimits, FsSearchStop, FsTextSearchRequest, FsToolContext},
};

use super::{collect_file_paths_bounded, invalid_request, relative_path_string, resolve_path};

pub const DEFAULT_GREP_LIMIT: usize = 1_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrepArgs {
    pub pattern: String,
    pub path: Option<FsPath>,
    pub include: Option<String>,
    #[serde(default)]
    pub case_sensitive: bool,
    pub max_depth: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GrepMatch {
    pub path: FsPath,
    pub line_number: usize,
    pub line: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GrepResult {
    pub path: FsPath,
    pub pattern: String,
    pub matches: Vec<GrepMatch>,
    pub truncated: bool,
    /// Why the search stopped early; absent when it exhausted the tree
    /// within its bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<FsSearchStop>,
}

pub async fn invoke_grep(ctx: &FsToolContext, args: GrepArgs) -> ToolResult<GrepResult> {
    if args.pattern.is_empty() {
        return Err(invalid_request("grep pattern must not be empty"));
    }
    let regex = RegexBuilder::new(&args.pattern)
        .case_insensitive(!args.case_sensitive)
        .build()
        .map_err(|error| invalid_request(format!("invalid grep regex: {error}")))?;
    let include = args
        .include
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .map_err(|error| invalid_request(format!("invalid grep include glob: {error}")))?;
    let requested_limit = args.limit.unwrap_or(DEFAULT_GREP_LIMIT);
    if requested_limit == 0 {
        return Err(invalid_request("grep limit must be 1 or greater"));
    }
    // A caller may request fewer matches than the deployment maximum but can
    // never raise any search bound.
    let max_matches = (requested_limit as u64).min(ctx.limits.max_search_matches);
    let limits = FsSearchLimits {
        max_matches,
        max_files: ctx.limits.max_search_files,
        max_bytes: ctx.limits.max_search_bytes,
        max_duration_ms: ctx.limits.max_search_duration_ms,
    };

    let root = match args.path.as_ref() {
        Some(path) => resolve_path(ctx, path)?,
        None => ctx.fs_cwd.clone().unwrap_or_else(FsPath::current_dir),
    };

    // A backend with a native search (e.g. a remote host) performs the
    // traversal and matching locally under the same bounds and returns only
    // matches and statistics.
    let native_request = FsTextSearchRequest {
        root: root.clone(),
        pattern: args.pattern.clone(),
        include: args.include.clone(),
        case_sensitive: args.case_sensitive,
        max_depth: args.max_depth,
        limits,
    };
    if let Some(native) = ctx.fs.search_text(&native_request).await? {
        let matches = native
            .matches
            .into_iter()
            .map(|item| GrepMatch {
                path: item.path,
                line_number: item.line_number as usize,
                line: item.line,
            })
            .collect::<Vec<_>>();
        return Ok(GrepResult {
            path: root,
            pattern: args.pattern,
            matches,
            truncated: native.stopped.is_some(),
            stopped: native.stopped,
        });
    }

    bounded_generic_grep(ctx, args, root, regex, include, limits).await
}

/// Generic fallback over the `FileSystem` interface, bounded by files
/// visited, cumulative bytes read, matches returned, and elapsed time. It
/// returns a typed truncated outcome instead of scanning without bound.
async fn bounded_generic_grep(
    ctx: &FsToolContext,
    args: GrepArgs,
    root: FsPath,
    regex: regex::Regex,
    include: Option<Pattern>,
    limits: FsSearchLimits,
) -> ToolResult<GrepResult> {
    let started = Instant::now();
    let deadline = Duration::from_millis(limits.max_duration_ms);
    let (paths, enumeration_stop) = collect_file_paths_bounded(
        ctx,
        root.clone(),
        args.max_depth,
        limits.max_files,
        started,
        deadline,
    )
    .await?;
    let mut stopped = enumeration_stop;
    let mut matches = Vec::new();
    let mut bytes_searched = 0u64;

    'files: for path in paths {
        if let Some(include) = &include
            && !path_matches_include(include, &path, &root)
        {
            continue;
        }
        if started.elapsed() >= deadline {
            stopped = Some(FsSearchStop::TimeLimit);
            break;
        }

        // Ranged read: a file beyond the per-file cap or the remaining byte
        // budget is truncated at the source instead of transferred in full.
        let remaining_budget = limits.max_bytes.saturating_sub(bytes_searched);
        let per_file_cap = ctx.limits.max_file_read_bytes.saturating_add(1);
        let read = ctx
            .fs
            .read_file_range(
                &path,
                0,
                Some(per_file_cap.min(remaining_budget.saturating_add(1))),
            )
            .await?;
        if read.file_size > ctx.limits.max_file_read_bytes {
            return Err(invalid_request(format!(
                "grep would read {} bytes from {}, exceeding max_file_read_bytes={}",
                read.file_size, path, ctx.limits.max_file_read_bytes
            )));
        }
        if read.truncated {
            // The file did not fit into the remaining byte budget; partial
            // content is not searched.
            stopped = Some(FsSearchStop::ByteLimit);
            break;
        }
        bytes_searched = bytes_searched.saturating_add(read.bytes.len() as u64);
        let over_byte_limit = bytes_searched > limits.max_bytes;
        let Ok(contents) = String::from_utf8(read.bytes) else {
            if over_byte_limit {
                stopped = Some(FsSearchStop::ByteLimit);
                break;
            }
            continue;
        };

        for (line_index, line) in contents.lines().enumerate() {
            if regex.is_match(line) {
                if matches.len() as u64 >= limits.max_matches {
                    stopped = Some(FsSearchStop::MatchLimit);
                    break 'files;
                }
                matches.push(GrepMatch {
                    path: path.clone(),
                    line_number: line_index + 1,
                    line: line.to_string(),
                });
            }
        }
        if over_byte_limit {
            stopped = Some(FsSearchStop::ByteLimit);
            break;
        }
    }

    Ok(GrepResult {
        path: root,
        pattern: args.pattern,
        matches,
        truncated: stopped.is_some(),
        stopped,
    })
}

fn path_matches_include(pattern: &Pattern, path: &FsPath, root: &FsPath) -> bool {
    let relative = relative_path_string(path, root);
    pattern.matches(&relative)
        || path
            .file_name()
            .is_some_and(|file_name| pattern.matches(file_name))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use harness::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        error::ToolError,
        fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem},
        limits::ToolLimits,
    };

    fn context(fs: Arc<dyn FileSystem>) -> FsToolContext {
        FsToolContext::new(fs, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_grep_finds_matching_lines() {
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/workspace/src").expect("src"),
            CreateDirectoryOptions::recursive(),
        )
        .await
        .expect("create src");
        fs.write_file(
            &FsPath::new("/workspace/src/lib.rs").unwrap(),
            b"pub fn target() {}\nfn other() {}\n".to_vec(),
        )
        .await
        .expect("write lib");
        fs.write_file(
            &FsPath::new("/workspace/readme.md").unwrap(),
            b"target\n".to_vec(),
        )
        .await
        .expect("write readme");
        let ctx = context(Arc::new(fs)).with_cwd(FsPath::new("/workspace").expect("cwd"));

        let result = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "target".to_string(),
                path: None,
                include: Some("*.rs".to_string()),
                case_sensitive: true,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("grep");

        assert_eq!(
            result.matches,
            vec![GrepMatch {
                path: FsPath::new("/workspace/src/lib.rs").unwrap(),
                line_number: 1,
                line: "pub fn target() {}".to_string(),
            }]
        );
        assert!(!result.truncated);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_grep_applies_case_insensitive_matching() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").unwrap(), b"Lightspeed\n".to_vec())
            .await
            .expect("write file");
        let ctx = context(Arc::new(fs));

        let result = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "lightspeed".to_string(),
                path: Some(FsPath::root()),
                include: None,
                case_sensitive: false,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("grep");

        assert_eq!(result.matches.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_grep_stops_at_file_budget_with_a_typed_reason() {
        let fs = InMemoryFileSystem::full_access();
        for index in 0..5 {
            fs.write_file(
                &FsPath::new(format!("/file_{index}.txt")).unwrap(),
                b"target\n".to_vec(),
            )
            .await
            .expect("write file");
        }
        let ctx = context(Arc::new(fs)).with_limits(ToolLimits {
            max_search_files: 2,
            ..ToolLimits::default()
        });

        let result = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "target".to_string(),
                path: Some(FsPath::root()),
                include: None,
                case_sensitive: true,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("grep");

        assert_eq!(result.stopped, Some(FsSearchStop::FileLimit));
        assert!(result.truncated);
        assert_eq!(result.matches.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_grep_stops_at_byte_budget_with_a_typed_reason() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(
            &FsPath::new("/a.txt").unwrap(),
            b"target line one\n".to_vec(),
        )
        .await
        .expect("write a");
        fs.write_file(
            &FsPath::new("/b.txt").unwrap(),
            b"target line two\n".to_vec(),
        )
        .await
        .expect("write b");
        let ctx = context(Arc::new(fs)).with_limits(ToolLimits {
            max_search_bytes: 20,
            ..ToolLimits::default()
        });

        let result = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "target".to_string(),
                path: Some(FsPath::root()),
                include: None,
                case_sensitive: true,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("grep");

        assert_eq!(result.stopped, Some(FsSearchStop::ByteLimit));
        assert!(result.truncated);
        // The file that would exceed the remaining budget is truncated at the
        // source and never searched.
        assert_eq!(result.matches.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_grep_clamps_requested_match_limit_to_the_deployment_bound() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(
            &FsPath::new("/file.txt").unwrap(),
            b"target\ntarget\ntarget\n".to_vec(),
        )
        .await
        .expect("write file");
        let ctx = context(Arc::new(fs)).with_limits(ToolLimits {
            max_search_matches: 2,
            ..ToolLimits::default()
        });

        let result = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "target".to_string(),
                path: Some(FsPath::root()),
                include: None,
                case_sensitive: true,
                max_depth: None,
                limit: Some(1_000_000),
            },
        )
        .await
        .expect("grep");

        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.stopped, Some(FsSearchStop::MatchLimit));
    }

    struct NativeSearchFileSystem {
        inner: InMemoryFileSystem,
        response: crate::fs::FsTextSearchResponse,
    }

    #[async_trait::async_trait]
    impl FileSystem for NativeSearchFileSystem {
        fn access_policy(&self) -> crate::fs::FileAccessPolicy {
            self.inner.access_policy()
        }

        async fn read_file(&self, _path: &FsPath) -> crate::fs::FsResult<Vec<u8>> {
            panic!("native search must not fall back to per-file reads");
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
            path: &FsPath,
        ) -> crate::fs::FsResult<crate::fs::FileMetadata> {
            self.inner.get_metadata(path).await
        }

        async fn read_directory(
            &self,
            path: &FsPath,
        ) -> crate::fs::FsResult<Vec<crate::fs::ReadDirectoryEntry>> {
            self.inner.read_directory(path).await
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

        async fn search_text(
            &self,
            request: &crate::fs::FsTextSearchRequest,
        ) -> crate::fs::FsResult<Option<crate::fs::FsTextSearchResponse>> {
            assert_eq!(request.pattern, "target");
            assert!(request.limits.max_matches <= ToolLimits::default().max_search_matches);
            Ok(Some(self.response.clone()))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_grep_prefers_a_native_search_backend() {
        let fs = NativeSearchFileSystem {
            inner: InMemoryFileSystem::full_access(),
            response: crate::fs::FsTextSearchResponse {
                matches: vec![crate::fs::FsTextSearchMatch {
                    path: FsPath::new("/remote/file.rs").unwrap(),
                    line_number: 7,
                    line: "native target".to_owned(),
                }],
                files_searched: 12,
                bytes_searched: 1024,
                stopped: Some(FsSearchStop::TimeLimit),
            },
        };
        let ctx = context(Arc::new(fs));

        let result = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "target".to_string(),
                path: Some(FsPath::root()),
                include: None,
                case_sensitive: true,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect("grep");

        assert_eq!(
            result.matches,
            vec![GrepMatch {
                path: FsPath::new("/remote/file.rs").unwrap(),
                line_number: 7,
                line: "native target".to_string(),
            }]
        );
        assert!(result.truncated);
        assert_eq!(result.stopped, Some(FsSearchStop::TimeLimit));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_grep_enforces_read_byte_limit() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").unwrap(), b"hello\n".to_vec())
            .await
            .expect("write file");
        let ctx = context(Arc::new(fs)).with_limits(ToolLimits {
            max_file_read_bytes: 4,
            ..ToolLimits::default()
        });

        let error = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "hello".to_string(),
                path: Some(FsPath::root()),
                include: None,
                case_sensitive: true,
                max_depth: None,
                limit: None,
            },
        )
        .await
        .expect_err("grep should fail");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }
}
