//! Shared helpers for generic filesystem tool operations.

use crate::{
    error::{ToolError, ToolResult},
    fs::{FsError, FsPath, FsToolContext},
};

pub(crate) fn resolve_path(ctx: &FsToolContext, path: &FsPath) -> ToolResult<FsPath> {
    if path.is_absolute() {
        return Ok(path.clone());
    }

    let Some(cwd) = &ctx.fs_cwd else {
        return Ok(path.clone());
    };

    cwd.join_path(path)
        .map_err(FsError::from)
        .map_err(ToolError::from)
}

pub(crate) fn invalid_request(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

/// Directories every walk prunes, as environment walks do.
const VCS_DIRECTORIES: [&str; 4] = [".git", ".hg", ".svn", ".jj"];
/// Ignore files read per directory, in ascending precedence after
/// `.git/info/exclude`.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];
/// Largest ignore file read; longer files are cut.
const MAX_IGNORE_FILE_BYTES: u64 = 1024 * 1024;

/// Bounded generic traversal shared by the grep and glob fallbacks: stops
/// enumerating once the file bound or the deadline is reached instead of
/// walking an arbitrarily large tree. `max_depth` counts the root's direct
/// children as depth 1, as environment walks do.
///
/// The walk applies an environment walk's defaults, so a VFS and an
/// environment select the same files: names starting with `.` are
/// skipped, `.git`, `.hg`, `.svn` and `.jj` are pruned, and `.gitignore`,
/// `.ignore` and `.git/info/exclude` apply (ascending precedence, deeper
/// directories overriding their ancestors, ancestors of `root` included).
pub(crate) async fn collect_file_paths_bounded(
    ctx: &FsToolContext,
    root: FsPath,
    max_depth: Option<usize>,
    max_files: u64,
    started: std::time::Instant,
    deadline: std::time::Duration,
) -> ToolResult<(Vec<FsPath>, Option<crate::fs::FsSearchStop>)> {
    let metadata = ctx.fs.get_metadata(&root).await?;
    if metadata.is_file {
        return Ok((vec![root], None));
    }
    if !metadata.is_directory {
        return Ok((Vec::new(), None));
    }

    let mut ancestors = Vec::new();
    let mut ancestor = root.parent();
    while let Some(directory) = ancestor {
        ancestor = directory.parent();
        ancestors.push(directory);
    }
    let mut chain = IgnoreChain::default();
    for directory in ancestors.into_iter().rev() {
        chain = chain.with(load_ignores(ctx, &directory).await);
    }

    let mut files = Vec::new();
    let mut stack = vec![(root, 1usize, chain)];
    let mut stopped = None;

    'walk: while let Some((dir, depth, parent_chain)) = stack.pop() {
        if started.elapsed() >= deadline {
            stopped = Some(crate::fs::FsSearchStop::TimeLimit);
            break;
        }
        let mut entries = ctx.fs.read_directory(&dir).await?;
        entries.sort_by(|left, right| right.file_name.cmp(&left.file_name));
        let has_ignore_files = entries.iter().any(|entry| {
            entry.file_name == ".git" || IGNORE_FILES.contains(&entry.file_name.as_str())
        });
        let chain = if has_ignore_files {
            parent_chain.with(load_ignores(ctx, &dir).await)
        } else {
            parent_chain
        };

        for entry in entries {
            if max_depth.is_some_and(|max_depth| depth > max_depth) {
                break;
            }
            let name = entry.file_name.as_str();
            if VCS_DIRECTORIES.contains(&name) || name.starts_with('.') {
                continue;
            }
            let child = dir.join(name).map_err(FsError::from)?;
            if chain.ignores(&child, entry.is_directory) {
                continue;
            }
            if entry.is_file {
                if files.len() as u64 >= max_files {
                    stopped = Some(crate::fs::FsSearchStop::FileLimit);
                    break 'walk;
                }
                files.push(child);
            } else if entry.is_directory && max_depth.is_none_or(|max_depth| depth < max_depth) {
                stack.push((child, depth + 1, chain.clone()));
            }
        }
    }

    files.sort();
    Ok((files, stopped))
}

/// The ignore levels in force for one directory, outermost first.
#[derive(Clone, Default)]
struct IgnoreChain {
    levels: std::sync::Arc<Vec<std::sync::Arc<ignore::gitignore::Gitignore>>>,
}

impl IgnoreChain {
    fn with(self, level: Option<ignore::gitignore::Gitignore>) -> Self {
        let Some(level) = level else {
            return self;
        };
        let mut levels = (*self.levels).clone();
        levels.push(std::sync::Arc::new(level));
        Self {
            levels: std::sync::Arc::new(levels),
        }
    }

    /// The deepest level with an opinion decides.
    fn ignores(&self, path: &FsPath, is_dir: bool) -> bool {
        let candidate = std::path::Path::new(path.as_str());
        for level in self.levels.iter().rev() {
            let matched = level.matched(candidate, is_dir);
            if matched.is_ignore() {
                return true;
            }
            if matched.is_whitelist() {
                return false;
            }
        }
        false
    }
}

/// The ignore level of one directory, or `None` when it has no rules.
/// Unreadable or missing ignore files count as empty.
async fn load_ignores(
    ctx: &FsToolContext,
    directory: &FsPath,
) -> Option<ignore::gitignore::Gitignore> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(directory.as_str());
    let mut lines = 0usize;
    let sources = [".git/info/exclude", IGNORE_FILES[0], IGNORE_FILES[1]];
    for source in sources {
        let Ok(path) = directory.join(source) else {
            continue;
        };
        let Ok(read) = ctx
            .fs
            .read_file_range(&path, 0, Some(MAX_IGNORE_FILE_BYTES))
            .await
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&read.bytes).lines() {
            if builder.add_line(None, line).is_ok() {
                lines += 1;
            }
        }
    }
    if lines == 0 {
        return None;
    }
    builder.build().ok()
}

/// The model-facing form of a path pattern, in the wire grammar: patterns
/// match paths relative to the tool's `path`, and a leading `/` anchors to
/// it, as in gitignore (`/*.rs` selects only `path`'s direct children).
/// As a convenience, a pattern that spells out `path`'s absolute prefix is
/// read as an absolute path below it and becomes the anchored form
/// (`/workspace/src/*.rs` under `/workspace` is `/src/*.rs`).
pub(crate) fn root_relative_pattern(pattern: &str, root: &FsPath) -> String {
    if !pattern.starts_with('/') || root.is_root() || !root.is_absolute() {
        return pattern.to_owned();
    }
    pattern
        .strip_prefix(root.as_str())
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
        .map_or_else(|| pattern.to_owned(), |relative| format!("/{relative}"))
}

pub(crate) fn relative_path_string(path: &FsPath, root: &FsPath) -> String {
    if !path.starts_with(root) {
        return path.as_str().to_string();
    }

    let root_segment_count = root.segments().count();
    let relative = path
        .segments()
        .skip(root_segment_count)
        .collect::<Vec<_>>()
        .join("/");
    if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    }
}
