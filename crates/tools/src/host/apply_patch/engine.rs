//! Apply-patch filesystem application engine.
//!
//! Adapted from Codex's `codex-apply-patch` application logic, with Lightspeed's
//! `FileSystem` trait replacing Codex's local executor filesystem.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    error::{ToolError, ToolResult},
    host::apply_patch::{
        parser::{Hunk, ParseError, UpdateFileChunk, parse_patch},
        seek_sequence,
    },
    host::fs::{CreateDirectoryOptions, FileMetadata, FileSystem, FsError, FsPath, RemoveOptions},
};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplyPatchSummary {
    pub added: Vec<FsPath>,
    pub modified: Vec<FsPath>,
    pub deleted: Vec<FsPath>,
}

impl ApplyPatchSummary {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    pub fn output(&self) -> String {
        let mut output = String::from("Success. Updated the following files:\n");
        for path in &self.added {
            output.push_str(&format!("A {path}\n"));
        }
        for path in &self.modified {
            output.push_str(&format!("M {path}\n"));
        }
        for path in &self.deleted {
            output.push_str(&format!("D {path}\n"));
        }
        output
    }
}

pub async fn apply_patch_text(
    fs: &dyn FileSystem,
    cwd: Option<&FsPath>,
    max_file_read_bytes: Option<u64>,
    patch: &str,
) -> ToolResult<ApplyPatchSummary> {
    let parsed = parse_patch(patch).map_err(parse_error_to_tool_error)?;
    apply_hunks(fs, cwd, max_file_read_bytes, &parsed.hunks).await
}

pub async fn apply_hunks(
    fs: &dyn FileSystem,
    cwd: Option<&FsPath>,
    max_file_read_bytes: Option<u64>,
    hunks: &[Hunk],
) -> ToolResult<ApplyPatchSummary> {
    if hunks.is_empty() {
        return Err(invalid_patch("No files were modified."));
    }

    let mut summary = ApplyPatchSummary::default();
    for hunk in hunks {
        let affected_path = resolve_patch_path(cwd, hunk.affected_path())?;
        match hunk {
            Hunk::AddFile { path, contents } => {
                let path = resolve_patch_path(cwd, path)?;
                write_file_with_parent_dirs(fs, &path, contents.clone().into_bytes()).await?;
                summary.added.push(affected_path);
            }
            Hunk::DeleteFile { path } => {
                let path = resolve_patch_path(cwd, path)?;
                ensure_file_not_directory(fs, &path).await?;
                fs.remove(&path, RemoveOptions::file()).await?;
                summary.deleted.push(affected_path);
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let path = resolve_patch_path(cwd, path)?;
                let new_contents =
                    derive_new_contents_from_chunks(fs, max_file_read_bytes, &path, chunks).await?;
                if let Some(move_path) = move_path {
                    let destination = resolve_patch_path(cwd, move_path)?;
                    write_file_with_parent_dirs(fs, &destination, new_contents.into_bytes())
                        .await?;
                    ensure_file_not_directory(fs, &path).await?;
                    fs.remove(&path, RemoveOptions::file()).await?;
                } else {
                    fs.write_file(&path, new_contents.into_bytes()).await?;
                }
                summary.modified.push(affected_path);
            }
        }
    }

    Ok(summary)
}

fn parse_error_to_tool_error(error: ParseError) -> ToolError {
    invalid_patch(error.to_string())
}

fn invalid_patch(message: impl Into<String>) -> ToolError {
    ToolError::InvalidRequest {
        message: message.into(),
    }
}

fn resolve_patch_path(cwd: Option<&FsPath>, path: &Path) -> ToolResult<FsPath> {
    let path = path.to_str().ok_or_else(|| FsError::InvalidInput {
        message: "patch path is not valid UTF-8".to_string(),
    })?;
    let path = FsPath::new(path).map_err(FsError::from)?;
    if path.is_absolute() {
        return Ok(path);
    }
    if let Some(cwd) = cwd {
        return cwd
            .join_path(&path)
            .map_err(FsError::from)
            .map_err(Into::into);
    }
    Ok(path)
}

async fn write_file_with_parent_dirs(
    fs: &dyn FileSystem,
    path: &FsPath,
    contents: Vec<u8>,
) -> ToolResult<()> {
    if let Some(parent) = path.parent()
        && !parent.is_root()
    {
        fs.create_directory(&parent, CreateDirectoryOptions::recursive())
            .await?;
    }
    fs.write_file(path, contents).await?;
    Ok(())
}

async fn ensure_file_not_directory(fs: &dyn FileSystem, path: &FsPath) -> ToolResult<()> {
    let FileMetadata { is_directory, .. } = fs.get_metadata(path).await?;
    if is_directory {
        return Err(FsError::InvalidInput {
            message: format!("path is a directory: {path}"),
        }
        .into());
    }
    Ok(())
}

async fn derive_new_contents_from_chunks(
    fs: &dyn FileSystem,
    max_file_read_bytes: Option<u64>,
    path: &FsPath,
    chunks: &[UpdateFileChunk],
) -> ToolResult<String> {
    let original_contents = read_file_text(fs, max_file_read_bytes, path).await?;
    let mut original_lines = original_contents
        .split('\n')
        .map(String::from)
        .collect::<Vec<_>>();

    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    Ok(new_lines.join("\n"))
}

async fn read_file_text(
    fs: &dyn FileSystem,
    max_file_read_bytes: Option<u64>,
    path: &FsPath,
) -> ToolResult<String> {
    let bytes = fs.read_file(path).await?;
    if let Some(max_file_read_bytes) = max_file_read_bytes
        && bytes.len() as u64 > max_file_read_bytes
    {
        return Err(invalid_patch(format!(
            "apply_patch read {} bytes from {}, exceeding max_file_read_bytes={}",
            bytes.len(),
            path,
            max_file_read_bytes
        )));
    }
    String::from_utf8(bytes)
        .map_err(FsError::invalid_data)
        .map_err(Into::into)
}

fn compute_replacements(
    original_lines: &[String],
    path: &FsPath,
    chunks: &[UpdateFileChunk],
) -> ToolResult<Vec<(usize, usize, Vec<String>)>> {
    let mut replacements = Vec::new();
    let mut line_index = 0usize;

    for chunk in chunks {
        if let Some(ctx_line) = &chunk.change_context {
            if let Some(index) = seek_sequence::seek_sequence(
                original_lines,
                std::slice::from_ref(ctx_line),
                line_index,
                false,
            ) {
                line_index = index + 1;
            } else {
                return Err(invalid_patch(format!(
                    "Failed to find context '{}' in {}",
                    ctx_line, path
                )));
            }
        }

        if chunk.old_lines.is_empty() {
            let insertion_index = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_index, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern: &[String] = &chunk.old_lines;
        let mut found =
            seek_sequence::seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        let mut new_slice: &[String] = &chunk.new_lines;

        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence::seek_sequence(
                original_lines,
                pattern,
                line_index,
                chunk.is_end_of_file,
            );
        }

        if let Some(start_index) = found {
            replacements.push((start_index, pattern.len(), new_slice.to_vec()));
            line_index = start_index + pattern.len();
        } else {
            return Err(invalid_patch(format!(
                "Failed to find expected lines in {}:\n{}",
                path,
                chunk.old_lines.join("\n")
            )));
        }
    }

    replacements.sort_by(|(left_index, _, _), (right_index, _, _)| left_index.cmp(right_index));
    Ok(replacements)
}

fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    for (start_index, old_len, new_segment) in replacements.iter().rev() {
        for _ in 0..*old_len {
            if *start_index < lines.len() {
                lines.remove(*start_index);
            }
        }
        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(*start_index + offset, new_line.clone());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::fs::{CreateDirectoryOptions, FileAccessPolicy, InMemoryFileSystem};

    #[tokio::test(flavor = "current_thread")]
    async fn apply_patch_text_adds_file_with_parent_dirs() {
        let fs = InMemoryFileSystem::full_access();
        let cwd = FsPath::new("/workspace").unwrap();
        let patch = r#"*** Begin Patch
*** Add File: src/lib.rs
+pub fn f() {}
*** End Patch"#;

        let summary = apply_patch_text(&fs, Some(&cwd), None, patch)
            .await
            .expect("apply patch");

        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/src/lib.rs").unwrap())
                .await
                .expect("read file"),
            "pub fn f() {}\n"
        );
        assert_eq!(
            summary.added,
            vec![FsPath::new("/workspace/src/lib.rs").unwrap()]
        );
        assert_eq!(
            summary.output(),
            "Success. Updated the following files:\nA /workspace/src/lib.rs\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_patch_text_updates_file() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").unwrap(), b"foo\nbar\n".to_vec())
            .await
            .expect("write file");
        let patch = r#"*** Begin Patch
*** Update File: /file.txt
@@
 foo
-bar
+baz
*** End Patch"#;

        let summary = apply_patch_text(&fs, None, None, patch)
            .await
            .expect("apply patch");

        assert_eq!(
            fs.read_file_text(&FsPath::new("/file.txt").unwrap())
                .await
                .expect("read file"),
            "foo\nbaz\n"
        );
        assert_eq!(summary.modified, vec![FsPath::new("/file.txt").unwrap()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_patch_text_deletes_file() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").unwrap(), b"delete\n".to_vec())
            .await
            .expect("write file");
        let patch = r#"*** Begin Patch
*** Delete File: /file.txt
*** End Patch"#;

        let summary = apply_patch_text(&fs, None, None, patch)
            .await
            .expect("apply patch");

        assert!(matches!(
            fs.read_file(&FsPath::new("/file.txt").unwrap()).await,
            Err(FsError::NotFound { .. })
        ));
        assert_eq!(summary.deleted, vec![FsPath::new("/file.txt").unwrap()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_patch_text_moves_updated_file() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/old.txt").unwrap(), b"line\n".to_vec())
            .await
            .expect("write file");
        let patch = r#"*** Begin Patch
*** Update File: /old.txt
*** Move to: /new.txt
@@
-line
+line2
*** End Patch"#;

        let summary = apply_patch_text(&fs, None, None, patch)
            .await
            .expect("apply patch");

        assert!(matches!(
            fs.read_file(&FsPath::new("/old.txt").unwrap()).await,
            Err(FsError::NotFound { .. })
        ));
        assert_eq!(
            fs.read_file_text(&FsPath::new("/new.txt").unwrap())
                .await
                .expect("read file"),
            "line2\n"
        );
        assert_eq!(summary.modified, vec![FsPath::new("/new.txt").unwrap()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_patch_text_rejects_bad_context() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").unwrap(), b"real\n".to_vec())
            .await
            .expect("write file");
        let patch = r#"*** Begin Patch
*** Update File: /file.txt
@@
-missing
+new
*** End Patch"#;

        let error = apply_patch_text(&fs, None, None, patch)
            .await
            .expect_err("patch should fail");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_patch_text_respects_read_only_filesystem() {
        let fs = InMemoryFileSystem::new(FileAccessPolicy::FullReadOnly);
        let patch = r#"*** Begin Patch
*** Add File: /file.txt
+hello
*** End Patch"#;

        let error = apply_patch_text(&fs, None, None, patch)
            .await
            .expect_err("patch should fail");

        assert!(matches!(error, ToolError::Filesystem(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_patch_text_rejects_deleting_directory() {
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/dir").unwrap(),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create dir");
        let patch = r#"*** Begin Patch
*** Delete File: /dir
*** End Patch"#;

        let error = apply_patch_text(&fs, None, None, patch)
            .await
            .expect_err("patch should fail");

        assert!(matches!(
            error,
            ToolError::Filesystem(FsError::InvalidInput { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn apply_patch_text_enforces_read_byte_limit() {
        let fs = InMemoryFileSystem::full_access();
        fs.write_file(&FsPath::new("/file.txt").unwrap(), b"hello\n".to_vec())
            .await
            .expect("write file");
        let patch = r#"*** Begin Patch
*** Update File: /file.txt
@@
-hello
+goodbye
*** End Patch"#;

        let error = apply_patch_text(&fs, None, Some(4), patch)
            .await
            .expect_err("patch should fail");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }
}
