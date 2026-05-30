//! Canonical apply-patch operation.
//!
//! Parsing and filesystem application internals live in `crate::host::apply_patch`.

use serde::{Deserialize, Serialize};

use crate::{
    error::ToolResult,
    host::apply_patch::{ApplyPatchSummary, apply_patch_text},
    host::{context::HostToolContext, fs::FsPath},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplyPatchArgs {
    pub patch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplyPatchResult {
    pub added: Vec<FsPath>,
    pub modified: Vec<FsPath>,
    pub deleted: Vec<FsPath>,
    pub output: String,
}

impl From<ApplyPatchSummary> for ApplyPatchResult {
    fn from(summary: ApplyPatchSummary) -> Self {
        Self {
            output: summary.output(),
            added: summary.added,
            modified: summary.modified,
            deleted: summary.deleted,
        }
    }
}

pub async fn invoke_apply_patch(
    ctx: &HostToolContext,
    args: ApplyPatchArgs,
) -> ToolResult<ApplyPatchResult> {
    apply_patch_text(
        ctx.fs.as_ref(),
        ctx.cwd.as_ref(),
        Some(ctx.limits.max_file_read_bytes),
        &args.patch,
    )
    .await
    .map(Into::into)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::storage::InMemoryBlobStore;

    use super::*;
    use crate::host::fs::{FileSystem, InMemoryFileSystem};

    fn context(fs: Arc<dyn FileSystem>) -> HostToolContext {
        HostToolContext::new(fs, None, Arc::new(InMemoryBlobStore::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invoke_apply_patch_returns_summary_output() {
        let fs = InMemoryFileSystem::full_access();
        let ctx = context(Arc::new(fs.clone())).with_cwd(FsPath::new("/workspace").unwrap());
        let patch = r#"*** Begin Patch
*** Add File: hello.txt
+hello
*** End Patch"#;

        let result = invoke_apply_patch(
            &ctx,
            ApplyPatchArgs {
                patch: patch.to_string(),
            },
        )
        .await
        .expect("apply patch");

        assert_eq!(
            result.added,
            vec![FsPath::new("/workspace/hello.txt").unwrap()]
        );
        assert_eq!(
            result.output,
            "Success. Updated the following files:\nA /workspace/hello.txt\n"
        );
        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/hello.txt").unwrap())
                .await
                .expect("read file"),
            "hello\n"
        );
    }
}
