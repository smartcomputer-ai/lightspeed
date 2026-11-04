//! Apply-patch filesystem application engine.
//!
//! Adapted from Codex's `codex-apply-patch` application logic, with Lightspeed's
//! `FileSystem` trait replacing Codex's local executor filesystem.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{ToolError, ToolResult},
    fs::{
        CreateDirectoryOptions, FileMetadata, FileSystem, FsError, FsPath, FsResult, FsVersion,
        RemoveOptions, RenameOptions, WriteOptions,
        apply_patch::{
            parser::{Hunk, ParseError, UpdateFileChunk, parse_patch},
            seek_sequence,
        },
    },
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
        format!("Success. Updated the following files:\n{}", self.listing())
    }

    fn listing(&self) -> String {
        let mut listing = String::new();
        for path in &self.added {
            listing.push_str(&format!("A {path}\n"));
        }
        for path in &self.modified {
            listing.push_str(&format!("M {path}\n"));
        }
        for path in &self.deleted {
            listing.push_str(&format!("D {path}\n"));
        }
        listing
    }
}

/// What the model reads when a patch stopped partway through its writes.
pub(crate) fn partially_applied_message(
    applied: &ApplyPatchSummary,
    failed_path: &FsPath,
    not_applied: &[String],
    source: &FsError,
) -> String {
    format!(
        "patch partly applied, stopped at {failed_path}: {source}\nApplied, do not resend these hunks:\n{}Not applied:\n{}",
        applied.listing(),
        not_applied.join("\n")
    )
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

    let plan = plan_hunks(fs, cwd, max_file_read_bytes, hunks).await?;
    commit_steps(fs, plan.steps).await
}

struct PlannedWrite {
    path: FsPath,
    contents: String,
    /// The version of the file this text was derived from. An added file's
    /// write has none and is unconditional.
    if_match: Option<FsVersion>,
    added: bool,
}

/// One filesystem change of a planned patch.
enum PatchStep {
    Write(PlannedWrite),
    Move { source: FsPath, destination: FsPath },
    Delete { path: FsPath },
}

impl PatchStep {
    fn target(&self) -> &FsPath {
        match self {
            Self::Write(PlannedWrite { path, .. }) | Self::Delete { path } => path,
            Self::Move { destination, .. } => destination,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Write(PlannedWrite {
                path, added: true, ..
            }) => format!("add {path}"),
            Self::Write(PlannedWrite { path, .. }) => format!("update {path}"),
            Self::Move {
                source,
                destination,
            } => format!("move {source} to {destination}"),
            Self::Delete { path } => format!("delete {path}"),
        }
    }
}

/// A patch resolved against what the files hold, with no write made yet. Later
/// hunks see earlier ones through `writes` and `removed`, so a file touched twice
/// is still written once, with the version of its first read.
#[derive(Default)]
struct PatchPlan {
    steps: Vec<PatchStep>,
    /// The write step holding each path's current text. A move brings its write
    /// along under the new path, since the rename carries that text there.
    writes: BTreeMap<FsPath, usize>,
    /// Paths this patch has deleted or moved away.
    removed: BTreeSet<FsPath>,
}

/// What a path holds once the planned steps so far have run.
enum PlannedKind {
    File,
    Directory,
    /// Absent, or nothing the backend would say about it.
    Unknown,
}

impl PatchPlan {
    async fn kind(&self, fs: &dyn FileSystem, path: &FsPath) -> PlannedKind {
        if self.writes.contains_key(path) {
            return PlannedKind::File;
        }
        let prefix = format!("{}/", path.as_str().trim_end_matches('/'));
        if self
            .writes
            .keys()
            .any(|written| written.as_str().starts_with(&prefix))
        {
            return PlannedKind::Directory;
        }
        if self.removed.contains(path) {
            return PlannedKind::Unknown;
        }
        match fs.get_metadata(path).await {
            Ok(metadata) if metadata.is_directory => PlannedKind::Directory,
            Ok(metadata) if metadata.is_file => PlannedKind::File,
            _ => PlannedKind::Unknown,
        }
    }

    /// Refuse a file target no write can reach: a directory at the path or a file
    /// among its ancestors. An existing file there is fine, the write replaces it.
    /// Only positive evidence refuses, because a backend may decline to describe an
    /// ancestor outside its scope.
    async fn check_file_target(&self, fs: &dyn FileSystem, path: &FsPath) -> ToolResult<()> {
        if let PlannedKind::Directory = self.kind(fs, path).await {
            return Err(FsError::InvalidInput {
                message: format!("path is a directory: {path}"),
            }
            .into());
        }
        let mut ancestor = path.parent();
        while let Some(current) = ancestor.filter(|current| !current.is_root()) {
            match self.kind(fs, &current).await {
                PlannedKind::File => {
                    return Err(FsError::InvalidInput {
                        message: format!("{path} cannot be created: {current} is a file"),
                    }
                    .into());
                }
                PlannedKind::Directory => break,
                PlannedKind::Unknown => ancestor = current.parent(),
            }
        }
        Ok(())
    }

    fn write_at(&mut self, index: usize) -> &mut PlannedWrite {
        match &mut self.steps[index] {
            PatchStep::Write(write) => write,
            _ => unreachable!("the write map only points at write steps"),
        }
    }

    fn plan_add(&mut self, path: FsPath, contents: String) {
        self.removed.remove(&path);
        if let Some(&index) = self.writes.get(&path) {
            let write = self.write_at(index);
            write.contents = contents;
            write.if_match = None;
            write.added = true;
            return;
        }
        self.writes.insert(path.clone(), self.steps.len());
        self.steps.push(PatchStep::Write(PlannedWrite {
            path,
            contents,
            if_match: None,
            added: true,
        }));
    }

    async fn plan_update(
        &mut self,
        fs: &dyn FileSystem,
        max_file_read_bytes: Option<u64>,
        path: FsPath,
        chunks: &[UpdateFileChunk],
    ) -> ToolResult<()> {
        if let Some(&index) = self.writes.get(&path) {
            let write = self.write_at(index);
            write.contents = derive_new_contents(&write.contents, &path, chunks)?;
            return Ok(());
        }
        if self.removed.contains(&path) {
            return Err(FsError::NotFound { path }.into());
        }
        let (original, version) = read_file_text(fs, max_file_read_bytes, &path).await?;
        let contents = derive_new_contents(&original, &path, chunks)?;
        self.writes.insert(path.clone(), self.steps.len());
        self.steps.push(PatchStep::Write(PlannedWrite {
            path,
            contents,
            if_match: version,
            added: false,
        }));
        Ok(())
    }

    fn plan_move(&mut self, source: FsPath, destination: FsPath) {
        if let Some(index) = self.writes.remove(&source) {
            self.writes.insert(destination.clone(), index);
        }
        self.removed.remove(&destination);
        self.removed.insert(source.clone());
        self.steps.push(PatchStep::Move {
            source,
            destination,
        });
    }

    async fn plan_delete(&mut self, fs: &dyn FileSystem, path: FsPath) -> ToolResult<()> {
        if self.removed.contains(&path) {
            return Err(FsError::NotFound { path }.into());
        }
        if !self.writes.contains_key(&path) {
            ensure_file_not_directory(fs, &path).await?;
        }
        self.writes.remove(&path);
        self.removed.insert(path.clone());
        self.steps.push(PatchStep::Delete { path });
        Ok(())
    }
}

/// Resolve every hunk with no writes, so a patch that cannot apply changes nothing.
async fn plan_hunks(
    fs: &dyn FileSystem,
    cwd: Option<&FsPath>,
    max_file_read_bytes: Option<u64>,
    hunks: &[Hunk],
) -> ToolResult<PatchPlan> {
    let mut plan = PatchPlan::default();
    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                let path = resolve_patch_path(cwd, path)?;
                plan.check_file_target(fs, &path).await?;
                plan.plan_add(path, contents.clone());
            }
            Hunk::DeleteFile { path } => {
                let path = resolve_patch_path(cwd, path)?;
                plan.plan_delete(fs, path).await?;
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let path = resolve_patch_path(cwd, path)?;
                plan.plan_update(fs, max_file_read_bytes, path.clone(), chunks)
                    .await?;
                if let Some(move_path) = move_path {
                    let destination = resolve_patch_path(cwd, move_path)?;
                    if destination != path {
                        plan.check_file_target(fs, &destination).await?;
                        plan.plan_move(path, destination);
                    }
                }
            }
        }
    }
    Ok(plan)
}

/// Run the planned steps in order. A failure with nothing applied is that plain
/// failure; after that it says what landed, because the model must not resend it.
async fn commit_steps(fs: &dyn FileSystem, steps: Vec<PatchStep>) -> ToolResult<ApplyPatchSummary> {
    let mut summary = ApplyPatchSummary::default();
    let mut steps = steps.into_iter();
    while let Some(step) = steps.next() {
        if let Err(source) = commit_step(fs, &step, &mut summary).await {
            if summary.is_empty() {
                return Err(source.into());
            }
            return Err(ToolError::PatchPartiallyApplied {
                applied: Box::new(summary),
                failed_path: step.target().clone(),
                not_applied: std::iter::once(step)
                    .chain(steps)
                    .map(|step| step.describe())
                    .collect(),
                source,
            });
        }
    }
    Ok(summary)
}

async fn commit_step(
    fs: &dyn FileSystem,
    step: &PatchStep,
    summary: &mut ApplyPatchSummary,
) -> FsResult<()> {
    match step {
        PatchStep::Write(PlannedWrite {
            path,
            contents,
            if_match,
            added,
        }) => {
            if *added {
                create_parent_directories(fs, path).await?;
            }
            fs.write_file_with(
                path,
                contents.clone().into_bytes(),
                WriteOptions {
                    if_match: if_match.clone(),
                },
            )
            .await?;
            if *added {
                summary.added.push(path.clone());
            } else {
                summary.modified.push(path.clone());
            }
        }
        PatchStep::Move {
            source,
            destination,
        } => {
            create_parent_directories(fs, destination).await?;
            fs.rename(source, destination, RenameOptions { overwrite: true })
                .await?;
            for list in [&mut summary.added, &mut summary.modified] {
                if let Some(entry) = list.iter_mut().find(|entry| *entry == source) {
                    *entry = destination.clone();
                }
            }
        }
        PatchStep::Delete { path } => {
            fs.remove(path, RemoveOptions::file()).await?;
            summary.deleted.push(path.clone());
        }
    }
    Ok(())
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

async fn create_parent_directories(fs: &dyn FileSystem, path: &FsPath) -> FsResult<()> {
    if let Some(parent) = path.parent()
        && !parent.is_root()
    {
        fs.create_directory(&parent, CreateDirectoryOptions::recursive())
            .await?;
    }
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

fn derive_new_contents(
    original_contents: &str,
    path: &FsPath,
    chunks: &[UpdateFileChunk],
) -> ToolResult<String> {
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
) -> ToolResult<(String, Option<FsVersion>)> {
    let read = fs.read_file_versioned(path).await?;
    let bytes = read.bytes;
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
    let text = String::from_utf8(bytes).map_err(FsError::invalid_data)?;
    Ok((text, read.version))
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

    replacements.sort_by_key(|(index, _, _)| *index);
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
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::fs::{
        CreateDirectoryOptions, FileAccessPolicy, InMemoryFileSystem, ReadDirectoryEntry,
        VersionedRead,
    };

    fn path(path: &str) -> FsPath {
        FsPath::new(path).unwrap()
    }

    async fn filesystem_with(files: &[(&str, &str)]) -> InMemoryFileSystem {
        let fs = InMemoryFileSystem::full_access();
        for (file, contents) in files {
            let file = path(file);
            if let Some(parent) = file.parent()
                && !parent.is_root()
            {
                fs.create_directory(&parent, CreateDirectoryOptions::recursive())
                    .await
                    .unwrap();
            }
            fs.write_file(&file, contents.as_bytes().to_vec())
                .await
                .unwrap();
        }
        fs
    }

    async fn read(fs: &impl FileSystem, file: &str) -> String {
        fs.read_file_text(&path(file)).await.unwrap()
    }

    /// Versions every file and records each mutating call, failing the ones told to.
    #[derive(Clone)]
    struct Recording {
        inner: InMemoryFileSystem,
        calls: Arc<Mutex<Vec<String>>>,
        fail_on: Arc<Mutex<Vec<String>>>,
    }

    impl Recording {
        fn new(inner: InMemoryFileSystem) -> Self {
            Self {
                inner,
                calls: Arc::default(),
                fail_on: Arc::default(),
            }
        }

        fn failing_on(self, call: &str) -> Self {
            self.fail_on.lock().unwrap().push(call.to_owned());
            self
        }

        fn record(&self, call: String) -> FsResult<()> {
            self.calls.lock().unwrap().push(call.clone());
            if self.fail_on.lock().unwrap().contains(&call) {
                return Err(FsError::Failed {
                    message: format!("injected failure: {call}"),
                });
            }
            Ok(())
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl FileSystem for Recording {
        fn access_policy(&self) -> FileAccessPolicy {
            FileAccessPolicy::FullReadWrite
        }
        async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
            self.inner.read_file(path).await
        }
        async fn read_file_versioned(&self, path: &FsPath) -> FsResult<VersionedRead> {
            let bytes = self.inner.read_file(path).await?;
            Ok(VersionedRead {
                version: Some(FsVersion::new(String::from_utf8_lossy(&bytes))),
                bytes,
            })
        }
        async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
            self.write_file_with(path, contents, WriteOptions::default())
                .await
        }
        async fn write_file_with(
            &self,
            path: &FsPath,
            contents: Vec<u8>,
            options: WriteOptions,
        ) -> FsResult<()> {
            let condition = match &options.if_match {
                Some(version) => format!(" if {:?}", version.as_str()),
                None => String::new(),
            };
            self.record(format!("write {path}{condition}"))?;
            if let Some(version) = &options.if_match {
                let current = self.inner.read_file(path).await?;
                if String::from_utf8_lossy(&current) != version.as_str() {
                    return Err(FsError::Conflict { path: path.clone() });
                }
            }
            self.inner.write_file(path, contents).await
        }
        async fn create_directory(
            &self,
            path: &FsPath,
            options: CreateDirectoryOptions,
        ) -> FsResult<()> {
            self.inner.create_directory(path, options).await
        }
        async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
            self.inner.get_metadata(path).await
        }
        async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
            self.inner.read_directory(path).await
        }
        async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
            self.record(format!("remove {path}"))?;
            self.inner.remove(path, options).await
        }
        async fn rename(
            &self,
            source: &FsPath,
            destination: &FsPath,
            options: RenameOptions,
        ) -> FsResult<()> {
            self.record(format!(
                "rename {source} {destination} overwrite={}",
                options.overwrite
            ))?;
            self.inner.rename(source, destination, options).await
        }
    }

    const STALE_SECOND_HUNK: &str = r#"*** Begin Patch
*** Update File: /a.py
@@
 import os
+import sys
*** Update File: /b.py
@@
-not in the file
+replacement
*** End Patch"#;

    #[tokio::test(flavor = "current_thread")]
    async fn a_hunk_that_does_not_apply_leaves_every_file_unwritten() {
        let fs =
            Recording::new(filesystem_with(&[("/a.py", "import os\n"), ("/b.py", "real\n")]).await);

        let error = apply_patch_text(&fs, None, None, STALE_SECOND_HUNK)
            .await
            .expect_err("the second hunk does not apply");

        assert!(matches!(error, ToolError::InvalidRequest { .. }));
        assert!(fs.calls().is_empty(), "wrote: {:?}", fs.calls());
        assert_eq!(read(&fs, "/a.py").await, "import os\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resending_a_corrected_patch_applies_each_hunk_once() {
        let fs = filesystem_with(&[("/a.py", "import os\n"), ("/b.py", "real\n")]).await;
        apply_patch_text(&fs, None, None, STALE_SECOND_HUNK)
            .await
            .expect_err("the second hunk does not apply");

        let corrected = STALE_SECOND_HUNK.replace("-not in the file", "-real");
        apply_patch_text(&fs, None, None, &corrected)
            .await
            .expect("the corrected patch applies");

        assert_eq!(read(&fs, "/a.py").await, "import os\nimport sys\n");
        assert_eq!(read(&fs, "/b.py").await, "replacement\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_hunk_after_a_delete_keeps_the_file_so_the_retry_works() {
        let fs = filesystem_with(&[("/gone.txt", "bye\n"), ("/b.txt", "real\n")]).await;
        let patch = r#"*** Begin Patch
*** Delete File: /gone.txt
*** Update File: /b.txt
@@
-stale
+new
*** End Patch"#;

        apply_patch_text(&fs, None, None, patch)
            .await
            .expect_err("the update does not apply");
        assert_eq!(read(&fs, "/gone.txt").await, "bye\n");

        let summary = apply_patch_text(&fs, None, None, &patch.replace("-stale", "-real"))
            .await
            .expect("the retry applies");
        assert_eq!(summary.deleted, vec![path("/gone.txt")]);
        assert_eq!(read(&fs, "/b.txt").await, "new\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn two_hunks_on_one_file_write_once_with_the_first_version() {
        let fs = Recording::new(filesystem_with(&[("/a.txt", "one\ntwo\n")]).await);
        let patch = r#"*** Begin Patch
*** Update File: /a.txt
@@
-one
+uno
*** Update File: /a.txt
@@
-uno
+eins
@@
-two
+zwei
*** End Patch"#;

        let summary = apply_patch_text(&fs, None, None, patch).await.unwrap();

        assert_eq!(read(&fs, "/a.txt").await, "eins\nzwei\n");
        assert_eq!(fs.calls(), vec![r#"write /a.txt if "one\ntwo\n""#]);
        assert_eq!(summary.modified, vec![path("/a.txt")]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn later_hunks_see_what_earlier_ones_planned() {
        let fs = Recording::new(filesystem_with(&[("/old.txt", "line\n")]).await);
        let patch = r#"*** Begin Patch
*** Add File: /new/made.txt
+made
*** Update File: /new/made.txt
@@
-made
+remade
*** Update File: /old.txt
*** Move to: /moved.txt
@@
-line
+line2
*** Update File: /moved.txt
@@
-line2
+line3
*** End Patch"#;

        let summary = apply_patch_text(&fs, None, None, patch).await.unwrap();

        assert_eq!(read(&fs, "/new/made.txt").await, "remade\n");
        assert_eq!(read(&fs, "/moved.txt").await, "line3\n");
        assert_eq!(
            fs.calls(),
            vec![
                "write /new/made.txt".to_owned(),
                r#"write /old.txt if "line\n""#.to_owned(),
                "rename /old.txt /moved.txt overwrite=true".to_owned(),
            ],
            "one write per path, carried by the rename when the file moves"
        );
        assert_eq!(summary.added, vec![path("/new/made.txt")]);
        assert_eq!(summary.modified, vec![path("/moved.txt")]);

        let fs = filesystem_with(&[("/a.txt", "a\n")]).await;
        for refused in [
            "*** Begin Patch\n*** Delete File: /a.txt\n*** Update File: /a.txt\n@@\n-a\n+b\n*** End Patch",
            "*** Begin Patch\n*** Delete File: /a.txt\n*** Delete File: /a.txt\n*** End Patch",
            "*** Begin Patch\n*** Add File: /d/x.txt\n+x\n*** Add File: /d\n+y\n*** End Patch",
            "*** Begin Patch\n*** Add File: /f\n+x\n*** Add File: /f/x.txt\n+y\n*** End Patch",
        ] {
            apply_patch_text(&fs, None, None, refused)
                .await
                .expect_err(refused);
        }
        assert_eq!(read(&fs, "/a.txt").await, "a\n");
        assert!(fs.get_metadata(&path("/d")).await.is_err());
        assert!(fs.get_metadata(&path("/f")).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_deleted_file_can_become_a_directory_in_the_same_patch() {
        let fs = filesystem_with(&[("/a", "file\n")]).await;
        let patch = r#"*** Begin Patch
*** Delete File: /a
*** Add File: /a/b.txt
+child
*** End Patch"#;

        apply_patch_text(&fs, None, None, patch).await.unwrap();

        assert_eq!(read(&fs, "/a/b.txt").await, "child\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn adding_over_an_existing_file_replaces_it_and_reports_an_add() {
        let fs = Recording::new(filesystem_with(&[("/a.txt", "old\n")]).await);
        let patch = "*** Begin Patch\n*** Add File: /a.txt\n+new\n*** End Patch";

        let summary = apply_patch_text(&fs, None, None, patch).await.unwrap();

        assert_eq!(read(&fs, "/a.txt").await, "new\n");
        assert_eq!(fs.calls(), vec!["write /a.txt"], "unconditional");
        assert_eq!(
            summary.output(),
            "Success. Updated the following files:\nA /a.txt\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn moving_over_an_existing_file_replaces_it_and_reports_a_modification() {
        let fs = Recording::new(
            filesystem_with(&[("/old.txt", "line\n"), ("/taken.txt", "other\n")]).await,
        );
        let patch = r#"*** Begin Patch
*** Update File: /old.txt
*** Move to: /taken.txt
@@
-line
+line2
*** End Patch"#;

        let summary = apply_patch_text(&fs, None, None, patch).await.unwrap();

        assert_eq!(read(&fs, "/taken.txt").await, "line2\n");
        assert!(fs.get_metadata(&path("/old.txt")).await.is_err());
        assert_eq!(
            fs.calls().last().unwrap(),
            "rename /old.txt /taken.txt overwrite=true"
        );
        assert_eq!(
            summary.output(),
            "Success. Updated the following files:\nM /taken.txt\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_move_to_its_own_path_is_a_plain_update() {
        let fs = Recording::new(filesystem_with(&[("/a.txt", "line\n")]).await);
        let patch = "*** Begin Patch\n*** Update File: /a.txt\n*** Move to: /a.txt\n@@\n-line\n+line2\n*** End Patch";

        let summary = apply_patch_text(&fs, None, None, patch).await.unwrap();

        assert_eq!(read(&fs, "/a.txt").await, "line2\n");
        assert_eq!(fs.calls(), vec![r#"write /a.txt if "line\n""#]);
        assert_eq!(summary.modified, vec![path("/a.txt")]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unreachable_move_or_add_target_refuses_with_nothing_changed() {
        let fs = Recording::new(
            filesystem_with(&[
                ("/old.txt", "line\n"),
                ("/file", "not a directory\n"),
                ("/dir/inner.txt", "inner\n"),
            ])
            .await,
        );
        for destination in ["/dir", "/file/child.txt", "/file/deep/child.txt"] {
            let patch = format!(
                "*** Begin Patch\n*** Update File: /old.txt\n*** Move to: {destination}\n@@\n-line\n+line2\n*** End Patch"
            );
            let error = apply_patch_text(&fs, None, None, &patch)
                .await
                .expect_err(destination);
            assert!(
                matches!(error, ToolError::Filesystem(FsError::InvalidInput { .. })),
                "{destination}: {error}"
            );
        }
        for target in ["/dir", "/file/child.txt"] {
            let patch = format!("*** Begin Patch\n*** Add File: {target}\n+x\n*** End Patch");
            let error = apply_patch_text(&fs, None, None, &patch)
                .await
                .expect_err(target);
            assert!(
                matches!(error, ToolError::Filesystem(FsError::InvalidInput { .. })),
                "{target}: {error}"
            );
        }
        assert!(fs.calls().is_empty(), "wrote: {:?}", fs.calls());
        assert_eq!(read(&fs, "/old.txt").await, "line\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_missing_delete_target_refuses_before_any_write() {
        let fs = Recording::new(filesystem_with(&[("/a.txt", "a\n")]).await);
        let patch = r#"*** Begin Patch
*** Add File: /new.txt
+new
*** Delete File: /missing.txt
*** End Patch"#;

        let error = apply_patch_text(&fs, None, None, patch)
            .await
            .expect_err("nothing to delete");

        assert!(matches!(
            error,
            ToolError::Filesystem(FsError::NotFound { .. })
        ));
        assert!(fs.calls().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failure_during_commit_lists_what_landed_and_what_did_not() {
        let fs = Recording::new(
            filesystem_with(&[
                ("/a.txt", "a\n"),
                ("/b.txt", "b\n"),
                ("/c.txt", "c\n"),
                ("/gone.txt", "gone\n"),
            ])
            .await,
        )
        .failing_on(r#"write /b.txt if "b\n""#);
        let patch = r#"*** Begin Patch
*** Update File: /a.txt
@@
-a
+a2
*** Add File: /new.txt
+new
*** Delete File: /gone.txt
*** Update File: /b.txt
@@
-b
+b2
*** Update File: /c.txt
@@
-c
+c2
*** End Patch"#;

        let error = apply_patch_text(&fs, None, None, patch)
            .await
            .expect_err("the write of b fails");

        let message = error.to_string();
        let ToolError::PatchPartiallyApplied {
            applied,
            failed_path,
            not_applied,
            source,
        } = error
        else {
            panic!("expected a partial failure, got {message}");
        };
        assert_eq!(applied.modified, vec![path("/a.txt")]);
        assert_eq!(applied.added, vec![path("/new.txt")]);
        assert_eq!(applied.deleted, vec![path("/gone.txt")]);
        assert_eq!(failed_path, path("/b.txt"));
        assert_eq!(not_applied, vec!["update /b.txt", "update /c.txt"]);
        assert!(matches!(source, FsError::Failed { .. }));
        assert_eq!(
            message,
            "patch partly applied, stopped at /b.txt: filesystem failure: injected failure: write /b.txt if \"b\\n\"\n\
             Applied, do not resend these hunks:\nA /new.txt\nM /a.txt\nD /gone.txt\n\
             Not applied:\nupdate /b.txt\nupdate /c.txt"
        );
        assert_eq!(read(&fs, "/a.txt").await, "a2\n");
        assert_eq!(read(&fs, "/c.txt").await, "c\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_rename_reports_the_updated_source_and_the_move_not_done() {
        let fs = Recording::new(filesystem_with(&[("/old.rs", "old\n")]).await)
            .failing_on("rename /old.rs /new.rs overwrite=true");
        let patch = r#"*** Begin Patch
*** Update File: /old.rs
*** Move to: /new.rs
@@
-old
+new
*** End Patch"#;

        let error = apply_patch_text(&fs, None, None, patch)
            .await
            .expect_err("the rename fails");

        let ToolError::PatchPartiallyApplied {
            applied,
            failed_path,
            not_applied,
            ..
        } = error
        else {
            panic!("expected a partial failure");
        };
        assert_eq!(applied.modified, vec![path("/old.rs")]);
        assert_eq!(failed_path, path("/new.rs"));
        assert_eq!(not_applied, vec!["move /old.rs to /new.rs"]);
        assert_eq!(read(&fs, "/old.rs").await, "new\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_update_conflict_at_commit_is_a_conflict() {
        // Nothing landed yet, so the plain conflict tells the whole story.
        let fs = Recording::new(filesystem_with(&[("/a.txt", "a\n")]).await);
        let plan = plan_hunks(
            &fs,
            None,
            None,
            &parse_patch("*** Begin Patch\n*** Update File: /a.txt\n@@\n-a\n+b\n*** End Patch")
                .unwrap()
                .hunks,
        )
        .await
        .unwrap();
        fs.inner
            .write_file(&path("/a.txt"), b"changed meanwhile\n".to_vec())
            .await
            .unwrap();

        let error = commit_steps(&fs, plan.steps)
            .await
            .expect_err("the file changed after planning");

        assert!(matches!(
            error,
            ToolError::Filesystem(FsError::Conflict { .. })
        ));
        assert_eq!(read(&fs, "/a.txt").await, "changed meanwhile\n");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn only_an_update_write_carries_a_version() {
        let fs =
            Recording::new(filesystem_with(&[("/u.txt", "u\n"), ("/existing.txt", "e\n")]).await);
        let patch = r#"*** Begin Patch
*** Add File: /existing.txt
+replaced
*** Update File: /u.txt
@@
-u
+u2
*** End Patch"#;

        apply_patch_text(&fs, None, None, patch).await.unwrap();

        assert_eq!(
            fs.calls(),
            vec!["write /existing.txt", r#"write /u.txt if "u\n""#]
        );
    }

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
