//! Canonical grep operation.

use glob::Pattern;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    host::{context::HostToolContext, fs::FsPath},
};

use super::{collect_file_paths, invalid_request, relative_path_string, resolve_path};

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrepMatch {
    pub path: FsPath,
    pub line_number: usize,
    pub line: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrepResult {
    pub path: FsPath,
    pub pattern: String,
    pub matches: Vec<GrepMatch>,
    pub truncated: bool,
}

pub async fn invoke_grep(ctx: &HostToolContext, args: GrepArgs) -> ToolResult<GrepResult> {
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
    let limit = args.limit.unwrap_or(DEFAULT_GREP_LIMIT);
    if limit == 0 {
        return Err(invalid_request("grep limit must be 1 or greater"));
    }

    let root = match args.path {
        Some(path) => resolve_path(ctx, &path)?,
        None => ctx.cwd.clone().unwrap_or_else(FsPath::current_dir),
    };
    let paths = collect_file_paths(ctx, root.clone(), args.max_depth).await?;
    let mut matches = Vec::new();

    for path in paths {
        if let Some(include) = &include
            && !path_matches_include(include, &path, &root)
        {
            continue;
        }

        let bytes = ctx.fs.read_file(&path).await?;
        if bytes.len() as u64 > ctx.limits.max_file_read_bytes {
            return Err(invalid_request(format!(
                "grep read {} bytes from {}, exceeding max_file_read_bytes={}",
                bytes.len(),
                path,
                ctx.limits.max_file_read_bytes
            )));
        }
        let Ok(contents) = String::from_utf8(bytes) else {
            continue;
        };

        for (line_index, line) in contents.lines().enumerate() {
            if regex.is_match(line) {
                matches.push(GrepMatch {
                    path: path.clone(),
                    line_number: line_index + 1,
                    line: line.to_string(),
                });
                if matches.len() > limit {
                    matches.truncate(limit);
                    return Ok(GrepResult {
                        path: root,
                        pattern: args.pattern,
                        matches,
                        truncated: true,
                    });
                }
            }
        }
    }

    Ok(GrepResult {
        path: root,
        pattern: args.pattern,
        matches,
        truncated: false,
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

    use agent_core::storage::InMemoryBlobStore;

    use super::*;
    use crate::{
        error::ToolError,
        host::{
            context::HostToolLimits,
            fs::{CreateDirectoryOptions, FileSystem, InMemoryFileSystem},
        },
    };

    fn context(fs: Arc<dyn FileSystem>) -> HostToolContext {
        HostToolContext::new(fs, None, Arc::new(InMemoryBlobStore::new()))
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
        fs.write_file(&FsPath::new("/file.txt").unwrap(), b"Forge\n".to_vec())
            .await
            .expect("write file");
        let ctx = context(Arc::new(fs));

        let result = invoke_grep(
            &ctx,
            GrepArgs {
                pattern: "forge".to_string(),
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
    async fn invoke_grep_enforces_read_byte_limit() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").unwrap(), b"hello\n".to_vec())
            .await
            .expect("write file");
        let ctx = context(Arc::new(fs)).with_limits(HostToolLimits {
            max_file_read_bytes: 4,
            ..HostToolLimits::default()
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
