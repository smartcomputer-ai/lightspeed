//! Transfer state machine tests against the handlers, without sockets.

use std::{os::unix::fs::symlink, path::Path};

use environment_protocol::{
    data::{
        fs::{TreeContent, TreeFilter, WriteCondition},
        transfer::{
            AppendTransferInventoryParams, BeginTransferParams, InventoryEntry,
            ListTransferMissingParams, ReadTransferContentParams, ReadTransferInventoryParams,
            SpecialFilePolicy, SymlinkPolicy, TransferLimits, TransferOnExisting,
            TransferOperationParams, TransferPhase, TransferSelection, TransferSkipReason,
            TransferStatus, WriteTransferContentParams,
        },
    },
    error::{
        AbortReason, BusyReason, ErrorData, PreconditionReason, ProtocolError, ProtocolErrorKind,
    },
    ids::TransferOperationId,
    paths::{ContentDigest, EnvironmentPath, TreePath},
};
use sha2::{Digest, Sha256};

use crate::filesystem::{
    LocalFileSystem,
    tests::{context, filesystem},
};

fn id(value: &str) -> TransferOperationId {
    TransferOperationId::new(value).unwrap()
}

fn path(root: &Path, relative: &str) -> EnvironmentPath {
    EnvironmentPath::new(format!("{}/{relative}", root.display())).unwrap()
}

fn limits() -> TransferLimits {
    TransferLimits {
        max_entries: 1000,
        max_depth: 16,
        max_file_bytes: 64 * 1024 * 1024,
        max_total_bytes: 256 * 1024 * 1024,
        max_manifest_bytes: 1024 * 1024,
        max_duration_ms: 600_000,
    }
}

fn digest(bytes: &[u8]) -> ContentDigest {
    ContentDigest::from_sha256(Sha256::digest(bytes).into())
}

fn operation(value: &str) -> TransferOperationParams {
    TransferOperationParams {
        operation_id: id(value),
    }
}

fn capture(source: EnvironmentPath, filter: TreeFilter) -> TransferSelection {
    TransferSelection::Capture {
        source,
        filter,
        symlinks: SymlinkPolicy::ExpandInside,
        special_files: SpecialFilePolicy::Reject,
    }
}

async fn begin(
    fs: &LocalFileSystem,
    operation_id: &str,
    selection: TransferSelection,
) -> Result<TransferStatus, ProtocolError> {
    fs.transfer_begin(
        &context(),
        BeginTransferParams {
            operation_id: id(operation_id),
            selection,
            limits: limits(),
        },
    )
    .await
}

async fn advance_until(
    fs: &LocalFileSystem,
    operation_id: &str,
    phase: TransferPhase,
) -> Result<TransferStatus, ProtocolError> {
    for _ in 0..10_000 {
        let status = fs
            .transfer_advance(&context(), operation(operation_id))
            .await?;
        if status.phase == phase {
            return Ok(status);
        }
    }
    panic!("{operation_id} never reached {phase:?}");
}

async fn inventory(fs: &LocalFileSystem, operation_id: &str) -> Vec<InventoryEntry> {
    let mut entries = Vec::new();
    let mut index = 0;
    loop {
        let page = fs
            .transfer_read_inventory(
                &context(),
                ReadTransferInventoryParams {
                    operation_id: id(operation_id),
                    index,
                    limit: 2,
                },
            )
            .await
            .unwrap();
        entries.extend(page.entries);
        match page.next_index {
            Some(next) => index = next,
            None => return entries,
        }
    }
}

fn reason(error: &ProtocolError) -> Option<PreconditionReason> {
    match error.data {
        ErrorData::FailedPrecondition { reason } => Some(reason),
        _ => None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn captures_expand_inside_links_skip_ignored_trees_and_serve_ranged_content() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fs = filesystem(&root, false);
    std::fs::create_dir_all(root.join("project/src")).unwrap();
    std::fs::create_dir_all(root.join("project/node_modules/.bin")).unwrap();
    std::fs::create_dir_all(root.join("project/.git")).unwrap();
    std::fs::write(root.join("project/.gitignore"), "node_modules/\n").unwrap();
    std::fs::write(root.join("project/src/lib.rs"), b"0123456789").unwrap();
    std::fs::write(root.join("project/.git/HEAD"), b"ref").unwrap();
    symlink("src/lib.rs", root.join("project/alias.rs")).unwrap();
    symlink(
        "../src/missing",
        root.join("project/node_modules/.bin/tool"),
    )
    .unwrap();

    let filter = TreeFilter {
        include_hidden: true,
        ..TreeFilter::default()
    };
    begin(&fs, "capture", capture(path(&root, "project"), filter))
        .await
        .unwrap();
    let status = advance_until(&fs, "capture", TransferPhase::Inventory)
        .await
        .unwrap();
    let entries = inventory(&fs, "capture").await;
    let paths: Vec<_> = entries.iter().map(|entry| entry.path.as_str()).collect();
    // The .git directory is always pruned and node_modules is ignored.
    assert_eq!(paths, [".", ".gitignore", "alias.rs", "src", "src/lib.rs"]);
    assert_eq!(status.entry_count, 5);
    let content = digest(b"0123456789");
    assert!(entries.iter().any(|entry| matches!(
        entry.content,
        TreeContent::File { digest: Some(found), .. } if found == content
    )));
    let first = fs
        .transfer_read(
            &context(),
            ReadTransferContentParams {
                operation_id: id("capture"),
                digest: content,
                offset: 0,
                max_bytes: 4,
            },
        )
        .await
        .unwrap();
    assert_eq!(first.data.as_slice(), b"0123");
    assert!(first.truncated);
    let rest = fs
        .transfer_read(
            &context(),
            ReadTransferContentParams {
                operation_id: id("capture"),
                digest: content,
                offset: 4,
                max_bytes: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.data.as_slice(), b"456789");
    assert!(!rest.truncated);
    let committed = fs
        .transfer_commit(&context(), operation("capture"))
        .await
        .unwrap();
    assert_eq!(committed.phase, TransferPhase::Complete);
    assert!(committed.receipt_expires_at_ms.is_some());
    assert!(committed.deadline_at_ms.is_none());
    // The receipt answers a retried commit and begin from the journal.
    assert_eq!(
        fs.transfer_commit(&context(), operation("capture"))
            .await
            .unwrap()
            .phase,
        TransferPhase::Complete
    );
    let error = begin(
        &fs,
        "capture",
        capture(path(&root, "other"), TreeFilter::default()),
    )
    .await
    .unwrap_err();
    assert_eq!(reason(&error), Some(PreconditionReason::OperationIdInUse));

    // The completed capture's inventory stays pageable with its receipt,
    // after the source changed and across a restart; its content does not.
    std::fs::remove_file(root.join("project/src/lib.rs")).unwrap();
    assert_eq!(inventory(&fs, "capture").await, entries);
    drop(fs);
    let fs = filesystem(&root, false);
    assert_eq!(inventory(&fs, "capture").await, entries);
    let error = fs
        .transfer_read(
            &context(),
            ReadTransferContentParams {
                operation_id: id("capture"),
                digest: content,
                offset: 0,
                max_bytes: 4,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(reason(&error), Some(PreconditionReason::TransferPhase));
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_capture_answers_its_first_error_and_never_commits() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = base.join("root");
    std::fs::create_dir_all(root.join("tree/a")).unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
    std::fs::write(root.join("tree/a/file"), b"a").unwrap();
    symlink(base.join("outside"), root.join("tree/escape")).unwrap();
    std::fs::write(root.join("tree/z"), b"z").unwrap();
    let fs = filesystem(&root, false);

    begin(
        &fs,
        "escape",
        capture(path(&root, "tree"), TreeFilter::default()),
    )
    .await
    .unwrap();
    let first = advance_until(&fs, "escape", TransferPhase::Inventory)
        .await
        .unwrap_err();
    assert_eq!(reason(&first), Some(PreconditionReason::SymlinkRejected));
    // Retries never continue the walk past the failure.
    for _ in 0..3 {
        let again = fs
            .transfer_advance(&context(), operation("escape"))
            .await
            .unwrap_err();
        assert_eq!(again, first);
    }
    let error = fs
        .transfer_commit(&context(), operation("escape"))
        .await
        .unwrap_err();
    assert_eq!(error, first);
    let status = fs
        .transfer_get_status(&context(), operation("escape"))
        .await
        .unwrap();
    assert_eq!(status.phase, TransferPhase::Failed);
    assert_eq!(status.error.as_ref(), Some(&first));
    let aborted = fs
        .transfer_abort(&context(), operation("escape"))
        .await
        .unwrap();
    assert_eq!(aborted.phase, TransferPhase::Failed);

    // A source that changes after hashing fails the operation at read time.
    std::fs::remove_file(root.join("tree/escape")).unwrap();
    begin(
        &fs,
        "changed",
        capture(path(&root, "tree"), TreeFilter::default()),
    )
    .await
    .unwrap();
    advance_until(&fs, "changed", TransferPhase::Inventory)
        .await
        .unwrap();
    std::fs::write(root.join("tree/z"), b"zz").unwrap();
    let error = fs
        .transfer_read(
            &context(),
            ReadTransferContentParams {
                operation_id: id("changed"),
                digest: digest(b"z"),
                offset: 0,
                max_bytes: 10,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.data,
        ErrorData::Aborted {
            reason: Some(AbortReason::SourceChanged)
        }
    ));
    let error = fs
        .transfer_commit(&context(), operation("changed"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::Aborted);
}

#[tokio::test(flavor = "current_thread")]
async fn entries_outside_the_selection_never_fail_a_capture() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = base.join("root");
    std::fs::create_dir_all(root.join("tree/src")).unwrap();
    std::fs::create_dir_all(root.join("tree/tmp")).unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
    std::fs::write(root.join("tree/src/lib.rs"), b"lib").unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(root.join("tree/tmp/sock")).unwrap();
    symlink(base.join("outside"), root.join("tree/tmp/escape")).unwrap();
    symlink("missing", root.join("tree/tmp/dangling")).unwrap();
    let fs = filesystem(&root, false);

    let filter = TreeFilter {
        include: vec!["src/**".to_owned()],
        ..TreeFilter::default()
    };
    begin(&fs, "selected", capture(path(&root, "tree"), filter))
        .await
        .unwrap();
    let status = advance_until(&fs, "selected", TransferPhase::Inventory)
        .await
        .unwrap();
    assert_eq!(status.skipped_entry_count, 0, "{:?}", status.skipped);
    let entries = inventory(&fs, "selected").await;
    assert!(
        entries
            .iter()
            .any(|entry| entry.path.as_str() == "src/lib.rs")
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry.path.as_str().starts_with("tmp/"))
    );

    // The same entries fail a capture that selects them.
    begin(
        &fs,
        "everything",
        capture(path(&root, "tree"), TreeFilter::default()),
    )
    .await
    .unwrap();
    let error = advance_until(&fs, "everything", TransferPhase::Inventory)
        .await
        .unwrap_err();
    assert!(matches!(
        reason(&error),
        Some(PreconditionReason::SymlinkRejected | PreconditionReason::NotRegularFile)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn skip_policies_report_what_a_capture_left_out() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("tree")).unwrap();
    std::fs::write(root.join("tree/file"), b"f").unwrap();
    symlink("file", root.join("tree/link")).unwrap();
    let fs = filesystem(&root, false);
    begin(
        &fs,
        "skips",
        TransferSelection::Capture {
            source: path(&root, "tree"),
            filter: TreeFilter::default(),
            symlinks: SymlinkPolicy::Skip,
            special_files: SpecialFilePolicy::Skip,
        },
    )
    .await
    .unwrap();
    let status = advance_until(&fs, "skips", TransferPhase::Inventory)
        .await
        .unwrap();
    assert_eq!(status.skipped_entry_count, 1);
    assert_eq!(status.skipped[0].reason, TransferSkipReason::Symlink);
    assert_eq!(
        status.skipped[0].path.as_ref().map(TreePath::as_str),
        Some("link")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn finished_operations_free_capacity_and_receipts_expire_from_completion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("file"), b"x").unwrap();
    let fs = filesystem(&root, false);
    // Twice the active ceiling of eight, each finished before the next.
    for index in 0..16 {
        let name = format!("done-{index}");
        begin(
            &fs,
            &name,
            capture(path(&root, "file"), TreeFilter::default()),
        )
        .await
        .unwrap();
        advance_until(&fs, &name, TransferPhase::Inventory)
            .await
            .unwrap();
        fs.transfer_commit(&context(), operation(&name))
            .await
            .unwrap();
    }
    for index in 0..8 {
        begin(
            &fs,
            &format!("active-{index}"),
            capture(path(&root, "file"), TreeFilter::default()),
        )
        .await
        .unwrap();
    }
    let error = begin(
        &fs,
        "one-more",
        capture(path(&root, "file"), TreeFilter::default()),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error.data,
        ErrorData::Busy {
            reason: Some(BusyReason::TransferCapacity),
            ..
        }
    ));
    fs.transfer_abort(&context(), operation("active-0"))
        .await
        .unwrap();
    begin(
        &fs,
        "one-more",
        capture(path(&root, "file"), TreeFilter::default()),
    )
    .await
    .unwrap();
}

async fn append_all(fs: &LocalFileSystem, operation_id: &str, entries: Vec<InventoryEntry>) {
    let status = fs
        .transfer_append(
            &context(),
            AppendTransferInventoryParams {
                operation_id: id(operation_id),
                index: 0,
                entries,
                last: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(status.phase, TransferPhase::Scanning);
}

fn file_entry(path: &str, bytes: &[u8], executable: bool) -> InventoryEntry {
    InventoryEntry {
        path: TreePath::new(path).unwrap(),
        content: TreeContent::File {
            size_bytes: bytes.len() as u64,
            executable,
            digest: Some(digest(bytes)),
        },
    }
}

fn directory_entry(path: &str) -> InventoryEntry {
    InventoryEntry {
        path: TreePath::new(path).unwrap(),
        content: TreeContent::Directory {},
    }
}

#[tokio::test(flavor = "current_thread")]
async fn materialize_reuses_matching_destination_files_and_publishes_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("target/old")).unwrap();
    std::fs::write(root.join("target/old/kept"), b"reused bytes").unwrap();
    std::fs::write(root.join("target/unrelated"), vec![7u8; 100_000]).unwrap();
    let fs = filesystem(&root, true);

    let error = begin(
        &fs,
        "refused",
        TransferSelection::Materialize {
            destination: path(&root, "target"),
            on_existing: TransferOnExisting::Error,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::AlreadyExists);

    begin(
        &fs,
        "tree",
        TransferSelection::Materialize {
            destination: path(&root, "target"),
            on_existing: TransferOnExisting::Replace,
        },
    )
    .await
    .unwrap();
    // A file entry without its digest is refused and changes nothing.
    let error = fs
        .transfer_append(
            &context(),
            AppendTransferInventoryParams {
                operation_id: id("tree"),
                index: 0,
                entries: vec![
                    directory_entry("."),
                    InventoryEntry {
                        path: TreePath::new("bad").unwrap(),
                        content: TreeContent::File {
                            size_bytes: 1,
                            executable: false,
                            digest: None,
                        },
                    },
                ],
                last: true,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        &error.data,
        ErrorData::InvalidParams { field: Some(field), .. } if field == "entries[1].content.digest"
    ));
    let uploaded = b"uploaded content".to_vec();
    append_all(
        &fs,
        "tree",
        vec![
            directory_entry("."),
            directory_entry("bin"),
            file_entry("bin/run", &uploaded, true),
            file_entry("copy", b"reused bytes", false),
            file_entry("empty", b"", false),
        ],
    )
    .await;
    advance_until(&fs, "tree", TransferPhase::Content)
        .await
        .unwrap();
    let missing = fs
        .transfer_list_missing(
            &context(),
            ListTransferMissingParams {
                operation_id: id("tree"),
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert_eq!(missing.digests, [digest(&uploaded)]);
    assert_eq!(missing.remaining_count, 0);
    let write = |offset: u64, data: &[u8]| WriteTransferContentParams {
        operation_id: id("tree"),
        digest: digest(&uploaded),
        offset,
        data: data.into(),
    };
    let next = fs
        .transfer_write(&context(), write(0, &uploaded[..8]))
        .await
        .unwrap()
        .next_offset;
    assert_eq!(next, 8);
    // An identical repeat is accepted; a gap is refused without failing.
    assert_eq!(
        fs.transfer_write(&context(), write(0, &uploaded[..8]))
            .await
            .unwrap()
            .next_offset,
        8
    );
    let error = fs
        .transfer_write(&context(), write(10, &uploaded[10..]))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), Some(PreconditionReason::TransferOffset));
    fs.transfer_write(&context(), write(8, &uploaded[8..]))
        .await
        .unwrap();
    let ready = advance_until(&fs, "tree", TransferPhase::Ready)
        .await
        .unwrap();
    assert_eq!(ready.reused_bytes, b"reused bytes".len() as u64);
    fs.transfer_commit(&context(), operation("tree"))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(root.join("target/bin/run")).unwrap(),
        uploaded
    );
    assert_eq!(
        std::fs::read(root.join("target/copy")).unwrap(),
        b"reused bytes"
    );
    assert!(root.join("target/empty").exists());
    assert!(!root.join("target/unrelated").exists());
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(root.join("target/bin/run"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
}

#[tokio::test(flavor = "current_thread")]
async fn a_digest_mismatch_fails_the_operation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fs = filesystem(&root, true);
    begin(
        &fs,
        "mismatch",
        TransferSelection::Materialize {
            destination: path(&root, "out"),
            on_existing: TransferOnExisting::Replace,
        },
    )
    .await
    .unwrap();
    append_all(&fs, "mismatch", vec![file_entry(".", b"expected", false)]).await;
    advance_until(&fs, "mismatch", TransferPhase::Content)
        .await
        .unwrap();
    let error = fs
        .transfer_write(
            &context(),
            WriteTransferContentParams {
                operation_id: id("mismatch"),
                digest: digest(b"expected"),
                offset: 0,
                data: b"tampered".as_slice().into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(reason(&error), Some(PreconditionReason::DigestMismatch));
    let status = fs
        .transfer_get_status(&context(), operation("mismatch"))
        .await
        .unwrap();
    assert_eq!(status.phase, TransferPhase::Failed);
    assert!(!root.join("out").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn transfer_write_file_follows_fs_write_file_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("target"), b"old").unwrap();
    symlink("target", root.join("link")).unwrap();
    let fs = filesystem(&root, true);
    let selection = |create: WriteCondition| TransferSelection::WriteFile {
        destination: path(&root, "link"),
        create,
        if_match: None,
        executable: false,
    };
    let error = begin(&fs, "absent", selection(WriteCondition::IfAbsent))
        .await
        .unwrap_err();
    assert_eq!(reason(&error), Some(PreconditionReason::CreateCondition));
    begin(&fs, "write", selection(WriteCondition::IfPresent))
        .await
        .unwrap();
    append_all(&fs, "write", vec![file_entry(".", b"new content", false)]).await;
    advance_until(&fs, "write", TransferPhase::Content)
        .await
        .unwrap();
    fs.transfer_write(
        &context(),
        WriteTransferContentParams {
            operation_id: id("write"),
            digest: digest(b"new content"),
            offset: 0,
            data: b"new content".as_slice().into(),
        },
    )
    .await
    .unwrap();
    advance_until(&fs, "write", TransferPhase::Ready)
        .await
        .unwrap();
    fs.transfer_commit(&context(), operation("write"))
        .await
        .unwrap();
    assert_eq!(std::fs::read(root.join("target")).unwrap(), b"new content");
    assert!(
        std::fs::symlink_metadata(root.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_daemons_refuse_publishes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fs = filesystem(&root, false);
    let error = begin(
        &fs,
        "denied",
        TransferSelection::Materialize {
            destination: path(&root, "out"),
            on_existing: TransferOnExisting::Replace,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::CapabilityUnavailable);
}

#[tokio::test(flavor = "current_thread")]
async fn a_restart_fails_active_operations_and_discards_their_staging() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let selection = TransferSelection::Materialize {
        destination: path(&root, "out"),
        on_existing: TransferOnExisting::Replace,
    };
    {
        let fs = filesystem(&root, true);
        begin(&fs, "interrupted", selection.clone()).await.unwrap();
        let staged = std::fs::read_dir(&root).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".lightspeed-transfer-")
        });
        assert!(staged);
    }
    let fs = filesystem(&root, true);
    let status = fs
        .transfer_get_status(&context(), operation("interrupted"))
        .await
        .unwrap();
    assert_eq!(status.phase, TransferPhase::Failed);
    assert_eq!(
        status.error.as_ref().map(ProtocolError::kind),
        Some(ProtocolErrorKind::Aborted)
    );
    let error = begin(&fs, "interrupted", selection).await.unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::Aborted);
    // Staging cleanup of the dropped instance and recovery both remove it.
    for _ in 0..100 {
        let staged = std::fs::read_dir(&root).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".lightspeed-transfer-")
        });
        if !staged {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("staging survived the restart");
}

#[tokio::test(flavor = "current_thread")]
async fn operations_fail_when_their_duration_runs_out() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("file"), b"x").unwrap();
    let fs = filesystem(&root, false);
    fs.transfer_begin(
        &context(),
        BeginTransferParams {
            operation_id: id("short"),
            selection: capture(path(&root, "file"), TreeFilter::default()),
            limits: TransferLimits {
                max_duration_ms: 1,
                ..limits()
            },
        },
    )
    .await
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let error = fs
        .transfer_advance(&context(), operation("short"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProtocolErrorKind::DeadlineExceeded);
    let status = fs
        .transfer_get_status(&context(), operation("short"))
        .await
        .unwrap();
    assert_eq!(status.phase, TransferPhase::Failed);
}

#[tokio::test(flavor = "current_thread")]
async fn a_retried_sealing_append_answers_the_status() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fs = filesystem(&root, true);
    begin(
        &fs,
        "sealed",
        TransferSelection::Materialize {
            destination: path(&root, "out"),
            on_existing: TransferOnExisting::Replace,
        },
    )
    .await
    .unwrap();
    let entries = vec![directory_entry("."), file_entry("file", b"bytes", false)];
    append_all(&fs, "sealed", entries.clone()).await;
    // The response to the sealing page was lost; the client sends it again.
    let retried = fs
        .transfer_append(
            &context(),
            AppendTransferInventoryParams {
                operation_id: id("sealed"),
                index: 0,
                entries,
                last: true,
            },
        )
        .await
        .unwrap();
    assert_ne!(retried.phase, TransferPhase::Inventory);
    assert_eq!(retried.entry_count, 2);
    let error = fs
        .transfer_append(
            &context(),
            AppendTransferInventoryParams {
                operation_id: id("sealed"),
                index: 0,
                entries: vec![directory_entry(".")],
                last: true,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(reason(&error), Some(PreconditionReason::TransferPhase));
}

#[tokio::test(flavor = "current_thread")]
async fn a_refused_begin_and_an_abort_leave_no_created_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fs = filesystem(&root, true);
    let materialize = |destination: &str| TransferSelection::Materialize {
        destination: path(&root, destination),
        on_existing: TransferOnExisting::Replace,
    };
    begin(&fs, "outer", materialize("a/b")).await.unwrap();
    assert!(root.join("a").is_dir(), "begin creates missing parents");
    let error = begin(&fs, "inner", materialize("a/b/c/d"))
        .await
        .unwrap_err();
    assert!(matches!(
        error.data,
        ErrorData::Busy {
            reason: Some(BusyReason::TransferCapacity),
            ..
        }
    ));
    assert!(
        !root.join("a/b").exists(),
        "a refused begin changes nothing"
    );

    fs.transfer_abort(&context(), operation("outer"))
        .await
        .unwrap();
    for _ in 0..100 {
        if !root.join("a").exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the aborted begin's parent directory survived");
}

#[tokio::test(flavor = "current_thread")]
async fn a_receipt_names_its_staging_until_removal_is_confirmed() {
    use super::journal::{self, Record, StageRecord};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let selection = TransferSelection::Materialize {
        destination: path(&root, "out"),
        on_existing: TransferOnExisting::Replace,
    };
    let status = {
        let fs = filesystem(&root, true);
        begin(&fs, "done", selection.clone()).await.unwrap();
        fs.transfer_abort(&context(), operation("done"))
            .await
            .unwrap()
    };
    assert_eq!(status.phase, TransferPhase::Aborted);
    // An exit before the background removal finished: the stage is still
    // there and the receipt names it.
    let stage = ".lightspeed-transfer-0123456789abcdef0123456789abcdef";
    std::fs::create_dir_all(root.join(stage).join("tree/replaced")).unwrap();
    let journal = root.join(".state/transfers");
    journal::write(
        &journal,
        &id("done"),
        &Record {
            params: BeginTransferParams {
                operation_id: id("done"),
                selection,
                limits: limits(),
            },
            status: status.clone(),
            stage: Some(StageRecord::new(&[], stage)),
            spool: None,
            committing: false,
        },
    )
    .unwrap();

    let fs = filesystem(&root, true);
    assert!(!root.join(stage).exists(), "recovery removes the stage");
    let record = journal::read(&journal, &id("done")).unwrap().unwrap();
    assert!(record.stage.is_none());
    let answered = fs
        .transfer_get_status(&context(), operation("done"))
        .await
        .unwrap();
    assert_eq!(answered.phase, TransferPhase::Aborted);
}
